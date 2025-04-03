//! Capture database implementation for USB 2.0

use std::cmp::min;
use std::fmt::{Debug, Write};
use std::iter::once;
use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::Arc;
use std::time::Duration;
use std::mem::size_of;

use crate::database::{
    CompactReader,
    CompactWriter,
    compact_index,
    DataReader,
    DataWriter,
    data_stream,
    data_stream_with_block_size,
};
use crate::usb::{self, prelude::*};
use crate::util::{
    id::Id,
    rcu::SingleWriterRcu,
    vec_map::{Key, VecMap},
    Bytes,
    RangeLength,
    fmt_count,
    fmt_size,
};

use anyhow::{Context, Error, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytemuck_derive::{Pod, Zeroable};
use itertools::Itertools;
use merge::Merge;
use num_enum::{IntoPrimitive, FromPrimitive};

// Use 2MB block size for packet data, which is a large page size on x86_64.
const PACKET_DATA_BLOCK_SIZE: usize = 0x200000;

/// Metadata about the capture.
#[derive(Clone, Default, Merge)]
pub struct CaptureMetadata {
    // Fields corresponding to PCapNG section header.
    pub application: Option<String>,
    pub os: Option<String>,
    pub hardware: Option<String>,
    pub comment: Option<String>,

    // Fields corresponding to PcapNG interface description.
    pub iface_desc: Option<String>,
    pub iface_hardware: Option<String>,
    pub iface_os: Option<String>,
    pub iface_speed: Option<Speed>,
    pub iface_snaplen: Option<NonZeroU32>,

    // Fields corresponding to PcapNG interface statistics.
    pub start_time: Option<Duration>,
    pub end_time: Option<Duration>,
    pub dropped: Option<u64>,
}

/// Capture state shared between readers and writers.
pub struct CaptureShared {
    pub metadata: ArcSwap<CaptureMetadata>,
    pub device_data: ArcSwap<VecMap<DeviceId, Arc<DeviceData>>>,
    pub endpoint_index: ArcSwap<VecMap<EndpointKey, EndpointId>>,
    pub endpoint_readers: ArcSwap<VecMap<EndpointId, Arc<EndpointReader>>>,
    pub complete: AtomicBool,
}

/// Unique handle for write access to a capture.
pub struct CaptureWriter {
    pub shared: Arc<CaptureShared>,
    pub packet_data: DataWriter<u8, PACKET_DATA_BLOCK_SIZE>,
    pub packet_index: CompactWriter<PacketId, PacketByteId, 2>,
    pub packet_times: CompactWriter<PacketId, Timestamp, 3>,
    pub transaction_index: CompactWriter<TransactionId, PacketId>,
    pub group_index: DataWriter<GroupIndexEntry>,
    pub item_index: CompactWriter<TrafficItemId, GroupId>,
    pub devices: DataWriter<Device>,
    pub endpoints: DataWriter<Endpoint>,
    pub endpoint_states: DataWriter<u8>,
    pub endpoint_state_index: CompactWriter<GroupId, Id<u8>>,
    #[allow(dead_code)]
    pub end_index: CompactWriter<GroupId, TrafficItemId>,
}

/// Cloneable handle for read access to a capture.
#[derive(Clone)]
pub struct CaptureReader {
    pub shared: Arc<CaptureShared>,
    endpoint_readers: VecMap<EndpointId, EndpointReader>,
    pub packet_data: DataReader<u8, PACKET_DATA_BLOCK_SIZE>,
    pub packet_index: CompactReader<PacketId, PacketByteId>,
    pub packet_times: CompactReader<PacketId, Timestamp>,
    pub transaction_index: CompactReader<TransactionId, PacketId>,
    pub group_index: DataReader<GroupIndexEntry>,
    pub item_index: CompactReader<TrafficItemId, GroupId>,
    pub devices: DataReader<Device>,
    pub endpoints: DataReader<Endpoint>,
    pub endpoint_states: DataReader<u8>,
    pub endpoint_state_index: CompactReader<GroupId, Id<u8>>,
    #[allow(dead_code)]
    pub end_index: CompactReader<GroupId, TrafficItemId>,
}

/// Create a capture reader-writer pair.
pub fn create_capture()
    -> Result<(CaptureWriter, CaptureReader), Error>
{
    // Create all the required streams.
    let (data_writer, data_reader) =
        data_stream_with_block_size::<_, PACKET_DATA_BLOCK_SIZE>()?;
    let (packets_writer, packets_reader) = compact_index()?;
    let (timestamp_writer, timestamp_reader) = compact_index()?;
    let (transactions_writer, transactions_reader) = compact_index()?;
    let (groups_writer, groups_reader) = data_stream()?;
    let (items_writer, items_reader) = compact_index()?;
    let (devices_writer, devices_reader) = data_stream()?;
    let (endpoints_writer, endpoints_reader) = data_stream()?;
    let (endpoint_state_writer, endpoint_state_reader) = data_stream()?;
    let (state_index_writer, state_index_reader) = compact_index()?;
    let (end_writer, end_reader) = compact_index()?;

    // Create the state shared by readers and writer.
    let shared = Arc::new(CaptureShared {
        metadata: ArcSwap::new(Arc::new(CaptureMetadata::default())),
        device_data: ArcSwap::new(Arc::new(VecMap::new())),
        endpoint_index: ArcSwap::new(Arc::new(VecMap::new())),
        endpoint_readers: ArcSwap::new(Arc::new(VecMap::new())),
        complete: AtomicBool::from(false),
    });

    // Create the write handle.
    let writer = CaptureWriter {
        shared: shared.clone(),
        packet_data: data_writer,
        packet_index: packets_writer,
        packet_times: timestamp_writer,
        transaction_index: transactions_writer,
        group_index: groups_writer,
        item_index: items_writer,
        devices: devices_writer,
        endpoints: endpoints_writer,
        endpoint_states: endpoint_state_writer,
        endpoint_state_index: state_index_writer,
        end_index: end_writer,
    };

    // Create the first read handle.
    let reader = CaptureReader {
        shared,
        endpoint_readers: VecMap::new(),
        packet_data: data_reader,
        packet_index: packets_reader,
        packet_times: timestamp_reader,
        transaction_index: transactions_reader,
        group_index: groups_reader,
        item_index: items_reader,
        devices: devices_reader,
        endpoints: endpoints_reader,
        endpoint_states: endpoint_state_reader,
        endpoint_state_index: state_index_reader,
        end_index: end_reader,
    };

    // Return the pair.
    Ok((writer, reader))
}

/// Per-endpoint state shared between readers and writers.
pub struct EndpointShared {
    pub total_data: AtomicU64,
    #[allow(dead_code)]
    pub first_item_id: ArcSwapOption<TrafficItemId>,
}

/// Unique handle for write access to endpoint data.
pub struct EndpointWriter {
    pub shared: Arc<EndpointShared>,
    pub transaction_ids: CompactWriter<EndpointTransactionId, TransactionId>,
    pub group_index: CompactWriter<EndpointGroupId, EndpointTransactionId>,
    pub data_transactions: CompactWriter<EndpointDataEvent, EndpointTransactionId>,
    pub data_byte_counts: CompactWriter<EndpointDataEvent, EndpointByteCount>,
    pub end_index: CompactWriter<EndpointGroupId, TrafficItemId>,
}

/// Cloneable handle for read access to endpoint data.
#[derive(Clone)]
pub struct EndpointReader {
    pub shared: Arc<EndpointShared>,
    pub transaction_ids: CompactReader<EndpointTransactionId, TransactionId>,
    pub group_index: CompactReader<EndpointGroupId, EndpointTransactionId>,
    pub data_transactions: CompactReader<EndpointDataEvent, EndpointTransactionId>,
    pub data_byte_counts: CompactReader<EndpointDataEvent, EndpointByteCount>,
    pub end_index: CompactReader<EndpointGroupId, TrafficItemId>,
}

/// Create a per-endpoint reader-writer pair.
pub fn create_endpoint()
    -> Result<(EndpointWriter, EndpointReader), Error>
{
    // Create all the required streams.
    let (transactions_writer, transactions_reader) = compact_index()?;
    let (groups_writer, groups_reader) = compact_index()?;
    let (data_transaction_writer, data_transaction_reader) = compact_index()?;
    let (data_byte_count_writer, data_byte_count_reader) = compact_index()?;
    let (end_writer, end_reader) = compact_index()?;

    // Create the shared state.
    let shared = Arc::new(EndpointShared {
        total_data: AtomicU64::from(0),
        first_item_id: ArcSwapOption::const_empty(),
    });

    // Create the write handle.
    let writer = EndpointWriter {
        shared: shared.clone(),
        transaction_ids: transactions_writer,
        group_index: groups_writer,
        data_transactions: data_transaction_writer,
        data_byte_counts: data_byte_count_writer,
        end_index: end_writer,
    };

    // Create the read handle.
    let reader = EndpointReader {
        shared,
        transaction_ids: transactions_reader,
        group_index: groups_reader,
        data_transactions: data_transaction_reader,
        data_byte_counts: data_byte_count_reader,
        end_index: end_reader,
    };

    // Return the pair.
    Ok((writer, reader))
}

pub type PacketByteId = Id<u8>;
pub type PacketId = Id<PacketByteId>;
pub type Timestamp = u64;
pub type TransactionId = Id<PacketId>;
pub type GroupId = Id<GroupIndexEntry>;
pub type EndpointTransactionId = Id<TransactionId>;
pub type EndpointGroupId = Id<EndpointTransactionId>;
pub type TrafficItemId = Id<GroupId>;
pub type DeviceId = Id<Device>;
pub type EndpointId = Id<Endpoint>;
pub type EndpointDataEvent = u64;
pub type EndpointByteCount = u64;
pub type DeviceVersion = u32;

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct Device {
    pub address: DeviceAddr,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct Endpoint(u64);
    pub u64, from into DeviceId, device_id, set_device_id: 50, 0;
    pub u8, from into DeviceAddr, device_address, set_device_address: 57, 51;
    pub u8, from into EndpointNum, number, set_number: 62, 58;
    pub u8, from into Direction, direction, set_direction: 63, 63;
}

impl Endpoint {
    fn address(&self) -> EndpointAddr {
        EndpointAddr::from_parts(self.number(), self.direction())
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{} {}",
               self.device_address(),
               self.number(),
               self.direction()
               )
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct GroupIndexEntry(u64);
    pub u64, from into EndpointGroupId, group_id, set_group_id: 51, 0;
    pub u64, from into EndpointId, endpoint_id, set_endpoint_id: 62, 52;
    pub u8, _is_start, _set_is_start: 63, 63;
}

impl GroupIndexEntry {
    pub fn is_start(&self) -> bool {
        self._is_start() != 0
    }
    pub fn set_is_start(&mut self, value: bool) {
        self._set_is_start(value as u8)
    }
}

#[derive(Copy, Clone, IntoPrimitive, FromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum EndpointState {
    #[default]
    Idle = 0,
    Starting = 1,
    Ongoing = 2,
    Ending = 3,
}

