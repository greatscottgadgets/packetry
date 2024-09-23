use std::thread::JoinHandle;

use anyhow::{bail, Error};
use futures_channel::oneshot;
use num_enum::{FromPrimitive, IntoPrimitive};

pub mod cynthion;
pub mod ice40usbtrace;
mod transfer_queue;

#[derive(Debug, Copy, Clone, PartialEq, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Speed {
    #[default]
    High = 0,
    Full = 1,
    Low  = 2,
    Auto = 3,
}

impl Speed {
    pub fn description(&self) -> &'static str {
        use Speed::*;
        match self {
            Auto => "Auto",
            High => "High (480Mbps)",
            Full => "Full (12Mbps)",
            Low => "Low (1.5Mbps)",
        }
    }

    pub fn mask(&self) -> u8 {
        use Speed::*;
        match self {
            Auto => 0b0001,
            Low  => 0b0010,
            Full => 0b0100,
            High => 0b1000,
        }
    }
}

pub struct InterfaceSelection {
    interface_number: u8,
    alt_setting_number: u8,
}

/// Whether a device is ready for use as an analyzer.
pub enum DeviceUsability {
    /// Device is usable via the given interface, at supported speeds.
    Usable(InterfaceSelection, Vec<Speed>),
    /// Device not usable, with a string explaining why.
    Unusable(String),
}

fn handle_thread_panic<T>(result: std::thread::Result<T>) -> Result<T, Error> {
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

#[derive(Debug)]
pub struct TracePacket {
    pub timestamp_ns: u64,
    pub bytes: Vec<u8>,
}

pub struct BackendStop {
    stop_request: oneshot::Sender<()>,
    worker: JoinHandle::<()>,
}

impl BackendStop {
    pub fn stop(self) -> Result<(), Error> {
        println!("Requesting capture stop");
        self.stop_request.send(())
            .or_else(|_| bail!("Failed sending stop request"))?;
        handle_thread_panic(self.worker.join())?;
        Ok(())
    }
}
