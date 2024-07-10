use std::cmp::Ordering;
use std::collections::VecDeque;
use std::thread::{spawn, sleep, JoinHandle};
use std::time::Duration;
use std::sync::mpsc;

use anyhow::{Context as ErrorContext, Error, bail};
use futures_channel::oneshot;
use futures_lite::future::block_on;
use futures_util::future::FusedFuture;
use futures_util::{select_biased, FutureExt};
use num_enum::{FromPrimitive, IntoPrimitive};
use nusb::{
    self,
    transfer::{
        Control,
        ControlType,
        Queue,
        Recipient,
        RequestBuffer,
        TransferError,
    },
    DeviceInfo,
    Interface
};

const VID: u16 = 0x1d50;
const PID: u16 = 0x615b;

const CLASS: u8 = 0xff;
const SUBCLASS: u8 = 0x10;
const PROTOCOL: u8 = 0x01;

const ENDPOINT: u8 = 0x81;

const READ_LEN: usize = 0x4000;
const NUM_TRANSFERS: usize = 4;

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

bitfield! {
    #[derive(Copy, Clone)]
    struct TestConfig(u8);
    bool, connect, set_connect: 0;
    u8, from into Speed, speed, set_speed: 2, 1;
}

impl TestConfig {
    fn new(speed: Option<Speed>) -> TestConfig {
        let mut config = TestConfig(0);
        match speed {
            Some(speed) => {
                config.set_connect(true);
                config.set_speed(speed);
            },
            None => {
                config.set_connect(false);
            }
        };
        config
    }
}

pub struct InterfaceSelection {
    interface_number: u8,
    alt_setting_number: u8,
}

/// Whether a Cynthion device is ready for use as an analyzer.
pub enum CynthionUsability {
    /// Device is usable via the given interface, at supported speeds.
    Usable(InterfaceSelection, Vec<Speed>),
    /// Device not usable, with a string explaining why.
    Unusable(String),
}

use CynthionUsability::*;

/// A Cynthion device attached to the system.
pub struct CynthionDevice {
    pub device_info: DeviceInfo,
    pub usability: CynthionUsability,
}

/// A handle to an open Cynthion device.
#[derive(Clone)]
pub struct CynthionHandle {
    interface: Interface,
}

pub struct CynthionQueue {
    tx: mpsc::Sender<Vec<u8>>,
    queue: Queue<RequestBuffer>,
}

pub struct CynthionStream {
    receiver: mpsc::Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
    padding_due: bool,
    total_clk_cycles: u64,
}

pub struct CynthionStop {
    stop_request: oneshot::Sender<()>,
    worker: JoinHandle::<()>,
}

pub struct CynthionPacket {
    pub timestamp_ns: u64,
    pub bytes: Vec<u8>,
}

/// Convert 60MHz clock cycles to nanoseconds, rounding down.
fn clk_to_ns(clk_cycles: u64) -> u64 {
    const TABLE: [u64; 3] = [0, 16, 33];
    let quotient = clk_cycles / 3;
    let remainder = clk_cycles % 3;
    quotient * 50 + TABLE[remainder as usize]
}

