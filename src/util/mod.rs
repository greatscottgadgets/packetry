//! Utility code that doesn't belong anywhere specific.

use std::ops::Range;

use anyhow::{Error, bail};
use num_format::{Locale, ToFormattedString};
use humansize::{SizeFormatter, BINARY};
use itertools::Itertools;

pub mod id;
pub mod vec_map;
pub mod rcu;

use id::Id;

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

pub fn handle_thread_panic<T>(result: std::thread::Result<T>)
    -> Result<T, Error>
{
    match result {
        Ok(x) => Ok(x),
        Err(panic) => {
            let msg = match (
                panic.downcast_ref::<&str>(),
                panic.downcast_ref::<String>())
            {
                (Some(&s), _) => s,
                (_,  Some(s)) => s,
                (None,  None) => "<No panic message>"
            };
            bail!("Worker thread panic: {msg}");
        }
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

pub struct Bytes<'src> {
    pub partial: bool,
    pub bytes: &'src [u8],
}

impl<'src> Bytes<'src> {
    pub fn first(max: usize, bytes: &'src [u8]) -> Self {
        if bytes.len() > max {
            Bytes {
                partial: true,
                bytes: &bytes[0..max],
            }
        } else {
            Bytes {
                partial: false,
                bytes,
            }
        }
    }

    fn looks_like_ascii(&self) -> bool {
        let mut num_printable = 0;
        for &byte in self.bytes {
            if byte == 0 || byte >= 0x80 {
                // Outside ASCII range.
                return false;
            }
            // Count printable and pseudo-printable characters.
            let printable = match byte {
                c if (0x20..0x7E).contains(&c) => true, // printable range
                0x09                           => true, // tab
                0x0A                           => true, // new line
                0x0D                           => true, // carriage return
                _ => false
            };
            if printable {
                num_printable += 1;
            }
        }
        // If the string is at least half printable, treat as ASCII.
        num_printable > 0 && num_printable >= self.bytes.len() / 2
    }
}

impl std::fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.looks_like_ascii() {
            write!(f, "'{}'", String::from_utf8(
                self.bytes.iter()
                          .flat_map(|c| {std::ascii::escape_default(*c)})
                          .collect::<Vec<u8>>()).unwrap())?
        } else {
            write!(f, "{:02X?}", self.bytes)?
        };
        if self.partial {
            write!(f, "...")
        } else {
            Ok(())
        }
    }
}

pub trait RangeLength {
   fn len(&self) -> u64;
}

impl<T> RangeLength for Range<Id<T>> {
   fn len(&self) -> u64 {
      self.end.value - self.start.value
   }
}

impl RangeLength for Range<u64> {
   fn len(&self) -> u64 {
      self.end - self.start
   }
}
