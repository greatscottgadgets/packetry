use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::{sleep, spawn};
use std::time::Duration;

use anyhow::{bail, Context as ErrorContext, Error};
use futures_channel::oneshot;
use futures_lite::future::block_on;
use futures_util::future::FusedFuture;
use futures_util::{select_biased, FutureExt};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use nusb::{
    self,
    transfer::{Control, ControlType, Queue, Recipient, RequestBuffer, TransferError},
    DeviceInfo, Interface,
};

use crate::usb::crc5;

use super::DeviceUsability::*;
use super::{handle_thread_panic, BackendStop, DeviceUsability, InterfaceSelection, Speed, TracePacket};

const VID: u16 = 0x1d50;
const PID: u16 = 0x617e;

const ENDPOINT: u8 = 0x81;

const READ_LEN: usize = 1024;
const NUM_TRANSFERS: usize = 4;

#[derive(Debug, Clone, Copy, IntoPrimitive)]
#[repr(u8)]
enum Command {
    //CaptureStatus = 0x10,
    CaptureStart = 0x12,
    CaptureStop = 0x13,
    //BufferGetLevel = 0x20,
    BufferFlush = 0x21,
}

/// A ICE40usbtrace device attached to the system.
pub struct Ice40UsbtraceDevice {
    pub device_info: DeviceInfo,
    pub usability: DeviceUsability,
}

/// A handle to an open ICE40usbtrace.
#[derive(Clone)]
pub struct Ice40UsbtraceHandle {
    interface: Interface,
}

pub struct Ice40UsbtraceQueue {
    tx: mpsc::Sender<Vec<u8>>,
    queue: Queue<RequestBuffer>,
}

pub struct Ice40UsbStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
    ts: u64,
}

/// Check whether a ICE40usbtrace device has an accessible analyzer interface.
fn check_device(device_info: &DeviceInfo) -> Result<(), Error> {
    // Check we can open the device.
    let device = device_info.open().context("Failed to open device")?;

    // Read the active configuration.
    let _config = device
        .active_configuration()
        .context("Failed to retrieve active configuration")?;

    // Try to claim the interface.
    let _interface = device.claim_interface(1).context("Failed to claim interface")?;

    // Now we have a usable device.
    Ok(())
}

impl Ice40UsbtraceDevice {
    pub fn scan() -> Result<Vec<Ice40UsbtraceDevice>, Error> {
        Ok(nusb::list_devices()?
            .filter(|info| info.vendor_id() == VID)
            .filter(|info| info.product_id() == PID)
            .map(|device_info| match check_device(&device_info) {
                Ok(()) => Ice40UsbtraceDevice {
                    device_info,
                    usability: Usable(
                        InterfaceSelection {
                            interface_number: 1,
                            alt_setting_number: 0,
                        },
                        vec![Speed::Full],
                    ),
                },
                Err(err) => Ice40UsbtraceDevice {
                    device_info,
                    usability: Unusable(format!("{}", err)),
                },
            })
            .collect())
    }

    pub fn open(&self) -> Result<Ice40UsbtraceHandle, Error> {
        match &self.usability {
            Usable(iface, _) => {
                let device = self.device_info.open()?;
                let interface = device.claim_interface(iface.interface_number)?;
                if iface.alt_setting_number != 0 {
                    interface.set_alt_setting(iface.alt_setting_number)?;
                }
                Ok(Ice40UsbtraceHandle { interface })
            }
            Unusable(reason) => bail!("Device not usable: {}", reason),
        }
    }
}

impl Ice40UsbtraceHandle {
    pub fn start<F>(&self, speed: Speed, result_handler: F) -> Result<(Ice40UsbStream, BackendStop), Error>
    where
        F: FnOnce(Result<(), Error>) + Send + 'static,
    {
        // Channel to pass captured data to the decoder thread.
        let (tx, rx) = mpsc::channel();
        // Channel to stop the capture thread on request.
        let (stop_tx, stop_rx) = oneshot::channel();
        // Clone handle to give to the worker thread.
        let handle = self.clone();
        // Start worker thread.
        let worker = spawn(move || result_handler(handle.run_capture(speed, tx, stop_rx)));
        Ok((
            Ice40UsbStream {
                receiver: rx,
                buffer: VecDeque::new(),
                ts: 0,
            },
            BackendStop {
                stop_request: stop_tx,
                worker,
            },
        ))
    }

