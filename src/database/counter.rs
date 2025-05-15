//! Atomic counters used in the database implementation.

use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Release}};

pub struct Counter(AtomicU64);

impl Counter {
    pub fn new() -> Self {
        Self(AtomicU64::from(0))
    }

    pub fn store(&self, value: u64) {
        self.0.store(value, Release);
    }

    pub fn load(&self) -> u64 {
        self.0.load(Acquire)
    }
}
