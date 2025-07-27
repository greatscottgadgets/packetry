//! Unified access to different forms of capture.

use crate::capture::{CaptureReader, CaptureReaderOps, CaptureSnapshot};
use crate::util::fmt_count;

#[derive(Clone)]
pub enum CaptureState {
    Ongoing(CaptureSnapshot),
    Complete
}

#[derive(Clone)]
pub struct Capture {
    pub reader: CaptureReader,
    pub state: CaptureState,
}

impl Capture {
    pub fn set_snapshot(&mut self, snapshot: CaptureSnapshot) {
        self.state = CaptureState::Ongoing(snapshot);
    }

    pub fn set_completed(&mut self) {
        self.state = CaptureState::Complete;
    }

    pub fn stats(&mut self) -> CaptureStats {
        if let CaptureState::Ongoing(snapshot) = &mut self.state {
            CaptureStats::from(&mut self.reader.at(snapshot))
        } else {
            CaptureStats::from(&mut self.reader)
        }
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