pub const CONTROL_EP_NUM: EndpointNum = EndpointNum(0);
pub const INVALID_EP_NUM: EndpointNum = EndpointNum(0x10);
pub const FRAMING_EP_NUM: EndpointNum = EndpointNum(0x11);
pub const INVALID_EP_ID: EndpointId = EndpointId::constant(0);
pub const FRAMING_EP_ID: EndpointId = EndpointId::constant(1);

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EndpointType {
    Unidentified,
    Framing,
    Invalid,
    Normal(usb::EndpointType)
}

impl std::fmt::Display for EndpointType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use EndpointType::*;
        match self {
            Normal(usb_type) => std::fmt::Display::fmt(&usb_type, f),
            Unidentified => write!(f, "unidentified"),
            Framing => write!(f, "framing"),
            Invalid => write!(f, "invalid"),
        }
    }
}

type EndpointDetails = (usb::EndpointType, Option<usize>);

#[derive(Copy, Clone)]
pub struct EndpointKey {
    pub dev_addr: DeviceAddr,
    pub direction: Direction,
    pub ep_num: EndpointNum,
}

impl Key for EndpointKey {
    fn id(self) -> usize {
        self.dev_addr.0 as usize * 32 +
            self.direction as usize * 16 +
                self.ep_num.0 as usize
    }

