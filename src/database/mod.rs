//! Storage primitives for the capture database.

mod counter;
mod stream;
mod data_stream;
mod index_stream;
mod compact_index;

pub use counter::{Counter, CounterSet};

pub use data_stream::{
    DataReader,
    DataWriter,
    DataReaderOps,
    data_stream,
    data_stream_with_block_size,
};

pub use compact_index::{
    CompactReader,
    CompactWriter,
    CompactReaderOps,
    compact_index,
};
