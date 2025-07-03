//! Unified access to different forms of capture.

use crate::capture::{CaptureReader, CaptureSnapshot};

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
}
