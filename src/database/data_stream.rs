//! Typed data stream implementation.
//!
//! Stores streams of specific types, rather than raw bytes.

use std::marker::PhantomData;
use std::mem::size_of;
use std::ops::{Deref, Range};

use anyhow::{Error, bail};
use bytemuck::{bytes_of, cast_slice, from_bytes, Pod};

use crate::util::id::Id;
use crate::database::{
    counter::{CounterSet, Snapshot},
    stream::{stream, StreamReader, StreamWriter, Data, MIN_BLOCK},
};
use crate::util::{dump::{Dump, restore}, fmt_count, fmt_size};

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

/// Handle for read-only access to a data stream at a snapshot.
pub struct DataSnapshot<'a, 'b, Value, const S: usize= MIN_BLOCK> {
    reader: &'a mut DataReader<Value, S>,
    snapshot: &'b Snapshot,
}

/// Iterator over data in a stream.
pub struct DataIterator<Value, const S: usize = MIN_BLOCK> {
    range: Range<Id<Value>>,
    stream_reader: StreamReader<S>,
    current_data: Option<(u64, Data<S>)>,
}

/// A read-only handle to values that are part of the stream.
struct Values<Data, Value> where Data: Deref<Target=[u8]> {
    marker: PhantomData<Value>,
    data: Data,
}

/// Operations supported by both `DataReader` and `DataSnapshot`.
pub trait DataReaderOps<Value, const S: usize = MIN_BLOCK> {
    const SOURCE_DESCRIPTION: &str;

    /// Current number of items in the stream.
    fn len(&self) -> u64;

    /// Get a single item from the stream.
    fn get(&mut self, id: Id<Value>) -> Result<Value, Error>;

    /// Get multiple items from the stream.
    fn get_range(&mut self, range: &Range<Id<Value>>)
        -> Result<Vec<Value>, Error>;

    /// Access values in the stream.
    ///
    /// Returns a reference to a slice of values, which may have less than the
    /// requested length. May be called again to access further values.
    ///
    fn access(&mut self, range: &Range<Id<Value>>)
        -> Result<impl Deref<Target=[Value]>, Error>;

    /// Create an iterator over values in the stream.
    fn iter(&self, range: &Range<Id<Value>>) -> DataIterator<Value, S>;

    fn check_id(&self, id: Id<Value>) -> Result<(), Error> {
        let length = self.len();
        let src = Self::SOURCE_DESCRIPTION;
        if id.value >= length {
            bail!("requested id {id:?} but {src} length is {length}")
        }
        Ok(())
    }

    fn check_range(&self, range: &Range<Id<Value>>) -> Result<(), Error> {
        let length = self.len();
        let src = Self::SOURCE_DESCRIPTION;
        if range.end.value > length {
            bail!("requested range {range:?} but {src} length is {length}")
        }
        Ok(())
    }
}

/// Construct a new data stream with the default block size.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn data_stream<Value>(db: &mut CounterSet)
    -> Result<(DataWriter<Value>, DataReader<Value>), Error>
{
    data_stream_with_block_size::<Value, MIN_BLOCK>(db)
}