    fn key(id: usize) -> EndpointKey {
        EndpointKey {
            dev_addr: DeviceAddr((id / 32) as u8),
            direction: Direction::from(((id / 16) % 2) as u8),
            ep_num: EndpointNum((id % 16) as u8),
        }
    }
}

#[derive(Default)]
pub struct DeviceData {
    pub device_descriptor: ArcSwapOption<DeviceDescriptor>,
    pub configurations: ArcSwap<VecMap<ConfigNum, Arc<Configuration>>>,
    pub config_number: ArcSwapOption<ConfigNum>,
    pub interface_settings: ArcSwap<VecMap<InterfaceNum, InterfaceAlt>>,
    pub endpoint_details: ArcSwap<VecMap<EndpointAddr, EndpointDetails>>,
    pub strings: ArcSwap<VecMap<StringId, UTF16ByteVec>>,
    pub version: AtomicU32,
}

impl DeviceData {
    pub fn description(&self) -> String {
        match self.device_descriptor.load().as_ref() {
            None => "Unknown".to_string(),
            Some(descriptor) => {
                let str_id = descriptor.product_str_id;
                if let Some(utf16) = self.strings.load().get(str_id) {
                    let chars = utf16.chars();
                    if let Ok(string) = String::from_utf16(&chars) {
                        return format!("{}", string.escape_default());
                    }
                }
                format!(
                    "{:04X}:{:04X}",
                    descriptor.vendor_id,
                    descriptor.product_id)
            }
        }
    }

    pub fn configuration(&self, number: ConfigNum)
        -> Result<Arc<Configuration>, Error>
    {
        match self.configurations.load().get(number) {
            Some(config) => Ok(config.clone()),
            None => bail!("No descriptor for config {number}")
        }
    }

    pub fn endpoint_details(&self, addr: EndpointAddr)
        -> (EndpointType, Option<usize>)
    {
        use EndpointType::*;
        match addr.number() {
            INVALID_EP_NUM => (Invalid, None),
            FRAMING_EP_NUM => (Framing, None),
            CONTROL_EP_NUM => (
                Normal(usb::EndpointType::Control),
                self.device_descriptor.load().as_ref().map(|desc| {
                    desc.max_packet_size_0 as usize
                })
            ),
            _ => match self.endpoint_details.load().get(addr) {
                Some((ep_type, ep_max)) => (Normal(*ep_type), *ep_max),
                None => (Unidentified, None)
            }
        }
    }

    pub fn update_endpoint_details(&self) {
        if let Some(number) = self.config_number.load().as_ref() {
            if let Some(config) = &self.configurations.load().get(**number) {
                let iface_settings = self.interface_settings.load();
                self.endpoint_details.update(|endpoint_details| {
                    for ((num, alt), iface) in config.interfaces.iter() {
                        if iface_settings.get(*num) == Some(alt) {
                            for endpoint in &iface.endpoints {
                                let ep_desc = &endpoint.descriptor;
                                let ep_addr = ep_desc.endpoint_address;
                                let ep_type = ep_desc.attributes.endpoint_type();
                                let ep_max = ep_desc.max_packet_size as usize;
                                endpoint_details.set(
                                    ep_addr,
                                    (ep_type, Some(ep_max))
                                );
                            }
                        }
                    }
                });
            }
        }
    }

    pub fn set_endpoint_type(&self,
                             addr: EndpointAddr,
                             ep_type: usb::EndpointType)
    {
        self.endpoint_details.maybe_update(|endpoint_details| {
            if endpoint_details.get(addr).is_none() {
                endpoint_details.set(addr, (ep_type, None));
                true
            } else {
                false
            }
        });
    }

    pub fn decode_request(&self, fields: &SetupFields, payload: &[u8])
        -> Result<(), Error>
    {
        let req_type = fields.type_fields.request_type();
        let request = StandardRequest::from(fields.request);
        match (req_type, request) {
            (RequestType::Standard, StandardRequest::GetDescriptor)
                => self.decode_descriptor_read(fields, payload)?,
            (RequestType::Standard, StandardRequest::SetConfiguration)
                => self.decode_configuration_set(fields)?,
            (RequestType::Standard, StandardRequest::SetInterface)
                => self.decode_interface_set(fields)?,
            _ => ()
        }
        Ok(())
    }

