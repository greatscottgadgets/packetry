//! Atomic counters used in the database implementation.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Release}};

use anyhow::Error;

use crate::util::dump::Dump;

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

impl Dump for Counter {
    fn dump(&self, dest: &Path) -> Result<(), Error> {
        self.load().dump(dest)
    }

    fn restore(src: &Path) -> Result<Self, Error> {
        let counter = Counter::new();
        counter.store(u64::restore(src)?);
        Ok(counter)
    }
}
