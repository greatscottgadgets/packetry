use std::fs::File;
use std::io::prelude::*;
use std::io::SeekFrom;
use std::marker::PhantomData;
use std::ops::Range;

use bufreaderwriter::BufReaderWriter;
use bytemuck::{bytes_of, bytes_of_mut, Pod};
use tempfile::tempfile;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FileVecError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

pub struct FileVec<T> where T: Pod + Default {
    _marker: PhantomData<T>,
    file: BufReaderWriter<File>,
    file_length: u64,
    item_count: u64,
}

impl<T: Pod + Default> FileVec<T> {
   pub fn new() -> Self {
        let file = tempfile().expect("Failed creating temporary file");
        Self {
            _marker: PhantomData,
            file: BufReaderWriter::new_writer(file),
            file_length: 0,
            item_count: 0,
        }
    }

    pub fn push(&mut self, item: &T) where T: Pod {
        let data = bytes_of(item);
        self.file.write_all(data)
                 .expect("Failed writing bytes to file");
        self.file_length += data.len() as u64;
        self.item_count += 1;
    }

    pub fn append(&mut self, items: &[T]) where T: Pod {
        for item in items {
            let data = bytes_of(item);
            self.file.write_all(data)
                     .expect("Failed writing bytes to file");
            self.file_length += data.len() as u64;
        }
        self.item_count += items.len() as u64;
    }

    pub fn get(&mut self, index: u64) -> T {
        let mut result: T = Default::default();
        let start = index * std::mem::size_of::<T>() as u64;
        self.file.seek(SeekFrom::Start(start as u64))
                 .expect("Failed to seek to position in file");
        self.file.read_exact(bytes_of_mut(&mut result))
                 .expect("Failed to read bytes from file");
        self.file.seek(SeekFrom::Start(self.file_length))
                 .expect("Failed to seek to file end");
        result
    }

    pub fn get_range(&mut self, range: Range<u64>) -> Vec<T> {
        let mut buf: T = Default::default();
        let mut result = Vec::new();
        let start = range.start * std::mem::size_of::<T>() as u64;
        self.file.seek(SeekFrom::Start(start as u64))
                 .expect("Failed to seek to position in file");
        for _ in range {
            self.file.read_exact(bytes_of_mut(&mut buf))
                     .expect("Failed to read bytes from file");
            result.push(buf);
        }
        self.file.seek(SeekFrom::Start(self.file_length))
                 .expect("Failed to seek to file end");
        result
    }

    pub fn len(&self) -> u64 {
        self.item_count
    }

    pub fn size(&self) -> u64 {
       self.file_length
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck_derive::{Pod, Zeroable};

    #[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
    #[repr(C)]
    struct Foo {
        bar: u32,
        baz: u32,
    }

    #[test]
    fn test_file_vec_push() {
        let mut v = FileVec::new();
        for i in 0..100 {
            let x= Foo{ bar: i, baz: i};
            v.push(&x);
            assert!(v.get(i as u64) == x);
        }
    }

    #[test]
    fn test_file_vec_append() {
        let mut file_vec = FileVec::new();

        // Build a (normal) Vec of data
        let mut data = Vec::new();
        for i in 0..100 {
            let item= Foo{ bar: i, baz: i};
            data.push(item)
        }

        // append it to the FileVec
        file_vec.append(&data.as_slice());

        // and check
        let vec: Vec<_> = file_vec.get_range(0..100);
        assert!(vec == data);
    }
}