    pub fn decode_descriptor_read(&self,
                                  fields: &SetupFields,
                                  payload: &[u8])
        -> Result<(), Error>
    {
        let recipient = fields.type_fields.recipient();
        let desc_type = DescriptorType::from((fields.value >> 8) as u8);
        let length = payload.len();
        match (recipient, desc_type) {
            (Recipient::Device, DescriptorType::Device) => {
                if length == size_of::<DeviceDescriptor>() {
                    let descriptor = DeviceDescriptor::from_bytes(payload);
                    self.device_descriptor.swap(Some(Arc::new(descriptor)));
                    self.increment_version();
                }
            },
            (Recipient::Device, DescriptorType::Configuration) => {
                let size = size_of::<ConfigDescriptor>();
                if length >= size {
                    let configuration = Configuration::from_bytes(payload);
                    if let Some(config) = configuration {
                        let config_num = ConfigNum::from(
                            config.descriptor.config_value);
                        self.configurations.update(|configurations| {
                            configurations.set(config_num, Arc::new(config));
                        });
                        self.update_endpoint_details();
                        self.increment_version();
                    }
                }
            },
            (Recipient::Device, DescriptorType::String) => {
                if length >= 2 {
                    let string = UTF16ByteVec(payload[2..length].to_vec());
                    let string_id =
                        StringId::from((fields.value & 0xFF) as u8);
                    self.strings.update(|strings| {
                        strings.set(string_id, string)
                    });
                    self.increment_version();
                }
            },
            _ => {}
        };
        Ok(())
    }

    fn decode_configuration_set(&self, fields: &SetupFields)
        -> Result<(), Error>
    {
        let config_number = ConfigNum(fields.value.try_into()?);
        self.config_number.swap(Some(Arc::new(config_number)));
        let mut interface_settings = VecMap::new();
        if let Some(config) = self.configurations.load().get(config_number) {
            // All interfaces are reset to setting zero.
            for (num, _alt) in config.interfaces
                .keys()
                .unique_by(|(num, _alt)| num)
            {
                interface_settings.set(*num, InterfaceAlt(0));
            }
        }
        self.interface_settings.swap(Arc::new(interface_settings));
        self.update_endpoint_details();
        self.increment_version();
        Ok(())
    }

    fn decode_interface_set(&self, fields: &SetupFields)
        -> Result<(), Error>
    {
        let iface_num = InterfaceNum(fields.index.try_into()?);
        let iface_alt = InterfaceAlt(fields.value.try_into()?);
        self.interface_settings.update(|interface_settings|
            interface_settings.set(iface_num, iface_alt)
        );
        self.update_endpoint_details();
        self.increment_version();
        Ok(())
    }

    fn increment_version(&self) {
        self.version.fetch_add(1, Release);
    }

    pub fn version(&self) -> DeviceVersion {
        self.version.load(Acquire)
    }
}

impl Configuration {
    pub fn function(&self, number: usize) -> Result<&Function, Error> {
        match self.functions.values().nth(number) {
            Some(function) => Ok(function),
            _ => bail!("Configuration has no function with index {number}")
        }
    }

    pub fn interface(&self, key: InterfaceKey)
        -> Result<&Interface, Error>
    {
        self.interfaces
            .get(&key)
            .context("Configuration has no interface matching {key:?}")
    }

    pub fn associated_interfaces(&self, desc: &InterfaceAssociationDescriptor)
        -> impl Iterator<Item=&Interface>
    {
        self.interfaces.range(desc.interface_range()).map(|(_k, v)| v)
    }

    pub fn unassociated_interfaces(&self)  -> impl Iterator<Item=&Interface> {
        let associated_ranges = self.functions
            .values()
            .map(|f| f.descriptor.interface_range())
            .collect::<Vec<_>>();
        self.interfaces
            .iter()
            .filter_map(move |(key, interface)| {
                if associated_ranges.iter().any(|range| range.contains(key)) {
                    None
                } else {
                    Some(interface)
                }
            })
    }

    pub fn other_descriptor(&self, number: usize)
        -> Result<&Descriptor, Error>
    {
        match self.other_descriptors.get(number) {
            Some(desc) => Ok(desc),
            _ => bail!("Configuration has no other descriptor {number}")
        }
    }
}

impl Interface {
    pub fn endpoint(&self, number: InterfaceEpNum)
        -> Result<&usb::Endpoint, Error>
    {
        match self.endpoints.get(number.0 as usize) {
            Some(ep) => Ok(ep),
            _ => bail!("Interface has no endpoint {number}")
        }
    }

    pub fn other_descriptor(&self, number: usize)
        -> Result<&Descriptor, Error>
    {
        match self.other_descriptors.get(number) {
            Some(desc) => Ok(desc),
            _ => bail!("Interface has no other descriptor {number}")
        }
    }
}

pub struct Transaction {
    start_pid: PID,
    end_pid: PID,
    split: Option<(SplitFields, PID)>,
    pub packet_id_range: Range<PacketId>,
    data_packet_id: Option<PacketId>,
    pub payload_byte_range: Option<Range<Id<u8>>>,
}

#[derive(PartialEq)]
pub enum TransactionResult {
    Success,
    Failure,
    Ambiguous
}

impl Transaction {
    fn packet_count(&self) -> u64 {
        self.packet_id_range.len()
    }

    fn payload_size(&self) -> Option<u64> {
        self.payload_byte_range.as_ref().map(|range| range.len())
    }

