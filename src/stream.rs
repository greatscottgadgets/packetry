#![deny(unsafe_op_in_unsafe_fn)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cmp::min;
use std::fs::File;
use std::io::Write;
use std::ops::{Deref, Range};
use std::ptr::copy_nonoverlapping;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Release}};

use anyhow::{Context, Error, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use lrumap::LruBTreeMap;
use memmap2::{Mmap, MmapOptions};
use tempfile::tempfile;

/// Minimum block size, defined by largest minimum page size on target systems.
pub const MIN_BLOCK: usize = 0x4000; // 16KB (Apple M1/M2)

/// Private data shared by the writer and multiple readers.
struct Shared<const S: usize> {
    /// Available length of the stream, including data in both file and buffer.
    length: AtomicU64,
    /// File handle used by readers to create mappings.
    file: ArcSwapOption<File>,
    /// Buffer currently in use for newly appended data.
    current_buffer: ArcSwap<Buffer<S>>,
}

/// Unique handle for append-only write access to a stream.
pub struct StreamWriter<const S: usize = MIN_BLOCK> {
    /// Shared data.
    shared: Arc<Shared<S>>,
    /// Total length of the stream.
    length: u64,
    /// Pointer to start of the buffer currently in use.
    buf: *mut u8,
    /// Pointer to current position in the current buffer.
    ptr: *mut u8,
    /// File handle used to append to the stream.
    file: Option<File>,
    /// Spare buffer to be potentially used when current buffer is full.
    spare_buffer: Option<Arc<Buffer<S>>>,
}

/// Cloneable handle for read-only random access to a stream.
pub struct StreamReader<const S: usize = MIN_BLOCK> {
    /// Shared data.
    shared: Arc<Shared<S>>,
    /// Cache of existing mappings into the file.
    mappings: LruBTreeMap<u64, Arc<Mmap>>,
}

/// Data that is part of a stream and currently in memory.
struct Buffer<const S: usize> {
    /// Block to which this data belongs.
    block_base: u64,
    /// Raw pointer to space allocated to hold the data.
    ptr: *mut u8,
}

/// A read-only handle to any data that is part of a stream.
enum Data<const S: usize> {
    /// Data in the file, accessed through a mapping.
    Mapped(Arc<Mmap>, Range<usize>),
    /// Data in memory, accessed within a buffer.
    Buffered(Arc<Buffer<S>>, Range<usize>),
}

// Number of most recent file mappings retained by each reader.
const MAP_CACHE_PER_READER: usize = 4;

type StreamPair<const S: usize> = (StreamWriter<S>, StreamReader<S>);

/// Construct a new stream.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn stream<const BLOCK_SIZE: usize>()
    -> Result<StreamPair<BLOCK_SIZE>, Error>
{
    let page_size = page_size::get();
    if BLOCK_SIZE < page_size {
        bail!("Block size {BLOCK_SIZE:x} is not a multiple \
               of the system page size {page_size:x}")
    }
    let buffer = Arc::new(Buffer::new(0)?);
    let shared = Arc::new(Shared {
        length: AtomicU64::from(0),
        file: ArcSwapOption::empty(),
        current_buffer: ArcSwap::new(buffer.clone()),
    });
    let writer = StreamWriter {
        shared: shared.clone(),
        length: 0,
        buf: buffer.ptr,
        ptr: buffer.ptr,
        file: None,
        spare_buffer: None,
    };
    let reader = StreamReader {
        shared,
        mappings: LruBTreeMap::new(MAP_CACHE_PER_READER),
    };
    Ok((writer, reader))
}

impl<const BLOCK_SIZE: usize> StreamWriter<BLOCK_SIZE> {
    /// Get the current length of the stream, in bytes.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Append data to the end of the stream.
    ///
    /// Returns the new stream length.
    ///
    pub fn append(&mut self, mut data: &[u8]) -> Result<u64, Error> {
        let length = data.len();
        let buffered = self.length as usize & Self::block_mask();
        if buffered + length <= Self::block_size() {
            // All the data will fit in the existing buffer.
            unsafe { self.write_to_buffer(data, length) };
            if self.length as usize & Self::block_mask() == 0 {
                // Buffer is now full, and can be written to file.
                unsafe { self.write_buffer_to_file()? };
            }
        } else {
            // The data will fill the existing buffer.
            if buffered > 0 {
                // The buffer is partly used. Fill it and write it out.
                let length = Self::block_size() - buffered;
                unsafe { self.write_to_buffer(data, length) };
                // Buffer is now full, and can be written to file.
                unsafe { self.write_buffer_to_file()? };
                data = &data[length..];
            }
            // The buffer is curently empty, so we are free to write whole
            // blocks of data directly to the file, bypassing the buffer.
            let direct = data.len() & !Self::block_mask();
            if direct > 0 {
                unsafe { self.write_to_file(&data[..direct])? };
                data = &data[direct..];
                self.length += direct as u64;
            }
            let length = data.len();
            if length > 0 {
                // There is data remaining which will fit in the next buffer.
                unsafe { self.write_to_buffer(data, length) };
            }
        }
        // Update shared length value, and return new length.
        self.shared.length.store(self.length, Release);
        Ok(self.length)
    }

    /// Helper method for writing data to buffer.
    ///
    /// Safety: The data must fit within the space remaining in the buffer.
    ///
    #[inline(always)]
    unsafe fn write_to_buffer(&mut self, data: &[u8], length: usize) {
        unsafe {
            copy_nonoverlapping(data.as_ptr(), self.ptr, length);
            self.ptr = self.ptr.add(length);
        }
        self.length += length as u64;
    }

    /// Helper method for writing buffer to file.
    ///
    /// Safety: The buffer must be full.
    ///
    #[inline(always)]
    unsafe fn write_buffer_to_file(&mut self) -> Result<(), Error> {
        unsafe {
            let buf = slice::from_raw_parts(self.buf, Self::block_size());
            self.write_to_file(buf)
        }
    }

    /// Helper method for writing data to file.
    ///
    /// Safety: The data must be a multiple of the block size.
    ///
    unsafe fn write_to_file(&mut self, data: &[u8]) -> Result<(), Error> {

        // Create the file if it does not exist yet.
        let file = match &mut self.file {
            None => {
                let file = tempfile().context("Failed creating temporary file")?;
                self.shared.file.store(Some(Arc::new(
                    file.try_clone().context("Failed cloning file handle")?)));
                self.file.insert(file)
            },
            Some(file) => file
        };

        // Write the data to file.
        file.write(data).context("Failed writing to stream file")?;

        // We must change the stream's current buffer to one for the new block.
        let block_base = self.length;

        // Look for a usable spare buffer.
        let next_buffer = match self.spare_buffer.take() {
            Some(mut arc) => {
                if let Some(buffer) = Arc::get_mut(&mut arc) {
                    // We are holding the only reference to this buffer,
                    // so we can update its block base and reuse it.
                    buffer.block_base = block_base;
                    arc
                } else {
                    // This buffer is still in use. Allocate a new one.
                    Arc::new(Buffer::new(block_base)?)
                }
            },
            // There is no spare buffer. Allocate a new one.
            None => Arc::new(Buffer::new(block_base)?)
        };

        // Set our pointers appropriately.
        self.buf = next_buffer.ptr;
        self.ptr = self.buf;

        // Swap in the next write buffer.
        let prev_buffer = self.shared.current_buffer.swap(next_buffer);

        // Store the previous buffer as the new spare.
        self.spare_buffer = Some(prev_buffer);

        Ok(())
    }

    /// Block size in bytes.
    pub const fn block_size() -> usize {
        BLOCK_SIZE
    }

    /// Bitmask for bits defining an offset within a block.
    pub const fn block_mask() -> usize {
        BLOCK_SIZE - 1
    }
}

impl<const BLOCK_SIZE: usize> StreamReader<BLOCK_SIZE> {
    /// Get the current length of the stream, in bytes.
    pub fn len(&self) -> u64 {
        self.shared.length.load(Acquire)
    }

    /// Access data in the stream.
    ///
    /// Returns a reference to a slice of data, which may have less than the
    /// requested length. The method may be called again to access further data.
    ///
    pub fn access(&mut self, range: &Range<u64>)
        -> Result<impl Deref<Target=[u8]>, Error>
    {
        use Data::*;

        // First guarantee that the requested data exists, somewhere.
        let available_length = self.shared.length.load(Acquire);
        if range.end > available_length {
            bail!("Requested read of range {range:?}, \
                   but stream length is {available_length}")
        }

        // Identify the block and the range required from within it.
        let block_base = range.start & !(Self::block_mask() as u64);
        let length = range.end - range.start;
        let start = range.start & (Self::block_mask() as u64);
        let end = min(start + length, Self::block_size() as u64);
        let range_in_block = (start as usize)..(end as usize);

        // Take our own reference to the current buffer.
        let buffer = self.shared.current_buffer.load();

        // Check if the required block is the one represented by this buffer.
        if buffer.block_base == block_base {
            // Return a handle to access the data in this buffer.
            Ok(Buffered(buffer.clone(), range_in_block))
        } else {
            // The requested block was already written to the file.
            // Look for an existing mapping of the block.
            let existing_mmap = self.mappings.get(&block_base);

            // If there is no existing mapping, create one.
            let mmap = match existing_mmap {
                Some(mmap) => Arc::clone(mmap),
                None => {
                    // Get the file handle to be used for mapping.
                    // The writer sets this before writing the first block.
                    let file_guard = self.shared.file.load();
                    let file_option = file_guard.deref().as_ref();
                    let file_arc = file_option.unwrap();
                    let file = file_arc.deref();
                    // The block was already written to the file and will not
                    // be modified by the writer, so it is safe to map it.
                    let mmap_result = unsafe {
                        MmapOptions::new()
                            .offset(block_base)
                            .len(Self::block_size())
                            .map(file)
                    };
                    let new_mmap = Arc::new(
                        mmap_result.context("Failed mapping stream file")?);
                    self.mappings.push(block_base, Arc::clone(&new_mmap));
                    new_mmap
                }
            };

            // Return a handle to access the data through this mapping.
            Ok(Mapped(mmap, range_in_block))
        }
    }

    /// Block size in bytes.
    pub const fn block_size() -> usize {
        BLOCK_SIZE
    }

    /// Bitmask for bits defining an offset within a block.
    pub const fn block_mask() -> usize {
        BLOCK_SIZE - 1
    }
}

impl<const BLOCK_SIZE: usize> Buffer<BLOCK_SIZE> {
    /// Create a new buffer for the specified block.
    fn new(block_base: u64) -> Result<Self, Error> {
        Ok(Buffer {
            block_base,
            // Calling System.alloc safely requires the layout to be known to
            // have non-zero size. If the allocation fails we return an error.
            // Otherwise, our Drop impl guarantees the allocation is freed.
            ptr: unsafe {
                let ptr = System.alloc(Self::block_layout());
                if ptr.is_null() {
                    bail!("Failed to allocate buffer")
                }
                ptr
            }
        })
    }

    /// Layout for allocating block-sized, block-aligned chunks of memory.
    const fn block_layout() -> Layout {
        match Layout::from_size_align(BLOCK_SIZE, BLOCK_SIZE) {
            Ok(layout) => layout,
            Err(_) => panic!("{}",
                if BLOCK_SIZE == 0 {
                    "Stream block size must not be zero"
                } else if !BLOCK_SIZE.is_power_of_two() {
                    "Stream block size must be a power of two"
                } else if BLOCK_SIZE > (isize::MAX as usize) {
                    "Stream block size must be within isize::MAX"
                } else {
                    "Unexpected layout error"
                }
            )
        }
    }
}

// Dropping a Buffer must free the allocated block.
impl<const S: usize> Drop for Buffer<S> {
    fn drop(&mut self) {
        unsafe {
            // This pointer was created in Buffer::new() and is known to point
            // to an allocation made by this allocator with the same layout.
            System.dealloc(self.ptr, Self::block_layout())
        }
    }
}

// Tell the compiler that it's safe to send and share our types between
// threads despite the *mut u8 fields, due to the way we manage the data.
unsafe impl<const S: usize> Send for StreamWriter<S> {}
unsafe impl<const S: usize> Sync for StreamWriter<S> {}
unsafe impl<const S: usize> Send for Buffer<S> {}
unsafe impl<const S: usize> Sync for Buffer<S> {}

// Data can be dereferenced by readers to access the stream data.
impl<const S: usize> Deref for Data<S> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        use Data::*;
        match self {
            Mapped(mmap, range) => &mmap[range.clone()],
            // This variant is constructed only by StreamReader::access, which
            // is responsible for ensuring that the range contains valid data.
            Buffered(buffer, range) => unsafe {
                let start = buffer.ptr.add(range.start);
                let length = range.end - range.start;
                slice::from_raw_parts(start, length)
            }
        }
    }
}

