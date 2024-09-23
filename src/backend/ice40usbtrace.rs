use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::{sleep, spawn};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Error};
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

/// An iCE40-usbtrace device attached to the system.
pub struct Ice40UsbtraceDevice {
    pub device_info: DeviceInfo,
    pub usability: DeviceUsability,
}

/// A handle to an open iCE40-usbtrace device.
#[derive(Clone)]
pub struct Ice40UsbtraceHandle {
    interface: Interface,
}

pub struct Ice40UsbtraceQueue {
    tx: mpsc::Sender<Vec<u8>>,
    queue: Queue<RequestBuffer>,
}

pub struct Ice40UsbtraceStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
    ts: u64,
}

/// Check whether an iCE40-usbtrace device has an accessible analyzer interface.
fn check_device(device_info: &DeviceInfo) -> Result<(), Error> {
    // Check we can open the device.
    let device = device_info
        .open()
        .context("Failed to open device")?;

    // Read the active configuration.
    let _config = device
        .active_configuration()
        .context("Failed to retrieve active configuration")?;

    // Try to claim the interface.
    let _interface = device
        .claim_interface(1)
        .context("Failed to claim interface")?;

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
    pub fn start<F>(&self, speed: Speed, result_handler: F) -> Result<(Ice40UsbtraceStream, BackendStop), Error>
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
            Ice40UsbtraceStream {
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

        // Stop the device if it was left running before and ignore any errors.
        let _ = self.stop_capture();
        // Leave queue worker running briefly to receive flushed data.
        sleep(Duration::from_millis(100));
        let _ = self.flush_buffer();

        // iCE40-usbtrace only supports full-speed captures.
        assert_eq!(speed, Speed::Full);

        // Start capture.
        self.start_capture()?;

        // Set up transfer queue.
        let mut queue = Ice40UsbtraceQueue::new(&self.interface, tx);

        // Spawn a worker thread to process queue until stopped.
        let worker = spawn(move || block_on(queue.process(queue_stop_rx)));

        // Wait until this thread is signalled to stop.
        block_on(stop)
            .context("Sender was dropped")?;

        // Stop capture.
        self.stop_capture()?;

        // Leave queue worker running briefly to receive flushed data.
        sleep(Duration::from_millis(100));

        // Signal queue processing to stop, then join the worker thread.
        queue_stop_tx
            .send(())
            .or_else(|_| bail!("Failed sending stop signal to queue worker"))?;
        handle_thread_panic(worker.join())?
            .context("Error in queue worker thread")?;

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

impl Iterator for Ice40UsbtraceStream {
    type Item = TracePacket;

    fn next(&mut self) -> Option<TracePacket> {
        use ParseResult::*;
        loop {
            match self.parse_packet() {
                // Parsed a packet, return it.
                Parsed(pkt) => return Some(pkt),
                // Parsed something we ignored, try again.
                Ignored => continue,
                // Need more data; block until we get it.
                NeedMoreData => match self.receiver.recv().ok() {
                    // Received more data; add it to the buffer and retry.
                    Some(bytes) => self.buffer.extend(bytes.iter()),
                    // Capture has ended, there are no more packets.
                    None => return None,
                },
                ParseError(e) => {
                    println!("{e}");
                }
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
        use Pid::*;
        let s = match self {
            Out => "OUT",
            In => "IN",
            Sof => "SOF",
            Setup => "SETUP",
            Data0 => "DATA0",
            Data1 => "DATA1",
            Ack => "ACK",
            Nak => "NAK",
            Stall => "STALL",
            TsOverflow => "TS OVERFLOW",
        };
        write!(f, "{}", s)
    }
}

impl Ice40UsbtraceStream {
    fn ns(&self) -> u64 {
        // 24MHz clock, a tick is 41.666...ns
        const TABLE: [u64; 3] = [0, 41, 83];
        let quotient = self.ts / 3;
        let remainder = self.ts % 3;
        quotient * 125 + TABLE[remainder as usize]
    }

    fn parse_packet(&mut self) -> ParseResult {
        use Pid::*;
        use ParseResult::*;

        // Need enough bytes for the header.
        if self.buffer.len() < 4 {
            return NeedMoreData;
        }

        let header: Vec<u8> = self.buffer.drain(0..4).collect();
        let header = Header(&header);

        self.ts += u64::from(header.ts());

        match (header.pid().try_into(), header.ok()) {
            // A SYNC pattern was seen on the wire but no valid PID followed,
            // so no packet data was captured. Generate a packet with a single
            // zero byte, which is an invalid PID. This will serve to indicate
            // the presence of a packet without a valid PID.
            (Ok(TsOverflow), false) => Parsed(
                TracePacket {
                    timestamp_ns: self.ns(),
                    bytes: vec![0]
                }
            ),

            // This header was sent because the timestamp field was
            // about to overflow. There was no packet captured.
            (Ok(TsOverflow), true) => Ignored,

            // A data packet was captured. The CRC16 may or may not be valid.
            // We'll pass the whole packet on either way, so we don't care
            // about the state of the OK flag here.
            (Ok(Data0 | Data1), _data_ok) => {
                // Check if we have the whole packet yet.
                let data_len: usize = header.dat().into();
                if self.buffer.len() < data_len {
                    // We don't have the whole packet yet. Put the header
                    // back in the buffer and wait for more data.
                    for byte in header.0.iter().rev() {
                        self.buffer.push_front(*byte);
                    }
                    return NeedMoreData;
                }
                let mut bytes = Vec::with_capacity(1 + data_len);
                bytes.push(header.pid_byte());
                bytes.extend(self.buffer.drain(0..data_len));
                Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }

            // A token packet was captured. The OK flag indicates if it
            // was valid, but we don't have the CRC bits seen on the wire.
            // Reconstruct the packet with a good or bad CRC as appropriate.
            (Ok(Sof | Setup | In | Out), data_ok) => {
                let mut bytes = vec![header.pid_byte()];
                let mut data = header.dat().to_le_bytes();
                // Calculate the CRC this packet should have had.
                let crc = crc5(u32::from_le_bytes([data[0], data[1], 0, 0]), 11);
                if data_ok {
                    // The packet was valid, so insert the correct CRC.
                    data[1] |= crc << 3;
                } else {
                    // The packet was invalid, so insert a bad CRC.
                    data[1] |= (!crc) << 3;
                }
                bytes.extend(data);

                Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }

            // A handshake packet was captured. If the OK flag is set then
            // the packet was valid, which implies the PID was the only byte
            // received.
            //
            // If the OK flag is not set then there must have been trailing
            // bytes present to make the packet invalid. So generate a packet
            // with the same flaw, by appending a single zero byte.
            (Ok(Ack | Nak | Stall), data_ok) => {
                let mut bytes = vec![header.pid_byte()];
                if !data_ok {
                    bytes.push(0);
                }
                Parsed(TracePacket {
                    timestamp_ns: self.ns(),
                    bytes,
                })
            }

            (Err(_), _) => ParseError(
                anyhow!("Error decoding PID for header:\n{header:?}")
            )
        }
    }
}

pub enum ParseResult {
    Parsed(TracePacket),
    ParseError(Error),
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
