use num_format::{Locale, ToFormattedString};
use humansize::{SizeFormatter, BINARY};
use itertools::Itertools;

pub fn fmt_count(count: u64) -> String {
    count.to_formatted_string(&Locale::en)
}

pub fn fmt_size(size: u64) -> String {
    if size == 1 {
        "1 byte".to_string()
    } else if size < 1024 {
        format!("{size} bytes")
    } else {
        format!("{}", SizeFormatter::new(size, BINARY))
    }
}

pub fn titlecase(text: &str) -> String {
    format!("{}{}",
        text
            .chars()
            .take(1)
            .map(|c| c.to_uppercase().to_string())
            .join(""),
        text
            .chars()
            .skip(1)
            .collect::<String>()
    )
}