/// Check whether a Cynthion device has an accessible analyzer interface.
fn check_device(device_info: &DeviceInfo)
    -> Result<(InterfaceSelection, Vec<Speed>), Error>
{
    // Check we can open the device.
    let device = device_info
        .open()
        .context("Failed to open device")?;

    // Read the active configuration.
    let config = device
        .active_configuration()
        .context("Failed to retrieve active configuration")?;

    // Iterate over the interfaces...
    for interface in config.interfaces() {
        let interface_number = interface.interface_number();

        // ...and alternate settings...
        for alt_setting in interface.alt_settings() {
            let alt_setting_number = alt_setting.alternate_setting();

            // Ignore if this is not our supported target.
            if alt_setting.class() != CLASS ||
               alt_setting.subclass() != SUBCLASS
            {
                continue;
            }

            // Check protocol version.
            let protocol = alt_setting.protocol();
            #[allow(clippy::absurd_extreme_comparisons)]
            match PROTOCOL.cmp(&protocol) {
                Ordering::Less =>
                    bail!("Analyzer gateware is newer (v{}) than supported by this version of Packetry (v{}). Please update Packetry.", protocol, PROTOCOL),
                Ordering::Greater =>
                    bail!("Analyzer gateware is older (v{}) than supported by this version of Packetry (v{}). Please update gateware.", protocol, PROTOCOL),
                Ordering::Equal => {}
            }

            // Try to claim the interface.
            let interface = device
                .claim_interface(interface_number)
                .context("Failed to claim interface")?;

            // Select the required alternate, if not the default.
            if alt_setting_number != 0 {
                interface
                    .set_alt_setting(alt_setting_number)
                    .context("Failed to select alternate setting")?;
            }

            // Fetch the available speeds.
            let handle = CynthionHandle { interface };
            let speeds = handle
                .speeds()
                .context("Failed to fetch available speeds")?;

            // Now we have a usable device.
            return Ok((
                InterfaceSelection {
                    interface_number,
                    alt_setting_number,
                },
                speeds
            ))
        }
    }

    bail!("No supported analyzer interface found");
}

impl CynthionDevice {
    pub fn scan() -> Result<Vec<CynthionDevice>, Error> {
        Ok(nusb::list_devices()?
            .filter(|info| info.vendor_id() == VID)
            .filter(|info| info.product_id() == PID)
            .map(|device_info|
                match check_device(&device_info) {
                    Ok((iface, speeds)) => CynthionDevice {
                        device_info,
                        usability: Usable(iface, speeds)
                    },
                    Err(err) => CynthionDevice {
                        device_info,
                        usability: Unusable(format!("{}", err))
                    }
                }
            )
            .collect())
    }

    pub fn open(&self) -> Result<CynthionHandle, Error> {
        match &self.usability {
            Usable(iface, _) => {
                let device = self.device_info.open()?;
                let interface = device.claim_interface(iface.interface_number)?;
                if iface.alt_setting_number != 0 {
                    interface.set_alt_setting(iface.alt_setting_number)?;
                }
                Ok(CynthionHandle { interface })
            },
            Unusable(reason) => bail!("Device not usable: {}", reason),
        }
    }
}

impl CynthionHandle {

    pub fn speeds(&self) -> Result<Vec<Speed>, Error> {
        use Speed::*;
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request: 2,
            value: 0,
            index: self.interface.interface_number() as u16,
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

    pub fn start<F>(&self, speed: Speed, result_handler: F)
        -> Result<(CynthionStream, CynthionStop), Error>
        where F: FnOnce(Result<(), Error>) + Send + 'static
    {
        // Channel to pass captured data to the decoder thread.
        let (tx, rx) = mpsc::channel();
        // Channel to stop the capture thread on request.
        let (stop_tx, stop_rx) = oneshot::channel();
        // Clone handle to give to the worker thread.
        let handle = self.clone();
        // Start worker thread.
        let worker = spawn(move ||
            result_handler(
                handle.run_capture(speed, tx, stop_rx)));
        Ok((
            CynthionStream {
                receiver: rx,
                buffer: VecDeque::new(),
                padding_due: false,
                total_clk_cycles: 0,
            },
            CynthionStop {
                stop_request: stop_tx,
                worker,
            }
        ))
    }

    fn run_capture(mut self,
                   speed: Speed,
                   tx: mpsc::Sender<Vec<u8>>,
                   stop: oneshot::Receiver<()>)
        -> Result<(), Error>
    {
        // Set up a separate channel pair to stop queue processing.
        let (queue_stop_tx, queue_stop_rx) = oneshot::channel();

        // Start capture.
        self.start_capture(speed)?;

        // Set up transfer queue.
        let mut queue = CynthionQueue::new(&self.interface, tx);

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
        queue_stop_tx.send(())
            .or_else(|_| bail!("Failed sending stop signal to queue worker"))?;
        handle_thread_panic(worker.join())?
            .context("Error in queue worker thread")?;

        Ok(())
    }

    fn start_capture(&mut self, speed: Speed) -> Result<(), Error> {
        self.write_request(1, State::new(true, speed).0)?;
        println!("Capture enabled, speed: {}", speed.description());
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<(), Error> {
        self.write_request(1, State::new(false, Speed::High).0)?;
        println!("Capture disabled");
        Ok(())
    }

    pub fn configure_test_device(&mut self, speed: Option<Speed>)
        -> Result<(), Error>
    {
        let test_config = TestConfig::new(speed);
        self.write_request(3, test_config.0)
            .context("Failed to set test device configuration")
    }

    fn write_request(&mut self, request: u8, value: u8) -> Result<(), Error> {
        let control = Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value: u16::from(value),
            index: self.interface.interface_number() as u16,
        };
        let data = &[];
        let timeout = Duration::from_secs(1);
        self.interface
            .control_out_blocking(control, data, timeout)
            .context("Write request failed")?;
        Ok(())
    }
}

impl CynthionQueue {

