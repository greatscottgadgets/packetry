use std::fs::File;
use std::io::prelude::*;
use std::io::SeekFrom;
use std::ops::Range;
use std::cmp::{min, max};

use bufreaderwriter::BufReaderWriter;
use tempfile::tempfile;
use thiserror::Error;
use bisection::bisect_right;

#[derive(Error, Debug)]
pub enum HybridIndexError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

bitfield! {
    pub struct IncrementFields(u64);
    u64, count, set_count: 59, 0;
    u8, width, set_width: 63, 60;
}

struct Entry {
    base_value: u64,
    file_offset: u64,
    increments: IncrementFields,
}

pub struct HybridIndex {
    min_width: u8,
    file: BufReaderWriter<File>,
    file_length: u64,
    total_count: u64,
    entries: Vec<Entry>,
    index: Vec<u64>,
    last_value: u64,
    at_end: bool,
}

impl HybridIndex {
    pub fn new(min_width: u8) -> Result<Self, HybridIndexError> {
        let file = tempfile()?;
        Ok(Self{
            min_width: min_width,
            file: BufReaderWriter::new_writer(file),
            file_length: 0,
            total_count: 0,
            entries: Vec::new(),
            index: Vec::new(),
            last_value: 0,
            at_end: true,
        })
    }

    pub fn push(&mut self, value: u64) -> Result<(), HybridIndexError> {
        if self.entries.len() == 0 {
            let first_entry = Entry {
                base_value: value,
                file_offset: 0,
                increments: IncrementFields(0),
            };
            self.entries.push(first_entry);
            self.index.push(0);
        } else {
            let last_entry = self.entries.last_mut().unwrap();
            let increment = value - last_entry.base_value;
            let width = max(byte_width(increment), self.min_width);
            let count = last_entry.increments.count();
            if count > 0 && width > last_entry.increments.width() {
                let new_entry = Entry {
                    base_value: value,
                    file_offset: self.file_length,
                    increments: IncrementFields(0),
                };
                self.entries.push(new_entry);
                self.index.push(self.total_count);
            } else {
                if last_entry.increments.width() == 0 {
                    last_entry.increments.set_width(width);
                }
                let bytes = increment.to_le_bytes();
                if !self.at_end {
                   self.file.seek(SeekFrom::Start(self.file_length))?;
                   self.at_end = true;
                }
                self.file.write_all(&bytes[0..width as usize])?;
                self.file_length += width as u64;
                last_entry.increments.set_count(count + 1);
            }
        }
        self.total_count += 1;
        self.last_value = value;
        Ok(())
    }

    pub fn get(&mut self, i: u64) -> Result<u64, HybridIndexError> {
        let entry_id = bisect_right(self.index.as_slice(), &i) - 1;
        let entry = &self.entries[entry_id];
        let increment_id = i - self.index[entry_id];
        if increment_id == 0 {
            Ok(entry.base_value)
        } else {
            let width = entry.increments.width();
            let start = entry.file_offset + (increment_id - 1) * width as u64;
            let mut bytes = [0 as u8; 8];
            self.file.seek(SeekFrom::Start(start))?;
            self.at_end = false;
            self.file.read_exact(&mut bytes[0..width as usize])?;
            let increment = u64::from_le_bytes(bytes);
            let value = entry.base_value + increment;
            Ok(value)
        }
    }

    pub fn get_range(&mut self, range: Range<u64>) -> Result<Vec<u64>, HybridIndexError> {
        let mut result = Vec::new();
        let mut i = range.start;
        while i < range.end {
            let entry_id = bisect_right(self.index.as_slice(), &i) - 1;
            let entry = &self.entries[entry_id];
            let mut increment_id = i - self.index[entry_id];
            if increment_id == 0 {
                result.push(entry.base_value);
                i += 1;
            } else {
                increment_id -= 1;
            }
            let available = entry.increments.count() - increment_id;
            let needed = range.end - i;
            let read_count = min(available, needed);
            if read_count == 0 {
                continue;
            }
            let width = entry.increments.width();
            let start = entry.file_offset + increment_id * width as u64;
            self.file.seek(SeekFrom::Start(start))?;
            self.at_end = false;
            let mut bytes = [0 as u8; 8];
            for _ in 0..read_count {
                self.file.read_exact(&mut bytes[0..width as usize])?;
                let increment = u64::from_le_bytes(bytes);
                let value = entry.base_value + increment;
                result.push(value);
            }
            i += read_count;
        }
        Ok(result)
    }

    pub fn len(&self) -> u64 {
        self.total_count
    }

    pub fn entry_count(&self) -> u64 {
        self.entries.len() as u64
    }

    pub fn size(&self) -> u64 {
       self.file_length +
           self.entries.len() as u64 * std::mem::size_of::<Entry>() as u64 +
           self.index.len() as u64 * std::mem::size_of::<u64>() as u64
    }
}

fn byte_width(value: u64) -> u8 {
    if value == 0 {
        1
    } else {
        (8 - value.leading_zeros() / 8) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_width() {
        assert!(byte_width(0x000000) == 1);
        assert!(byte_width(0x000001) == 1);
        assert!(byte_width(0x0000FF) == 1);
        assert!(byte_width(0x000100) == 2);
        assert!(byte_width(0x000101) == 2);
        assert!(byte_width(0x00FFFF) == 2);
        assert!(byte_width(0x010000) == 3);
        assert!(byte_width(0x010001) == 3);
        assert!(byte_width(0xFFFFFF) == 3);
    }

    #[test]
    fn test_hybrid_index() {
        let mut v = HybridIndex::new(1).unwrap();
        let mut expected = Vec::<u64>::new();
        let mut x = 10;
        let n = 321;
        for i in 0..n {
            x += 1 + i % 3;
            expected.push(x);
            v.push(x).unwrap();
        }
        for i in 0..n {
            let vi = v.get(i).unwrap();
            let xi = expected[i as usize];
            assert!(vi == xi);
        }
        for i in 0..n {
            let vrng = i as u64 .. n as u64;
            let xrng = i as usize .. n as usize;
            let vr = v.get_range(vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
        for i in 0..n {
            let vrng = 0 as u64 .. i as u64;
            let xrng = 0 as usize .. i as usize;
            let vr = v.get_range(vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
        for i in 0..(n - 10) {
            let vrng = i as u64 .. (i + 10) as u64;
            let xrng = i as usize .. (i + 10) as usize;
            let vr = v.get_range(vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
    }
}
