use std::collections::HashMap;
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
    mpsc::{Sender, Receiver, RecvError, channel},
};
use std::thread::{JoinHandle, spawn};

use anyhow::{Error, bail};
use delegate::delegate;

use crate::capture::prelude::*;

use crate::database::{
    CounterSet,
    Snapshot,
    CompactSnapshot,
    CompactReader,
    CompactReaderOps,
    CompactWriter,
    compact_index,
};

use crate::item::{
    ItemSource, UnfilteredItemSource, CompletionStatus,
    TrafficItem, TrafficViewMode,
    DeviceItem, DeviceViewMode,
};

use crate::usb::PID;

use crate::util::{RangeExt, handle_thread_panic};
use crate::util::vec_map::VecMap;

pub mod and;
pub mod decision;
pub mod nak;
pub mod sof;

pub use decision::Decision::{self, *};

/// Trait to be implemented by filters.
pub trait FilterOps: Send {

    /// Decide whether to accept a device.
    fn inspect_device<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: DeviceId
    ) -> Result<Decision, Error> {
        Ok(AcceptWithChildren)
    }

    /// Decide whether to accept an endpoint.
    fn inspect_endpoint<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: EndpointId
    ) -> Result<Decision, Error> {
        Ok(AcceptWithChildren)
    }

    /// Decide whether to accept a top level traffic item.
    fn inspect_item<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: TrafficItemId
    ) -> Result<Option<Decision>, Error> {
        Ok(Some(AcceptWithChildren))
    }

    /// Decide whether to accept an incomplete transaction.
    fn inspect_transaction<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: TransactionId
    )  -> Result<Option<Decision>, Error> {
        Ok(Some(AcceptWithChildren))
    }

    /// Decide whether to accept a completed transaction.
    fn decide_transaction<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: TransactionId,
        _packet_range: &Range<PacketId>,
    )  -> Result<Decision, Error> {
        Ok(AcceptWithChildren)
    }

    /// Decide whether to accept a packet.
    fn decide_packet<C: CaptureReaderOps>(
        &mut self,
        _cap: &mut C,
        _id: PacketId
    ) -> Result<bool, Error> {
        Ok(true)
    }
}

