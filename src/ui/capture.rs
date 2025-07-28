//! Unified access to different forms of capture.

use crate::capture::{CaptureReader, CaptureSnapshot, CaptureStats};
use crate::filter::{FilterReader, FilterSnapshot, FilterThread, create_filter};

use anyhow::Error;

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
        let (filter_reader, filter_thread, filter_snapshot) =
            create_filter(&mut self.reader, self.snapshot.as_ref())?;
        self.filter = Some(filter_reader);
        self.filter_snapshot = Some(filter_snapshot);
        Ok(filter_thread)
    }

    pub fn stop_filtering(&mut self) -> Result<(), Error> {
        self.filter = None;
        self.filter_snapshot = None;
        Ok(())
    }
}
