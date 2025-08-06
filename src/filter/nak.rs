use std::ops::Range;

use crate::capture::prelude::*;
use crate::filter::{Decision::{self, *}, FilterOps};
use crate::usb::PID::NAK;

use anyhow::Error;

/// A filter that removes all NAKed transactions.
pub struct NAKFilter;

impl FilterOps for NAKFilter {
    fn inspect_device<C: CaptureReaderOps> (
        &mut self,
        _cap: &mut C,
        _id: DeviceId
    ) -> Result<Decision, Error> {
        Ok(FilterChildren)
    }

    fn inspect_endpoint<C: CaptureReaderOps> (
        &mut self,
        _cap: &mut C,
        id: EndpointId
    ) -> Result<Decision, Error> {
        if id == FRAMING_EP_ID {
            Ok(AcceptWithChildren)
        } else {
            Ok(FilterChildren)
        }
    }

    fn inspect_item<C: CaptureReaderOps> (
        &mut self,
        cap: &mut C,
        item_id: TrafficItemId
    ) -> Result<Option<Decision>, Error> {
        use GroupContent::*;

        // Get the transaction group associated with this item.
        let group_id = cap.item_group(item_id)?;

        Ok(match cap.group(group_id) {
            Ok(group) => Some(match group.content {
                // These groups are entirely NAKed transactions.
                Polling(_) => RejectWithChildren,
                // These groups may contain NAKed transactions.
                Request(_) | IncompleteRequest | Ambiguous(..) => FilterChildren,
                // Other groups may not contain NAKed transactions.
                _ => AcceptWithChildren
            }),
            Err(_) => {
                // Assume we don't have enough packets/transcations
                // to identify the group content yet.
                None
            }
        })
    }

    fn inspect_transaction<C: CaptureReaderOps> (
        &mut self,
        _cap: &mut C,
        _transaction_id: TransactionId,
    ) -> Result<Option<Decision>, Error> {
        // Defer decision until we have the whole transaction.
        Ok(None)
    }

    fn decide_transaction<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
        _transaction_id: TransactionId,
        packet_range: &Range<PacketId>,
    ) -> Result<Decision, Error> {
        // Fetch the PID of the last packet.
        let end_packet_id = packet_range.end - 1;
        let last_pid = cap.packet_pid(end_packet_id)?;

        // Reject if the PID of the last packet is a NAK.
        Ok(
            if last_pid == NAK {
                RejectWithChildren
            } else {
                AcceptWithChildren
            }
        )
    }
}
