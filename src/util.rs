use num_format::{Locale, ToFormattedString};
use humansize::{FileSize, file_size_opts as options};

pub fn fmt_count(count: u64) -> String {
    count.to_formatted_string(&Locale::en)
}

pub fn fmt_size(size: u64) -> String {
    if size == 1 {
        "1 byte".to_string()
    } else if size < 1024 {
        format!("{size} bytes")
    } else {
        match size.file_size(options::BINARY) {
            Ok(string) => string,
            Err(e) => format!("<Error: {e}>")
        }
    }
}
