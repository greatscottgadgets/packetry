use std::collections::VecDeque;
use std::thread::{spawn, JoinHandle};
use std::time::Duration;
use std::sync::mpsc;

use anyhow::{Context as ErrorContext, Error, bail};
use futures_channel::oneshot;
use futures_lite::future::block_on;
use num_enum::{FromPrimitive, IntoPrimitive};
use nusb::{
    self,
    transfer::{
        Control,
        ControlType,
        Recipient,
        RequestBuffer,
    },
    DeviceInfo,
    Interface
};

const VID: u16 = 0x1d50;
const PID: u16 = 0x615b;

const MIN_SUPPORTED: u16 = 0x0002;
const NOT_SUPPORTED: u16 = 0x0003;

const ENDPOINT: u8 = 0x81;

const READ_LEN: usize = 0x4000;

#[derive(Copy, Clone, FromPrimitive, IntoPrimitive)]
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

bitfield! {
    #[derive(Copy, Clone)]
    struct State(u8);
    bool, enable, set_enable: 0;
    u8, from into Speed, speed, set_speed: 2, 1;
}

impl State {
    fn new(enable: bool, speed: Speed) -> State {
        let mut state = State(0);
        state.set_enable(enable);
        state.set_speed(speed);
        state
    }
}

/// A Luna device attached to the system.
pub struct LunaDevice {
    device_info: DeviceInfo,
    pub description: String,
    pub speeds: Vec<Speed>,
}

/// A handle to an open Luna device.
pub struct LunaHandle {
    interface: Interface,
}

pub struct LunaStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
}

pub struct LunaStop {
    stop_request: oneshot::Sender<()>,
    worker: JoinHandle::<()>,
}

impl LunaDevice {
    pub fn scan() -> Result<Vec<LunaDevice>, Error> {
        let mut result = Vec::new();
        for device_info in nusb::list_devices()? {
            if device_info.vendor_id() == VID &&
               device_info.product_id() == PID
            {
                let version = device_info.device_version();
                if !(MIN_SUPPORTED..=NOT_SUPPORTED).contains(&version) {
                    continue;
                }
                let manufacturer = device_info
                    .manufacturer_string()
                    .unwrap_or("Unknown");
                let product = device_info
                    .product_string()
                    .unwrap_or("Device");
                let description = format!("{} {}", manufacturer, product);
                let handle = LunaHandle::new(&device_info)?;
                let speeds = handle.speeds()?;
                result.push(LunaDevice{
                    device_info,
                    description,
                    speeds,
                })
            }
        }
        Ok(result)
    }

    pub fn open(&self) -> Result<LunaHandle, Error> {
        LunaHandle::new(&self.device_info)
    }
}

impl LunaHandle {
    fn new(device_info: &DeviceInfo) -> Result<LunaHandle, Error> {
        let device = device_info.open()?;
        let interface = device.claim_interface(0)?;
        Ok(LunaHandle { interface })
    }

    pub fn speeds(&self) -> Result<Vec<Speed>, Error> {
        use Speed::*;
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: 2,
            value: 0,
            index: 0,
        };
        let mut buf = [0; 64];
        let timeout = Duration::from_secs(1);
        let size = self.interface
            .control_in_blocking(control, &mut buf, timeout)
            .context("Failed retrieving supported speeds from device")?;
        if size != 1 {
            bail!("Expected 1-byte response to speed request, got {size}");
        }
        let mut speeds = Vec::new();
        for speed in [Auto, High, Full, Low] {
            if buf[0] & speed.mask() != 0 {
                speeds.push(speed);
            }
        }
        Ok(speeds)
    }

    pub fn start<F>(mut self, speed: Speed, result_handler: F)
        -> Result<(LunaStream, LunaStop), Error>
        where F: FnOnce(Result<(), Error>) + Send + 'static
    {
        // Channel to pass captured data to the decoder thread.
        let (tx, rx) = mpsc::channel();
        // Channel to stop the capture thread on request.
        let (stop_tx, mut stop_rx) = oneshot::channel();
        // Capture thread.
        let mut run_capture = move || {
            let mut state = State::new(true, speed);
            self.write_state(state)?;
            println!("Capture enabled, speed: {}", speed.description());
            while stop_rx.try_recv() == Ok(None) {
                let buffer = RequestBuffer::new(READ_LEN);
                let completion = block_on(self.interface.bulk_in(ENDPOINT, buffer));
                match completion.status {
                    Ok(()) => {
                        // Transfer successful. Send data to decoder thread.
                        tx.send(completion.data)
                            .context("Failed sending capture data to channel")?;
                    },
                    Err(usb_error) => {
                        // Transfer failed.
                        return Err(Error::from(usb_error));
                    }
                }
            }
            // Stop capture.
            state.set_enable(false);
            self.write_state(state)?;
            println!("Capture disabled");
            Ok(())
        };
        let worker = spawn(move || result_handler(run_capture()));
        Ok((
            LunaStream {
                receiver: rx,
                buffer: VecDeque::new(),
            },
            LunaStop {
                stop_request: stop_tx,
                worker,
            }
        ))
    }

    fn write_state(&mut self, state: State) -> Result<(), Error> {
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: 1,
            value: u16::from(state.0),
            index: 0,
        };
        let data = &[];
        let timeout = Duration::from_secs(1);
        self.interface
            .control_out_blocking(control, data, timeout)
            .context("Failed writing state to device")?;
        Ok(())
    }
}

impl Iterator for LunaStream {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        loop {
            // Do we have another packet already in the buffer?
            match self.next_buffered_packet() {
                // Yes; return the packet.
                Some(packet) => return Some(packet),
                // No; wait for more data from the capture thread.
                None => match self.receiver.recv().ok() {
                    // Received more data; add it to the buffer and retry.
                    Some(bytes) => self.buffer.extend(bytes.iter()),
                    // Capture has ended, there are no more packets.
                    None => return None
                }
            }
        }
    }
}

impl LunaStream {
    fn next_buffered_packet(&mut self) -> Option<Vec<u8>> {
        // Do we have the length header for the next packet?
        let buffer_len = self.buffer.len();
        if buffer_len <= 2 {
            return None;
        }

        // Do we have all the data for the next packet?
        let packet_len = u16::from_be_bytes(
            [self.buffer[0], self.buffer[1]]) as usize;
        if buffer_len <= 2 + packet_len {
            return None;
        }

        // Remove the length header from the buffer.
        self.buffer.drain(0..2);

        // Remove the packet from the buffer and return it.
        Some(self.buffer.drain(0..packet_len).collect())
    }
}

impl LunaStop {
    pub fn stop(self) -> Result<(), Error> {
        println!("Requesting capture stop");
        self.stop_request.send(())
            .or_else(|_| bail!("Failed sending stop request"))?;
        match self.worker.join() {
            Ok(()) => Ok(()),
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
}
