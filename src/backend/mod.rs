//! The backend API for USB capture devices.

use std::collections::BTreeMap;
use std::panic::UnwindSafe;
use std::sync::mpsc;
use std::thread::{JoinHandle, spawn};
use std::time::Duration;

use anyhow::{Context, Error};
use async_trait::async_trait;
use futures_channel::oneshot;
use futures_lite::future::block_on;
use futures_util::{stream::iter, StreamExt};
use nusb::{self, Device, DeviceInfo, Interface, transfer::Buffer};
use once_cell::sync::Lazy;
use portable_async_sleep::async_sleep;

use crate::capture::CaptureMetadata;
use crate::util::handle_thread_panic;
pub use crate::usb::Speed;
pub use crate::event::EventType;

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

/// Probe a USB device.
pub async fn probe(info: DeviceInfo) -> Option<ProbeResult> {
    SUPPORTED_DEVICES
        .get(&(info.vendor_id(), info.product_id()))
        .map(|(name, probe)| (name, probe(info.clone())))
        .map(|(name, result)|
            ProbeResult {
                name,
                info,
                result: result.map_err(|e| format!("{e}"))
            }
        )
}

/// Scan for supported devices.
pub async fn scan() -> Result<Vec<ProbeResult>, Error> {
    let devices = nusb::list_devices().await?;
    Ok(iter(devices)
        .filter_map(probe)
        .collect::<Vec<_>>()
        .await)
}

/// A capture device connected to the system, not currently opened.
#[async_trait]
pub trait BackendDevice {
    /// Open this device to use it as a generic capture device.
    async fn open_as_generic(&self) -> Result<Box<dyn BackendHandle>, Error>;

    /// Duplicate this device with Box::new(self.clone()).
    fn duplicate(&self) -> Box<dyn BackendDevice>;
}

/// A timestamped event.
pub enum TimestampedEvent {
    /// A packet was captured.
    Packet {
        /// Timestamp in nanoseconds.
        timestamp_ns: u64,
        /// The bytes of the packet.
        bytes: Vec<u8>,
    },
    /// An event occured.
    #[allow(dead_code)]
    Event {
        /// Timestamp in nanoseconds.
        timestamp_ns: u64,
        /// The type of event.
        event_type: EventType,
    }
}

/// Handle used to stop an ongoing capture.
pub struct BackendStop {
    stop_tx: oneshot::Sender<()>,
    worker: JoinHandle::<()>,
}

pub type EventResult = Result<TimestampedEvent, Error>;
pub trait EventIterator: Iterator<Item=EventResult> + Send + UnwindSafe {}

/// Configuration for power control.
#[derive(Clone)]
pub struct PowerConfig {
    /// Which source to power the target from.
    pub source_index: usize,
    /// Whether the target is on now.
    pub on_now: bool,
    /// Turn on when capture starts.
    pub start_on: bool,
    /// Turn off when capture stops.
    pub stop_off: bool,
}

/// A handle to an open capture device.
#[async_trait(?Send)]
pub trait BackendHandle: Send + Sync {

    /// Which speeds this device supports.
    fn supported_speeds(&self) -> &[Speed];

    /// Get metadata about the capture device.
    fn metadata(&self) -> &CaptureMetadata;

    /// Which power sources this device supports.
    fn power_sources(&self) -> Option<&[&str]>;

    /// The last known power configuration of this device.
    async fn power_config(&self) -> Option<PowerConfig>;

    /// Set power configuration.
    async fn set_power_config(&mut self, config: PowerConfig)
        -> Result<(), Error>;