    fn result(&self, ep_type: EndpointType) -> TransactionResult {
        use PID::*;
        use EndpointType::*;
        use usb::EndpointType::*;
        use TransactionResult::*;
        match (self.start_pid, self.end_pid) {

            // SPLIT is successful if it ends with DATA0/DATA1/ACK/NYET.
            (SPLIT, DATA0 | DATA1 | ACK | NYET) => Success,

            // SETUP/IN/OUT is successful if it ends with ACK/NYET.
            (SETUP | IN | OUT, ACK | NYET) => Success,

            // IN/OUT followed by DATA0/DATA1 depends on endpoint type.
            (IN | OUT, DATA0 | DATA1) => match ep_type {
                // For an isochronous endpoint this is a success.
                Normal(Isochronous) => Success,
                // For an unidentified endpoint this is ambiguous.
                Unidentified => Ambiguous,
                // For any other endpoint type this is a failure (no handshake).
                _ => Failure,
            },

            (..) => Failure
        }
    }

    fn control_result(&self, direction: Direction) -> ControlResult {
        use ControlResult::*;
        use StartComplete::*;
        use Direction::*;
        use PID::*;
        use EndpointType::*;
        use usb::EndpointType::*;
        use TransactionResult::*;
        let end_pid = match (direction, self.start_pid, self.split.as_ref()) {
            (In,  OUT,   None) |
            (Out, IN,    None) =>
                self.end_pid,
            (In,  SPLIT, Some((split_fields, OUT))) |
            (Out, SPLIT, Some((split_fields, IN ))) => {
                if split_fields.sc() == Complete {
                    self.end_pid
                } else {
                    return Incomplete
                }
            },
            _ => return if self.end_pid == STALL { Stalled } else { Incomplete }
        };
        if end_pid == STALL {
            Stalled
        } else if self.result(Normal(Control)) == Success {
            Completed
        } else {
            Incomplete
        }
    }

    fn outcome(&self) -> Option<PID> {
        use PID::*;
        match self.end_pid {
            // Any handshake response should be displayed as an outcome.
            ACK | NAK | NYET | STALL | ERR => Some(self.end_pid),
            _ => None
        }
    }

    pub fn description(
        &self,
        capture: &mut CaptureReader,
        endpoint: &Endpoint,
        detail: bool
    ) -> Result<String, Error> {
        use PID::*;
        use StartComplete::*;
        Ok(match (self.start_pid, &self.split) {
            (SOF, _) => format!(
                "{} SOF packets", self.packet_count()),
            (SPLIT, Some((split_fields, token_pid))) => format!(
                "{} {}",
                match split_fields.sc() {
                    Start => "Starting",
                    Complete => "Completing",
                },
                self.inner_description(capture, endpoint, *token_pid, detail)?
            ),
            (pid, _) => self.inner_description(capture, endpoint, pid, detail)?
        })
    }

    fn inner_description(&self,
                         capture: &mut CaptureReader,
                         endpoint: &Endpoint,
                         pid: PID,
                         detail: bool)
        -> Result<String, Error>
    {
        if endpoint.number() == INVALID_EP_NUM {
            return Ok(format!("{pid} transaction"));
        }
        let mut s = String::new();
        if detail {
            write!(s, "{} transaction on device {}, endpoint {}",
                pid, endpoint.device_address(), endpoint.number())
        } else {
            write!(s, "{} transaction on {}.{}",
                pid, endpoint.device_address(), endpoint.number())
        }?;
        match (self.payload_size(), self.outcome(), detail) {
            (None, None, _) => Ok(()),
            (None, Some(outcome), false) => write!(s,
                ", {outcome}"),
            (None, Some(outcome), true) => write!(s,
                ", {outcome} response"),
            (Some(0), None, _) => write!(s,
                " with no data"),
            (Some(0), Some(outcome), false) => write!(s,
                " with no data, {outcome}"),
            (Some(0), Some(outcome), true) => write!(s,
                " with no data, {outcome} response"),
            (Some(size), None, false) => write!(s,
                " with {size} data bytes: {}",
                Bytes::first(100, &capture.transaction_bytes(self)?)),
            (Some(size), None, true) => write!(s,
                " with {size} data bytes\nPayload: {}",
                Bytes::first(1024, &capture.transaction_bytes(self)?)),
            (Some(size), Some(outcome), false) => write!(s,
                " with {size} data bytes, {outcome}: {}",
                Bytes::first(100, &capture.transaction_bytes(self)?)),
            (Some(size), Some(outcome), true) => write!(s,
                " with {size} data bytes, {outcome} response\nPayload: {}",
                Bytes::first(1024, &capture.transaction_bytes(self)?)),
        }?;
        Ok(s)
    }
}

pub struct Group {
    pub endpoint_id: EndpointId,
    pub endpoint: Endpoint,
    pub endpoint_type: EndpointType,
    pub range: Range<EndpointTransactionId>,
    pub count: u64,
    pub content: GroupContent,
    pub is_start: bool,
}

pub enum GroupContent {
    Request(ControlTransfer),
    Data(Range<EndpointDataEvent>),
    Polling(u64),
    Ambiguous(Range<EndpointDataEvent>, u64),
    IncompleteRequest,
    Framing,
    Invalid,
}

