use std::cmp::max;
use std::fmt::Debug;
use std::iter::once;
use std::marker::PhantomData;
use std::ops::{Add, AddAssign, Range, Sub, SubAssign};
use std::sync::atomic::{AtomicU64, Ordering::{Acquire, Release}};
use std::sync::Arc;

use anyhow::{Error, bail};
use itertools::multizip;

use crate::data_stream::{data_stream, DataReader, DataWriter};
use crate::id::Id;
use crate::index_stream::{index_stream, IndexReader, IndexWriter};
use crate::util::{fmt_count, fmt_size};

type Offset = Id<u8>;
type SegmentId = Id<u8>;

/// Unique handle for append-only write access to a compact index stream.
pub struct CompactWriter<Position, Value, const MIN_WIDTH: usize = 1> {
    /// Committed length of this index available to readers.
    shared_length: Arc<AtomicU64>,
    /// Index of starting positions of each segment.
    segment_start_writer: IndexWriter<SegmentId, Position>,
    /// Index of base values of each segment.
    segment_base_writer: IndexWriter<SegmentId, Value>,
    /// Index of data offsets of each segment.
    segment_offset_writer: IndexWriter<SegmentId, Offset>,
    /// Delta widths of each segment.
    segment_width_writer: DataWriter<u8>,
    /// Byte stream containing deltas for all segments.
    data_writer: DataWriter<u8>,
    /// Current write position in the data stream.
    data_offset: Offset,
    /// Base value of the current segment, if there is one.
    current_base_value: Option<Value>,
    /// Delta width of the current segment, if there is one.
    current_delta_width: Option<usize>,
    /// Master length of the index.
    length: u64,
}

/// Cloneable handle for read-only random access to a compact index stream.
#[derive(Clone)]
pub struct CompactReader<Position, Value> {
    _marker: PhantomData<Value>,
    /// Committed length of this index available to readers.
    shared_length: Arc<AtomicU64>,
    /// Index of starting positions of each segment.
    segment_start_reader: IndexReader<SegmentId, Position>,
    /// Index of base values of each segment.
    segment_base_reader: IndexReader<SegmentId, Value>,
    /// Index of data offsets of each segment.
    segment_offset_reader: IndexReader<SegmentId, Offset>,
    /// Delta widths of each segment.
    segment_width_reader: DataReader<u8>,
    /// Byte stream containing deltas for all segments.
    data_reader: DataReader<u8>,
}

type CompactPair<P, V, const W: usize> =
    (CompactWriter<P, V, W>, CompactReader<P, V>);

/// Construct a new index stream.
///
/// Returns a unique writer and a cloneable reader.
///
pub fn compact_index<P, V, const W: usize>()
    -> Result<CompactPair<P, V, W>, Error>
{
    let (segment_start_writer, segment_start_reader) = index_stream()?;
    let (segment_base_writer, segment_base_reader) = index_stream()?;
    let (segment_offset_writer, segment_offset_reader) = index_stream()?;
    let (segment_width_writer, segment_width_reader) = data_stream()?;
    let (data_writer, data_reader) = data_stream()?;
    let shared_length = Arc::new(AtomicU64::from(0));
    let writer = CompactWriter {
        shared_length: shared_length.clone(),
        segment_start_writer,
        segment_base_writer,
        segment_offset_writer,
        segment_width_writer,
        data_writer,
        data_offset: Offset::from(0),
        current_base_value: None,
        current_delta_width: None,
        length: 0,
    };
    let reader = CompactReader {
        _marker: PhantomData,
        shared_length,
        segment_start_reader,
        segment_base_reader,
        segment_offset_reader,
        segment_width_reader,
        data_reader,
    };
    Ok((writer, reader))
}

impl<Position, Value, const MIN_WIDTH: usize>
CompactWriter<Position, Value, MIN_WIDTH>
where Position: Copy + From<u64> + Into<u64>,
      Value: Copy + Into<u64> + Sub<Output=u64>
{
    /// Current number of entries in the index.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Current size of the index in bytes.
    pub fn size(&self) -> u64 {
        self.segment_start_writer.size() +
            self.segment_base_writer.size() +
            self.segment_offset_writer.size() +
            self.segment_width_writer.size() +
            self.data_writer.size()
    }

    /// Add a single value to the end of the index.
    ///
    /// Returns the position of the added value.
    pub fn push(&mut self, value: Value) -> Result<Position, Error> {
        match self.current_base_value {
            None => self.start_segment(value)?,
            Some(current_base_value) => {
                let delta = value - current_base_value;
                let delta_width = max(byte_width(delta), MIN_WIDTH);
                match self.current_delta_width {
                    None => {
                        let delta_bytes = delta.to_le_bytes();
                        self.segment_width_writer.push(&(delta_width as u8))?;
                        self.data_writer.append(&delta_bytes[..delta_width])?;
                        self.data_offset += delta_width as u64;
                        self.current_delta_width = Some(delta_width);
                    },
                    Some(current_width) if delta_width > current_width => {
                        self.start_segment(value)?;
                    },
                    Some(current_width) => {
                        let delta_bytes = delta.to_le_bytes();
                        self.data_writer.append(&delta_bytes[..current_width])?;
                        self.data_offset += current_width as u64;
                    }
                }
            }
        }
        let position = Position::from(self.length);
        self.length += 1;
        self.shared_length.store(self.length, Release);
        Ok(position)
    }

    fn start_segment(&mut self, base_value: Value) -> Result<(), Error> {
        let segment_start = Position::from(self.length);
        self.segment_start_writer.push(segment_start)?;
        self.segment_base_writer.push(base_value)?;
        self.segment_offset_writer.push(self.data_offset)?;
        self.current_base_value = Some(base_value);
        self.current_delta_width = None;
        Ok(())
    }
}

