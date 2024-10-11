use anyhow::{Error, bail};
use num_format::{Locale, ToFormattedString};
use humansize::{SizeFormatter, BINARY};

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
