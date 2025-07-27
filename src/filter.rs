use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use anyhow::Error;
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

pub struct FilterWriter {
    counters: CounterSet,
    packets: CompactWriter<u64, PacketId>,
    transactions: CompactWriter<u64, TransactionId>,
    items: CompactWriter<u64, TrafficItemId>,
    devices: CompactWriter<u64, DeviceId>,
    next_packet_id: PacketId,
    next_transaction_id: TransactionId,
    next_item_id: TrafficItemId,
    next_device_id: DeviceId,
    complete: Arc<AtomicBool>,
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

pub fn create_filter() -> Result<(FilterWriter, FilterReader), Error> {
    let mut counters = CounterSet::new();
    let db = &mut counters;
    let (packet_writer, packet_reader) = compact_index(db)?;
    let (transaction_writer, transaction_reader) = compact_index(db)?;
    let (item_writer, item_reader) = compact_index(db)?;
    let (device_writer, device_reader) = compact_index(db)?;
    let complete = Arc::new(AtomicBool::new(false));
    Ok((
        FilterWriter {
            counters,
            packets: packet_writer,
            transactions: transaction_writer,
            items: item_writer,
            devices: device_writer,
            next_packet_id: PacketId::from(0),
            next_transaction_id: TransactionId::from(0),
            next_item_id: TrafficItemId::from(0),
            next_device_id: DeviceId::from(0),
            complete: complete.clone(),
        },
        FilterReader {
            packets: packet_reader,
            transactions: transaction_reader,
            items: item_reader,
            devices: device_reader,
            complete,
        }
    ))
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

impl FilterWriter {
    pub fn snapshot(&mut self) -> FilterSnapshot {
        FilterSnapshot {
            counters: self.counters.snapshot(),
            complete: self.complete.load(Relaxed),
        }
    }

    pub fn complete(&self) -> bool {
        self.complete.load(Relaxed)
    }

    pub fn set_complete(&self) {
        self.complete.store(true, Relaxed);
    }

    pub fn catchup<C: CaptureReaderOps>(
        &mut self,
        cap: &mut C,
    ) -> Result<(), Error> {
        for i in self.next_packet_id.value .. cap.packet_count() {
            let packet_id = PacketId::from(i);
            if cap.packet_pid(packet_id)? != PID::SOF {
                self.packets.push(packet_id)?;
            }
            self.next_packet_id = packet_id + 1;
        }
        for i in self.next_transaction_id.value .. cap.transaction_count() {
            let transaction_id = TransactionId::from(i);
            let start_packet_id = cap.transaction_start(transaction_id)?;
            if cap.packet_pid(start_packet_id)? != PID::SOF {
                self.transactions.push(transaction_id)?;
            }
            self.next_transaction_id = transaction_id + 1;
        }
        for i in self.next_item_id.value..cap.item_count() {
            let item_id = TrafficItemId::from(i);
            let group_id = cap.item_group(item_id)?;
            let entry = cap.group_entry(group_id)?;
            if entry.endpoint_id() != FRAMING_EP_ID {
                self.items.push(item_id)?;
            }
            self.next_item_id = item_id + 1;
        }
        for i in self.next_device_id.value..cap.device_count() {
            let device_id = DeviceId::from(i);
            self.devices.push(device_id)?;
            self.next_device_id = device_id + 1;
        }
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
