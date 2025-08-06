use std::ops::Range;

use crate::capture::prelude::*;
use crate::filter::{Decision::{self, *}, FilterOps};

use anyhow::Error;

/// A filter that ANDs two other filters.
pub struct ANDFilter<A, B> {
    pub a: A,
    pub b: B,
}

impl<A, B> FilterOps for ANDFilter<A, B>
where A: FilterOps, B: FilterOps
{
    fn inspect_device<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: DeviceId
    ) -> Result<Decision, Error> {
        Ok(match self.a.inspect_device(cap, id)? {
            RejectWithChildren => RejectWithChildren,
            AcceptWithChildren => self.b.inspect_device(cap, id)?,
            FilterChildren => match self.b.inspect_device(cap, id)? {
                RejectWithChildren => RejectWithChildren,
                _ => FilterChildren,
            }
        })
    }

    fn inspect_endpoint<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: EndpointId
    ) -> Result<Decision, Error> {
        Ok(match self.a.inspect_endpoint(cap, id)? {
            RejectWithChildren => RejectWithChildren,
            AcceptWithChildren => self.b.inspect_endpoint(cap, id)?,
            FilterChildren => match self.b.inspect_endpoint(cap, id)? {
                RejectWithChildren => RejectWithChildren,
                _ => FilterChildren,
            }
        })
    }

    /// Decide whether to accept a top level traffic item.
    fn inspect_item<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: TrafficItemId
    ) -> Result<Option<Decision>, Error> {
        Ok(match self.a.inspect_item(cap, id)? {
            None => None,
            Some(RejectWithChildren) => Some(RejectWithChildren),
            Some(AcceptWithChildren) => self.b.inspect_item(cap, id)?,
            Some(FilterChildren) => match self.b.inspect_item(cap, id)? {
                None => None,
                Some(RejectWithChildren) => Some(RejectWithChildren),
                Some(_) => Some(FilterChildren),
            }
        })
    }

    fn inspect_transaction<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: TransactionId
    )  -> Result<Option<Decision>, Error> {
        Ok(match self.a.inspect_transaction(cap, id)? {
            None => None,
            Some(RejectWithChildren) =>
                Some(RejectWithChildren),
            Some(AcceptWithChildren) =>
                self.b.inspect_transaction(cap, id)?,
            Some(FilterChildren) => match
                self.b.inspect_transaction(cap, id)?
            {
                None => None,
                Some(RejectWithChildren) => Some(RejectWithChildren),
                Some(_) => Some(FilterChildren),
            }
        })
    }

    fn decide_transaction<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: TransactionId,
        packet_range: &Range<PacketId>,
    )  -> Result<Decision, Error> {
        Ok(match self.a.decide_transaction(cap, id, packet_range)? {
            RejectWithChildren =>
                RejectWithChildren,
            AcceptWithChildren =>
                self.b.decide_transaction(cap, id, packet_range)?,
            FilterChildren => match
                self.b.decide_transaction(cap, id, packet_range)?
            {
                RejectWithChildren => RejectWithChildren,
                _ => FilterChildren,
            }
        })
    }

    fn decide_packet<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        id: PacketId
    ) -> Result<bool, Error> {
        Ok(self.a.decide_packet(cap, id)? && self.b.decide_packet(cap, id)?)
    }
}
