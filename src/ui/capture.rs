//! Unified access to different forms of capture.

use std::sync::mpsc::{Sender, Receiver, channel};
use std::thread::{JoinHandle, spawn};

use crate::capture::{CaptureReader, CaptureReaderOps, CaptureSnapshot};
use crate::filter::{FilterReader, FilterSnapshot, create_filter};
use crate::util::{fmt_count, handle_thread_panic};

use anyhow::{Context, Error};

#[derive(Clone)]
pub struct Capture {
    pub reader: CaptureReader,
    pub snapshot: Option<CaptureSnapshot>,
    pub filter: Option<FilterReader>,
    pub filter_snapshot: Option<FilterSnapshot>,
}

impl Capture {
    pub fn set_snapshot(&mut self, snapshot: CaptureSnapshot) {
        if snapshot.complete {
            self.snapshot = None;
        } else {
            self.snapshot = Some(snapshot);
        }
    }

    pub fn set_filter_snapshot(&mut self, filter_snapshot: FilterSnapshot) {
        if filter_snapshot.complete {
            self.filter_snapshot = None;
        } else {
            self.filter_snapshot = Some(filter_snapshot);
        }
    }

    pub fn update_from(&mut self, other: &Capture) {
        self.snapshot = other.snapshot.clone();
        self.filter_snapshot = other.filter_snapshot.clone();
    }

    pub fn stats(&mut self) -> CaptureStats {
        if let Some(snapshot) = &self.snapshot {
            CaptureStats::from(&mut self.reader.at(snapshot))
        } else {
            CaptureStats::from(&mut self.reader)
        }
    }

    pub fn start_filtering(&mut self) -> Result<FilterThread, Error> {
        let (mut filter_writer, filter_reader) = create_filter()?;
        let filter_snapshot = filter_writer.snapshot();
        self.filter = Some(filter_reader);
        self.filter_snapshot = Some(filter_snapshot);
        let mut reader = self.reader.clone();
        if let Some(snapshot) = &self.snapshot {
            let snapshot = snapshot.clone();
            let (capture_snapshot_tx, capture_snapshot_rx) = channel();
            let (filter_snapshot_tx, filter_snapshot_rx) = channel();
            let join_handle = spawn(move || {
                filter_writer.catchup(&mut reader.at(&snapshot))?;
                loop {
                    filter_snapshot_tx.send(filter_writer.snapshot())?;
                    if filter_writer.complete() {
                        return Ok(());
                    }
                    let capture_snapshot = capture_snapshot_rx.recv()?;
                    filter_writer.catchup(&mut reader.at(&capture_snapshot))?;
                    if capture_snapshot.complete {
                        filter_writer.set_complete();
                    }
                }
            });
            Ok(FilterThread {
                join_handle,
                capture_snapshot_tx: Some(capture_snapshot_tx),
                filter_snapshot_rx,
            })
        } else {
            let (filter_snapshot_tx, filter_snapshot_rx) = channel();
            let join_handle = spawn(move || {
                filter_writer.catchup(&mut reader)?;
                filter_writer.set_complete();
                filter_snapshot_tx.send(filter_writer.snapshot())?;
                Ok(())
            });
            Ok(FilterThread {
                join_handle,
                capture_snapshot_tx: None,
                filter_snapshot_rx,
            })
        }
    }

    pub fn stop_filtering(&mut self) -> Result<(), Error> {
        self.filter = None;
        self.filter_snapshot = None;
        Ok(())
    }
}

pub struct FilterThread {
    join_handle: JoinHandle<Result<(), Error>>,
    capture_snapshot_tx: Option<Sender<CaptureSnapshot>>,
    pub filter_snapshot_rx: Receiver<FilterSnapshot>,
}

impl FilterThread {
    pub fn send_capture_snapshot(&mut self, snapshot: CaptureSnapshot)
        -> Result<(), Error>
    {
        self.capture_snapshot_tx
            .as_mut()
            .context("Thread has no snapshot TX")?
            .send(snapshot)?;
        Ok(())
    }

    pub fn join(self) -> Result<(), Error> {
        handle_thread_panic(self.join_handle.join())?
    }
}

pub struct CaptureStats {
    pub devices: u64,
    pub endpoints: u64,
    pub transactions: u64,
    pub packets: u64,
}

impl CaptureStats {
    fn from<C: CaptureReaderOps>(cap: &mut C) -> CaptureStats {
        CaptureStats {
            devices: cap.device_count().saturating_sub(1),
            endpoints: cap.endpoint_count().saturating_sub(2),
            transactions: cap.transaction_count(),
            packets: cap.packet_count(),
        }
    }
}

impl std::fmt::Display for CaptureStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)
        -> Result<(), std::fmt::Error>
    {
        write!(f, "{} devices, {} endpoints, {} transactions, {} packets",
            fmt_count(self.devices),
            fmt_count(self.endpoints),
            fmt_count(self.transactions),
            fmt_count(self.packets)
        )
    }
}
