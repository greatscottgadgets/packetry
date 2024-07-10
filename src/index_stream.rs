use std::cmp::min;
use std::marker::PhantomData;
use std::ops::Range;

use anyhow::Error;

use crate::data_stream::{data_stream, DataReader, DataWriter};
use crate::id::Id;
use crate::stream::MIN_BLOCK;
use crate::util::{fmt_count, fmt_size};

/// Unique handle for append-only write access to an index.
pub struct IndexWriter<Position, Value, const S: usize = MIN_BLOCK> {
    marker: PhantomData<(Position, Value)>,
    data_writer: DataWriter<u64, S>,
}

/// Cloneable handle for read-only random access to an index.
#[derive(Clone)]
pub struct IndexReader<Position, Value, const S: usize = MIN_BLOCK> {
    marker: PhantomData<(Position, Value)>,
    data_reader: DataReader<u64, S>,
}

type IndexPair<P, V> = (IndexWriter<P, V>, IndexReader<P, V>);

/// Construct a new index stream.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn index_stream<P, V>() -> Result<IndexPair<P, V>, Error> {
    let (data_writer, data_reader) = data_stream()?;
    let writer = IndexWriter {
        marker: PhantomData,
        data_writer,
    };
    let reader = IndexReader {
        marker: PhantomData,
        data_reader,
    };
    Ok((writer, reader))
}

impl<Position, Value> IndexWriter<Position, Value>
where Position: From<u64>, Value: Into<u64>
{
    /// Number of entries in the index.
    pub fn len(&self) -> u64 {
        self.data_writer.len()
    }

    /// Size of the index in bytes.
    pub fn size(&self) -> u64 {
        self.data_writer.size()
    }

    /// Add a single value to the end of the index.
    ///
    /// Returns the position of the added value.
    pub fn push(&mut self, value: Value) -> Result<Position, Error> {
        let id = self.data_writer.push(&value.into())?;
        let position = Position::from(id.into());
        Ok(position)
    }
}

impl<Position, Value> IndexReader<Position, Value>
where Position: Copy + From<u64> + Into<u64>,
      Value: Copy + From<u64> + Into<u64> + Ord
{
    /// Current number of indices in the index.
    pub fn len(&self) -> u64 {
        self.data_reader.len()
    }

    /// Current size of the index in bytes.
    pub fn size(&self) -> u64 {
        self.data_reader.size()
    }

    /// Get a single value from the index, by position.
    pub fn get(&mut self, position: Position) -> Result<Value, Error> {
        let id = Id::<u64>::from(position.into());
        let value = self.data_reader.get(id)?;
        Ok(Value::from(value))
    }

    /// Get multiple values from the index, for a range of positions.
    pub fn get_range(&mut self, range: &Range<Position>)
        -> Result<Vec<Value>, Error>
    {
        let start = Id::<u64>::from(range.start.into());
        let end = Id::<u64>::from(range.end.into());
        let data = self.data_reader.get_range(&(start..end))?;
        let values = data.into_iter().map(Value::from).collect();
        Ok(values)
    }

    /// Get the range of values between the specified position and the next.
    ///
    /// The length of the data referenced by this index must be passed
    /// as a parameter. If the specified position is the last in the
    /// index, the range will be from the last value in the index to the
    /// end of the referenced data.
    pub fn target_range(&mut self, position: Position, target_length: u64)
        -> Result<Range<Value>, Error>
    {
        let stop = position.into() + 2;
        let range = if stop > self.len() {
            let start = self.get(position)?;
            let end = Value::from(target_length);
            start..end
        } else {
            let range = position..Position::from(stop);
            let vec = self.get_range(&range)?;
            let start = vec[0];
            let end = vec[1];
            start..end
        };
        Ok(range)
    }

    /// Leftmost position where a value would be ordered within this index.
    pub fn bisect_left(&mut self, value: &Value)
        -> Result<Position, Error>
    {
        let range = Position::from(0)..Position::from(self.len());
        self.bisect_range_left(&range, value)
    }

    /// Rightmost position where a value would be ordered within this index.
    pub fn bisect_right(&mut self, value: &Value)
        -> Result<Position, Error>
    {
        let range = Position::from(0)..Position::from(self.len());
        self.bisect_range_right(&range, value)
    }

    /// Leftmost position where a value would be ordered within this range.
    pub fn bisect_range_left(&mut self, range: &Range<Position>, value: &Value)
        -> Result<Position, Error>
    {
        let mut search_start = range.start.into();
        let mut search_end = range.end.into();
        let search_length = search_end - search_start;
        if search_length == 0 {
            return Ok(Position::from(search_start));
        }
        let value = (*value).into();
        let block_length = self.data_reader.block_length() as u64;
        let block_mask = !(block_length - 1);
        let mut midpoint = (search_start + search_end) / 2;
        let position = loop {
            let block_start = midpoint & block_mask;
            let block_end = min(block_start + block_length, search_end);
            let block_range = block_start.into()..block_end.into();
            let block_values = self.data_reader.access(&block_range)?;
            let first = 0;
            let last = ((block_end - block_start) as usize) - 1;
            if block_values[first] >= value {
                if block_start == search_start {
                    break search_start;
                } else {
                    search_end = block_start;
                    midpoint = (search_start + block_start) / 2
                }
            } else if block_values[last] < value {
                if block_end == search_end {
                    break search_end;
                } else {
                    search_start = block_end;
                    midpoint = (block_end + search_end) / 2
                }
            } else {
                let mut lower_bound = 0;
                let mut upper_bound = block_values.len();
                while lower_bound < upper_bound {
                    let midpoint = (lower_bound + upper_bound) / 2;
                    if block_values[midpoint] < value {
                        lower_bound = midpoint + 1;
                    } else {
                        upper_bound = midpoint;
                    }
                }
                break block_start + lower_bound as u64;
            };
        };
        Ok(Position::from(position))
    }

    /// Rightmost position where a value would be ordered within this range.
    pub fn bisect_range_right(&mut self, range: &Range<Position>, value: &Value)
        -> Result<Position, Error>
    {
        let mut search_start = range.start.into();
        let mut search_end = range.end.into();
        let search_length = search_end - search_start;
        if search_length == 0 {
            return Ok(Position::from(search_start));
        }
        let value = (*value).into();
        let block_length = self.data_reader.block_length() as u64;
        let block_mask = !(block_length - 1);
        let mut midpoint = search_start + search_length / 2;
        let position = loop {
            let block_start = midpoint & block_mask;
            let block_end = min(block_start + block_length, search_end);
            let block_range = block_start.into()..block_end.into();
            let block_values = self.data_reader.access(&block_range)?;
            let first = 0;
            let last = ((block_end - block_start) as usize) - 1;
            if block_values[first] > value {
                if block_start == search_start {
                    break search_start;
                } else {
                    let length_before = block_start - search_start;
                    search_end = block_start;
                    midpoint = block_start - length_before / 2;
                }
            } else if block_values[last] <= value {
                if block_end == search_end {
                    break search_end;
                } else {
                    search_start = block_end;
                    let length_after = search_end - block_end;
                    midpoint = block_end + length_after / 2;
                }
            } else {
                let mut lower_bound = 0;
                let mut upper_bound = block_values.len();
                while lower_bound < upper_bound {
                    let midpoint = (lower_bound + upper_bound) / 2;
                    if value < block_values[midpoint] {
                        upper_bound = midpoint;
                    } else {
                        lower_bound = midpoint + 1;
                    }
                }
                break block_start + lower_bound as u64;
            };
        };
        Ok(Position::from(position))
    }
}

