//! Items displayed in the UI tree views.
//!
//! Defines how items are fetched from the database and described with text.

use std::cmp::{Ordering, min};
use std::fmt::Write;
use std::ops::Range;

use anyhow::{Context, Error, bail};

use crate::capture::{
    CaptureReaderOps,
    DeviceId,
    DeviceVersion,
    EndpointReaderOps,
    EndpointState,
    GroupContent,
    GroupId,
    Timestamp,
    TrafficItemId,
    TransactionId,
    PacketId,
    INVALID_EP_ID,
};
use crate::database::{DataReaderOps, CompactReaderOps};
use crate::usb::{self, prelude::*, validate_packet};
use crate::util::{Bytes, RangeLength, fmt_count, fmt_size, titlecase};

pub trait ItemSource<Item, ViewMode> {
    /// Fetch an item from the source by index, relative to either the root
    /// of the item tree or to a parent item.
    fn item(
        &mut self,
        parent: Option<&Item>,
        view_mode: ViewMode,
        index: u64,
    ) -> Result<Item, Error>;

    /// Count how many children this item has, and whether it is complete.
    fn item_children(
        &mut self,
        parent: Option<&Item>,
        view_mode: ViewMode,
    ) -> Result<(CompletionStatus, u64), Error>;

    /// Fetch a child item by index, relative to a parent item.
    fn child_item(&mut self, parent: &Item, index: u64) -> Result<Item, Error>;

    /// Check whether a newer version of this item is available.
    fn item_update(&mut self, item: &Item) -> Result<Option<Item>, Error>;

    /// Generate a description for this item, either one line or with detail.
    fn description(
        &mut self,
        item: &Item,
        detail: bool,
    ) -> Result<String, Error>;

    /// Generate connecting lines for this item.
    fn connectors(
        &mut self,
        view_mode: ViewMode,
        item: &Item,
    ) -> Result<String, Error>;

    /// Get the timestamp of this item.
    fn timestamp(&mut self, item: &Item) -> Result<Timestamp, Error>;

    /// Count children of a top-level item within a region.
    #[allow(unused_variables)]
    fn count_within(
        &mut self,
        item_index: u64,
        item: &Item,
        region: &Range<u64>
    ) -> Result<u64, Error> {
        unimplemented!()
    }

    /// Count children of a top-level item within a span, up to a specified child.
    #[allow(unused_variables)]
    fn count_before(
        &mut self,
        item_index: u64,
        item: &Item,
        span_index: u64,
        child: &Item
    ) -> Result<u64, Error> {
        unimplemented!()
    }

    /// Find a specific child item in a region, given a set of expanded items.
    #[allow(unused_variables)]
    fn find_child(
        &mut self,
        expanded: &mut dyn Iterator<Item=(u64, Item)>,
        region: &Range<u64>,
        index: u64
    ) -> Result<SearchResult<Item>, Error> {
        unimplemented!()
    }
}

pub enum SearchResult<Item> {
    TopLevelItem(u64, Item),
    NextLevelItem(u64, u64, u64, Item),
}

#[derive(Copy, Clone)]
pub enum CompletionStatus {
    Complete,
    Ongoing,
    InterleavedComplete(u64),
    InterleavedOngoing,
}

impl CompletionStatus {
    pub fn is_complete(&self) -> bool {
        use CompletionStatus::*;
        match self {
            Complete | InterleavedComplete(_) => true,
            Ongoing | InterleavedOngoing => false,
        }
    }