impl CaptureShared {
    pub fn packet_endpoint(&self, pid: PID, packet: &[u8])
        -> Result<EndpointId, EndpointKey>
    {
        match PacketFields::from_packet(packet) {
            PacketFields::SOF(_) => Ok(FRAMING_EP_ID),
            PacketFields::Token(token) => {
                let dev_addr = token.device_address();
                let ep_num = token.endpoint_number();
                let direction = match (ep_num.0, pid) {
                    (0, _)          => Direction::Out,
                    (_, PID::SETUP) => Direction::Out,
                    (_, PID::IN)    => Direction::In,
                    (_, PID::OUT)   => Direction::Out,
                    (_, PID::PING)  => Direction::Out,
                    _ => panic!("PID {pid} does not indicate a direction")
                };
                let key = EndpointKey {
                    dev_addr,
                    ep_num,
                    direction
                };
                match self.endpoint_index.load().get(key) {
                    Some(id) => Ok(*id),
                    None => Err(key),
                }
            },
            _ => Ok(INVALID_EP_ID),
        }
    }
}

impl CaptureWriter {
    pub fn device_data(&self, id: DeviceId)
        -> Result<Arc<DeviceData>, Error>
    {
        Ok(self.shared.device_data
            .load()
            .get(id)
            .context("Capture has no device with ID {id}")?
            .clone())
    }

    pub fn print_storage_summary(&self) {
        let mut overhead: u64 =
            self.packet_index.size() +
            self.transaction_index.size() +
            self.group_index.size() +
            self.endpoint_states.size() +
            self.endpoint_state_index.size();
        let mut trx_count = 0;
        let mut trx_size = 0;
        let mut xfr_count = 0;
        let mut xfr_size = 0;
        for ep_traf in self.shared.endpoint_readers.load().as_ref() {
            trx_count += ep_traf.transaction_ids.len();
            trx_size += ep_traf.transaction_ids.size();
            xfr_count += ep_traf.group_index.len();
            xfr_size += ep_traf.group_index.size();
            overhead += trx_size + xfr_size;
        }
        let ratio = (overhead as f32) / (self.packet_data.size() as f32);
        let percentage = ratio * 100.0;
        print!(concat!(
            "Storage summary:\n",
            "  Packet data: {}\n",
            "  Packet index: {}\n",
            "  Transaction index: {}\n",
            "  Transaction group index: {}\n",
            "  Endpoint states: {}\n",
            "  Endpoint state index: {}\n",
            "  Endpoint transaction indices: {} values, {}\n",
            "  Endpoint transaction group indices: {} values, {}\n",
            "Total overhead: {:.1}% ({})\n"),
            fmt_size(self.packet_data.size()),
            &self.packet_index,
            &self.transaction_index,
            &self.group_index,
            &self.endpoint_states,
            &self.endpoint_state_index,
            fmt_count(trx_count), fmt_size(trx_size),
            fmt_count(xfr_count), fmt_size(xfr_size),
            percentage, fmt_size(overhead),
        )
    }
}

impl CaptureReader {
    pub fn endpoint_traffic(&mut self, endpoint_id: EndpointId)
        -> Result<&mut EndpointReader, Error>
    {
        if self.shared.endpoint_readers.load().get(endpoint_id).is_none() {
            bail!("Capture has no endpoint ID {endpoint_id}")
        }

        if self.endpoint_readers.get(endpoint_id).is_none() {
            let reader = self.shared.endpoint_readers
                .load()
                .get(endpoint_id)
                .unwrap()
                .as_ref()
                .clone();
            self.endpoint_readers.set(endpoint_id, reader);
        }

        Ok(self.endpoint_readers.get_mut(endpoint_id).unwrap())
    }

    pub fn group_range(&mut self, entry: &GroupIndexEntry)
        -> Result<Range<EndpointTransactionId>, Error>
    {
        let endpoint_id = entry.endpoint_id();
        let ep_group_id = entry.group_id();
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        ep_traf.group_index.target_range(
            ep_group_id, ep_traf.transaction_ids.len())
    }

    fn transaction_fields(&mut self, transaction: &Transaction)
        -> Result<SetupFields, Error>
    {
        match transaction.data_packet_id {
            None => bail!("Transaction has no data packet"),
            Some(data_packet_id) => {
                let data_packet = self.packet(data_packet_id)?;
                match data_packet.first() {
                    None => bail!("Found empty packet instead of setup data"),
                    Some(byte) => {
                        let pid = PID::from(byte);
                        if pid != PID::DATA0 {
                            bail!("Found {pid} packet instead of setup data")
                        } else if data_packet.len() != 11 {
                            bail!("Found DATA0 with packet length {} \
                                   instead of setup data", data_packet.len())
                        } else {
                            Ok(SetupFields::from_data_packet(&data_packet))
                        }
                    }
                }
            }
        }
    }

    pub fn transaction_bytes(&mut self, transaction: &Transaction)
        -> Result<Vec<u8>, Error>
    {
        let data_packet_id = transaction.data_packet_id
            .context("Transaction has no data packet")?;
        let packet_byte_range = self.packet_index.target_range(
            data_packet_id, self.packet_data.len())?;
        let data_byte_range =
            packet_byte_range.start + 1 .. packet_byte_range.end - 2;
        self.packet_data.get_range(&data_byte_range)
    }

