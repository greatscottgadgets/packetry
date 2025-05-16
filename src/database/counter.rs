//! Atomic counters used in the database implementation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicPtr, AtomicU64,
    Ordering::{Acquire, Release, Relaxed}
};

use arc_swap::ArcSwap;

struct CounterInner {
    buffer: ArcSwap<AtomicPtr<AtomicU64>>,
    index: usize
}

pub struct Counter(Arc<CounterInner>);

impl Counter {
    fn ptr(&self) -> &AtomicU64 {
        let inner = self.0.deref();
        unsafe {
            inner.buffer
                .load()
                .load(Relaxed)
                .add(inner.index)
                .as_ref()
                .unwrap_unchecked()
        }
    }

    pub fn store(&self, value: u64) {
        self.ptr().store(value, Release);
    }

    pub fn load(&self) -> u64 {
        self.ptr().load(Acquire)
    }
}

pub struct CounterSet {
    length: usize,
    capacity: usize,
    buffer: Arc<AtomicPtr<AtomicU64>>,
    refs: Vec<Counter>,
}

impl CounterSet {
    fn new(capacity: usize) -> Self {
        CounterSet {
            length: 0,
            capacity,
            buffer: Arc::new(Self::allocate(capacity).into()),
            refs: Vec::new(),
        }
    }

    pub fn new_counter(&mut self) -> Counter {
        if self.length == self.capacity {
            let new_capacity = self.capacity * 2;
            let new_buffer =
                Arc::new(AtomicPtr::new(self.reallocate(new_capacity)));
            for counter in self.refs.iter() {
                counter.0.deref().buffer.swap(new_buffer.clone());
            }
            self.capacity = new_capacity;
            self.buffer = new_buffer;
        }
        let inner = CounterInner {
            buffer: ArcSwap::new(self.buffer.clone()),
            index: self.length,
        };
        self.length += 1;
        Counter(Arc::new(inner))
    }

    fn layout(capacity: usize) -> Layout {
        Layout::array::<AtomicU64>(capacity)
            .expect("Required allocation exceeds isize::MAX")
    }

    fn allocate(capacity: usize) -> *mut AtomicU64 {
        let buffer = unsafe {
            System.alloc_zeroed(Self::layout(capacity))
        };
        if buffer.is_null() {
            panic!("Allocation failed");
        }
        unsafe {
            std::mem::transmute(buffer)
        }
    }

    fn reallocate(&self, capacity: usize) -> *mut AtomicU64 {
        let old_buf: *const AtomicU64 = self.buffer.deref().load(Relaxed);
        let new_buf: *mut AtomicU64 = Self::allocate(capacity);
        unsafe {
            core::ptr::copy_nonoverlapping(old_buf, new_buf, self.length);
        }
        new_buf
    }
}

impl Default for CounterSet {
    fn default() -> Self {
        Self::new(512)
    }
}

impl Clone for CounterSet {
    fn clone(&self) -> Self {
        CounterSet {
            length: self.length,
            capacity: self.capacity,
            buffer: Arc::new(self.reallocate(self.capacity).into()),
            refs: Vec::new()
        }
    }
}