pub struct FilterWriter<Filter> {
    filter: Filter,
    counters: CounterSet,
    endpoint_index: VecMap<EndpointKey, EndpointId>,
    device_decisions: VecMap<DeviceId, Decision>,
    endpoint_decisions: VecMap<EndpointId, Decision>,
    endpoint_items: VecMap<EndpointId, TrafficItemId>,
    transaction_items: HashMap<TransactionId, TrafficItemId>,
    item_endpoints: HashMap<TrafficItemId, EndpointId>,
    item_decisions: HashMap<TrafficItemId, Decision>,
    transaction_decision: Option<Decision>,
    packets: CompactWriter<u64, PacketId>,
    transactions: CompactWriter<u64, TransactionId>,
    items: CompactWriter<u64, TrafficItemId>,
    devices: CompactWriter<u64, DeviceId>,
    next_packet_id: PacketId,
    next_transaction_id: TransactionId,
    next_item_id: TrafficItemId,
    next_endpoint_id: EndpointId,
    next_device_id: DeviceId,
    complete: Arc<AtomicBool>,
    snapshot_req: Arc<AtomicBool>,
    snapshot_tx: Sender<FilterSnapshot>,
    stop_req: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct FilterReader {
    packets: CompactReader<u64, PacketId>,
    transactions: CompactReader<u64, TransactionId>,
    items: CompactReader<u64, TrafficItemId>,
    devices: CompactReader<u64, DeviceId>,
    complete: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct FilterSnapshot {
    counters: Snapshot,
    pub stats: CaptureStats,
    pub complete: bool,
}

pub struct FilterSnapshotReader<'f, 's> {
    packets: CompactSnapshot<'f, 's, u64, PacketId>,
    transactions: CompactSnapshot<'f, 's, u64, TransactionId>,
    items: CompactSnapshot<'f, 's, u64, TrafficItemId>,
    devices: CompactSnapshot<'f, 's, u64, DeviceId>,
    complete: bool,
}

pub struct CaptureFilterReader<'f, 'c, C: CaptureReaderOps> {
    capture: &'c mut C,
    packets: &'f mut CompactReader<u64, PacketId>,
    transactions: &'f mut CompactReader<u64, TransactionId>,
    items: &'f mut CompactReader<u64, TrafficItemId>,
    devices: &'f mut CompactReader<u64, DeviceId>,
    complete: &'f Arc<AtomicBool>,
}

pub struct CaptureFilterSnapshotReader<'f, 'c, 's, C: CaptureReaderOps> {
    capture: &'c mut C,
    packets: &'f mut CompactSnapshot<'f, 's, u64, PacketId>,
    transactions: &'f mut CompactSnapshot<'f, 's, u64, TransactionId>,
    items: &'f mut CompactSnapshot<'f, 's, u64, TrafficItemId>,
    devices: &'f mut CompactSnapshot<'f, 's, u64, DeviceId>,
    complete: bool,
}

pub struct FilterThread {
    join_handle: JoinHandle<Result<(), Error>>,
    capture_snapshot_tx: Option<Sender<CaptureSnapshot>>,
    pub filter_snapshot_rx: Receiver<FilterSnapshot>,
    pub filter_snapshot_req: Arc<AtomicBool>,
    stop_req: Arc<AtomicBool>,
}

pub fn create_filter<Filter: FilterOps + 'static>(
    filter: Filter,
    capture: &mut CaptureReader,
    snapshot: Option<&CaptureSnapshot>,
)
    -> Result<(FilterReader, FilterThread, FilterSnapshot), Error>
{
    let mut counters = CounterSet::new();
    let db = &mut counters;
    let (packet_writer, packet_reader) = compact_index(db)?;
    let (transaction_writer, transaction_reader) = compact_index(db)?;
    let (item_writer, item_reader) = compact_index(db)?;
    let (device_writer, device_reader) = compact_index(db)?;
    let complete = Arc::new(AtomicBool::new(false));
    let filter_snapshot_req = Arc::new(AtomicBool::new(false));
    let (filter_snapshot_tx, filter_snapshot_rx) = channel();
    let stop_req = Arc::new(AtomicBool::new(false));
    let mut filter_writer = FilterWriter {
        filter,
        counters,
        packets: packet_writer,
        transactions: transaction_writer,
        items: item_writer,
        devices: device_writer,
        device_decisions: VecMap::new(),
        endpoint_index: VecMap::new(),
        endpoint_decisions: VecMap::new(),
        endpoint_items: VecMap::new(),
        transaction_items: HashMap::new(),
        item_endpoints: HashMap::new(),
        item_decisions: HashMap::new(),
        transaction_decision: None,
        next_packet_id: PacketId::from(0),
        next_transaction_id: TransactionId::from(0),
        next_item_id: TrafficItemId::from(0),
        next_endpoint_id: EndpointId::from(0),
        next_device_id: DeviceId::from(0),
        complete: complete.clone(),
        snapshot_req: filter_snapshot_req.clone(),
        snapshot_tx: filter_snapshot_tx,
        stop_req: stop_req.clone(),
    };
    let filter_reader = FilterReader {
        packets: packet_reader,
        transactions: transaction_reader,
        items: item_reader,
        devices: device_reader,
        complete,
    };
    let filter_snapshot = filter_writer.snapshot();
    let mut capture = capture.clone();
    let (join_handle, capture_snapshot_tx) = if let Some(snapshot) = snapshot {
        let (snapshot_tx, snapshot_rx) = channel();
        let snapshot = snapshot.clone();
        let thread = spawn(
            move || {
                filter_writer.catchup(&mut capture.at(&snapshot))?;
                loop {
                    if filter_writer.complete() {
                        break
                    }
                    match snapshot_rx.recv() {
                        Ok(snapshot) => {
                            if filter_writer.stop_req.load(Relaxed) {
                                break
                            }
                            filter_writer.catchup(&mut capture.at(&snapshot))?;
                        },
                        Err(RecvError) => break
                    }
                }
                Ok(())
            }
        );
        (thread, Some(snapshot_tx))
    } else {
        let thread = spawn(
            move || {
                filter_writer.catchup(&mut capture)?;
                Ok(())
            }
        );
        (thread, None)
    };
    let filter_thread = FilterThread {
        join_handle,
        capture_snapshot_tx,
        filter_snapshot_req,
        filter_snapshot_rx,
        stop_req,
    };
    Ok((filter_reader, filter_thread, filter_snapshot))
}

