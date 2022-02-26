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
   pub fn new() -> Result<Self, FileVecError> {
        let file = tempfile()?;
        Ok(Self{
            _marker: PhantomData,
            file: BufReaderWriter::new_writer(file),
            file_length: 0,
            item_count: 0,
        })
    }

    pub fn push(&mut self, item: &T) -> Result<(), FileVecError> where T: Pod {
        let data= bytes_of(item);
        self.file.write_all(data)?;
        self.file_length += data.len() as u64;
        self.item_count += 1;
        Ok(())
    }

    pub fn append(&mut self, items: &[T]) -> Result<(), FileVecError> where T: Pod {
        for item in items {
            let data = bytes_of(item);
            self.file.write_all(data)?;
            self.file_length += data.len() as u64;
        }
        self.item_count += items.len() as u64;
        Ok(())
    }

    pub fn insert(&mut self, item: &T, index: u64) -> Result<(), FileVecError> where T: Pod {
        let start = index * std::mem::size_of::<T>() as u64;
        let data= bytes_of(item);
        self.file.seek(SeekFrom::Start(start as u64))?;
        self.file.write_all(data)?;
        self.file.seek(SeekFrom::Start(self.file_length))?;
        Ok(())
    }

    pub fn get(&mut self, index: u64) -> Result<T, FileVecError> {
        let mut result: T = Default::default();
        let start = index * std::mem::size_of::<T>() as u64;
        self.file.seek(SeekFrom::Start(start as u64))?;
        self.file.read_exact(bytes_of_mut(&mut result))?;
        self.file.seek(SeekFrom::Start(self.file_length))?;
        Ok(result)
    }

    pub fn get_range(&mut self, range: Range<u64>) -> Result<Vec<T>, FileVecError> {
        let mut buf: T = Default::default();
        let mut result = Vec::new();
        let start = range.start * std::mem::size_of::<T>() as u64;
        self.file.seek(SeekFrom::Start(start as u64))?;
        for _ in range {
            self.file.read_exact(bytes_of_mut(&mut buf))?;
            result.push(buf);
        }
        self.file.seek(SeekFrom::Start(self.file_length))?;
        Ok(result)
    }

    pub fn len(&self) -> u64 {
        self.item_count
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
        let mut v = FileVec::new().unwrap();
        for i in 0..100 {
            let x= Foo{ bar: i, baz: i};
            v.push(&x).unwrap();
            assert!(v.get(i as u64).unwrap() == x);
        }
    }

    #[test]
    fn test_file_vec_append() {
        let mut file_vec = FileVec::new().unwrap();

        // Build a (normal) Vec of data
        let mut data = Vec::new();
        for i in 0..100 {
            let item= Foo{ bar: i, baz: i};
            data.push(item)
        }

        // append it to the FileVec
        file_vec.append(&data.as_slice()).unwrap();

        // and check
        let vec: Vec<_> = file_vec.get_range(0..100).unwrap();
        assert!(vec == data);
    }
}