    fn run_capture(
        mut self,
        speed: Speed,
        tx: mpsc::Sender<Vec<u8>>,
        stop: oneshot::Receiver<()>,
    ) -> Result<(), Error> {
        // Set up a separate channel pair to stop queue processing.
        let (queue_stop_tx, queue_stop_rx) = oneshot::channel();

        // Stop the ICE40usbtrace was left running before and ignore any errors
        let _ = self.stop_capture();
        // Leave queue worker running briefly to receive flushed data.
        sleep(Duration::from_millis(100));
        let _ = self.flush_buffer();

        // ICE40usbtrace only supports full-speed captures
        assert_eq!(speed, Speed::Full);

        // Start capture.
        self.start_capture()?;

        // Set up transfer queue.
        let mut queue = Ice40UsbtraceQueue::new(&self.interface, tx);

        // Spawn a worker thread to process queue until stopped.
        let worker = spawn(move || block_on(queue.process(queue_stop_rx)));

        // Wait until this thread is signalled to stop.
        block_on(stop).context("Sender was dropped")?;

        // Stop capture.
        self.stop_capture()?;

        // Leave queue worker running briefly to receive flushed data.
        sleep(Duration::from_millis(100));

        // Signal queue processing to stop, then join the worker thread.
        queue_stop_tx
            .send(())
            .or_else(|_| bail!("Failed sending stop signal to queue worker"))?;
        handle_thread_panic(worker.join())?.context("Error in queue worker thread")?;

        self.flush_buffer()?;

        Ok(())
    }

    fn start_capture(&mut self) -> Result<(), Error> {
        self.write_request(Command::CaptureStart)?;
        //println!("Capture enabled, speed: {}", speed.description());
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<(), Error> {
        self.write_request(Command::CaptureStop)?;
        println!("Capture disabled");
        Ok(())
    }

    fn flush_buffer(&mut self) -> Result<(), Error> {
        self.write_request(Command::BufferFlush)?;
        println!("Buffer flushed");
        Ok(())
    }

    fn write_request(&mut self, request: Command) -> Result<(), Error> {
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request: request.into(),
            value: 0,
            index: 0,
        };
        let data = &[];
        let timeout = Duration::from_secs(1);
        self.interface
            .control_out_blocking(control, data, timeout)
            .context("Write request failed")?;
        Ok(())
    }
}

impl Ice40UsbtraceQueue {
    fn new(interface: &Interface, tx: mpsc::Sender<Vec<u8>>) -> Ice40UsbtraceQueue {
        let mut queue = interface.bulk_in_queue(ENDPOINT);
        while queue.pending() < NUM_TRANSFERS {
            queue.submit(RequestBuffer::new(READ_LEN));
        }
        Ice40UsbtraceQueue { queue, tx }
    }

    async fn process(&mut self, mut stop: oneshot::Receiver<()>) -> Result<(), Error> {
        use TransferError::Cancelled;
        loop {
            select_biased!(
                _ = stop => {
                    // Stop requested. Cancel all transfers.
                    self.queue.cancel_all();
                }
                completion = self.queue.next_complete().fuse() => {
                    match completion.status {
                        Ok(()) => {
                            // Send data to decoder thread.
                            self.tx.send(completion.data)
                                .context("Failed sending capture data to channel")?;
                            if !stop.is_terminated() {
                                // Submit next transfer.
                                self.queue.submit(RequestBuffer::new(READ_LEN));
                            }
                        },
                        Err(Cancelled) if stop.is_terminated() => {
                            // Transfer cancelled during shutdown. Drop it.
                            drop(completion);
                            if self.queue.pending() == 0 {
                                // All cancellations now handled.
                                return Ok(());
                            }
                        },
                        Err(usb_error) => {
                            // Transfer failed.
                            return Err(Error::from(usb_error));
                        }
                    }
                }
            );
        }
    }
}

impl Iterator for Ice40UsbStream {
    type Item = TracePacket;

    fn next(&mut self) -> Option<TracePacket> {
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
                    None => return None,
                },
            }
        }
    }
}

bitfield! {
    pub struct Header(MSB0 [u8]);
    impl Debug;
    u16;
    // 24MHz ticks
    pub ts, _: 31, 16;
    pub u8, pid, _: 3, 0;
    pub ok, _: 4;
    pub dat, _: 15, 5;
}