impl FilterThread {
    pub fn request_filter_snapshot(&self) {
        self.filter_snapshot_req.store(true, Relaxed);
    }

    pub fn receive_filter_snapshot(&mut self) -> Option<FilterSnapshot> {
        self.filter_snapshot_rx.try_recv().ok()
    }

    pub fn send_capture_snapshot(&mut self, snapshot: CaptureSnapshot)
        -> Result<(), Error>
    {
        if let Some(snapshot_tx) = &self.capture_snapshot_tx {
            snapshot_tx.send(snapshot)?;
        }
        Ok(())
    }

    pub fn join(mut self) -> Result<(), Error> {
        self.stop_req.store(true, Relaxed);
        self.capture_snapshot_tx.take();
        handle_thread_panic(self.join_handle.join())??;
        Ok(())
    }
}

impl FilterReader {
    pub fn apply<'f, 'c, C: CaptureReaderOps>(
        &'f mut self,
        capture: &'c mut C,
    ) -> CaptureFilterReader<'f, 'c, C> {
        CaptureFilterReader {
            capture,
            packets: &mut self.packets,
            transactions: &mut self.transactions,
            items: &mut self.items,
            devices: &mut self.devices,
            complete: &self.complete,
        }
    }

    pub fn at<'f, 's>(
        &'f mut self,
        snapshot: &'s FilterSnapshot
    ) -> FilterSnapshotReader<'f, 's> {
        FilterSnapshotReader {
            packets: self.packets.at(&snapshot.counters),
            transactions: self.transactions.at(&snapshot.counters),
            items: self.items.at(&snapshot.counters),
            devices: self.devices.at(&snapshot.counters),
            complete: snapshot.complete,
        }
    }
}

impl<'f, 's> FilterSnapshotReader<'f, 's> {
    pub fn apply<'c, C: CaptureReaderOps>(
        &'f mut self,
        capture: &'c mut C,
    ) -> CaptureFilterSnapshotReader<'f, 'c, 's, C> {
        CaptureFilterSnapshotReader {
            capture,
            packets: &mut self.packets,
            transactions: &mut self.transactions,
            items: &mut self.items,
            devices: &mut self.devices,
            complete: self.complete,
        }
    }
}

impl<Filter: FilterOps> EndpointLookup for FilterWriter<Filter> {
    fn endpoint_lookup(&self, key: EndpointKey) -> Option<EndpointId> {
        self.endpoint_index.get(key).copied()
    }
}

impl<Filter: FilterOps> FilterWriter<Filter> {
    pub fn snapshot(&mut self) -> FilterSnapshot {
        FilterSnapshot {
            counters: self.counters.snapshot(),
            complete: self.complete.load(Relaxed),
            stats: CaptureStats {
                packets: self.next_packet_id.value,
                transactions: self.next_transaction_id.value,
                items: self.next_item_id.value,
                devices: self.next_device_id.value,
                endpoints: self.next_endpoint_id.value,
            }
        }
    }

    pub fn complete(&self) -> bool {
        self.complete.load(Relaxed)
    }

