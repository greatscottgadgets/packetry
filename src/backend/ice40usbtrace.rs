//! USB capture backend for iCE40-usbtrace.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Error, anyhow};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use nusb::{
    self,
    transfer::{Control, ControlType, Recipient, TransferError},
    DeviceInfo, Interface,
};

use crate::capture::CaptureMetadata;
use crate::usb::crc5;

use super::{
    BackendDevice,
    BackendHandle,
    PacketIterator,
    PacketResult,
    Speed,
    TimestampedPacket,
    TransferQueue,
};

pub const VID_PID: (u16, u16) = (0x1d50, 0x617e);
const INTERFACE: u8 = 1;
const ENDPOINT: u8 = 0x81;
const READ_LEN: usize = 1024;
const NUM_TRANSFERS: usize = 4;
const FS_ONLY: [Speed; 1] = [Speed::Full];

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
}

/// A handle to an open iCE40-usbtrace device.
#[derive(Clone)]
pub struct Ice40UsbtraceHandle {
    interface: Interface,
    metadata: CaptureMetadata,
}

/// Converts from received data bytes to timestamped packets.
pub struct Ice40UsbtraceStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
    ts: u64,
}

/// Probe an iCE40-usbtrace device.
pub fn probe(device_info: DeviceInfo) -> Result<Box<dyn BackendDevice>, Error> {
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
        .claim_interface(INTERFACE)
        .context("Failed to claim interface")?;

    // Now we have a usable device.
    Ok(Box::new(Ice40UsbtraceDevice { device_info }))
}

impl BackendDevice for Ice40UsbtraceDevice {
    fn open_as_generic(&self) -> Result<Box<dyn BackendHandle>, Error> {
        let device = self.device_info.open()?;
        let interface = device.claim_interface(INTERFACE)?;
        let metadata = CaptureMetadata {
            iface_desc: Some("iCE40-usbtrace".to_string()),
            .. Default::default()
        };
        Ok(Box::new(Ice40UsbtraceHandle { interface, metadata }))
    }

    fn supported_speeds(&self) -> &[Speed] {
        &FS_ONLY
    }
}

impl BackendHandle for Ice40UsbtraceHandle {
    fn metadata(&self) -> &CaptureMetadata {
        &self.metadata
    }

    fn begin_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Vec<u8>>
    ) -> Result<TransferQueue, Error> {
        // iCE40-usbtrace only supports full-speed captures
        assert_eq!(speed, Speed::Full);

        // Stop the device if it was left running before and ignore any errors
        self.write_request(Command::CaptureStop)?;
        self.write_request(Command::BufferFlush)?;

        // Start capture.
        self.write_request(Command::CaptureStart)?;

        // Set up transfer queue.
        Ok(TransferQueue::new(&self.interface, data_tx,
            ENDPOINT, NUM_TRANSFERS, READ_LEN))
    }

    fn end_capture(&mut self) -> Result<(), Error> {
        self.write_request(Command::CaptureStop)
    }

    fn post_capture(&mut self) -> Result<(), Error> {
        self.write_request(Command::BufferFlush)
    }

    fn timestamped_packets(&self, data_rx: mpsc::Receiver<Vec<u8>>)
        -> Box<dyn PacketIterator> {
        Box::new(
            Ice40UsbtraceStream {
                receiver: data_rx,
                buffer: VecDeque::new(),
                ts: 0,
            }
        )
    }

    fn duplicate(&self) -> Box<dyn BackendHandle> {
        Box::new(self.clone())
    }
}

impl Ice40UsbtraceHandle {
    fn write_request(&mut self, request: Command) -> Result<(), Error> {
        use Command::*;
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request: request.into(),
            value: 0,
            index: 0,
        };
        let data = &[];
        let timeout = Duration::from_secs(1);
        match self.interface.control_out_blocking(control, data, timeout) {
            Ok(_) => Ok(()),
            Err(err) => match (request, err) {
                (CaptureStop | BufferFlush, TransferError::Stall) => {
                    // Ignore a STALL for these commands. This can happen when
                    // the device was already stopped (e.g. because it went
                    // into the overrun state, or because we just haven't
                    // started it yet).
                    Ok(())
                }
                _ => {
                    // Propagate any other error.
                    Err(anyhow!("{request:?} command failed: {err}"))
                }
            }
        }
    }
}

impl PacketIterator for Ice40UsbtraceStream {}

impl Iterator for Ice40UsbtraceStream {
    type Item = PacketResult;

    fn next(&mut self) -> Option<PacketResult> {
        use ParseResult::*;
        loop {
            match self.parse_packet() {
                // Parsed a packet, return it.
                Parsed(pkt) => return Some(Ok(pkt)),
                // Parsed something we ignored, try again.
                Ignored => continue,
                // Need more data; block until we get it.
                NeedMoreData => match self.receiver.recv().ok() {
                    // Received more data; add it to the buffer and retry.
                    Some(bytes) => self.buffer.extend(bytes.iter()),
                    // Capture has ended, there are no more packets.
                    None => return None,
                },
                // Error; an invalid header was seen.
                ParseError(e) => return Some(Err(e)),
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
                TimestampedPacket {
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
                Parsed(TimestampedPacket {
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
                Parsed(TimestampedPacket {
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
                Parsed(TimestampedPacket {
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
    Parsed(TimestampedPacket),
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
