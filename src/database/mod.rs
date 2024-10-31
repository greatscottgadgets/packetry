//! Storage primitives for the capture database.

mod stream;
mod data_stream;
mod index_stream;
mod compact_index;

pub use data_stream::{
    DataReader,
    DataWriter,
    data_stream,
    data_stream_with_block_size,
};
pub use compact_index::{
    CompactReader,
    CompactWriter,
    compact_index,
};