    pub fn catchup<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
    ) -> Result<(), Error> {

        let devices_end = DeviceId::from(cap.device_count());
        let endpoints_end = EndpointId::from(cap.endpoint_count());
        let items_end = TrafficItemId::from(cap.item_count());

        // Process new devices.
        for device_id in (self.next_device_id .. devices_end).iter() {
            let decision = self.filter.inspect_device(cap, device_id)?;

            if decision.accepts_parent() {
                // Include this device in the filter output.
                self.devices.push(device_id)?;
            };

            // This device is now processed.
            self.device_decisions.push(decision);
            self.next_device_id = device_id + 1;

            if self.checkpoint()? {
                return Ok(())
            }
        }

        // Process new endpoints.
        for endpoint_id in (self.next_endpoint_id .. endpoints_end).iter() {

            // Identify the associated device.
            let endpoint = cap.endpoint(endpoint_id)?;
            let device_id = endpoint.device_id();

            // Add this endpoint to our lookup table.
            // TODO: change this at the right time when the address is reused.
            self.endpoint_index.set(endpoint.key(), endpoint_id);

            // Make a decision about this endpoint.
            let decision = match self.device_decisions[device_id] {
                FilterChildren =>
                    self.filter.inspect_endpoint(cap, endpoint_id)?,
                other_device_decision => other_device_decision,
            };

            // This endpoint is now processed.
            self.endpoint_decisions.push(decision);
            self.next_endpoint_id = endpoint_id + 1;

            if self.checkpoint()? {
                return Ok(())
            }
        }

        // Process traffic items.
        for item_id in (self.next_item_id .. items_end).iter() {

            // Identify the associated endpoint.
            let group_id = cap.item_group(item_id)?;
            let entry = cap.group_entry(group_id)?;
            let endpoint_id = entry.endpoint_id();

            // Link the endpoint with this item.
            self.item_endpoints.insert(item_id, endpoint_id);

            // Identify the transaction that starts this item.
            let ep_group_id = entry.group_id();
            let ep_traf = cap.endpoint_traffic(endpoint_id)?;
            let ep_transaction_id = ep_traf.group_start(ep_group_id)?;
            let transaction_id = ep_traf.transaction_id(ep_transaction_id)?;

            // Link this item to that transaction.
            self.transaction_items.insert(transaction_id, item_id);

            // Make a decision about this item.
            let item_decision = match self.endpoint_decisions[endpoint_id] {
                FilterChildren if entry.is_start() =>
                    self.filter.inspect_item(cap, item_id)?,
                FilterChildren =>
                    self.item_decisions.get(&item_id).copied(),
                other_endpoint_decision => Some(other_endpoint_decision)
            };

            // Apply the decision.
            if let Some(decision) = item_decision {

                // Optionally include this item in the filter output.
                if decision.accepts_parent() {
                    self.items.push(item_id)?;
                };

                // Store the decision.
                self.item_decisions.insert(item_id, decision);
            }

            // Advance to the next item.
            self.next_item_id = item_id + 1;

            if self.checkpoint()? {
                return Ok(())
            }
        }

        // Process new packets and transactions.
        loop {
            // Identify the range of packets in this transaction, and which
            // are new since the last filter catchup.
            let transaction_id = self.next_transaction_id;
            let packet_range = cap.transaction_packet_range(transaction_id)?;
            let new_packets = self.next_packet_id..packet_range.end;

            // Do we have all the packets of this transaction now?
            let ended = packet_range.end.value < cap.packet_count();

            // Try to make a decision about this transaction.
            let decision = match &self.transaction_decision {
                Some(decision) => Some(*decision),
                None => {
                    // Find the item associated with this transaction.
                    let item_id = match
                        self.transaction_items.get(&transaction_id)
                    {
                        // This transaction starts the next item.
                        Some(item_id) => {
                            // Get the endpoint ID associated with this item.
                            let endpoint_id = self.item_endpoints[item_id];
                            // This is now the current item on that endpoint.
                            self.endpoint_items.set(endpoint_id, *item_id);
                            *item_id
                        },
                        // This transaction continues an existing item.
                        None => {
                            // We're going to need to identify its endpoint.
                            let packet_id = packet_range.start;
                            let packet = cap.packet(packet_id)?;
                            let endpoint_id = match
                                packet.first().map(PID::from)
                            {
                                Some(pid) => self
                                    .packet_endpoint(pid, &packet)
                                    .or_else(|key|
                                        bail!("No endpoint for: {key:?}"))?,
                                None => INVALID_EP_ID,
                            };
                            self.endpoint_items[endpoint_id]
                        }
                    };

                    // Apply item decision to transaction.
                    let decision = match self.item_decisions[&item_id] {
                        FilterChildren if ended => Some(
                            self.filter.decide_transaction(
                                cap, transaction_id, &packet_range)?
                            ),
                        FilterChildren =>
                            self.filter.inspect_transaction(
                                cap, transaction_id)?,
                        other => Some(other)
                    };

                    // Include the transaction now if decided.
                    if let Some(decision) = decision {
                        if decision.accepts_parent() {
                            self.transactions.push(transaction_id)?;
                        }
                    }

                    self.transaction_decision = decision;

                    decision
                }
            };

            // Apply decision about this transaction.
            match decision {
                Some(RejectWithChildren) => {
                    // Skip over these packets.
                },
                Some(AcceptWithChildren) => {
                    // Accept these packets.
                    for packet_id in new_packets.iter() {
                        self.packets.push(packet_id)?;
                    }
                },
                Some(FilterChildren) => {
                    // Filter these packets.
                    for packet_id in new_packets.iter() {
                        if self.filter.decide_packet(cap, packet_id)? {
                            self.packets.push(packet_id)?;
                        }
                    }
                },
                None if ended => {
                    // Transaction decision was deferred and must now be made.
                    match self.filter.decide_transaction(
                        cap, transaction_id, &packet_range)?
                    {
                        RejectWithChildren => {}
                        AcceptWithChildren => {
                            // Accept this transaction and its packets.
                            self.transactions.push(transaction_id)?;
                            for packet_id in packet_range.iter() {
                                self.packets.push(packet_id)?;
                            }
                        },
                        FilterChildren => {
                            // Accept this transaction and filter its packets.
                            self.transactions.push(transaction_id)?;
                            for packet_id in packet_range.iter() {
                                if self.filter.decide_packet(cap, packet_id)? {
                                    self.packets.push(packet_id)?;
                                }
                            }
                        }
                    }
                },
                None => {
                    // Decision was deferred. Do nothing for now.
                }
            };
            // These packets are now processed.
            self.next_packet_id = packet_range.end;
            if ended {
                // This transaction is now processed, move onto the next.
                self.next_transaction_id += 1;
                self.transaction_decision = None
            } else {
                // This transaction is still ongoing.
                break
            }

            if self.checkpoint()? {
                return Ok(())
            }
        }

        if cap.complete() {
            self.complete.store(true, Relaxed);
            self.send_snapshot()?;
        }

        Ok(())
    }

    fn checkpoint(&mut self) -> Result<bool, Error> {
        if self.snapshot_req.swap(false, Relaxed) {
            self.send_snapshot()?;
        }
        let stop_request = self.stop_req.load(Relaxed);
        Ok(stop_request)
    }

    fn send_snapshot(&mut self) -> Result<(), Error> {
        let snapshot = self.snapshot();
        self.snapshot_tx.send(snapshot)?;
        Ok(())
    }
}