    pub fn is_interleaved(&self) -> bool {
        use CompletionStatus::*;
        match self {
            InterleavedComplete(_) | InterleavedOngoing => true,
            Complete | Ongoing => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrafficItem {
    TransactionGroup(GroupId),
    Transaction(Option<GroupId>, TransactionId),
    Packet(Option<GroupId>, Option<TransactionId>, PacketId),
}

#[derive(Clone, Debug)]
pub struct DeviceItem {
    pub device_id: DeviceId,
    pub version: DeviceVersion,
    pub content: DeviceItemContent,
    pub indent: u8,
}

#[derive(Clone, Debug)]
pub enum DeviceItemContent {
    Device(Option<DeviceDescriptor>),
    DeviceDescriptor(Option<DeviceDescriptor>),
    DeviceDescriptorField(DeviceDescriptor, DeviceField),
    Configuration(ConfigNum, ConfigDescriptor, Option<ClassId>),
    ConfigurationDescriptor(ConfigDescriptor),
    ConfigurationDescriptorField(ConfigDescriptor, ConfigField),
    Function(ConfigNum, InterfaceAssociationDescriptor),
    FunctionDescriptor(InterfaceAssociationDescriptor),
    FunctionDescriptorField(InterfaceAssociationDescriptor, IfaceAssocField),
    Interface(ConfigNum, InterfaceDescriptor),
    InterfaceDescriptor(InterfaceDescriptor),
    InterfaceDescriptorField(InterfaceDescriptor, InterfaceField),
    Endpoint(ConfigNum, InterfaceKey, InterfaceEpNum),
    EndpointDescriptor(EndpointDescriptor),
    EndpointDescriptorField(EndpointDescriptor, EndpointField),
    HidDescriptor(HidDescriptor),
    HidDescriptorField(HidDescriptor, HidField),
    HidDescriptorList(HidDescriptor),
    HidDescriptorEntry(HidDescriptor, HidField),
    OtherDescriptor(Descriptor, Option<ClassId>),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TrafficViewMode {
    Hierarchical,
    Interleaved,
    Transactions,
    Packets,
}

impl TrafficViewMode {
    pub const fn display_name(&self) -> &'static str {
        use TrafficViewMode::*;
        match self {
            Hierarchical => "Hierarchical",
            Interleaved  => "Interleaved",
            Transactions => "Transactions",
            Packets      => "Packets",
        }
    }

    #[cfg(any(test, feature="record-ui-test"))]
    pub const fn log_name(&self) -> &'static str {
        use TrafficViewMode::*;
        match self {
            Hierarchical => "traffic-hierarchical",
            Interleaved  => "traffic-interleaved",
            Transactions => "traffic-transactions",
            Packets      => "traffic-packets",
        }
    }

    #[cfg(test)]
    pub fn from_log_name(log_name: &str) -> TrafficViewMode {
        use TrafficViewMode::*;
        match log_name {
            "traffic-hierarchical" => Hierarchical,
            "traffic-interleaved"  => Interleaved,
            "traffic-transactions" => Transactions,
            "traffic-packets"      => Packets,
            _ => panic!("Unrecognised log name '{log_name}'")
        }
    }
}

pub type DeviceViewMode = ();

impl<T: CaptureReaderOps> ItemSource<TrafficItem, TrafficViewMode> for T {
    fn item(
        &mut self,
        parent: Option<&TrafficItem>,
        view_mode: TrafficViewMode,
        index: u64,
    ) -> Result<TrafficItem, Error> {
        use TrafficItem::*;
        use TrafficViewMode::*;
        match parent {
            None => Ok(match view_mode {
                Hierarchical | Interleaved => {
                    let item_id = TrafficItemId::from(index);
                    let group_id = self.item_index().get(item_id)?;
                    TransactionGroup(group_id)
                },
                Transactions =>
                    Transaction(None, TransactionId::from(index)),
                Packets =>
                    Packet(None, None, PacketId::from(index)),
            }),
            Some(item) => self.child_item(item, index)
        }
    }

    fn item_update(
        &mut self,
        _item: &TrafficItem,
    ) -> Result<Option<TrafficItem>, Error> {
        Ok(None)
    }

    fn child_item(
        &mut self,
        parent: &TrafficItem,
        index: u64,
    ) -> Result<TrafficItem, Error> {
        use TrafficItem::*;
        Ok(match parent {
            TransactionGroup(group_id) =>
                Transaction(Some(*group_id), {
                    let entry = self.group_index().get(*group_id)?;
                    let endpoint_id = entry.endpoint_id();
                    let ep_group_id = entry.group_id();
                    let ep_traf = self.endpoint_traffic(endpoint_id)?;
                    let offset = ep_traf.group_index().get(ep_group_id)?;
                    ep_traf.transaction_ids().get(offset + index)?
                }),
            Transaction(group_id_opt, transaction_id) =>
                Packet(*group_id_opt, Some(*transaction_id), {
                    self.transaction_index().get(*transaction_id)? + index}),
            Packet(..) => bail!("Packets have no child items")
        })
    }

    fn item_children(
        &mut self,
        parent: Option<&TrafficItem>,
        view_mode: TrafficViewMode,
    ) -> Result<(CompletionStatus, u64), Error> {
        use TrafficItem::*;
        use TrafficViewMode::*;
        use CompletionStatus::*;
        Ok(match parent {
            None => {
                let completion = if self.complete() {
                    Complete
                } else {
                    Ongoing
                };
                (completion, match view_mode {
                    Hierarchical | Interleaved => self.item_index().len(),
                    Transactions => self.transaction_index().len(),
                    Packets => self.packet_index().len(),
                })
            },
            Some(TransactionGroup(group_id)) => {
                let entry = self.group_index().get(*group_id)?;
                if !entry.is_start() {
                    return Ok((Complete, 0));
                }
                let transaction_count = self.group_range(&entry)?.len();
                let ep_traf = self.endpoint_traffic(entry.endpoint_id())?;
                let ep_group_id = entry.group_id();
                let ongoing = ep_group_id.value >= ep_traf.end_index().len();
                let status = match (view_mode, ongoing) {
                    (Hierarchical, true) => Ongoing,
                    (Hierarchical, false) => Complete,
                    (Interleaved, true) => InterleavedOngoing,
                    (Interleaved, false) => {
                        let end = ep_traf.end_index().get(ep_group_id)?;
                        InterleavedComplete(end.value)
                    }
                    (Transactions | Packets, _) => unreachable!(),
                };
                (status, transaction_count)
            },
            Some(Transaction(_, transaction_id)) => {
                let total_packets = self.packet_index().len();
                let packet_count = self
                    .transaction_index()
                    .target_range(*transaction_id, total_packets)?
                    .len();
                if transaction_id.value < self.transaction_index().len() - 1 {
                    (Complete, packet_count)
                } else {
                    (Ongoing, packet_count)
                }
            },
            Some(Packet(..)) => (Complete, 0),
        })
    }

    fn description(
        &mut self,
        item: &TrafficItem,
        detail: bool,
    ) -> Result<String, Error> {
        use PID::*;
        use TrafficItem::*;
        use usb::StartComplete::*;
        let mut s = String::new();
        Ok(match item {
            Packet(.., packet_id) => {
                let packet = self.packet(*packet_id)?;
                let len = packet.len();
                let too_long = len > 1027;
                if detail {
                    writeln!(s, "Packet #{} with {len} bytes",
                        packet_id.value + 1)?;
                    writeln!(s, "Timestamp: {} ns from capture start",
                        fmt_count(self.packet_time(*packet_id)?))?;
                }
                match validate_packet(&packet) {
                    Err(None) => {
                        write!(s, "Malformed 0-byte packet")?;
                    },
                    Err(Some(pid)) => {
                        write!(s, "Malformed packet")?;
                        match pid {
                            RSVD if too_long => write!(s,
                                " (reserved PID, and too long)"),
                            Malformed if too_long => write!(s,
                                " (invalid PID, and too long)"),
                            RSVD => write!(s,
                                " (reserved PID)"),
                            Malformed => write!(s,
                                " (invalid PID)"),
                            pid if too_long => write!(s,
                                " (possibly {pid}, but too long)"),
                            pid => write!(s,
                                " (possibly {pid}, but {})",
                                match pid {
                                    SOF|SETUP|IN|OUT|PING => {
                                        if len != 3 {
                                            "wrong length"
                                        } else {
                                            "bad CRC"
                                        }
                                    },
                                    SPLIT => {
                                        if len != 4 {
                                            "wrong length"
                                        } else {
                                            "bad CRC"
                                        }
                                    },
                                    DATA0|DATA1|DATA2|MDATA => {
                                        if len < 3 {
                                            "too short"
                                        } else {
                                            "bad CRC"
                                        }
                                    },
                                    ACK|NAK|NYET|STALL|ERR => "too long",
                                    RSVD|Malformed => unreachable!(),
                                }
                            ),
                        }?;
                        if len == 1 {
                            write!(s, " of 1 byte")
                        } else {
                            write!(s, " of {len} bytes")
                        }?;
                        if detail {
                            write!(s, "\nHex bytes: {}", Bytes::first(1024, &packet))
                        } else {
                            write!(s, ": {}", Bytes::first(100, &packet))
                        }?;
                    },
                    Ok(pid) => {
                        write!(s, "{pid} packet")?;
                        let fields = PacketFields::from_packet(&packet);
                        match &fields {
                            PacketFields::SOF(sof) => write!(s,
                                " with frame number {}, CRC {:02X}",
                                sof.frame_number(),
                                sof.crc()),
                            PacketFields::Token(token) => write!(s,
                                " on {}.{}, CRC {:02X}",
                                token.device_address(),
                                token.endpoint_number(),
                                token.crc()),
                            PacketFields::Data(data) if len <= 3 => write!(s,
                                " with CRC {:04X} and no data",
                                data.crc),
                            PacketFields::Data(data) => write!(s,
                                " with CRC {:04X} and {} data bytes",
                                data.crc,
                                len - 3),
                            PacketFields::Split(split) => write!(s,
                                " {} {} speed {} transaction on hub {} port {}",
                                match split.sc() {
                                    Start => "starting",
                                    Complete => "completing",
                                },
                                format!("{:?}", split.speed())
                                    .to_lowercase(),
                                format!("{:?}", split.endpoint_type())
                                    .to_lowercase(),
                                split.hub_address(),
                                split.port()),
                            PacketFields::None => Ok(()),
                        }?;
                        if matches!(fields, PacketFields::Data(_)) && len > 3 {
                            let data = &packet[1 .. len - 2];
                            if detail {
                                write!(s, concat!(
                                    "\nHex bytes: [{:02X}, <payload>, {:02X}, {:02X}]",
                                    "\nPayload: {}"),
                                    packet[0], packet[len - 2], packet[len - 1],
                                    Bytes::first(1024, data))
                            } else {
                                write!(s, ": {}", Bytes::first(100, data))
                            }?;
                        } else if detail {
                            write!(s, "\nHex bytes: {packet:02X?}")?;
                        }
                    }
                }
                s
            },
            Transaction(group_id_opt, transaction_id) => {
                let num_packets = self.packet_index().len();
                let packet_id_range = self
                    .transaction_index()
                    .target_range(*transaction_id, num_packets)?;
                let start_packet_id = packet_id_range.start;
                let start_packet = self.packet(start_packet_id)?;
                let packet_count = packet_id_range.len();
                if detail {
                    writeln!(s, "Transaction #{} with {} {}",
                        transaction_id.value + 1,
                        packet_count,
                        if packet_count == 1 {"packet"} else {"packets"})?;
                    writeln!(s, "Timestamp: {} ns from capture start",
                        fmt_count(self.packet_time(start_packet_id)?))?;
                    write!(s, "Packets: #{}", packet_id_range.start + 1)?;
                    if packet_count > 1 {
                        write!(s, " to #{}", packet_id_range.end)?;
                    }
                    writeln!(s)?;
                }
                if let Ok(pid) = validate_packet(&start_packet) {
                    if pid == SPLIT && start_packet_id.value + 1 == num_packets {
                        // We can't know the endpoint yet.
                        let split = SplitFields::from_packet(&start_packet);
                        return Ok(format!(
                            "{} {} speed {} transaction on hub {} port {}",
                            match split.sc() {
                                Start => "Starting",
                                Complete => "Completing",
                            },
                            format!("{:?}", split.speed()).to_lowercase(),
                            format!("{:?}", split.endpoint_type()).to_lowercase(),
                            split.hub_address(),
                            split.port()))
                    }
                    let endpoint_id = match group_id_opt {
                        Some(group_id) => {
                            let entry = self.group_index().get(*group_id)?;
                            entry.endpoint_id()
                        },
                        None => match self.shared().packet_endpoint(
                            pid, &start_packet)
                        {
                            Ok(endpoint_id) => endpoint_id,
                            Err(_) => INVALID_EP_ID
                        }
                    };
                    let endpoint = self.endpoints().get(endpoint_id)?;
                    let transaction = self.transaction(*transaction_id)?;
                    s += &transaction.description(self, &endpoint, detail)?
                } else {
                    let packet_count = packet_id_range.len();
                    write!(s,
                        "{} malformed {}",
                        packet_count,
                        if packet_count == 1 {"packet"} else {"packets"})?;
                }
                s
            },
            TransactionGroup(group_id) => {
                use GroupContent::*;
                let group = self.group(*group_id)?;
                if detail && group.is_start {
                    let ep_traf =
                        self.endpoint_traffic(group.endpoint_id)?;
                    let start_ep_transaction_id = group.range.start;
                    let start_transaction_id =
                        ep_traf.transaction_ids().get(start_ep_transaction_id)?;
                    let start_packet_id =
                        self.transaction_index().get(start_transaction_id)?;
                    if group.count == 1 {
                        writeln!(s, "Transaction group with 1 transaction")?;
                    } else {
                        writeln!(s, "Transaction group with {} transactions",
                            group.count)?;
                    }
                    writeln!(s, "Timestamp: {} ns from start of capture",
                        fmt_count(self.packet_time(start_packet_id)?))?;
                    writeln!(s, "First transaction #{}, first packet #{}",
                        start_transaction_id.value + 1,
                        start_packet_id.value + 1)?;
                }
                let endpoint = &group.endpoint;
                let endpoint_type = group.endpoint_type;
                let addr = group.endpoint.device_address();
                let count = group.count;
                match (group.content, group.is_start) {
                    (Invalid, true) => write!(s,
                        "{count} invalid groups"),
                    (Invalid, false) => write!(s,
                        "End of invalid groups"),
                    (Framing, true) => write!(s,
                        "{count} SOF groups"),
                    (Framing, false) => write!(s,
                        "End of SOF groups"),
                    (Request(transfer), true) if detail => write!(s,
                        "Control transfer on device {addr}\n{}",
                        transfer.summary(true)),
                    (Request(transfer), true) => write!(s,
                        "{}", transfer.summary(false)),
                    (IncompleteRequest, true) => write!(s,
                        "Incomplete control transfer on device {addr}"),
                    (Request(_) | IncompleteRequest, false) => write!(s,
                        "End of control transfer on device {addr}"),
                    (Data(data_range), true) => {
                        let ep_traf =
                            self.endpoint_traffic(group.endpoint_id)?;
                        let length =
                            ep_traf.transfer_data_length(&data_range)?;
                        let length_string = fmt_size(length);
                        let max = if detail { 1024 } else { 100 };
                        let display_length = min(length, max) as usize;
                        let transfer_bytes = self.transfer_bytes(
                            group.endpoint_id, &data_range, display_length)?;
                        let display_bytes = Bytes {
                            partial: length > display_length as u64,
                            bytes: &transfer_bytes,
                        };
                        let ep_type_string = titlecase(
                            &format!("{endpoint_type}"));
                        write!(s, "{ep_type_string} transfer ")?;
                        write!(s, "of {length_string} ")?;
                        write!(s, "on endpoint {endpoint}")?;
                        if detail {
                            write!(s, "\nPayload: {display_bytes}")
                        } else {
                            write!(s, ": {display_bytes}")
                        }
                    },
                    (Data(_), false) => write!(s,
                        "End of {endpoint_type} transfer on endpoint {endpoint}"),
                    (Polling(count), true) => write!(s,
                        "Polling {count} times for {endpoint_type} transfer on endpoint {endpoint}"),
                    (Polling(_count), false) => write!(s,
                        "End polling for {endpoint_type} transfer on endpoint {endpoint}"),
                    (Ambiguous(_data_range, count), true) => {
                        write!(s, "{count} ambiguous transactions on endpoint {endpoint}")?;
                        if detail {
                            write!(s, "\nThe result of these transactions is ambiguous because the endpoint type is not known.")?;
                            write!(s, "\nTry starting the capture before this device is enumerated, so that its descriptors are captured.")?;
                        }
                        Ok(())
                    },
                    (Ambiguous(..), false) => write!(s,
                        "End of ambiguous transactions."),
                }?;
                s
            }
        })
    }

    fn connectors(
        &mut self,
        view_mode: TrafficViewMode,
        item: &TrafficItem,
    ) -> Result<String, Error> {
        use EndpointState::*;
        use TrafficItem::*;
        use TrafficViewMode::*;
        if view_mode == Packets {
            return Ok(String::from(""));
        }
        let last_packet = match item {
            Packet(_, Some(transaction_id), packet_id) => {
                let num_packets = self.packet_index().len();
                let range = self
                    .transaction_index()
                    .target_range(*transaction_id, num_packets)?;
                *packet_id == range.end - 1
            }, _ => false
        };
        if view_mode == Transactions {
            return Ok(String::from(match (item, last_packet) {
                (TransactionGroup(_), _) => unreachable!(),
                (Transaction(..), _)     => "○",
                (Packet(..), false)      => "├──",
                (Packet(..), true )      => "└──",
            }));
        }
        let endpoint_count = self.endpoints().len() as usize;
        let max_string_length = endpoint_count + "    └──".len();
        let mut connectors = String::with_capacity(max_string_length);
        let group_id = match item {
            TransactionGroup(i) |
            Transaction(Some(i), _) |
            Packet(Some(i), ..) => *i,
            _ => unreachable!()
        };
        let entry = self.group_index().get(group_id)?;
        let endpoint_id = entry.endpoint_id();
        let endpoint_state = self.endpoint_state(group_id)?;
        let extended = self.group_extended(endpoint_id, group_id)?;
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let last_transaction = match item {
            Transaction(_, transaction_id) |
            Packet(_, Some(transaction_id), _) => {
                let ep_transactions = ep_traf.transaction_ids().len();
                let range = ep_traf
                    .group_index()
                    .target_range(entry.group_id(), ep_transactions)?;
                let last_transaction_id =
                    ep_traf.transaction_ids().get(range.end - 1)?;
                *transaction_id == last_transaction_id
            }, _ => false
        };
        let last = last_transaction && !extended;
        let mut thru = false;
        for (i, &state) in endpoint_state.iter().enumerate() {
            let state = EndpointState::from(state);
            let active = state != Idle;
            let on_endpoint = i == endpoint_id.value as usize;
            thru |= match (item, state, on_endpoint) {
                (TransactionGroup(..), Starting | Ending, _) => true,
                (Transaction(..) | Packet(..), _, true) => on_endpoint,
                _ => false,
            };
            connectors.push(match item {
                TransactionGroup(..) => {
                    match (state, thru) {
                        (Idle,     false) => ' ',
                        (Idle,     true ) => '─',
                        (Starting, _    ) => '○',
                        (Ongoing,  false) => '│',
                        (Ongoing,  true ) => '┼',
                        (Ending,   _    ) => '└',
                    }
                },
                Transaction(..) => {
                    match (on_endpoint, active, thru, last) {
                        (false, false, false, _    ) => ' ',
                        (false, false, true,  _    ) => '─',
                        (false, true,  false, _    ) => '│',
                        (false, true,  true,  _    ) => '┼',
                        (true,  _,     _,     false) => '├',
                        (true,  _,     _,     true ) => '└',
                    }
                },
                Packet(..) => {
                    match (on_endpoint, active, last) {
                        (false, false, _    ) => ' ',
                        (false, true,  _    ) => '│',
                        (true,  _,     false) => '│',
                        (true,  _,     true ) => ' ',
                    }
                }
            });
        };
        let state_length = endpoint_state.len();
        for _ in state_length..endpoint_count {
            connectors.push(match item {
                TransactionGroup(..)    => '─',
                Transaction(..)         => '─',
                Packet(..)              => ' ',
            });
        }
        connectors.push_str(
            match (item, last_packet) {
                (TransactionGroup(_), _) if entry.is_start() => "─",
                (TransactionGroup(_), _)                     => "──□ ",
                (Transaction(..), _)                         => "───",
                (Packet(..), false)                          => "    ├──",
                (Packet(..), true)                           => "    └──",
            }
        );
        Ok(connectors)
    }

    fn timestamp(&mut self, item: &TrafficItem) -> Result<Timestamp, Error> {
        use TrafficItem::*;
        let packet_id = match item {
            TransactionGroup(group_id) => {
                let entry = self.group_index().get(*group_id)?;
                let ep_traf = self.endpoint_traffic(entry.endpoint_id())?;
                let ep_transaction_id =
                    ep_traf.group_index().get(entry.group_id())?;
                let transaction_id =
                    ep_traf.transaction_ids().get(ep_transaction_id)?;
                self.transaction_index().get(transaction_id)?
            },
            Transaction(.., transaction_id) =>
                self.transaction_index().get(*transaction_id)?,
            Packet(.., packet_id) => *packet_id,
        };
        self.packet_time(packet_id)
    }

    fn count_within(
        &mut self,
        item_index: u64,
        item: &TrafficItem,
        region: &Range<u64>
    ) -> Result<u64, Error> {
        // Count the transactions of this transfer item within a region.
        let transfer = self.transfer(item_index, item)?;
        let ep_traf = self.endpoint_traffic(transfer.endpoint_id)?;
        let start_item_id = TrafficItemId::from(region.start);
        let end_item_id = TrafficItemId::from(region.end);
        let start_offset = start_item_id - transfer.ep_first_item_id;
        let end_offset = end_item_id - transfer.ep_first_item_id;
        let start_count = ep_traf.progress_index().get(start_offset)?.value;
        let end_count =
            if end_offset >= ep_traf.progress_index().len() {
                ep_traf.transaction_ids().len()
            } else {
                ep_traf.progress_index().get(end_offset)?.value
            };
        Ok(end_count - start_count)
    }

    fn count_before(
        &mut self,
        item_index: u64,
        item: &TrafficItem,
        span_index: u64,
        child: &TrafficItem
    ) -> Result<u64, Error> {
        // Count the transactions of this transfer item within a span,
        // up to the specified child transaction item.
        let transfer = self.transfer(item_index, item)?;
        let ep_traf = self.endpoint_traffic(transfer.endpoint_id)?;
        let span_item_id = TrafficItemId::from(span_index);
        let span_offset = span_item_id - transfer.ep_first_item_id;
        let ep_transactions = ep_traf.transaction_ids().len();
        let transaction_range = ep_traf
            .progress_index()
            .target_range(span_offset, ep_transactions)?;
        let transaction_count = transaction_range.len();
        if let TrafficItem::Transaction(_, transaction_id) = child {
            let expected = transaction_id.value;
            for index in 0..transaction_count {
                let ep_transaction_id = transaction_range.start + index;
                let id = ep_traf.transaction_ids().get(ep_transaction_id)?;
                if id.value >= expected {
                    return Ok(index)
                }
            }
            Ok(transaction_count)
        } else {
            bail!("Child {child:?} is not a transaction")
        }
    }

    fn find_child(
        &mut self,
        expanded: &mut dyn Iterator<Item=(u64, TrafficItem)>,
        region: &Range<u64>,
        mut index: u64
    ) -> Result<SearchResult<TrafficItem>, Error> {
        use SearchResult::*;
        use TrafficItem::*;

        // Collect data on the expanded transfers.
        let mut transfers = self.transfers(expanded)?;
        assert!(!transfers.is_empty());

        // First, find the right span: the space between two contiguous items
        // in which this transaction is to be found.
        let mut total_transactions = 0;
        let mut span_index = region.start;
        for i in 0..region.len() {
            span_index = region.start + i;
            let span_item_id = TrafficItemId::from(span_index);
            // Count the transactions within this span.
            for transfer in transfers.iter_mut() {
                let ep_traf = self.endpoint_traffic(transfer.endpoint_id)?;
                // Find the transaction counts for this transfer at the
                // beginning and end of this span.
                let item_offset = span_item_id - transfer.ep_first_item_id;
                let ep_transactions = ep_traf.transaction_ids().len();
                transfer.transaction_range = ep_traf
                    .progress_index()
                    .target_range(item_offset, ep_transactions)?;
                // Add to the total count for this span.
                total_transactions += transfer.transaction_range.len();
            }
            // If the index is within this span, proceed to the next stage.
            if index < total_transactions {
                break;
            // Otherwise, advance to the end of this span.
            } else {
                index -= total_transactions;
                total_transactions = 0;
            }
            // We are now at the end of a span. If the index is now zero,
            // return the transfer item after this span.
            if index == 0 {
                let item_id = span_item_id + 1;
                let group_id = self.item_index().get(item_id)?;
                let item = TransactionGroup(group_id);
                return Ok(TopLevelItem(item_id.value, item))
            // Otherwise, skip over the transfer item.
            } else {
                index -= 1;
            }
        }

        // Check the index is within the span found by the loop above. This
        // will fail if the index was past the end of this region's rows.
        if index >= total_transactions {
            bail!("Index {index} is beyond the \
                  {total_transactions} transactions in this span");
        }

        // Now we have identified the correct span. Find the transaction with
        // the remaining index from among the active transfers.
        loop {
            // Exclude transfers with no remaining transactions.
            transfers.retain(|transfer|
                !transfer.transaction_range.is_empty());

            // If only one remains, look up directly.
            if transfers.len() == 1 {
                let transfer = &transfers[0];
                let ep_traf = self.endpoint_traffic(transfer.endpoint_id)?;
                // Get the next transaction ID for this transfer.
                let ep_transaction_id =
                    transfer.transaction_range.start + index;
                let transaction_id =
                    ep_traf.transaction_ids().get(ep_transaction_id)?;
                let parent_index = transfer.start_item_id.value;
                let child_index =
                    ep_transaction_id - transfer.first_ep_transaction_id;
                let item = Transaction(
                    Some(transfer.group_id), transaction_id);
                return Ok(NextLevelItem(
                    span_index, parent_index, child_index, item))
            }

            // Exclude transactions that cannot possibly match the index.
            for transfer in transfers.iter_mut() {
                let range = &transfer.transaction_range;
                if range.len() > index + 1 {
                    transfer.transaction_range.end =
                        range.start + index + 1;
                }
            }

            // Choose the transfer with the most transactions.
            let (longest, longest_length) = transfers
                .iter()
                .enumerate()
                .map(|(i, transfer)| (i, transfer.transaction_range.len()))
                .max_by_key(|(_, length)| *length)
                .context("No transfers remaining")?;

            // If there are no transfers with more than 1 transaction,
            // proceed to selecting from the remaining candidates.
            if longest_length < 2 {
                break
            }

            // Identify the midpoint of the longest transfer.
            let midpoint_offset = longest_length / 2;

            // Get the transaction ID at the midpoint, as a pivot.
            let ep_traf =
                self.endpoint_traffic(transfers[longest].endpoint_id)?;
            let ep_transaction_id =
                transfers[longest].transaction_range.start + midpoint_offset;
            let pivot_transaction_id =
                ep_traf.transaction_ids().get(ep_transaction_id)?;

            // Find the offset of the pivot within each transfer.
            let mut offsets = Vec::with_capacity(transfers.len());
            for transfer in transfers.iter() {
                offsets.push(
                    if std::ptr::eq(transfer, &transfers[longest]) {
                        midpoint_offset
                    } else {
                        let ep_traf =
                            self.endpoint_traffic(transfer.endpoint_id)?;
                        let position =
                            ep_traf.transaction_ids().bisect_range_left(
                                &transfer.transaction_range,
                                &pivot_transaction_id)?;
                        position - transfer.transaction_range.start
                    }
                );
            }

            // Count the total transactions before the pivot.
            let count = offsets.iter().sum::<u64>();

            use std::cmp::Ordering::*;
            match index.cmp(&count) {
                Equal => {
                    // If the index equals the count, return the pivot.
                    let parent_index =
                        transfers[longest].start_item_id.value;
                    let child_index = ep_transaction_id -
                        transfers[longest].first_ep_transaction_id;
                    let item = Transaction(
                        Some(transfers[longest].group_id),
                        pivot_transaction_id);
                    return Ok(NextLevelItem(
                        span_index, parent_index, child_index, item));
                },
                Less => {
                    // If the index is less than the count, split the ranges
                    // and discard the upper ends.
                    for (transfer, offset) in transfers.iter_mut().zip(offsets) {
                        transfer.transaction_range.end =
                            transfer.transaction_range.start + offset;
                    }
                }
                Greater => {
                    // If the index is greater than the count, split the ranges
                    // and discard the lower ends.
                    for (transfer, offset) in transfers.iter_mut().zip(offsets) {
                        transfer.transaction_range.start += offset;
                    }
                    // Reduce the index by the count of excluded transactions.
                    index -= count;
                }
            }
        }

        // There is now at most one transaction in each transfer. Retrieve each
        // and find the one with the lowest transaction ID.
        let mut results = Vec::with_capacity(transfers.len());
        for transfer in transfers.iter() {
            if !transfer.transaction_range.is_empty() {
                let ep_traf = self.endpoint_traffic(transfer.endpoint_id)?;
                let transaction_id =
                    ep_traf.transaction_ids().get(
                        transfer.transaction_range.start)?;
                results.push((transfer, transaction_id));
            }
        }
        results.sort_by_key(|(_, id)| id.value);
        let (transfer, transaction_id) = results
            .get(index as usize)
            .context("Index not found")?;
        let parent_index = transfer.start_item_id.value;
        let child_index = transfer.transaction_range.start -
            transfer.first_ep_transaction_id;
        let item = Transaction(Some(transfer.group_id), *transaction_id);
        Ok(NextLevelItem(span_index, parent_index, child_index, item))
    }
}

impl<T: CaptureReaderOps> ItemSource<DeviceItem, DeviceViewMode> for T {
    fn item(
        &mut self,
        parent: Option<&DeviceItem>,
        _view_mode: DeviceViewMode,
        index: u64,
    ) -> Result<DeviceItem, Error> {
        match parent {
            None => {
                let device_id = DeviceId::from(index + 1);
                let data = self.device_data(device_id)?;
                let descriptor = data.device_descriptor.load_full();
                Ok(DeviceItem {
                    device_id,
                    version: data.version(),
                    content: DeviceItemContent::Device(
                        descriptor.map(|arc| *arc)
                    ),
                    indent: 0,
                })
            },
            Some(item) => self.child_item(item, index)
        }
    }

    fn item_update(
        &mut self,
        item: &DeviceItem,
    ) -> Result<Option<DeviceItem>, Error> {
        use DeviceItemContent::*;
        let data = self.device_data(item.device_id)?;
        if data.version() == item.version {
            return Ok(None)
        }
        // These items may have changed because we saw a new descriptor.
        Ok(match item.content {
            Device(_) |
            DeviceDescriptorField(..) |
            ConfigurationDescriptorField(..) |
            InterfaceDescriptorField(..) => Some(
                DeviceItem {
                    device_id: item.device_id,
                    version: data.version(),
                    content: item.content.clone(),
                    indent: item.indent,
                }
            ),
            _ => None,
        })
    }

    fn child_item(
        &mut self,
        parent: &DeviceItem,
        index: u64,
    ) -> Result<DeviceItem, Error> {
        use DeviceItemContent::*;
        let data = self.device_data(parent.device_id)?;
        let content = match &parent.content {
            Device(desc_opt) => match index {
                0 => DeviceDescriptor(*desc_opt),
                n => {
                    let conf = ConfigNum(n.try_into()?);
                    let config = data.configuration(conf)?;
                    Configuration(
                        conf,
                        config.descriptor,
                        desc_opt.map(|desc| desc.device_class)
                    )
                }
            },
            DeviceDescriptor(desc_opt) => match desc_opt {
                Some(desc) =>
                    DeviceDescriptorField(*desc,
                        DeviceField(index.try_into()?)),
                None => bail!("Device descriptor fields not available")
            },
            Configuration(conf, desc, class) => {
                let config = data.configuration(*conf)?;
                let other_count = config.other_descriptors.len();
                let func_count = config.functions.len();
                match index.try_into()? {
                    0 => ConfigurationDescriptor(*desc),
                    n if n < 1 + other_count =>
                        OtherDescriptor(
                            config
                                .other_descriptor(n - 1)?
                                .clone(),
                            *class),
                    n if n < 1 + other_count + func_count =>
                        Function(*conf, config
                            .function(n - 1 - other_count)?
                            .descriptor),
                    n => Interface(*conf, config
                            .unassociated_interfaces()
                            .nth(n - 1 - other_count - func_count)
                            .context("Failed to find unassociated interface")?
                            .descriptor)
                }
            },
            ConfigurationDescriptor(desc) =>
                ConfigurationDescriptorField(*desc,
                    ConfigField(index.try_into()?)),
            Function(conf, desc) => {
                let config = data.configuration(*conf)?;
                match index.try_into()? {
                    0 => FunctionDescriptor(*desc),
                    n => match config.associated_interfaces(desc).nth(n - 1) {
                        Some(interface) =>
                            Interface(*conf, interface.descriptor),
                        None => bail!(
                            "Function has no interface with index {n}")
                    }
                }
            },
            FunctionDescriptor(desc) =>
                FunctionDescriptorField(*desc,
                    IfaceAssocField(index.try_into()?)),
            Interface(conf, if_desc) => {
                let config = data.configuration(*conf)?;
                let interface = config.interface(if_desc.key())?;
                let desc_count = interface.other_descriptors.len();
                match index.try_into()? {
                    0 => InterfaceDescriptor(*if_desc),
                    n if n < 1 + desc_count => {
                        let desc = interface.other_descriptor(n - 1)?.clone();
                        if let Descriptor::Hid(hid_desc) = desc {
                            HidDescriptor(hid_desc)
                        } else {
                            OtherDescriptor(desc,
                                Some(interface.descriptor.interface_class))
                        }
                    },
                    n => {
                        let ep_num = InterfaceEpNum(
                            (n - 1 - desc_count).try_into()?);
                        Endpoint(*conf, if_desc.key(), ep_num)
                    }
                }
            },
            Endpoint(conf, if_key, ep_num) => {
                let config = data.configuration(*conf)?;
                let interface = config.interface(*if_key)?;
                let endpoint = interface.endpoint(*ep_num)?;
                match index.try_into()? {
                    0 => EndpointDescriptor(endpoint.descriptor),
                    n => OtherDescriptor(
                        endpoint.other_descriptors
                            .get(n - 1)
                            .context("Other endpoint descriptor not found")?
                            .clone(),
                        Some(interface.descriptor.interface_class)
                    )
                }
            },
            InterfaceDescriptor(desc) =>
                InterfaceDescriptorField(*desc,
                    InterfaceField(index.try_into()?)),
            EndpointDescriptor(desc) =>
                EndpointDescriptorField(*desc,
                    EndpointField(index.try_into()?)),
            HidDescriptor(desc) => {
                const N: usize = usb::HidDescriptor::NUM_FIELDS;
                const LAST_FIELD: usize = N - 1;
                match index.try_into()? {
                    0..=LAST_FIELD =>
                        HidDescriptorField(desc.clone(),
                            HidField(index.try_into()?)),
                    N => HidDescriptorList(desc.clone()),
                    _ => bail!("HID descriptor has no child with index {index}")
                }
            },
            HidDescriptorList(desc) =>
                HidDescriptorEntry(desc.clone(),
                    HidField(index.try_into()?)),
            _ => bail!("This device item type cannot have children")
        };
        Ok(DeviceItem {
            device_id: parent.device_id,
            version: data.version(),
            content,
            indent: parent.indent + 1,
        })
    }

    fn item_children(
        &mut self,
        parent: Option<&DeviceItem>,
        _view_mode: DeviceViewMode
    ) -> Result<(CompletionStatus, u64), Error> {
        use DeviceItemContent::*;
        use CompletionStatus::*;
        let (completion, children) = match parent {
            None => {
                let completion = if self.complete() {
                    Complete
                } else {
                    Ongoing
                };
                let children = self.devices().len().saturating_sub(1) as usize;
                (completion, children)
            },
            Some(item) => {
                let data = self.device_data(item.device_id)?;
                match &item.content {
                    Device(_) => {
                        let count = data.configurations.load().len();
                        (Ongoing, if count == 0 { 1 } else { count })
                    },
                    DeviceDescriptor(_) =>
                        match data.device_descriptor.load().as_ref() {
                            Some(_) =>
                                (Ongoing, usb::DeviceDescriptor::NUM_FIELDS),
                            None => (Ongoing, 0),
                        },
                    Configuration(conf, ..) => {
                        let config = data.configuration(*conf)?;
                        (Ongoing,
                         1 + config.other_descriptors.len()
                           + config.functions.len()
                           + config.unassociated_interfaces().count())
                    }
                    ConfigurationDescriptor(_) =>
                        (Ongoing, usb::ConfigDescriptor::NUM_FIELDS),
                    Function(conf, desc) => {
                        let config = data.configuration(*conf)?;
                        let interfaces = config.associated_interfaces(desc);
                        (Complete, 1 + interfaces.count())
                    }
                    FunctionDescriptor(_) =>
                        (Complete,
                         usb::InterfaceAssociationDescriptor::NUM_FIELDS),
                    Interface(conf, desc) => {
                        let config = data.configuration(*conf)?;
                        let interface = config.interface(desc.key())?;
                        (Ongoing,
                         1 + interface.endpoints.len()
                           + interface.other_descriptors.len())
                    },
                    Endpoint(conf, key, ep_num) => {
                        let config = data.configuration(*conf)?;
                        let interface = config.interface(*key)?;
                        let endpoint = interface.endpoint(*ep_num)?;
                        (Complete, 1 + endpoint.other_descriptors.len())
                    }
                    InterfaceDescriptor(_) =>
                        (Ongoing, usb::InterfaceDescriptor::NUM_FIELDS),
                    EndpointDescriptor(_) =>
                        (Complete, usb::EndpointDescriptor::NUM_FIELDS),
                    HidDescriptor(_) =>
                        (Complete, usb::HidDescriptor::NUM_FIELDS + 1),
                    HidDescriptorList(desc) =>
                        (Complete, desc.available_descriptors.len()),
                    // Other types have no children.
                    _ => (Complete, 0),
                }
            }
        };
        Ok((completion, children as u64))
    }

    fn description(
        &mut self,
        item: &DeviceItem,
        _detail: bool
    ) -> Result<String, Error> {
        use DeviceItemContent::*;
        let data = self.device_data(item.device_id)?;
        Ok(match &item.content {
            Device(_) => {
                let device = self.devices().get(item.device_id)?;
                format!("Device {}: {}", device.address, data.description())
            },
            DeviceDescriptor(desc) => {
                match desc {
                    Some(_) => "Device descriptor",
                    None => "No device descriptor"
                }.to_string()
            },
            DeviceDescriptorField(desc, field) => {
                let strings = data.strings.load();
                desc.field_text(*field, strings.as_ref())
            },
            Configuration(conf, ..) => format!(
                "Configuration {conf}"),
            ConfigurationDescriptor(_) =>
                "Configuration descriptor".to_string(),
            ConfigurationDescriptorField(desc, field) => {
                let strings = data.strings.load();
                desc.field_text(*field, strings.as_ref())
            },
            Function(_conf, desc) => {
                format!("Function {}: {}",
                    desc.function,
                    desc.function_class.name()
                )
            },
            FunctionDescriptor(_) =>
                "Interface association descriptor".to_string(),
            FunctionDescriptorField(desc, field) => desc.field_text(*field),
            Interface(_conf, desc) => {
                let num = desc.interface_number;
                let class = desc.interface_class.name();
                match desc.alternate_setting {
                    InterfaceAlt(0) => format!(
                        "Interface {num}: {class}"),
                    InterfaceAlt(alt) => format!(
                        "Interface {num} alt {alt}: {class}"),
                }
            },
            InterfaceDescriptor(_) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(desc, field) => {
                let strings = data.strings.load();
                desc.field_text(*field, strings.as_ref())
            },
            Endpoint(conf, if_key, ep_num) => {
                let config = data.configuration(*conf)?;
                let interface = config.interface(*if_key)?;
                let endpoint = interface.endpoint(*ep_num)?;
                let desc = &endpoint.descriptor;
                let addr = desc.endpoint_address;
                let attrs = desc.attributes;
                format!("Endpoint {} {} ({})", addr.number(),
                   addr.direction(), attrs.endpoint_type())
            },
            EndpointDescriptor(_) =>
                "Endpoint descriptor".to_string(),
            EndpointDescriptorField(desc, field) => desc.field_text(*field),
            HidDescriptor(_) => "HID descriptor".to_string(),
            HidDescriptorField(desc, field) => desc.field_text(*field),
            HidDescriptorList(_) => "Available descriptors".to_string(),
            HidDescriptorEntry(desc, field) => {
                let (desc_type, length) =
                    desc.available_descriptors
                        .get(field.0 as usize)
                        .context("Not enough entries in descriptor list")?;
                format!("{}, {} bytes",
                    desc_type.description_with_class(ClassId::HID), length)
            },
            OtherDescriptor(desc, class) => desc.description(*class),
        })
    }

    fn connectors(
        &mut self,
        _view_mode: (),
        item: &DeviceItem
    ) -> Result<String, Error> {
        Ok("   ".repeat(item.indent as usize))
    }

    fn timestamp(&mut self, _item: &DeviceItem) -> Result<Timestamp, Error> {
        unreachable!()
    }
}

impl PartialOrd for TrafficItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        use TrafficItem::*;
        use Ordering::*;
        match (self, other) {
            // Groups must be ordered with each other.
            (TransactionGroup(a), TransactionGroup(b)) => Some(a.cmp(b)),
            // Transactions must be ordered with each other.
            (Transaction(_, a), Transaction(_, b)) => Some(a.cmp(b)),
            // Packets must satisfy both transaction and packet ordering.
            (Packet(_, a, i), Packet(_, b, j)) => {
                match (a.cmp(b), (i.cmp(j))) {
                    (Equal, ordering) => Some(ordering),
                    (Greater, Greater) => Some(Greater),
                    (Less, Less) => Some(Less),
                    _ => panic!("Packets have inconsistent ordering")
                }
            },
            // Groups must precede their own transactions and packets.
            (TransactionGroup(a), Transaction(Some(b), _) | Packet(Some(b), ..))
                if a == b => Some(Less),
            // ...and vice versa.
            (Transaction(Some(a), _) | Packet(Some(a), ..), TransactionGroup(b))
                if a == b => Some(Greater),
            // Transactions precede their own packets.
            (Transaction(_, a), Packet(_, Some(b), _)) => {
                match a.cmp(b) {
                    Equal => Some(Less),
                    ordering => Some(ordering),
                }
            },
            // ...and vice versa.
            (Packet(_, Some(a), _), Transaction(_, b)) => {
                match a.cmp(b) {
                    Equal => Some(Greater),
                    ordering => Some(ordering),
                }
            },
            // Otherwise, ordering cannot be determined from items alone.
            _ => None
        }
    }
}