    fn new(interface: &Interface, tx: mpsc::Sender<Vec<u8>>)
        -> CynthionQueue
    {
        let mut queue = interface.bulk_in_queue(ENDPOINT);
        while queue.pending() < NUM_TRANSFERS {
            queue.submit(RequestBuffer::new(READ_LEN));
        }
        CynthionQueue { queue, tx }
    }

    async fn process(&mut self, mut stop: oneshot::Receiver<()>)
        -> Result<(), Error>
    {
        use TransferError::{Cancelled, Unknown};
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
                        //
                        // As of nusb 0.1.9, TransferError::Unknown may be
                        // returned instead of TransferError::Cancelled when
                        // Windows returns ERROR_OPERATION_ABORTED. This should
                        // be fixed in a future nusb release; see nusb PR #63.
                        //
                        Err(Cancelled | Unknown) if stop.is_terminated() => {
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

impl Iterator for CynthionStream {
    type Item = CynthionPacket;

    fn next(&mut self) -> Option<CynthionPacket> {
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

impl CynthionStream {
    fn next_buffered_packet(&mut self) -> Option<CynthionPacket> {
        // Are we waiting for a padding byte?
        if self.padding_due {
            if self.buffer.is_empty() {
                return None;
            } else {
                self.buffer.pop_front();
                self.padding_due= false;
            }
        }

        // Loop over any non-packet events, until we get to a packet.
        loop {
            // Do we have the length and timestamp for the next packet/event?
            if self.buffer.len() < 4 {
                return None;
            }

            if self.buffer[0] == 0xFF {
                // This is an event.
                let _event_code = self.buffer[1];

                // Update our cycle count.
                self.update_cycle_count();

                // Remove event from buffer.
                self.buffer.drain(0..4);
            } else {
                // This is a packet, handle it below.
                break;
            }
        }

        // Do we have all the data for the next packet?
        let packet_len = u16::from_be_bytes(
            [self.buffer[0], self.buffer[1]]) as usize;
        if self.buffer.len() <= 4 + packet_len {
            return None;
        }

        // Update our cycle count.
        self.update_cycle_count();

        // Remove the length and timestamp from the buffer.
        self.buffer.drain(0..4);

        // If packet length is odd, we will need to skip a padding byte after.
        if packet_len % 2 == 1 {
            self.padding_due = true;
        }

        // Remove the rest of the packet from the buffer and return it.
        Some(CynthionPacket {
            timestamp_ns: clk_to_ns(self.total_clk_cycles),
            bytes: self.buffer.drain(0..packet_len).collect()
        })
    }

    fn update_cycle_count(&mut self) {
        // Decode the cycle count.
        let clk_cycles = u16::from_be_bytes(
            [self.buffer[2], self.buffer[3]]);

        // Update our running total.
        self.total_clk_cycles += clk_cycles as u64;
    }
}

impl CynthionStop {
    pub fn stop(self) -> Result<(), Error> {
        println!("Requesting capture stop");
        self.stop_request.send(())
            .or_else(|_| bail!("Failed sending stop request"))?;
        handle_thread_panic(self.worker.join())?;
        Ok(())
    }
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