trait FilterReaderOps {
    type Capture: CaptureReaderOps;
    fn capture(&mut self) -> &mut Self::Capture;
    fn packets(&mut self) -> &mut impl CompactReaderOps<u64, PacketId>;
    fn transactions(&mut self) -> &mut impl CompactReaderOps<u64, TransactionId>;
    fn items(&mut self) -> &mut impl CompactReaderOps<u64, TrafficItemId>;
    fn devices(&mut self) -> &mut impl CompactReaderOps<u64, DeviceId>;
    fn complete(&self) -> bool;
}

impl<C> FilterReaderOps for CaptureFilterReader<'_, '_, C>
where C: CaptureReaderOps
{
    type Capture = C;

    fn capture(&mut self) -> &mut Self::Capture {
        self.capture
    }

    fn packets(&mut self) -> &mut impl CompactReaderOps<u64, PacketId> {
        self.packets
    }

    fn transactions(&mut self) -> &mut impl CompactReaderOps<u64, TransactionId> {
        self.transactions
    }

    fn items(&mut self) -> &mut impl CompactReaderOps<u64, TrafficItemId> {
        self.items
    }

    fn devices(&mut self) -> &mut impl CompactReaderOps<u64, DeviceId> {
        self.devices
    }

    fn complete(&self) -> bool {
        self.complete.load(Relaxed)
    }
}