impl PartialOrd for DeviceItem {
    fn partial_cmp(&self, _other: &Self) -> Option<Ordering> {
        None
    }
}

impl PartialEq for DeviceItem {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, BufWriter, BufRead, Write};
    use std::path::PathBuf;
    use itertools::Itertools;
    use crate::capture::{CaptureReader, create_capture};
    use crate::database::CounterSet;
    use crate::decoder::Decoder;
    use crate::file::{GenericLoader, GenericPacket, LoaderItem, PcapLoader};
    use crate::util::dump::Dump;

    fn summarize_item<Item, ViewMode>(
        cap: &mut CaptureReader,
        item: &Item,
        mode: ViewMode,
    ) -> String
        where CaptureReader: ItemSource<Item, ViewMode>,
              ViewMode: Copy
    {
        let mut summary = format!("{} {}",
            cap.connectors(mode, item).unwrap(),
            cap.description(item, false).unwrap()
        );
        let (_completion, num_children) =
            cap.item_children(Some(item), mode).unwrap();
        let child_ids = 0..num_children;
        for (n, child_summary) in child_ids
            .map(|child_id| {
                let child = cap.child_item(item, child_id).unwrap();
                summarize_item(cap, &child, mode)
            })
            .dedup_with_count()
        {
            summary += "\n";
            if n > 1 {
                summary += &format!("{} ({} times)", &child_summary, n);
            } else {
                summary += &child_summary;
            }
        }
        summary
    }

    fn write_item<Item, ViewMode>(
        cap: &mut CaptureReader,
        item: &Item,
        mode: ViewMode,
        writer: &mut dyn Write
    )
        where CaptureReader: ItemSource<Item, ViewMode>,
              ViewMode: Copy
    {
        let summary = summarize_item(cap, item, mode);
        writer.write_all(summary.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
    }

    #[test]
    fn test_captures() {
        let test_dir = PathBuf::from("./tests/");
        let mut list_path = test_dir.clone();
        list_path.push("tests.txt");
        let list_file = File::open(list_path).unwrap();
        let mode = TrafficViewMode::Hierarchical;
        for test_name in BufReader::new(list_file).lines() {
            let test_path = test_dir.join(test_name.unwrap());
            let cap_path = test_path.join("capture.pcap");
            let traf_ref_path = test_path.join("reference.txt");
            let traf_out_path = test_path.join("output.txt");
            let dev_ref_path = test_path.join("devices-reference.txt");
            let dev_out_path = test_path.join("devices-output.txt");
            let dump_path = test_path.join("dump");
            {
                let file = File::open(cap_path).unwrap();
                let mut loader = PcapLoader::new(file).unwrap();
                let (writer, mut reader) = create_capture().unwrap();
                let mut decoder = Decoder::new(writer).unwrap();
                loop {
                    use LoaderItem::*;
                    match loader.next() {
                        Packet(packet) => decoder
                            .handle_raw_packet(
                                packet.bytes(), packet.timestamp_ns())
                            .unwrap(),
                        Metadata(meta) => decoder.handle_metadata(meta),
                        LoadError(e) => panic!("{e}"),
                        Ignore => continue,
                        End => break,
                    }
                }
                decoder.finish().unwrap();
                reader.dump(&dump_path).unwrap();
                let mut db = CounterSet::new();
                reader = CaptureReader::restore(&mut db, &dump_path).unwrap();
                let traf_out_file = File::create(traf_out_path.clone()).unwrap();
                let mut traf_out_writer = BufWriter::new(traf_out_file);
                let num_items = reader.item_index.len();
                for item_id in 0 .. num_items {
                    let item = reader.item(None, mode, item_id).unwrap();
                    write_item(&mut reader, &item, mode, &mut traf_out_writer);
                }
                let dev_out_file = File::create(dev_out_path.clone()).unwrap();
                let mut dev_out_writer = BufWriter::new(dev_out_file);
                let num_devices = reader.devices.len() - 1;
                for device_id in 0 .. num_devices {
                    let item = reader.item(None, (), device_id).unwrap();
                    write_item(&mut reader, &item, (), &mut dev_out_writer);
                }
            }
            for (ref_path, out_path) in [
                (traf_ref_path, traf_out_path),
                (dev_ref_path, dev_out_path),
            ] {
                let ref_file = File::open(ref_path).unwrap();
                let out_file = File::open(out_path.clone()).unwrap();
                let ref_reader = BufReader::new(ref_file);
                let out_reader = BufReader::new(out_file);
                let mut out_lines = out_reader.lines();
                for line in ref_reader.lines() {
                    let expected = line.unwrap();
                    let actual = out_lines.next().unwrap().unwrap();
                    assert_eq!(actual, expected);
                }
            }
        }
    }
}
