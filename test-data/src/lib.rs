use std::path::{Path, PathBuf};

pub fn data_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
}

pub fn path(relative: &str) -> PathBuf {
    data_dir().join(relative)
}

pub fn read(relative: &str) -> String {
    std::fs::read_to_string(path(relative)).unwrap()
}