impl<C> FilterReaderOps for CaptureFilterSnapshotReader<'_, '_, '_, C>
where C: CaptureReaderOps
{
    type Capture = C;

    fn capture(&mut self) -> &mut Self::Capture {
        self.capture
    }

    fn packets(&mut self) -> &mut impl CompactReaderOps<u64, PacketId> {
        self.packets
    }

    fn transactions(&mut self) -> &mut impl CompactReaderOps<u64, TransactionId> {
        self.transactions
    }

    fn items(&mut self) -> &mut impl CompactReaderOps<u64, TrafficItemId> {
        self.items
    }

    fn devices(&mut self) -> &mut impl CompactReaderOps<u64, DeviceId> {
        self.devices
    }

    fn complete(&self) -> bool {
        self.complete
    }
}

impl<F, C> ItemSource<TrafficItem, TrafficViewMode> for F
where F: FilterReaderOps<Capture=C>,
      C: CaptureReaderOps + UnfilteredItemSource<TrafficItem, TrafficViewMode>
{
    fn item(
        &mut self,
        parent: Option<&TrafficItem>,
        view_mode: TrafficViewMode,
        mut index: u64
    ) -> Result<TrafficItem, Error> {
        use TrafficViewMode::*;
        if parent.is_none() {
            index = match view_mode {
                Hierarchical => self.items().get(index)?.value,
                Transactions => self.transactions().get(index)?.value,
                Packets => self.packets().get(index)?.value,
            };
        }
        self.capture().item(parent, view_mode, index)
    }

    fn item_children(
        &mut self,
        parent: Option<&TrafficItem>,
        view_mode: TrafficViewMode
    ) -> Result<(CompletionStatus, u64), Error> {
        use TrafficViewMode::*;
        if parent.is_none() {
            let count = match view_mode {
                Hierarchical => self.items().len(),
                Transactions => self.transactions().len(),
                Packets => self.packets().len(),
            };
            let status = if self.complete() {
                CompletionStatus::Complete
            } else {
                CompletionStatus::Ongoing
            };
            Ok((status, count))
        } else {
            self.capture().item_children(parent, view_mode)
        }
    }

    delegate! {
        to self.capture() {
            fn child_item(&mut self, parent: &TrafficItem, index: u64)
                -> Result<TrafficItem, Error>;

            fn item_update(&mut self, item: &TrafficItem)
                -> Result<Option<TrafficItem>, Error>;

            fn description(&mut self, item: &TrafficItem, detail: bool)
                -> Result<String, Error>;

            fn connectors(
                &mut self,
                view_mode: TrafficViewMode,
                item: &TrafficItem)
            -> Result<String, Error>;

            fn timestamp(&mut self, item: &TrafficItem)
                -> Result<Timestamp, Error>;
        }
    }
}

impl<F, C> ItemSource<DeviceItem, DeviceViewMode> for F
where F: FilterReaderOps<Capture=C>,
      C: CaptureReaderOps + UnfilteredItemSource<DeviceItem, DeviceViewMode>
{
    fn item(
        &mut self,
        parent: Option<&DeviceItem>,
        view_mode: DeviceViewMode,
        mut index: u64,
    ) -> Result<DeviceItem, Error> {
        if parent.is_none() {
            index = self.devices().get(index)?.value
        }
        self.capture().item(parent, view_mode, index)
    }

    fn item_children(
        &mut self,
        parent: Option<&DeviceItem>,
        view_mode: DeviceViewMode,
    ) -> Result<(CompletionStatus, u64), Error> {
        if parent.is_none() {
            let count = self.devices().len().saturating_sub(1);
            let status = if self.complete() {
                CompletionStatus::Complete
            } else {
                CompletionStatus::Ongoing
            };
            Ok((status, count))
        } else {
            self.capture().item_children(parent, view_mode)
        }
    }

    delegate! {
        to self.capture() {

            fn child_item(&mut self, parent: &DeviceItem, index: u64)
                -> Result<DeviceItem, Error>;

            fn item_update(&mut self, item: &DeviceItem)
                -> Result<Option<DeviceItem>, Error>;

            fn description(&mut self, item: &DeviceItem, detail: bool)
                -> Result<String, Error>;

            fn connectors(
                &mut self,
                view_mode: DeviceViewMode,
                item: &DeviceItem
            ) -> Result<String, Error>;

            fn timestamp(&mut self, item: &DeviceItem)
                -> Result<Timestamp, Error>;
        }
    }
}