impl<T: std::convert::AsRef<[u8]>> Header<T> {
    pub fn pid_byte(&self) -> u8 {
        let pid = self.pid();

        pid | ((pid ^ 0xf) << 4)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Pid {
    Out = 0b0001,
    In = 0b1001,
    Sof = 0b0101,
    Setup = 0b1101,

    Data0 = 0b0011,
    Data1 = 0b1011,

    Ack = 0b0010,
    Nak = 0b1010,
    Stall = 0b1110,
    TsOverflow = 0b0000,
}

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Pid::Out => "OUT",
            Pid::In => "IN",
            Pid::Sof => "SOF",
            Pid::Setup => "SETUP",
            Pid::Data0 => "DATA0",
            Pid::Data1 => "DATA1",
            Pid::Ack => "ACK",
            Pid::Nak => "NAK",
            Pid::Stall => "STALL",
            Pid::TsOverflow => "TS OVERFLOW",
        };
        write!(f, "{}", s)
    }
}

impl Ice40UsbStream {
    fn ns(&self) -> u64 {
        // 24MHz clock, a tick is 41.666...ns
        const TABLE: [u64; 3] = [0, 41, 83];
        let quotient = self.ts / 3;
        let remainder = self.ts % 3;
        quotient * 125 + TABLE[remainder as usize]
    }

    fn parse_packet(&mut self) -> ParseResult<TracePacket> {
        let header: Vec<u8> = self.buffer.drain(0..4).collect();
        let header = Header(&header);

        self.ts += u64::from(header.ts());

        let pkt = match (header.pid().try_into(), header.ok()) {
            // The packet header could not even be decoded, skip it
            (Ok(Pid::TsOverflow), false) => {
                println!("Bad packet!\n{header:?}");
                return ParseResult::Ignored;
            }
            // Need to increment self.ts
            (Ok(Pid::TsOverflow), true) => ParseResult::Ignored,
            // Handle Data packet. If the CRC16 is wrong get_ok() returns false - push broken packet regardless
            (Ok(Pid::Data0 | Pid::Data1), data_ok) => {
                if !data_ok {
                    println!("Data packet with corrupt checksum:\n{header:?}");
                }

                let mut bytes = vec![header.pid_byte()];
                let data_len: usize = header.dat().into();
                if self.buffer.len() < data_len {
                    for byte in header.0.iter().rev() {
                        self.buffer.push_front(*byte);
                    }
                    return ParseResult::NeedMoreData;
                }
                bytes.extend(self.buffer.drain(0..data_len));
                ParseResult::Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }
            (Ok(Pid::Sof | Pid::Setup | Pid::In | Pid::Out), data_ok) => {
                let mut bytes = vec![header.pid_byte()];
                let mut data = header.dat().to_le_bytes();
                let crc = crc5(u32::from_le_bytes([data[0], data[1], 0, 0]), 11);
                if data_ok {
                    data[1] |= crc << 3;
                } else {
                    println!("PID pattern correct, but broken CRC5:\n{header:?}");
                    data[1] |= (!crc) << 3;
                }
                bytes.extend(data);

                ParseResult::Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }
            (Ok(Pid::Ack | Pid::Nak | Pid::Stall), data_ok) => {
                assert!(data_ok, "PID is all there is to decode!");
                let bytes = vec![header.pid_byte()];
                ParseResult::Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }
            (Err(_), _) => {
                println!("Error decoding PID for header:\n{header:?}");
                ParseResult::Ignored
            }
        };

        pkt
    }

    fn next_buffered_packet(&mut self) -> Option<TracePacket> {
        loop {
            // Need more bytes for the header
            if self.buffer.len() < 4 {
                return None;
            }

            match self.parse_packet() {
                ParseResult::Parsed(pkt) => return Some(pkt),
                ParseResult::Ignored => continue,
                ParseResult::NeedMoreData => return None,
            }
        }
    }
}

pub enum ParseResult<T> {
    Parsed(T),
    Ignored,
    NeedMoreData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header() {
        let data: [u8; 4] = [8, 1, 255, 241];
        let header = Header(&data);

        assert_eq!(header.ts(), 65521);
        assert_eq!(header.pid(), 0);
        assert!(header.ok());
        assert_eq!(header.dat(), 1);
    }
}
