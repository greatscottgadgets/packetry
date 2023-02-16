use std::alloc::{GlobalAlloc, Layout, System};
use std::cmp::min;
use std::fs::File;
use std::io::Write;
use std::ops::{Deref, Range};
use std::ptr::copy_nonoverlapping;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Release}};

use arc_swap::ArcSwap;
use lrumap::LruBTreeMap;
use memmap2::{Mmap, MmapOptions};
use tempfile::tempfile;
use thiserror::Error;

/// Private data shared by the writer and multiple readers.
struct Shared {
    /// Available length of the stream, including data in both file and buffer.
    length: AtomicU64,
    /// File handle used by readers to create mappings.
    file: File,
    /// Buffer currently in use for newly appended data.
    current_buffer: ArcSwap<Buffer>,
}

/// Unique handle for append-only write access to a stream.
pub struct StreamWriter {
    /// Shared data.
    shared: Arc<Shared>,
    /// Total length of the stream.
    length: u64,
    /// Pointer to start of the buffer currently in use.
    buf: *mut u8,
    /// Pointer to current position in the current buffer.
    ptr: *mut u8,
    /// File handle used to append to the stream.
    file: File,
    /// Spare buffer to be potentially used when current buffer is full.
    spare_buffer: Option<Arc<Buffer>>,
}

/// Cloneable handle for read-only random access to a stream.
pub struct StreamReader {
    /// Shared data.
    shared: Arc<Shared>,
    /// Cache of existing mappings into the file.
    mappings: LruBTreeMap<u64, Arc<Mmap>>,
}

/// Data that is part of a stream and currently in memory.
struct Buffer {
    /// Block to which this data belongs.
    block_base: u64,
    /// Raw pointer to space allocated to hold the data.
    ptr: *mut u8,
}

/// A read-only handle to any data that is part of a stream.
enum Data {
    /// Data in the file, accessed through a mapping.
    Mapped(Arc<Mmap>, Range<usize>),
    /// Data in memory, accessed within a buffer.
    Buffered(Arc<Buffer>, Range<usize>),
}

/// Error type returned by stream operations.
#[derive(Debug, Error)]
pub enum StreamError {
    /// Failed to create temporary file to store the stream.
    #[error("failed creating temporary file: {0}")]
    TempFile(std::io::Error),
    /// Failed to clone file handle to the stream file.
    #[error("failed cloning file handle: {0}")]
    CloneFile(std::io::Error),
    /// Failed to write to the end of the stream file.
    #[error("failed writing to stream file: {0}")]
    WriteFile(std::io::Error),
    /// Failed to create a memory mapping into part of the stream file.
    #[error("failed mapping stream file: {0}")]
    MapFile(std::io::Error),
    /// Failed to allocate a buffer.
    #[error("failed to allocate buffer")]
    Alloc(),
    /// Attempted to read past the end of the stream.
    #[error("attemped to read past end of stream: {0}")]
    ReadPastEnd(String),
}

// Use 2MB block size, which provides reasonable efficiency
// tradeoffs and is a multiple of all relevant page sizes.
const BLOCK_SIZE: usize = 0x200000;
const BLOCK_MASK: usize = BLOCK_SIZE - 1;
const BLOCK_SIZE_U64: u64 = BLOCK_SIZE as u64;
const BLOCK_MASK_U64: u64 = BLOCK_MASK as u64;

// Layout used to allocate block-sized, block-aligned chunks of memory.
const BLOCK_LAYOUT: Layout = unsafe {
    // Safety: align is non-zero, a power of 2, and does not overflow isize.
    Layout::from_size_align_unchecked(BLOCK_SIZE, BLOCK_SIZE)
};

// Number of most recent file mappings retained by each reader.
const MAP_CACHE_PER_READER: usize = 4;

/// Construct a new stream.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn stream() -> Result<(StreamWriter, StreamReader), StreamError> {
    let buffer = Arc::new(Buffer::new(0)?);
    let shared = Arc::new(Shared {
        length: AtomicU64::from(0),
        file: tempfile().map_err(StreamError::TempFile)?,
        current_buffer: ArcSwap::new(buffer.clone()),
    });
    let writer = StreamWriter {
        shared: shared.clone(),
        length: 0,
        buf: buffer.ptr,
        ptr: buffer.ptr,
        file: shared.file.try_clone().map_err(StreamError::CloneFile)?,
        spare_buffer: None,
    };
    let reader = StreamReader {
        shared,
        mappings: LruBTreeMap::new(MAP_CACHE_PER_READER),
    };
    Ok((writer, reader))
}

