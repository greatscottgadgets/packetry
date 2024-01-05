use std::marker::PhantomData;
use std::mem::size_of;
use std::ops::{Deref, Range};

use anyhow::Error;
use bytemuck::{bytes_of, cast_slice, from_bytes, Pod};

use crate::id::Id;
use crate::stream::{stream, StreamReader, StreamWriter, MIN_BLOCK};
use crate::util::{fmt_count, fmt_size};

/// Unique handle for append-only write access to a data stream.
pub struct DataWriter<Value, const S: usize = MIN_BLOCK> {
    marker: PhantomData<Value>,
    stream_writer: StreamWriter<S>,
}

/// Cloneable handle for read-only random access to a data stream.
#[derive(Clone)]
pub struct DataReader<Value, const S: usize = MIN_BLOCK> {
    marker: PhantomData<Value>,
    stream_reader: StreamReader<S>,
}

/// A read-only handle to values that are part of the stream.
struct Values<Data, Value> where Data: Deref<Target=[u8]> {
    marker: PhantomData<Value>,
    data: Data,
}

/// Construct a new data stream with the default block size.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn data_stream<Value>()
    -> Result<(DataWriter<Value>, DataReader<Value>), Error>
{
    data_stream_with_block_size::<Value, MIN_BLOCK>()
}

/// Construct a new data stream with a specific block size.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn data_stream_with_block_size<Value, const S: usize>()
    -> Result<(DataWriter<Value, S>, DataReader<Value, S>), Error>
{
    let (stream_writer, stream_reader) = stream()?;
    let data_writer = DataWriter {
        marker: PhantomData,
        stream_writer,
    };
    let data_reader = DataReader {
        marker: PhantomData,
        stream_reader,
    };
    Ok((data_writer, data_reader))
}

impl<Value, const S: usize> DataWriter<Value, S>
where Value: Pod + Default
{
    /// Number of items in the stream.
    pub fn len(&self) -> u64 {
        self.stream_writer.len() / size_of::<Value>() as u64
    }

    /// Size of the stream in bytes.
    pub fn size(&self) -> u64 {
        self.stream_writer.len()
    }

    /// Add a single item to the end of the stream.
    ///
    /// Returns the position of the added item.
    pub fn push(&mut self, item: &Value) -> Result<Id<Value>, Error> {
        let id = Id::<Value>::from_offset(self.size());
        self.stream_writer.append(bytes_of(item))?;
        Ok(id)
    }

    /// Add multiple items to the end of the stream.
    ///
    /// Returns the ID range of the added items.
    pub fn append(&mut self, items: &[Value])
        -> Result<Range<Id<Value>>, Error>
    {
        let mut size = self.size();
        let start = Id::<Value>::from_offset(size);
        size = self.stream_writer.append(cast_slice(items))?;
        let end = Id::<Value>::from_offset(size);
        Ok(start..end)
    }
}

impl<Value, const S: usize> DataReader<Value, S>
where Value: Pod + Default
{
    /// Current number of items in the stream.
    pub fn len(&self) -> u64 {
        self.stream_reader.len() / size_of::<Value>() as u64
    }

    /// Number of items in one block of the stream.
    pub const fn block_length(&self) -> usize {
        StreamReader::<S>::block_size() / size_of::<Value>()
    }

    /// Current size of the stream in bytes.
    pub fn size(&self) -> u64 {
        self.stream_reader.len()
    }

    /// Get a single item from the stream.
    pub fn get(&mut self, id: Id<Value>) -> Result<Value, Error> {
        let byte_range = id.offset_range();
        let bytes = self.stream_reader.access(&byte_range)?;
        let value = from_bytes(&bytes);
        Ok(*value)
    }

    /// Get multiple items from the stream.
    pub fn get_range(&mut self, range: &Range<Id<Value>>)
        -> Result<Vec<Value>, Error>
    {
        let count = (range.end - range.start).try_into().unwrap();
        let mut result = Vec::with_capacity(count);
        let mut byte_range = range.start.offset()..range.end.offset();
        while result.len() < count {
            let bytes = self.stream_reader.access(&byte_range)?;
            let values = cast_slice(&bytes);
            result.extend_from_slice(values);
            byte_range.start += bytes.len() as u64;
        }
        Ok(result)
    }

    /// Access values in the stream.
    ///
    /// Returns a reference to a slice of values, which may have less than the
    /// requested length. May be called again to access further values.
    ///
    pub fn access(&mut self, range: &Range<Id<Value>>)
        -> Result<impl Deref<Target=[Value]>, Error>
    {
        let range = range.start.offset()..range.end.offset();
        Ok(Values {
            marker: PhantomData,
            data: self.stream_reader.access(&range)?
        })
    }
}

impl<Data, Value> Deref for Values<Data, Value>
where Data: Deref<Target=[u8]>,
      Value: Pod
{
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        cast_slice(self.data.deref())
    }
}

impl<Value, const S: usize> std::fmt::Display for DataWriter<Value, S>
where Value: Pod + Default
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} items, {}", fmt_count(self.len()), fmt_size(self.size()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck_derive::{Pod, Zeroable};

    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
    #[repr(C)]
    struct Foo {
        bar: u32,
        baz: u32,
    }

    #[test]
    fn test_data_stream_push() {
        let (mut writer, mut reader) = data_stream().unwrap();
        for i in 0..100 {
            let x = Foo { bar: i, baz: i };
            writer.push(&x).unwrap();
            assert!(reader.get(Id::<Foo>::from(i as u64)).unwrap() == x);
        }
    }

    #[test]
    fn test_data_stream_append() {
        let (mut writer, mut reader) = data_stream().unwrap();

        // Build a Vec of data
        let mut data = Vec::new();
        for i in 0..100 {
            let item = Foo { bar: i, baz: i };
            data.push(item)
        }

        // append it to the stream
        writer.append(&data.as_slice()).unwrap();

        // and check
        let start = Id::<Foo>::from(0);
        let end = Id::<Foo>::from(100);
        let range = start..end;
        let vec: Vec<_> = reader.get_range(&range).unwrap();
        assert!(vec == data);
    }
}
