//! Storage primitives for the capture database.

pub mod counter;
pub mod stream;
pub mod data_stream;
pub mod index_stream;
pub mod compact_index;
pub mod util;

pub use counter::{Counter, CounterSet, Snapshot};

pub use data_stream::{
    DataReader,
    DataWriter,
    DataSnapshot,
    DataReaderOps,
    data_stream,
    data_stream_with_block_size,
};

pub use compact_index::{
    CompactReader,
    CompactWriter,
    CompactSnapshot,
    CompactReaderOps,
    compact_index,
};