impl<Position, Value> std::fmt::Display for IndexWriter<Position, Value>
where Position: From<u64>, Value: Into<u64>
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} entries, {}", fmt_count(self.len()), fmt_size(self.size()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_stream() {
        let (mut writer, mut reader) = index_stream().unwrap();
        let mut expected = Vec::<Id<u8>>::new();
        let mut x = 10;
        let n = 4321;
        for i in 0..n {
            x += 1 + i % 3;
            let id = Id::<u8>::from(x);
            expected.push(id);
            writer.push(id).unwrap();
        }
        for i in 0..n {
            let id = Id::<Id<u8>>::from(i);
            let vi = reader.get(id).unwrap();
            let xi = expected[i as usize];
            assert!(vi == xi);
        }
        let end = Id::<Id<u8>>::from(n as u64);
        for i in 0..n {
            let start = Id::<Id<u8>>::from(i as u64);
            let vrng = start .. end;
            let xrng = i as usize .. n as usize;
            let vr = reader.get_range(&vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
        let start = Id::<Id<u8>>::from(0 as u64);
        for i in 0..n {
            let end = Id::<Id<u8>>::from(i as u64);
            let vrng = start .. end;
            let xrng = 0 as usize .. i as usize;
            let vr = reader.get_range(&vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
        for i in 0..(n - 10) {
            let start = Id::<Id<u8>>::from(i as u64);
            let end = Id::<Id<u8>>::from(i + 10 as u64);
            let vrng = start .. end;
            let xrng = i as usize .. (i + 10) as usize;
            let vr = reader.get_range(&vrng).unwrap();
            let xr = &expected[xrng];
            assert!(vr == xr);
        }
        for i in 0..n {
            let id = Id::<Id<u8>>::from(i);
            let vi = expected[i as usize];
            let bl = reader.bisect_left(&vi).unwrap();
            assert!(bl == id);
        }
        for i in 0..n {
            let id = Id::<Id<u8>>::from(i);
            let vi = expected[i as usize];
            let br = reader.bisect_right(&vi).unwrap();
            assert!(br == id + 1);
        }
        let end = Id::<Id<u8>>::from(n);
        let big = expected[(n - 1) as usize] + 1;
        let bl = reader.bisect_left(&big).unwrap();
        let br = reader.bisect_right(&big).unwrap();
        assert!(bl == end);
        assert!(br == end);
    }
}
