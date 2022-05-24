use std::fs::File;
use std::io::prelude::*;
use std::io::SeekFrom;
use std::marker::PhantomData;
use std::ops::Range;

use bufreaderwriter::BufReaderWriter;
use bytemuck::{bytes_of, bytes_of_mut, Pod};
use tempfile::tempfile;
use thiserror::Error;

use crate::id::Id;

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

    pub fn push(&mut self, item: &T)
       -> Result<Id<T>, FileVecError> where T: Pod
    {
        let data= bytes_of(item);
        self.file.write_all(data)?;
        self.file_length += data.len() as u64;
        let id = Id::<T>::from(self.item_count);
        self.item_count += 1;
        Ok(id)
    }

    pub fn append(&mut self, items: &[T])
       -> Result<Id<T>, FileVecError> where T: Pod
    {
        for item in items {
            let data = bytes_of(item);
            self.file.write_all(data)?;
            self.file_length += data.len() as u64;
        }
        let id = Id::<T>::from(self.item_count);
        self.item_count += items.len() as u64;
        Ok(id)
    }

    pub fn get(&mut self, id: Id<T>) -> Result<T, FileVecError> {
        let mut result: T = Default::default();
        let start = id.value * std::mem::size_of::<T>() as u64;
        self.file.seek(SeekFrom::Start(start as u64))?;
        self.file.read_exact(bytes_of_mut(&mut result))?;
        self.file.seek(SeekFrom::Start(self.file_length))?;
        Ok(result)
    }

    pub fn get_range(&mut self, range: Range<Id<T>>) -> Result<Vec<T>, FileVecError> {
        let mut buf: T = Default::default();
        let mut result = Vec::new();
        let start = range.start.value * std::mem::size_of::<T>() as u64;
        let end = range.end.value;
        self.file.seek(SeekFrom::Start(start as u64))?;
        for _ in start .. end {
            self.file.read_exact(bytes_of_mut(&mut buf))?;
            result.push(buf);
        }
        self.file.seek(SeekFrom::Start(self.file_length))?;
        Ok(result)
    }

    pub fn next_id(&self) -> Id<T> {
       Id::<T>::from(self.item_count)
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
        let mut v = FileVec::new().unwrap();
        for i in 0..100 {
            let x= Foo{ bar: i, baz: i};
            v.push(&x).unwrap();
            assert!(v.get(Id::<Foo>::from(i as u64)).unwrap() == x);
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
        let start = Id::<Foo>::from(0);
        let end = Id::<Foo>::from(100);
        let range = start .. end;
        let vec: Vec<_> = file_vec.get_range(range).unwrap();
        assert!(vec == data);
    }
}
