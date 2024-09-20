//! The backend API for USB capture devices.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread::{JoinHandle, spawn, sleep};
use std::time::Duration;

use anyhow::{Context, Error};
use futures_channel::oneshot;
use futures_lite::future::block_on;
use nusb::{self, DeviceInfo};
use once_cell::sync::Lazy;

use crate::capture::CaptureMetadata;
use crate::util::handle_thread_panic;
pub use crate::usb::Speed;

pub mod cynthion;
pub mod ice40usbtrace;
pub mod transfer_queue;

use transfer_queue::TransferQueue;

type VidPid = (u16, u16);
type ProbeFn = fn(DeviceInfo) -> Result<Box<dyn BackendDevice>, Error>;

/// Map of supported (VID, PID) pairs to device-specific probe functions.
static SUPPORTED_DEVICES: Lazy<BTreeMap<VidPid, (&str, ProbeFn)>> = Lazy::new(||
    BTreeMap::from_iter([
        (cynthion::VID_PID,
            ("Cynthion", cynthion::probe as ProbeFn)),
        (ice40usbtrace::VID_PID,
            ("iCE40-usbtrace", ice40usbtrace::probe as ProbeFn)),
    ])
);

/// The result of identifying and probing a supported USB device.
pub struct ProbeResult {
    pub name: &'static str,
    pub info: DeviceInfo,
    pub result: Result<Box<dyn BackendDevice>, String>,
}

/// Scan for supported devices.
pub fn scan() -> Result<Vec<ProbeResult>, Error> {
    Ok(nusb::list_devices()?
        .filter_map(|info|
            SUPPORTED_DEVICES
                .get(&(info.vendor_id(), info.product_id()))
                .map(|(name, probe)| (name, probe(info.clone())))
                .map(|(name, result)|
                    ProbeResult {
                        name,
                        info,
                        result: result.map_err(|e| format!("{e}"))
                    }
                ))
        .collect()
    )
}

/// A capture device connected to the system, not currently opened.
pub trait BackendDevice {
    /// Open this device to use it as a generic capture device.
    fn open_as_generic(&self) -> Result<Box<dyn BackendHandle>, Error>;

    /// Which speeds this device supports.
    fn supported_speeds(&self) -> &[Speed];
}

/// A timestamped packet.
pub struct TimestampedPacket {
    pub timestamp_ns: u64,
    pub bytes: Vec<u8>,
}

/// Handle used to stop an ongoing capture.
pub struct BackendStop {
    stop_tx: oneshot::Sender<()>,
    worker: JoinHandle::<()>,
}

pub type PacketResult = Result<TimestampedPacket, Error>;
pub trait PacketIterator: Iterator<Item=PacketResult> + Send {}

/// A handle to an open capture device.
pub trait BackendHandle: Send + Sync {

    /// Get metadata about the capture device.
    fn metadata(&self) -> &CaptureMetadata;

    /// Begin capture.
    ///
    /// This method should send whatever control requests etc are necessary to
    /// start capture, then set up and return a `TransferQueue` that sends the
    /// raw data from the device to `data_tx`.
    fn begin_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Vec<u8>>)
    -> Result<TransferQueue, Error>;

    /// End capture.
    ///
    /// This method should send whatever control requests etc are necessary to
    /// stop the capture. The transfer queue will be kept running for a short
    /// while afterwards to receive data that is still queued in the device.
    fn end_capture(&mut self) -> Result<(), Error>;

    /// Post-capture cleanup.
    ///
    /// This method will be called after the transfer queue has been shut down,
    /// and should do any cleanup necessary before next use.
    fn post_capture(&mut self) -> Result<(), Error>;

    /// Construct an iterator that produces timestamped packets from raw data.
    ///
    /// This method must construct a suitable iterator type around `data_rx`,
    /// which will parse the raw data from the device to produce timestamped
    /// packets. The iterator type must be `Send` so that it can be passed to
    /// a separate decoder thread.
    ///
    fn timestamped_packets(&self, data_rx: mpsc::Receiver<Vec<u8>>)
        -> Box<dyn PacketIterator>;

    /// Duplicate this handle with Box::new(self.clone())
    ///
    /// The device handle must be cloneable, so that one worker thread can
    /// process the data transfer queue asynchronously, whilst another thread
    /// does control transfers using synchronous calls.
    ///
    /// However, it turns out we cannot actually make `Clone` a prerequisite
    /// of `BackendHandle`, because doing so prevents the trait from being
    /// object safe. This method provides a workaround.
    fn duplicate(&self) -> Box<dyn BackendHandle>;

    /// Start capturing in the background.
    ///
    /// The `result_handler` callback will be invoked later from a worker
    /// thread, once the capture is either stopped normally or terminates with
    /// an error.
    ///
    /// Returns:
    /// - an iterator over timestamped packets
    /// - a handle to stop the capture
    fn start(
        &self,
        speed: Speed,
        result_handler: Box<dyn FnOnce(Result<(), Error>) + Send>
    ) -> Result<(Box<dyn PacketIterator>, BackendStop), Error> {
        // Channel to pass captured data to the decoder thread.
        let (data_tx, data_rx) = mpsc::channel();

        // Channel to stop the capture thread on request.
        let (stop_tx, stop_rx) = oneshot::channel();

        // Duplicate this handle to pass to the worker thread.
        let mut handle = self.duplicate();

        // Start worker thread to run the capture.
        let worker = spawn(move || result_handler(
            handle.run_capture(speed, data_tx, stop_rx)
        ));

        // Iterator over timestamped packets.
        let packets = self.timestamped_packets(data_rx);

        // Handle to stop the worker thread.
        let stop_handle = BackendStop { worker, stop_tx };

        Ok((packets, stop_handle))
    }

    /// Worker that runs the whole lifecycle of a capture from start to finish.
    fn run_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Vec<u8>>,
        stop_rx: oneshot::Receiver<()>,
    ) -> Result<(), Error> {
        // Set up a separate channel pair to stop queue processing.
        let (queue_stop_tx, queue_stop_rx) = oneshot::channel();

        // Begin capture and set up transfer queue.
        let mut transfer_queue = self.begin_capture(speed, data_tx)?;
        println!("Capture enabled, speed: {}", speed.description());

        // Spawn a worker thread to process the transfer queue until stopped.
        let queue_worker = spawn(move ||
            block_on(transfer_queue.process(queue_stop_rx))
        );

        // Wait until this thread is signalled to stop, or the stop request
        // sender is dropped.
        let _ = block_on(stop_rx);

        // End capture.
        self.end_capture()?;
        println!("Capture disabled");

        // Leave queue worker running briefly to receive flushed data.
        sleep(Duration::from_millis(100));

        // Signal queue processing to stop, then join the worker thread. If
        // sending fails, assume the thread is already stopping.
        let _ = queue_stop_tx.send(());

        handle_thread_panic(queue_worker.join())?
            .context("Error in queue worker thread")?;

        // Run any post-capture cleanup required by the device.
        self.post_capture()?;

        Ok(())
    }
}

impl BackendStop {
    /// Stop the capture associated with this handle.
    pub fn stop(self) -> Result<(), Error> {
        println!("Requesting capture stop");
        // Signal the capture thread to stop, then join it. If sending fails,
        // assume the thread is already stopping.
        let _ = self.stop_tx.send(());
        handle_thread_panic(self.worker.join())?;
        Ok(())
    }
}
