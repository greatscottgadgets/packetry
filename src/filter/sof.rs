use crate::capture::prelude::*;
use crate::filter::{Decision::{self, *}, FilterOps};

use anyhow::Error;

/// A filter that removes all SOF packets.
pub struct SOFFilter;

impl FilterOps for SOFFilter {
    fn inspect_device<C: CaptureReaderOps> (
        &mut self,
        _cap: &mut C,
        id: DeviceId
    ) -> Result<Decision, Error> {
        if id == DEFAULT_DEV_ID {
            Ok(FilterChildren)
        } else {
            Ok(AcceptWithChildren)
        }
    }

    fn inspect_endpoint<C: CaptureReaderOps> (
        &mut self,
        _cap: &mut C,
        id: EndpointId
    ) -> Result<Decision, Error> {
        if id == FRAMING_EP_ID {
            Ok(RejectWithChildren)
        } else {
            Ok(AcceptWithChildren)
        }
    }
}