impl StreamWriter {
    /// Get the current length of the stream, in bytes.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Append data to the end of the stream.
    ///
    /// Returns the new stream length.
    ///
    pub fn append(&mut self, mut data: &[u8]) -> Result<u64, StreamError> {
        let length = data.len();
        let buffered = (self.length & BLOCK_MASK_U64) as usize;
        if buffered + length <= BLOCK_SIZE {
            // All the data will fit in the existing buffer.
            unsafe { self.write_to_buffer(data, length) };
            if self.length & BLOCK_MASK_U64 == 0 {
                // Buffer is now full, and can be written to file.
                unsafe { self.write_buffer_to_file()? };
            }
        } else {
            // The data will fill the existing buffer.
            if buffered > 0 {
                // The buffer is partly used. Fill it and write it out.
                let length = BLOCK_SIZE - buffered;
                unsafe { self.write_to_buffer(data, length) };
                // Buffer is now full, and can be written to file.
                unsafe { self.write_buffer_to_file()? };
                data = &data[length..];
            }
            // The buffer is curently empty, so we are free to write whole
            // blocks of data directly to the file, bypassing the buffer.
            let direct = data.len() & !BLOCK_MASK;
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
    /// The data must be guaranteed to fit within the remaining space.
    ///
    #[inline(always)]
    unsafe fn write_to_buffer(&mut self, data: &[u8], length: usize) {
        copy_nonoverlapping(data.as_ptr(), self.ptr, length);
        self.ptr = self.ptr.add(length);
        self.length += length as u64;
    }

    /// Helper method for writing buffer to file.
    ///
    /// The buffer must be guaranteed to be full.
    #[inline(always)]
    unsafe fn write_buffer_to_file(&mut self) -> Result<(), StreamError> {
        let buf = slice::from_raw_parts(self.buf, BLOCK_SIZE);
        self.write_to_file(buf)
    }

    /// Helper method for writing data to file.
    ///
    /// The data must be guaranteed to be a multiple of the block size.
    ///
    unsafe fn write_to_file(&mut self, data: &[u8]) -> Result<(), StreamError> {

        // Write the data to file.
        self.file.write(data).map_err(StreamError::WriteFile)?;

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
}

impl StreamReader {
    /// Get the current length of the stream, in bytes.
    pub fn len(&self) -> u64 {
        self.shared.length.load(Acquire)
    }

    /// Access data in the stream.
    ///
    /// Returns a reference to a slice of data, which may have less than the
    /// requested length. The method may be called again to access further data.
    ///
    pub fn access(&mut self, range: Range<u64>)
        -> Result<impl Deref<Target=[u8]>, StreamError>
    {
        use Data::*;

        // First guarantee that the requested data exists, somewhere.
        let available_length = self.shared.length.load(Acquire);
        if range.end > available_length {
            return Err(StreamError::ReadPastEnd(format!(
                "requested read of range {:?}, but stream length is {}",
                range, available_length)));
        }

        // Identify the block and the range required from within it.
        let block_base = range.start & !BLOCK_MASK_U64;
        let length = range.end - range.start;
        let start = range.start & BLOCK_MASK_U64;
        let end = min(start + length, BLOCK_SIZE_U64);
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
                    // The block was already written to the file and will not
                    // be modified by the writer, so it is safe to map it.
                    let mmap_result = unsafe {
                        MmapOptions::new()
                            .offset(block_base)
                            .len(BLOCK_SIZE)
                            .map(&self.shared.file)
                    };
                    let new_mmap =
                        Arc::new(mmap_result.map_err(StreamError::MapFile)?);
                    self.mappings.push(block_base, Arc::clone(&new_mmap));
                    new_mmap
                }
            };

            // Return a handle to access the data through this mapping.
            Ok(Mapped(mmap, range_in_block))
        }
    }
}

impl Buffer {
    /// Create a new buffer for the specified block.
    fn new(block_base: u64) -> Result<Buffer, StreamError> {
        Ok(Buffer {
            block_base,
            // Calling System.alloc safely requires the layout to be known to
            // have non-zero size. If the allocation fails we return an error.
            // Otherwise, our Drop impl guarantees the allocation is freed.
            ptr: unsafe {
                let ptr = System.alloc(BLOCK_LAYOUT);
                if ptr.is_null() {
                    return Err(StreamError::Alloc());
                }
                ptr
            }
        })
    }
}

// Dropping a Buffer must free the allocated block.
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            // This pointer was created in Buffer::new() and is known to point
            // to an allocation made by this allocator with the same layout.
            System.dealloc(self.ptr, BLOCK_LAYOUT)
        }
    }
}

// Tell the compiler that it's safe to send and share our types between
// threads despite the *mut u8 fields, due to the way we manage the data.
unsafe impl Send for StreamWriter {}
unsafe impl Sync for StreamWriter {}
unsafe impl Send for Buffer {}
unsafe impl Sync for Buffer {}

// Data can be dereferenced by readers to access the stream data.
impl Deref for Data {
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
impl Clone for StreamReader {
    fn clone(&self) -> StreamReader {
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
        // Create a reader-writer pair.
        let (mut writer, reader) = stream().unwrap();

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

                    // Access the generated range.
                    let data = reader.access(req_start..req_end).unwrap();

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

        // Stop the readers.
        stop.store(true, Relaxed);
        for thread in readers {
            thread.join().unwrap();
        }
    }
}