    pub fn transfer_bytes(&mut self,
                          endpoint_id: EndpointId,
                          data_range: &Range<EndpointDataEvent>,
                          length: usize)
        -> Result<Vec<u8>, Error>
    {
        let mut transfer_bytes = Vec::with_capacity(length);
        let mut data_range = data_range.clone();
        while transfer_bytes.len() < length {
            let data_id = data_range.next().with_context(|| format!(
                "Ran out of data events after fetching {}/{} requested bytes",
                transfer_bytes.len(), length))?;
            let ep_traf = self.endpoint_traffic(endpoint_id)?;
            let ep_transaction_id = ep_traf.data_transactions.get(data_id)?;
            let transaction_id = ep_traf.transaction_ids.get(ep_transaction_id)?;
            let transaction = self.transaction(transaction_id)?;
            let transaction_bytes = self.transaction_bytes(&transaction)?;
            let required = min(
                length - transfer_bytes.len(),
                transaction_bytes.len()
            );
            transfer_bytes.extend(&transaction_bytes[..required]);
        }
        Ok(transfer_bytes)
    }

    pub fn endpoint_state(&mut self, group_id: GroupId)
        -> Result<Vec<u8>, Error>
    {
        let range = self.endpoint_state_index.target_range(
            group_id, self.endpoint_states.len())?;
        self.endpoint_states.get_range(&range)
    }

    pub fn packet(&mut self, id: PacketId)
        -> Result<Vec<u8>, Error>
    {
        let range = self.packet_index.target_range(
            id, self.packet_data.len())?;
        self.packet_data.get_range(&range)
    }

    pub fn packet_time(&mut self, id: PacketId)
        -> Result<Timestamp, Error>
    {
        self.packet_times.get(id)
    }

    pub fn timestamped_packets(&mut self)
        -> Result<impl Iterator<Item=Result<(u64, Vec<u8>), Error>>, Error>
    {
        let packet_count = self.packet_index.len();
        let packet_ids = PacketId::from(0)..PacketId::from(packet_count);
        let timestamps = self.packet_times.iter(&packet_ids)?;
        let packet_starts = self.packet_index.iter(&packet_ids)?;
        let packet_ends = self.packet_index
            .iter(&packet_ids)?
            .skip(1)
            .chain(once(Ok(PacketByteId::from(self.packet_data.len()))));
        let data_ranges = packet_starts.zip(packet_ends);
        let mut packet_data = self.packet_data.clone();
        Ok(timestamps
            .zip(data_ranges)
            .map(move |(ts, (start, end))| -> Result<(u64, Vec<u8>), Error> {
                let timestamp = ts?;
                let data_range = start?..end?;
                let packet = packet_data.get_range(&data_range)?;
                Ok((timestamp, packet))
            })
        )
    }

    fn packet_pid(&mut self, id: PacketId)
        -> Result<PID, Error>
    {
        let offset: Id<u8> = self.packet_index.get(id)?;
        Ok(PID::from(self.packet_data.get(offset)?))
    }

    pub fn transaction(&mut self, id: TransactionId)
        -> Result<Transaction, Error>
    {
        let packet_id_range = self.transaction_index.target_range(
            id, self.packet_index.len())?;
        let packet_count = packet_id_range.len();
        let start_packet_id = packet_id_range.start;
        let start_pid = self.packet_pid(start_packet_id)?;
        let end_pid = self.packet_pid(packet_id_range.end - 1)?;
        use PID::*;
        use StartComplete::*;
        let (split, data_packet_id) = match start_pid {
            SETUP | IN | OUT if packet_count >= 2 =>
                (None, Some(start_packet_id + 1)),
            SPLIT => {
                let token_packet_id = start_packet_id + 1;
                let split_packet = self.packet(start_packet_id)?;
                let token_pid = self.packet_pid(token_packet_id)?;
                let split_fields = SplitFields::from_packet(&split_packet);
                let data_packet_id = match (split_fields.sc(), token_pid) {
                    (Start, SETUP | OUT) | (Complete, IN) => {
                        if packet_count >= 3 {
                            Some(start_packet_id + 2)
                        } else {
                            None
                        }
                    },
                    (..) => None
                };
                (Some((split_fields, token_pid)), data_packet_id)
            },
            _ => (None, None)
        };
        let payload_byte_range = if let Some(packet_id) = data_packet_id {
            let packet_byte_range = self.packet_index.target_range(
                packet_id, self.packet_data.len())?;
            let pid = self.packet_data.get(packet_byte_range.start)?;
            match PID::from(pid) {
                DATA0 | DATA1 => Some({
                    packet_byte_range.start + 1 .. packet_byte_range.end - 2
                }),
                _ => None
            }
        } else {
            None
        };
        Ok(Transaction {
            start_pid,
            end_pid,
            split,
            data_packet_id,
            packet_id_range,
            payload_byte_range,
        })
    }

    pub fn group(&mut self, group_id: GroupId) -> Result<Group, Error> {
        let entry = self.group_index.get(group_id)?;
        let endpoint_id = entry.endpoint_id();
        let endpoint = self.endpoints.get(endpoint_id)?;
        let device_id = endpoint.device_id();
        let dev_data = self.device_data(device_id)?;
        let ep_addr = endpoint.address();
        let (endpoint_type, _) = dev_data.endpoint_details(ep_addr);
        let range = self.group_range(&entry)?;
        let count = range.len();
        let content = match endpoint_type {
            EndpointType::Invalid => GroupContent::Invalid,
            EndpointType::Framing => GroupContent::Framing,
            EndpointType::Normal(usb::EndpointType::Control) => {
                let addr = endpoint.device_address();
                match self.control_transfer(
                    device_id, addr, endpoint_id, &range)
                {
                    Ok(transfer) => GroupContent::Request(transfer),
                    Err(_) => GroupContent::IncompleteRequest,
                }
            },
            _ => {
                let ep_group_id = entry.group_id();
                let ep_traf = self.endpoint_traffic(endpoint_id)?;
                let range = ep_traf.group_index.target_range(
                    ep_group_id, ep_traf.transaction_ids.len())?;
                let first_transaction_id =
                    ep_traf.transaction_ids.get(range.start)?;
                let first_transaction =
                    self.transaction(first_transaction_id)?;
                let count = if first_transaction.split.is_some() {
                    count.div_ceil(2)
                } else {
                    count
                };
                match first_transaction.result(endpoint_type) {
                    TransactionResult::Success => {
                        let ep_traf = self.endpoint_traffic(endpoint_id)?;
                        let data_range = ep_traf.transfer_data_range(&range)?;
                        GroupContent::Data(data_range)
                    },
                    TransactionResult::Ambiguous => {
                        let ep_traf = self.endpoint_traffic(endpoint_id)?;
                        let data_range = ep_traf.transfer_data_range(&range)?;
                        GroupContent::Ambiguous(data_range, count)
                    },
                    TransactionResult::Failure => GroupContent::Polling(count),
                }
            }
        };
        Ok(Group {
            endpoint_id,
            endpoint,
            endpoint_type,
            range,
            count,
            content,
            is_start: entry.is_start(),
        })
    }