    /// Begin capture.
    ///
    /// This method should send whatever control requests etc are necessary to
    /// start capture, then set up and return a `TransferQueue` that sends the
    /// raw data from the device to `data_tx`.
    async fn begin_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Buffer>)
    -> Result<TransferQueue, Error>;

    /// End capture.
    ///
    /// This method should send whatever control requests etc are necessary to
    /// stop the capture. The transfer queue will be kept running for a short
    /// while afterwards to receive data that is still queued in the device.
    async fn end_capture(&mut self) -> Result<(), Error>;

    /// Post-capture cleanup.
    ///
    /// This method will be called after the transfer queue has been shut down,
    /// and should do any cleanup necessary before next use.
    async fn post_capture(&mut self) -> Result<(), Error>;

    /// Construct an iterator that produces timestamped events from raw data.
    ///
    /// This method must construct a suitable iterator type around `data_rx`
    /// and `reuse_tx`, which will parse the raw data in USB buffers from
    /// the device to produce timestamped events. Used buffers should be sent
    /// to `reuse_tx` for reuse.
    ///
    /// The iterator type must be `Send` so that it can be passed to a
    /// separate decoder thread.
    ///
    fn timestamped_events(
        &self,
        data_rx: mpsc::Receiver<Buffer>,
        reuse_tx: mpsc::Sender<Buffer>,
    ) -> Box<dyn EventIterator>;

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
    /// - an iterator over timestamped events
    /// - a handle to stop the capture
    fn start(
        &self,
        speed: Speed,
        result_handler: Box<dyn FnOnce(Result<(), Error>) + Send>
    ) -> Result<(Box<dyn EventIterator>, BackendStop), Error> {
        // Channel to pass captured data to the decoder thread.
        let (data_tx, data_rx) = mpsc::channel();

        // Channel to return buffers for reuse.
        let (reuse_tx, reuse_rx) = mpsc::channel();

        // Channel to stop the capture thread on request.
        let (stop_tx, stop_rx) = oneshot::channel();

        // Duplicate this handle to pass to the worker thread.
        let mut handle = self.duplicate();

        // Start worker thread to run the capture.
        let worker = spawn(move || result_handler(
            block_on(handle.run_capture(speed, data_tx, reuse_rx, stop_rx))
        ));

        // Iterator over timestamped events.
        let events = self.timestamped_events(data_rx, reuse_tx);

        // Handle to stop the worker thread.
        let stop_handle = BackendStop { worker, stop_tx };

        Ok((events, stop_handle))
    }

    /// Worker that runs the whole lifecycle of a capture from start to finish.
    async fn run_capture(
        &mut self,
        speed: Speed,
        data_tx: mpsc::Sender<Buffer>,
        reuse_rx: mpsc::Receiver<Buffer>,
        stop_rx: oneshot::Receiver<()>,
    ) -> Result<(), Error> {
        // Set up a separate channel pair to stop queue processing.
        let (queue_stop_tx, queue_stop_rx) = oneshot::channel();

        // Begin capture and set up transfer queue.
        let mut transfer_queue = self.begin_capture(speed, data_tx).await?;
        println!("Capture enabled, speed: {}", speed.description());

        // Spawn a worker thread to process the transfer queue until stopped.
        let queue_worker = spawn(move ||
            block_on(transfer_queue.process(reuse_rx, queue_stop_rx))
        );

        // Wait until this thread is signalled to stop, or the stop request
        // sender is dropped.
        let _ = stop_rx.await;

        // End capture.
        self.end_capture().await?;
        println!("Capture disabled");

        // Leave queue worker running briefly to receive flushed data.
        async_sleep(Duration::from_millis(100)).await;

        // Signal queue processing to stop, then join the worker thread. If
        // sending fails, assume the thread is already stopping.
        let _ = queue_stop_tx.send(());

        handle_thread_panic(queue_worker.join())?
            .context("Error in queue worker thread")?;

        // Run any post-capture cleanup required by the device.
        self.post_capture().await?;

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

#[cfg(not(target_os="windows"))]
async fn claim_interface(device: &Device, interface: u8)
    -> Result<Interface, Error>
{
    device
        .claim_interface(interface)
        .await
        .context("Failed to claim interface")
}

#[cfg(target_os="windows")]
async fn claim_interface(device: &Device, interface: u8)
    -> Result<Interface, Error>
{
    let mut attempts = 0;
    loop {
        match device.claim_interface(interface).await {
            Err(_) if attempts < 5 => {
                async_sleep(Duration::from_millis(50)).await;
                attempts += 1;
                continue
            },
            result => return result.context("Failed to claim interface")
        }
    }
}
