//! Atomic counters used in the database implementation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::sync::atomic::{
    AtomicPtr, AtomicU64,
    Ordering::{Acquire, Release}
};

use anyhow::Error;
use arc_swap::ArcSwap;

use crate::util::dump::Dump;

/// An atomic counter, stored contiguously with others.
pub struct Counter {
    inner: Arc<CounterInner>,
}

/// A buffer of AtomicU64s.
struct Buffer {
    ptr: AtomicPtr<AtomicU64>,
    capacity: usize,
    layout: Layout,
}

/// A set of atomic counters, stored contiguously in memory.
pub struct CounterSet {
    length: usize,
    capacity: usize,
    buffer: Arc<Buffer>,
    refs: Vec<Weak<CounterInner>>,
}

/// A snapshot of the values from a `CounterSet` at a given instant.
pub struct Snapshot {
    buffer: Arc<Buffer>,
}

struct CounterInner {
    buffer: ArcSwap<Buffer>,
    index: usize,
}

impl Counter {
    /// Set the value of this counter.
    pub fn store(&self, value: u64) {
        self.buffer().get(self.inner.index).store(value, Release);
    }

    /// Get the current value from this counter.
    pub fn load(&self) -> u64 {
        self.buffer().get(self.inner.index).load(Acquire)
    }

    /// Get the value from this counter at the given snapshot.
    pub fn load_at(&self, snapshot: &Snapshot) -> u64 {
        snapshot.buffer.get(self.inner.index).load(Acquire)
    }

    fn buffer(&self) -> impl Deref<Target=Arc<Buffer>> {
        self.inner.buffer.load()
    }
}

impl Buffer {
    fn new(capacity: usize) -> Buffer {
        assert!(capacity != 0, "Required capacity was zero");
        let layout = Layout::array::<AtomicU64>(capacity)
            .expect("Required allocation exceeds isize::MAX");
        let raw_ptr: *mut u8 = unsafe {
            // SAFETY: Layout has non-zero size.
            System.alloc_zeroed(layout)
        };
        if raw_ptr.is_null() {
            panic!("Allocation failed");
        }
        let ptr: AtomicPtr<AtomicU64> = unsafe {
            // SAFETY:
            // 1. *mut u8 can be safely converted to *mut AtomicU64,
            //    because we allocated for an AtomicU64 array layout.
            // 2. *mut AtomicU64 can be converted to AtomicPtr<AtomicU64>,
            //    because the AtomicPtr docs guarantee that AtomicPtr<T>
            //    and *mut T have the same size and validity.
            std::mem::transmute(raw_ptr)
        };
        Buffer { layout, capacity, ptr }
    }

    fn get(&self, index: usize) -> &AtomicU64 {
        if index >= self.capacity {
            panic!("Required index exceeds buffer capacity");
        }
        unsafe {
            // SAFETY: index is within our capacity, so we can add it
            // to our base pointer and get a valid AtomicU64 pointer.
            self.ptr
                .load(Acquire)
                .add(index)
                .as_ref()
                .unwrap_unchecked()
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: This is the inverse of the transmutation from
            // raw_ptr to ptr in Buffer::new, see safety note there.
            let raw_ptr: *mut u8 = std::mem::transmute(*self.ptr.as_ptr());

            // SAFETY: raw_ptr was allocated with this layout.
            System.dealloc(raw_ptr, self.layout);
        }
    }
}

impl CounterSet {
    /// Create a new counter set with the specified capacity.
    pub fn new() -> CounterSet {
        let capacity = 1;
        CounterSet {
            length: 0,
            capacity,
            buffer: Arc::new(Buffer::new(capacity)),
            refs: Vec::new(),
        }
    }

    /// Add a new counter to this set and return it.
    pub fn new_counter(&mut self) -> Counter {
        // If we've filled the current buffer, allocate a bigger one.
        if self.length == self.capacity {
            let new_capacity = self.capacity * 2;
            let new_buffer = Arc::new(self.reallocate(new_capacity));
            // Update all existing counters to point to the new buffer.
            for counter in self.refs.iter().filter_map(Weak::upgrade) {
                counter.buffer.swap(Arc::clone(&new_buffer));
            }
            self.capacity = new_capacity;
            self.buffer = new_buffer;
        }
        let inner = Arc::new(CounterInner {
            buffer: ArcSwap::new(Arc::clone(&self.buffer)),
            index: self.length,
        });
        self.length += 1;
        self.refs.push(Arc::downgrade(&inner));
        Counter { inner }
    }

    /// Take a snapshot of the current counter values.
    pub fn snapshot(&mut self) -> Snapshot {
        Snapshot {
            buffer: Arc::new(self.reallocate(self.length)),
        }
    }

    fn reallocate(&self, capacity: usize) -> Buffer {
        assert!(capacity >= self.length, "Requested capacity less than length");
        let new_buf = Buffer::new(capacity);
        let old_ptr = self.buffer.ptr.load(Acquire);
        let new_ptr = new_buf.ptr.load(Acquire);
        unsafe {
            // SAFETY:
            // 1. old_ptr is valid for reading self.length, because we ensure
            //    that self.length is always within our current buffer capacity.
            // 2. new_ptr is valid for writing self.length, because we checked
            //    that the new capacity is sufficient above.
            // 3. Both pointers are properly aligned, guaranteed by their type.
            // 4. The regions do not overlap, guaranteed by new allocation.
            core::ptr::copy_nonoverlapping(old_ptr, new_ptr, self.length);
        }
        new_buf
    }
}

impl Dump for Counter {
    fn dump(&self, dest: &Path) -> Result<(), Error> {
        self.load().dump(dest)
    }

    fn restore(db: &mut CounterSet, src: &Path) -> Result<Self, Error> {
        let counter = db.new_counter();
        counter.store(u64::restore(db, src)?);
        Ok(counter)
    }
}