    fn control_transfer(&mut self,
                        device_id: DeviceId,
                        address: DeviceAddr,
                        endpoint_id: EndpointId,
                        range: &Range<EndpointTransactionId>)
        -> Result<ControlTransfer, Error>
    {
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let transaction_ids = ep_traf.transaction_ids.get_range(range)?;
        let data_range = ep_traf.transfer_data_range(range)?;
        let data_length = ep_traf
            .transfer_data_length(&data_range)?
            .try_into()?;
        let data = self.transfer_bytes(endpoint_id, &data_range, data_length)?;
        let setup_transaction = self.transaction(transaction_ids[0])?;
        let fields = self.transaction_fields(&setup_transaction)?;
        let direction = fields.type_fields.direction();
        let last = transaction_ids.len() - 1;
        let last_transaction = self.transaction(transaction_ids[last])?;
        let result = last_transaction.control_result(direction);
        let recipient = fields.type_fields.recipient();
        let dev_data = self.device_data(device_id)?;
        let recipient_class = match recipient {
            Recipient::Device => dev_data.device_descriptor
                .load()
                .as_ref()
                .map(|desc| desc.device_class),
            Recipient::Interface => {
                let iface_num = InterfaceNum(fields.index as u8);
                if let (Some(config_num), Some(iface_alt)) = (
                    dev_data.config_number.load().as_ref(),
                    dev_data.interface_settings.load().get(iface_num))
                {
                    let iface_key = (iface_num, *iface_alt);
                    dev_data
                        .configurations
                        .load()
                        .get(**config_num)
                        .and_then(|config| config.interfaces.get(&iface_key))
                        .map(|interface| interface.descriptor.interface_class)
                } else {
                    None
                }
            }
            _ => None,
        };
        Ok(ControlTransfer {
            address,
            fields,
            data,
            result,
            recipient_class,
        })
    }

    pub fn device_data(&self, id: DeviceId)
        -> Result<Arc<DeviceData>, Error>
    {
        Ok(self.shared.device_data
            .load()
            .get(id)
            .with_context(|| format!("Capture has no device with ID {id}"))?
            .clone())
    }

    pub fn group_extended(
        &mut self,
        endpoint_id: EndpointId,
        group_id: GroupId
    ) -> Result<bool, Error> {
        use EndpointState::*;
        let count = self.group_index.len();
        if group_id.value + 1 >= count {
            return Ok(false);
        };
        let state = self.endpoint_state(group_id + 1)?;
        Ok(match state.get(endpoint_id.value as usize) {
            Some(ep_state) => EndpointState::from(*ep_state) == Ongoing,
            None => false
        })
    }

    pub fn complete(&self) -> bool {
        self.shared.complete.load(Acquire)
    }
}

impl EndpointReader {
    pub fn transfer_data_range(&mut self, range: &Range<EndpointTransactionId>)
        -> Result<Range<EndpointDataEvent>, Error>
    {
        let first_data_id = self.data_transactions.bisect_left(&range.start)?;
        let last_data_id = self.data_transactions.bisect_left(&range.end)?;
        Ok(first_data_id..last_data_id)
    }

    pub fn transfer_data_length(&mut self, range: &Range<EndpointDataEvent>)
        -> Result<u64, Error>
    {
        if range.start == range.end {
            return Ok(0);
        }
        let num_data_events = self.data_byte_counts.len();
        let first_byte_count = self.data_byte_counts.get(range.start)?;
        let last_byte_count = if range.end >= num_data_events {
            self.shared.as_ref().total_data.load(Acquire)
        } else {
            self.data_byte_counts.get(range.end)?
        };
        Ok(last_byte_count - first_byte_count)
    }
}

pub mod prelude {
    #[allow(unused_imports)]
    pub use super::{
        create_capture,
        create_endpoint,
        CaptureReader,
        CaptureWriter,
        CaptureMetadata,
        Device,
        DeviceId,
        DeviceData,
        Endpoint,
        EndpointId,
        EndpointKey,
        EndpointType,
        EndpointState,
        EndpointReader,
        EndpointWriter,
        EndpointTransactionId,
        EndpointGroupId,
        PacketId,
        TrafficItemId,
        TransactionId,
        GroupId,
        Group,
        GroupContent,
        GroupIndexEntry,
        INVALID_EP_NUM,
        FRAMING_EP_NUM,
        INVALID_EP_ID,
        FRAMING_EP_ID,
    };
}