/// Construct a new data stream with a specific block size.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn data_stream_with_block_size<Value, const S: usize>(db: &mut CounterSet)
    -> Result<(DataWriter<Value, S>, DataReader<Value, S>), Error>
{
    let (stream_writer, stream_reader) = stream(db)?;
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

impl<Value, const S: usize> DataReader<Value, S> {

    /// Current size of the stream in bytes.
    pub fn size(&self) -> u64 {
        self.stream_reader.len()
    }

    /// Number of items in one block of the stream.
    pub const fn block_length(&self) -> usize {
        StreamReader::<S>::block_size() / size_of::<Value>()
    }

    /// Create a handle to access this stream at a snapshot.
    pub fn at<'r, 's>(&'r mut self, snapshot: &'s Snapshot)
        -> DataSnapshot<'r, 's, Value, S>
    {
        DataSnapshot { reader: self, snapshot }
    }
}

impl<Value, const S: usize> DataReaderOps<Value, S> for DataReader<Value, S>
where Value: Pod + Default
{
    const SOURCE_DESCRIPTION: &str = "data stream";

    fn len(&self) -> u64 {
        self.stream_reader.len() / size_of::<Value>() as u64
    }

    fn get(&mut self, id: Id<Value>) -> Result<Value, Error> {
        self.check_id(id)?;
        let byte_range = id.offset_range();
        let bytes = self.stream_reader.access(&byte_range)?;
        let value = from_bytes(&bytes);
        Ok(*value)
    }

    fn get_range(&mut self, range: &Range<Id<Value>>)
        -> Result<Vec<Value>, Error>
    {
        self.check_range(range)?;
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

    fn access(&mut self, range: &Range<Id<Value>>)
        -> Result<impl Deref<Target=[Value]>, Error>
    {
        self.check_range(range)?;
        let range = range.start.offset()..range.end.offset();
        Ok(Values {
            marker: PhantomData,
            data: self.stream_reader.access(&range)?
        })
    }

    fn iter(&self, range: &Range<Id<Value>>) -> DataIterator<Value, S> {
        DataIterator {
            range: range.clone(),
            stream_reader: self.stream_reader.clone(),
            current_data: None,
        }
    }
}

impl<Value, const S: usize> DataReaderOps<Value, S>
for DataSnapshot<'_, '_, Value, S>
where Value: Pod + Default
{
    const SOURCE_DESCRIPTION: &'static str = "snapshot";

    fn len(&self) -> u64 {
        self.reader
            .stream_reader
            .len_at(self.snapshot) / size_of::<Value>() as u64
    }

    fn get(&mut self, id: Id<Value>) -> Result<Value, Error> {
        self.check_id(id)?;
        self.reader.get(id)
    }

    fn get_range(&mut self, range: &Range<Id<Value>>)
        -> Result<Vec<Value>, Error>
    {
        self.check_range(range)?;
        self.reader.get_range(range)
    }

    fn access(&mut self, range: &Range<Id<Value>>)
        -> Result<impl Deref<Target=[Value]>, Error>
    {
        self.check_range(range)?;
        self.reader.access(range)
    }

    fn iter(&self, range: &Range<Id<Value>>) -> DataIterator<Value, S> {
        self.reader.iter(range)
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

impl<Value, const S: usize> std::iter::Iterator for DataIterator<Value, S>
where Value: Pod
{
    type Item = Result<Value, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.range.is_empty() {
            return None
        }
        let start = self.range.start.offset();
        let end = start + size_of::<Value>() as u64;
        // Get the data this value can be found within.
        let (offset, data) = match &self.current_data {
            // We can reuse the current data...
            Some(current @ (offset, data))
                // ...if the data for the current value is within it.
                if start >= *offset && end <= *offset + data.len() as u64
                    => current,
            // Otherwise, make a new request for all remaining data.
            _ => {
                let remaining = start..self.range.end.offset();
                match self.stream_reader.access(&remaining) {
                    Err(err) => return Some(Err(err)),
                    Ok(data) => self.current_data.insert(
                        (start, data)
                    )
                }
            }
        };
        // Retrieve the value from the data.
        let value_start = (start - *offset) as usize;
        let value_end = (end - *offset) as usize;
        let value = *from_bytes(&data[value_start..value_end]);
        // Advance our range start for next iteration.
        self.range.start += 1;
        // Return the value found.
        Some(Ok(value))
    }
}

impl<V, const S: usize> Dump for DataReader<V, S> {
    fn dump(&self, dest: &std::path::Path) -> Result<(), Error> {
        self.stream_reader.dump(dest)
    }

    fn restore(db: &mut CounterSet, src: &std::path::Path)
        -> Result<Self, anyhow::Error>
    {
        Ok(DataReader {
            marker: PhantomData,
            stream_reader: restore(db, src)?,
        })
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

    fn setup<T>() -> (DataWriter::<T>, DataReader::<T>) {
        // Create a counter set.
        let mut db = CounterSet::new();
        data_stream(&mut db).unwrap()
    }

    #[test]
    fn test_data_stream_push() {
        let (mut writer, mut reader) = setup();
        for i in 0..100 {
            let x = Foo { bar: i, baz: i };
            writer.push(&x).unwrap();
            assert!(reader.get(Id::<Foo>::from(i as u64)).unwrap() == x);
        }
    }

    #[test]
    fn test_data_stream_append() {
        let (mut writer, mut reader) = setup();

        // Build a Vec of data
        let mut data = Vec::new();
        for i in 0..100 {
            let item = Foo { bar: i, baz: i };
            data.push(item)
        }

        // append it to the stream
        writer.append(data.as_slice()).unwrap();

        // and check
        let start = Id::<Foo>::from(0);
        let end = Id::<Foo>::from(100);
        let range = start..end;
        let vec: Vec<_> = reader.get_range(&range).unwrap();
        assert!(vec == data);
    }

    #[test]
    fn test_data_stream_iter() {
        let (mut writer, mut reader) = setup();
        for i in 0..100 {
            let x = Foo { bar: i, baz: i };
            writer.push(&x).unwrap();
        }
        let start = Id::<Foo>::from(0);
        let end = Id::<Foo>::from(100);
        let range = start..end;
        assert_eq!(
            reader
                .get_range(&range)
                .unwrap(),
            reader
                .iter(&range)
                .collect::<Result<Vec<Foo>, Error>>()
                .unwrap()
        );
    }
}
