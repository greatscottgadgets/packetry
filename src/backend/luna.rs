use rusb::{
    GlobalContext, DeviceHandle,
};
use std::collections::VecDeque;
use std::thread::{spawn, JoinHandle};
use std::sync::mpsc::{channel, Sender, Receiver};
use std::time::Duration;

const VID: u16 = 0x1d50;
const PID: u16 = 0x615b;

const ENDPOINT: u8 = 0x81;

const READ_LEN: usize = 0x4000;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Usb(#[from] rusb::Error),
    #[error("channel send error")]
    ChannelSend,
    #[error("worker thread panic")]
    ThreadPanic,
    #[error("device not found")]
    NotFound,
}

pub struct LunaDevice {
    handle: DeviceHandle<GlobalContext>,
}

pub struct LunaCapture {
    receiver: Receiver<Vec<u8>>,
    stop_request: Sender<()>,
    worker: JoinHandle::<Result<(), Error>>,
}

impl LunaDevice {
    pub fn open() -> Result<Self, Error> {
        let handle = rusb::open_device_with_vid_pid(VID, PID).ok_or(Error::NotFound)?;
        Ok(LunaDevice {
            handle,
        })
    }

    pub fn start(mut self) -> Result<LunaCapture, Error> {
        self.handle.claim_interface(0)?;
        let (tx, rx) = channel();
        let (stop_tx, stop_rx) = channel();
        let worker = spawn(move || {
            let mut buffer = [0u8; READ_LEN];
            let mut packet_queue = PacketQueue::new();
            self.enable_capture(true)?;
            println!("Capture enabled");
            while stop_rx.try_recv().is_err() {
                let result = self.handle.read_bulk(
                    ENDPOINT, &mut buffer, Duration::from_millis(100));
                match result {
                    Ok(count) => {
                        packet_queue.extend(&buffer[..count]);
                        while let Some(packet) = packet_queue.next() {
                            tx.send(packet).or(Err(Error::ChannelSend))?;
                        };
                    },
                    Err(rusb::Error::Timeout) => continue,
                    Err(error) => return Err(Error::from(error)),
                }
            }
            self.enable_capture(false)?;
            println!("Capture disabled");
            Ok(())
        });
        Ok(LunaCapture {
            stop_request: stop_tx,
            receiver: rx,
            worker,
        })
    }

    fn enable_capture(&mut self, enable: bool) -> Result<(), Error> {
        use rusb::{Direction, RequestType, Recipient, request_type};
        self.handle.write_control(
            request_type(Direction::Out, RequestType::Vendor, Recipient::Device),
            1,
            if enable { 1 } else { 0 },
            0,
            &[],
            Duration::from_secs(5),
        )?;
        Ok(())
    }
}

impl LunaCapture {
    pub fn next(&mut self) -> Option<Vec<u8>> {
        self.receiver.try_recv().ok()
    }

    pub fn stop(self) -> Result<(), Error> {
        use Error::*;
        println!("Requesting capture stop");
        self.stop_request.send(()).or(Err(ChannelSend))?;
        self.worker.join().or(Err(ThreadPanic))?
    }
}

struct PacketQueue {
    buffer: VecDeque<u8>,
}

impl PacketQueue {
    pub fn new() -> Self {
        PacketQueue {
            buffer: VecDeque::new(),
        }
    }

    pub fn extend(&mut self, slice: &[u8]) {
        self.buffer.extend(slice.iter());
    }

    pub fn next(&mut self) -> Option<Vec<u8>> {
        let buffer_len = self.buffer.len();
        if buffer_len <= 2 {
            return None;
        }
        let packet_len = u16::from_be_bytes([self.buffer[0], self.buffer[1]]) as usize;
        if buffer_len <= 2 + packet_len {
            return None;
        }

        self.buffer.drain(0..2);

        Some(self.buffer.drain(0..packet_len).collect())
    }
}