impl<Position, Value> CompactReader<Position, Value>
where
    Position: Copy + From<u64> + Into<u64> + Ord
        + Add<u64, Output=Position> + AddAssign<u64>
        + Sub<u64, Output=Position> + SubAssign<u64> + Sub<Output=u64>
        + Debug,
    Value: Copy + From<u64> + Into<u64> + Ord
        + Add<u64, Output=Value>
        + Sub<Output=u64>
{
    /// Number of entries in the index.
    pub fn len(&self) -> u64 {
        self.shared_length.load(Acquire)
    }

    /// Size of the index in bytes.
    pub fn size(&self) -> u64 {
        self.segment_start_reader.size() +
            self.segment_base_reader.size() +
            self.segment_offset_reader.size() +
            self.segment_width_reader.size() +
            self.data_reader.size()
    }

    /// Get a single value from the index, by position.
    pub fn get(&mut self, position: Position) -> Result<Value, Error> {
        // Check position is valid.
        let length = self.len();
        if position.into() >= length {
            bail!("requested position {position:?} but index length is {length}")
        }
        // Find the segment required.
        let segment_id = self.segment_start_reader.bisect_right(&position)? - 1;
        let segment_start = self.segment_start_reader.get(segment_id)?;
        let base_value = self.segment_base_reader.get(segment_id)?;
        // If we only need the base value, return it.
        if position == segment_start {
            return Ok(base_value)
        }
        // Otherwise, get the details of the segment.
        let data_offset = self.segment_offset_reader.get(segment_id)?;
        let width = self.segment_width_reader.get(segment_id)? as usize;
        // Identify the delta we need and fetch it.
        let delta_index = position - segment_start - 1;
        let delta_start = data_offset + delta_index * width as u64;
        let byte_range = delta_start..(delta_start + width as u64);
        let delta_low_bytes = self.data_reader.get_range(&byte_range)?;
        // Reconstruct the delta and the complete value.
        let mut delta_bytes = [0; 8];
        delta_bytes[..width].copy_from_slice(delta_low_bytes.as_slice());
        let delta = u64::from_le_bytes(delta_bytes);
        Ok(base_value + delta)
    }

    /// Get multiple values from the index, for a range of positions.
    pub fn get_range(&mut self, range: &Range<Position>)
        -> Result<Vec<Value>, Error>
    {
        // Check range is valid.
        let length = self.len();
        if range.end.into() > length {
            bail!("requested range {range:?} but index length is {length}")
        }
        // Allocate space for the result.
        let total_count: usize = (range.end - range.start).try_into().unwrap();
        let mut values = Vec::with_capacity(total_count);
        // Determine which segments we need to read from.
        let first = self.segment_start_reader.bisect_right(&range.start)? - 1;
        let last = self.segment_start_reader.bisect_left(&range.end)? - 1;
        let seg_range = first..(last + 1);
        let segment_starts = self.segment_start_reader.get_range(&seg_range)?;
        let base_values = self.segment_base_reader.get_range(&seg_range)?;
        let data_offsets = self.segment_offset_reader.get_range(&seg_range)?;
        // Iterate over the segments.
        for (segment_id,
             segment_start,
             base_value,
             data_offset,
             start_position,
             end_position)
        in
            multizip((
                // The ID of each segment.
                (seg_range.start.value..seg_range.end.value).map(SegmentId::from),
                // The starting position of each segment.
                segment_starts.iter(),
                // The base value of each segment.
                base_values.iter(),
                // The data offset of each segment.
                data_offsets.into_iter(),
                // The start of the positions we need to read from each segment.
                once(&range.start).chain(segment_starts.iter().skip(1)),
                // The end of the positions we need to read from each segment.
                segment_starts.iter().chain(once(&range.end)).skip(1)))
        {
            // Count how many values we need to retrieve from this segment.
            let this_segment_count = *end_position - *start_position;
            // Check if we are including the base value of this segment.
            let base_included = *start_position == *segment_start;
            // If fetching the base value only, we don't need the delta width.
            let base_only = base_included && this_segment_count == 1;
            // Include the base value in the result if needed.
            if base_included {
                values.push(*base_value);
                // If we only needed the base value, proceed to next segment.
                if base_only {
                    continue;
                }
            }
            // Otherwise, fetch the width.
            let width = self.segment_width_reader.get(segment_id)? as usize;
            // Get delta range required.
            let (delta_start, num_deltas) = if base_included {
                // Deltas start at the beginning, and we need one fewer.
                (data_offset, this_segment_count - 1)
            } else {
                // Deltas start at an offset, and we need one for every value.
                let first_delta = *start_position - *segment_start - 1;
                let offset = data_offset + first_delta * width as u64;
                (offset, this_segment_count)
            };
            let delta_end = delta_start + num_deltas * width as u64;
            let byte_range = delta_start..delta_end;
            // Fetch all the required delta bytes for this segment.
            let all_delta_bytes = self.data_reader.get_range(&byte_range)?;
            // Reconstruct deltas and values to include in result.
            let mut delta_bytes = [0; 8];
            for low_bytes in all_delta_bytes.chunks_exact(width) {
                delta_bytes[..width].copy_from_slice(low_bytes);
                let delta = u64::from_le_bytes(delta_bytes);
                assert!(values.len() < total_count);
                values.push(*base_value + delta);
            }
        }
        assert!(values.len() == total_count);
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
        let range = if position.into() + 2 > self.len() {
            let start = self.get(position)?;
            let end = Value::from(target_length);
            start..end
        } else {
            let vec = self.get_range(&(position..(position + 2)))?;
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

    /// Leftmost position where a value would be ordered within this range.
    pub fn bisect_range_left(&mut self, range: &Range<Position>, value: &Value)
        -> Result<Position, Error>
    {
        // Find the segment required.
        let segment_id = match self.segment_base_reader.bisect_right(value)? {
            id if id.value == 0 => return Ok(Position::from(0)),
            id => id - 1,
        };
        let segment_start = self.segment_start_reader.get(segment_id)?;
        let base_value = self.segment_base_reader.get(segment_id)?;
        let delta_start = segment_start + 1;
        // If the value equals the base value, position is the segment start.
        if base_value == *value {
            return Ok(segment_start)
        // If there is no delta width yet, position follows the base value.
        } else if segment_id.value >= self.segment_width_reader.len() {
            return Ok(delta_start)
        }
        // Otherwise, get the delta width and delta byte range for the segment.
        let delta_start = segment_start + 1;
        let width = self.segment_width_reader.get(segment_id)? as usize;
        let mut byte_range = self.segment_offset_reader
            .target_range(segment_id, self.data_reader.len())?;
        let mut num_deltas = (byte_range.end - byte_range.start) / width as u64;
        let mut delta_range = delta_start..(delta_start + num_deltas);
        // Limit the range to access if possible.
        if range.start > delta_range.start {
            let skip = range.start - delta_range.start;
            byte_range.start += skip * width as u64;
            delta_range.start += skip;
            num_deltas -= skip;
        }
        if range.end < delta_range.end {
            let skip = delta_range.end - range.end;
            byte_range.end -= skip * width as u64;
            delta_range.end -= skip;
            num_deltas -= skip;
        }
        // Fetch all the delta bytes needed.
        let all_delta_bytes = self.data_reader.access(&byte_range)?;
        // Reconstruct deltas and values.
        let mut values = Vec::with_capacity(num_deltas as usize);
        let mut delta_bytes = [0; 8];
        for low_bytes in all_delta_bytes.chunks_exact(width) {
            delta_bytes[..width].copy_from_slice(low_bytes);
            let delta = u64::from_le_bytes(delta_bytes);
            values.push(base_value + delta);
        }
        // Bisect the values to find the position.
        let mut lower_bound = 0;
        let mut upper_bound = values.len();
        while lower_bound < upper_bound {
            let midpoint = (lower_bound + upper_bound) / 2;
            if &values[midpoint] < value {
                lower_bound = midpoint + 1;
            } else {
                upper_bound = midpoint;
            }
        }
        let position = delta_range.start + lower_bound as u64;
        Ok(position)
    }
}

fn byte_width(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        (8 - value.leading_zeros() / 8) as usize
    }
}

impl<Position, Value, const MIN_WIDTH: usize>
std::fmt::Display for CompactWriter<Position, Value, MIN_WIDTH>
where Position: Copy + From<u64> + Into<u64>,
      Value: Copy + Into<u64> + Sub<Output=u64>
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} entries in {} segments, {}",
               fmt_count(self.len()),
               fmt_count(self.segment_start_writer.len()),
               fmt_size(self.size()))
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
    fn test_compact_index() {
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
        let end = Id::<Id<u8>>::from(n);
        let big = expected[(n - 1) as usize] + 1;
        let bl = reader.bisect_left(&big).unwrap();
        assert!(bl == end);
    }
}