// StreamReader can be cloned to set up multiple readers.
impl<const S: usize> Clone for StreamReader<S> {
    fn clone(&self) -> StreamReader<S> {
        StreamReader {
            shared: self.shared.clone(),
            mappings: LruBTreeMap::new(MAP_CACHE_PER_READER),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::time::Duration;
    use std::thread::{spawn, sleep};
    use rand_xorshift::XorShiftRng;
    use rand::{Rng, SeedableRng};

    #[test]
    fn test_stream() {
        const BLOCK_SIZE: usize = 0x4000;

        // Create a reader-writer pair.
        let (mut writer, reader) = stream::<BLOCK_SIZE>().unwrap();

        // Build a reference array with ~8MB of random data.
        let mut prng = XorShiftRng::seed_from_u64(42);
        let reference = Arc::new({
            let mut data = vec![0u8; 8012345];
            prng.fill(data.as_mut_slice());
            data
        });

        // Spawn 10 reader threads which will each continually try to access
        // random chunks of the stream, and verify data against the reference.
        let mut readers = Vec::new();
        let stop = Arc::new(AtomicBool::from(false));
        for i in 0..10 {
            let mut reader = reader.clone();
            let reference = reference.clone();
            let stop = stop.clone();
            readers.push(spawn(move || {
                // Give each thread its own PRNG.
                let mut prng = XorShiftRng::seed_from_u64(i);

                // Read randomly until stopped.
                while !stop.load(Relaxed) {
                    // Get the current length of the stream.
                    let limit = reader.len();

                    // Generate a random range to request.
                    if limit < 2 { continue }
                    let req_start = prng.gen_range(0..(limit - 1));
                    let req_end = prng.gen_range((req_start + 1)..limit);
                    let req_range = req_start..req_end;

                    // Access the generated range.
                    let data = reader.access(&req_range).unwrap();

                    // Check against the reference.
                    let ref_start = req_start as usize;
                    let ref_end = ref_start + data.len();
                    let expected = &reference[ref_start..ref_end];
                    for (i, (a, b)) in data
                         .iter()
                         .zip(expected.iter())
                         .enumerate()
                    {
                        if a != b {
                            let req_range = req_start..req_end;
                            let ref_length = ref_end - ref_start;
                            panic!("Mismatch in data \
                                   at byte {i} of {ref_length}: \
                                   got {a}, expected {b}. \
                                   Requested range was {req_range:?}")
                        }
                    }
                }
            }));
        }

        // Make random small writes until all data has been written.
        let mut start = 0usize;
        while start < reference.len() {
            // Choose a random length to write.
            let end = start + min(
                prng.gen_range(1..12345),
                reference.len() - start
            );

            // Wait briefly between writes.
            sleep(Duration::from_millis(1));

            // Append the data to the stream and check the return value.
            let new_length = writer.append(&reference[start..end]).unwrap();
            assert!(new_length == end as u64);

            start = end;
        }

        assert!(writer.len() == reference.len() as u64);

        // Stop the readers.
        stop.store(true, Relaxed);
        for thread in readers {
            thread.join().unwrap();
        }
    }
}
