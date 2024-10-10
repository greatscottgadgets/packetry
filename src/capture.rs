use std::cmp::min;
use std::fmt::{Debug, Write};
use std::iter::once;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::atomic::Ordering::{Acquire, Release};
use std::sync::Arc;
use std::mem::size_of;

use crate::id::{Id, HasLength};
use crate::data_stream::{
    data_stream, data_stream_with_block_size, DataWriter, DataReader};
use crate::compact_index::{compact_index, CompactWriter, CompactReader};
use crate::rcu::SingleWriterRcu;
use crate::vec_map::{Key, VecMap};
use crate::usb::{self, prelude::*, validate_packet};
use crate::util::{fmt_count, fmt_size};

use anyhow::{Context, Error, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytemuck_derive::{Pod, Zeroable};
use itertools::Itertools;
use num_enum::{IntoPrimitive, FromPrimitive};
use usb_ids::FromId;

// Use 2MB block size for packet data, which is a large page size on x86_64.
const PACKET_DATA_BLOCK_SIZE: usize = 0x200000;

/// Capture state shared between readers and writers.
pub struct CaptureShared {
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
    pub transfer_index: DataWriter<TransferIndexEntry>,
    pub item_index: CompactWriter<TrafficItemId, TransferId>,
    pub devices: DataWriter<Device>,
    pub endpoints: DataWriter<Endpoint>,
    pub endpoint_states: DataWriter<u8>,
    pub endpoint_state_index: CompactWriter<TransferId, Id<u8>>,
    #[allow(dead_code)]
    pub end_index: CompactWriter<TransferId, TrafficItemId>,
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
    pub transfer_index: DataReader<TransferIndexEntry>,
    pub item_index: CompactReader<TrafficItemId, TransferId>,
    pub devices: DataReader<Device>,
    pub endpoints: DataReader<Endpoint>,
    pub endpoint_states: DataReader<u8>,
    pub endpoint_state_index: CompactReader<TransferId, Id<u8>>,
    #[allow(dead_code)]
    pub end_index: CompactReader<TransferId, TrafficItemId>,
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
    let (transfers_writer, transfers_reader) = data_stream()?;
    let (items_writer, items_reader) = compact_index()?;
    let (devices_writer, devices_reader) = data_stream()?;
    let (endpoints_writer, endpoints_reader) = data_stream()?;
    let (endpoint_state_writer, endpoint_state_reader) = data_stream()?;
    let (state_index_writer, state_index_reader) = compact_index()?;
    let (end_writer, end_reader) = compact_index()?;

    // Create the state shared by readers and writer.
    let shared = Arc::new(CaptureShared {
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
        transfer_index: transfers_writer,
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
        transfer_index: transfers_reader,
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
    pub transfer_index: CompactWriter<EndpointTransferId, EndpointTransactionId>,
    pub data_transactions: CompactWriter<EndpointDataEvent, EndpointTransactionId>,
    pub data_byte_counts: CompactWriter<EndpointDataEvent, EndpointByteCount>,
    pub end_index: CompactWriter<EndpointTransferId, TrafficItemId>,
}

/// Cloneable handle for read access to endpoint data.
#[derive(Clone)]
pub struct EndpointReader {
    pub shared: Arc<EndpointShared>,
    pub transaction_ids: CompactReader<EndpointTransactionId, TransactionId>,
    pub transfer_index: CompactReader<EndpointTransferId, EndpointTransactionId>,
    pub data_transactions: CompactReader<EndpointDataEvent, EndpointTransactionId>,
    pub data_byte_counts: CompactReader<EndpointDataEvent, EndpointByteCount>,
    pub end_index: CompactReader<EndpointTransferId, TrafficItemId>,
}

/// Create a per-endpoint reader-writer pair.
pub fn create_endpoint()
    -> Result<(EndpointWriter, EndpointReader), Error>
{
    // Create all the required streams.
    let (transactions_writer, transactions_reader) = compact_index()?;
    let (transfers_writer, transfers_reader) = compact_index()?;
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
        transfer_index: transfers_writer,
        data_transactions: data_transaction_writer,
        data_byte_counts: data_byte_count_writer,
        end_index: end_writer,
    };

    // Create the read handle.
    let reader = EndpointReader {
        shared,
        transaction_ids: transactions_reader,
        transfer_index: transfers_reader,
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
pub type TransferId = Id<TransferIndexEntry>;
pub type EndpointTransactionId = Id<TransactionId>;
pub type EndpointTransferId = Id<EndpointTransactionId>;
pub type TrafficItemId = Id<TransferId>;
pub type DeviceId = Id<Device>;
pub type EndpointId = Id<Endpoint>;
pub type EndpointDataEvent = u64;
pub type EndpointByteCount = u64;
pub type DeviceVersion = u32;

#[derive(Clone, Debug)]
pub enum TrafficItem {
    Transfer(TransferId),
    Transaction(Option<TransferId>, TransactionId),
    Packet(Option<TransferId>, Option<TransactionId>, PacketId),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TrafficViewMode {
    Hierarchical,
    Transactions,
    Packets,
}

pub type DeviceViewMode = ();

impl TrafficViewMode {
    pub const fn display_name(&self) -> &'static str {
        use TrafficViewMode::*;
        match self {
            Hierarchical => "Hierarchical",
            Transactions => "Transactions",
            Packets      => "Packets",
        }
    }

    #[cfg(any(test, feature="record-ui-test"))]
    pub const fn log_name(&self) -> &'static str {
        use TrafficViewMode::*;
        match self {
            Hierarchical => "traffic-hierarchical",
            Transactions => "traffic-transactions",
            Packets      => "traffic-packets",
        }
    }

    #[cfg(any(test, feature="record-ui-test"))]
    pub fn from_log_name(log_name: &str) -> TrafficViewMode {
        use TrafficViewMode::*;
        match log_name {
            "traffic-hierarchical" => Hierarchical,
            "traffic-transactions" => Transactions,
            "traffic-packets"      => Packets,
            _ => panic!("Unrecognised log name '{log_name}'")
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceItem {
    device_id: DeviceId,
    version: DeviceVersion,
    content: DeviceItemContent,
    indent: u8,
}

#[derive(Clone, Debug)]
pub enum DeviceItemContent {
    Device(Option<DeviceDescriptor>),
    DeviceDescriptor(Option<DeviceDescriptor>),
    DeviceDescriptorField(DeviceDescriptor, DeviceField),
    Configuration(ConfigNum, ConfigDescriptor),
    ConfigurationDescriptor(ConfigDescriptor),
    ConfigurationDescriptorField(ConfigDescriptor, ConfigField),
    Function(ConfigNum, InterfaceAssociationDescriptor),
    FunctionDescriptor(InterfaceAssociationDescriptor),
    FunctionDescriptorField(InterfaceAssociationDescriptor, IfaceAssocField),
    Interface(ConfigNum, InterfaceDescriptor),
    InterfaceDescriptor(InterfaceDescriptor),
    InterfaceDescriptorField(InterfaceDescriptor, InterfaceField),
    Endpoint(ConfigNum, InterfaceKey, EndpointDescriptor),
    EndpointDescriptor(EndpointDescriptor),
    EndpointDescriptorField(EndpointDescriptor, EndpointField),
    OtherDescriptor(Descriptor),
}

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
    pub struct TransferIndexEntry(u64);
    pub u64, from into EndpointTransferId, transfer_id, set_transfer_id: 51, 0;
    pub u64, from into EndpointId, endpoint_id, set_endpoint_id: 62, 52;
    pub u8, _is_start, _set_is_start: 63, 63;
}

impl TransferIndexEntry {
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
        match self {
            EndpointType::Normal(usb_type) => write!(f, "{usb_type:?}"),
            special_type => write!(f, "{special_type:?}"),
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
                            for ep_desc in &iface.endpoint_descriptors {
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

    fn version(&self) -> DeviceVersion {
        self.version.load(Acquire)
    }
}

impl Configuration {
    pub fn function(&self, number: ConfigFuncNum)
        -> Result<&Function, Error>
    {
        let index = number.0 as usize;
        match self.functions.values().nth(index) {
            Some(function) => Ok(function),
            _ => bail!("Configuration has no function with index {index}")
        }
    }

    pub fn interface(&self, desc: &InterfaceDescriptor)
        -> Result<&Interface, Error>
    {
        self.interfaces
            .get(&desc.key())
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

    pub fn other_descriptor(&self, number: ConfigOtherNum)
        -> Result<&Descriptor, Error>
    {
        match self.other_descriptors.get(number) {
            Some(desc) => Ok(desc),
            _ => bail!("Configuration has no other descriptor {number}")
        }
    }
}

impl Interface {
    pub fn endpoint_descriptor(&self, number: InterfaceEpNum)
        -> Result<&EndpointDescriptor, Error>
    {
        match self.endpoint_descriptors.get(number) {
            Some(desc) => Ok(desc),
            _ => bail!("Interface has no endpoint descriptor {number}")
        }
    }

    pub fn other_descriptor(&self, number: IfaceOtherNum)
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
    payload_byte_range: Option<Range<Id<u8>>>,
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

    fn description(&self,
                   capture: &mut CaptureReader,
                   endpoint: &Endpoint,
                   detail: bool)
        -> Result<String, Error>
    {
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

struct Bytes<'src> {
    partial: bool,
    bytes: &'src [u8],
}

impl<'src> Bytes<'src> {
    fn first(max: usize, bytes: &'src [u8]) -> Self {
        if bytes.len() > max {
            Bytes {
                partial: true,
                bytes: &bytes[0..max],
            }
        } else {
            Bytes {
                partial: false,
                bytes,
            }
        }
    }

    fn looks_like_ascii(&self) -> bool {
        let mut num_printable = 0;
        for &byte in self.bytes {
            if byte == 0 || byte >= 0x80 {
                // Outside ASCII range.
                return false;
            }
            // Count printable and pseudo-printable characters.
            let printable = match byte {
                c if (0x20..0x7E).contains(&c) => true, // printable range
                0x09                           => true, // tab
                0x0A                           => true, // new line
                0x0D                           => true, // carriage return
                _ => false
            };
            if printable {
                num_printable += 1;
            }
        }
        // If the string is at least half printable, treat as ASCII.
        num_printable > 0 && num_printable >= self.bytes.len() / 2
    }
}

impl std::fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.looks_like_ascii() {
            write!(f, "'{}'", String::from_utf8(
                self.bytes.iter()
                          .flat_map(|c| {std::ascii::escape_default(*c)})
                          .collect::<Vec<u8>>()).unwrap())?
        } else {
            write!(f, "{:02X?}", self.bytes)?
        };
        if self.partial {
            write!(f, "...")
        } else {
            Ok(())
        }
    }
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
            self.transfer_index.size() +
            self.endpoint_states.size() +
            self.endpoint_state_index.size();
        let mut trx_count = 0;
        let mut trx_size = 0;
        let mut xfr_count = 0;
        let mut xfr_size = 0;
        for ep_traf in self.shared.endpoint_readers.load().as_ref() {
            trx_count += ep_traf.transaction_ids.len();
            trx_size += ep_traf.transaction_ids.size();
            xfr_count += ep_traf.transfer_index.len();
            xfr_size += ep_traf.transfer_index.size();
            overhead += trx_size + xfr_size;
        }
        let ratio = (overhead as f32) / (self.packet_data.size() as f32);
        let percentage = ratio * 100.0;
        print!(concat!(
            "Storage summary:\n",
            "  Packet data: {}\n",
            "  Packet index: {}\n",
            "  Transaction index: {}\n",
            "  Transfer index: {}\n",
            "  Endpoint states: {}\n",
            "  Endpoint state index: {}\n",
            "  Endpoint transaction indices: {} values, {}\n",
            "  Endpoint transfer indices: {} values, {}\n",
            "Total overhead: {:.1}% ({})\n"),
            fmt_size(self.packet_data.size()),
            &self.packet_index,
            &self.transaction_index,
            &self.transfer_index,
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

    fn transfer_range(&mut self, entry: &TransferIndexEntry)
        -> Result<Range<EndpointTransactionId>, Error>
    {
        let endpoint_id = entry.endpoint_id();
        let ep_transfer_id = entry.transfer_id();
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        ep_traf.transfer_index.target_range(
            ep_transfer_id, ep_traf.transaction_ids.len())
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

    fn transaction_bytes(&mut self, transaction: &Transaction)
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

    fn endpoint_state(&mut self, transfer_id: TransferId)
        -> Result<Vec<u8>, Error>
    {
        let range = self.endpoint_state_index.target_range(
            transfer_id, self.endpoint_states.len())?;
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

    fn control_transfer(&mut self,
                        address: DeviceAddr,
                        endpoint_id: EndpointId,
                        range: Range<EndpointTransactionId>)
        -> Result<ControlTransfer, Error>
    {
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let transaction_ids = ep_traf.transaction_ids.get_range(&range)?;
        let data_range = ep_traf.transfer_data_range(&range)?;
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
        Ok(ControlTransfer {
            address,
            fields,
            data,
            result,
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

    fn transfer_extended(&mut self,
                         endpoint_id: EndpointId,
                         transfer_id: TransferId)
        -> Result<bool, Error>
    {
        use EndpointState::*;
        let count = self.transfer_index.len();
        if transfer_id.value + 1 >= count {
            return Ok(false);
        };
        let state = self.endpoint_state(transfer_id + 1)?;
        Ok(match state.get(endpoint_id.value as usize) {
            Some(ep_state) => EndpointState::from(*ep_state) == Ongoing,
            None => false
        })
    }

    fn completion(&self) -> CompletionStatus {
        use CompletionStatus::*;
        match self.shared.complete.load(Acquire) {
            false => Ongoing,
            true => Complete,
        }
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

#[derive(Copy, Clone)]
pub enum CompletionStatus {
    Complete,
    Ongoing
}

impl CompletionStatus {
    pub fn is_complete(&self) -> bool {
        use CompletionStatus::*;
        match self {
            Complete => true,
            Ongoing => false,
        }
    }
}

pub trait ItemSource<Item, ViewMode> {
    fn item(&mut self,
            parent: Option<&Item>,
            view_mode: ViewMode,
            index: u64)
        -> Result<Item, Error>;
    fn item_update(&mut self, item: &Item)
        -> Result<Option<Item>, Error>;
    fn child_item(&mut self, parent: &Item, index: u64)
        -> Result<Item, Error>;
    fn item_children(&mut self,
                     parent: Option<&Item>,
                     view_mode: ViewMode)
        -> Result<(CompletionStatus, u64), Error>;
    fn description(&mut self,
                   item: &Item,
                   detail: bool)
        -> Result<String, Error>;
    fn connectors(&mut self,
                  view_mode: ViewMode,
                  item: &Item)
        -> Result<String, Error>;
    fn timestamp(&mut self, item: &Item) -> Result<Timestamp, Error>;
}

impl ItemSource<TrafficItem, TrafficViewMode> for CaptureReader {
    fn item(&mut self,
            parent: Option<&TrafficItem>,
            view_mode: TrafficViewMode,
            index: u64)
        -> Result<TrafficItem, Error>
    {
        use TrafficItem::*;
        use TrafficViewMode::*;
        match parent {
            None => Ok(match view_mode {
                Hierarchical => {
                    let item_id = TrafficItemId::from(index);
                    let transfer_id = self.item_index.get(item_id)?;
                    Transfer(transfer_id)
                },
                Transactions =>
                    Transaction(None, TransactionId::from(index)),
                Packets =>
                    Packet(None, None, PacketId::from(index)),
            }),
            Some(item) => self.child_item(item, index)
        }
    }

    fn item_update(&mut self, _item: &TrafficItem)
        -> Result<Option<TrafficItem>, Error>
    {
        Ok(None)
    }

    fn child_item(&mut self, parent: &TrafficItem, index: u64)
        -> Result<TrafficItem, Error>
    {
        use TrafficItem::*;
        Ok(match parent {
            Transfer(transfer_id) =>
                Transaction(Some(*transfer_id), {
                    let entry = self.transfer_index.get(*transfer_id)?;
                    let endpoint_id = entry.endpoint_id();
                    let ep_transfer_id = entry.transfer_id();
                    let ep_traf = self.endpoint_traffic(endpoint_id)?;
                    let offset = ep_traf.transfer_index.get(ep_transfer_id)?;
                    ep_traf.transaction_ids.get(offset + index)?
                }),
            Transaction(transfer_id_opt, transaction_id) =>
                Packet(*transfer_id_opt, Some(*transaction_id), {
                    self.transaction_index.get(*transaction_id)? + index}),
            Packet(..) => bail!("Packets have no child items")
        })
    }

    fn item_children(&mut self,
                     parent: Option<&TrafficItem>,
                     view_mode: TrafficViewMode)
        -> Result<(CompletionStatus, u64), Error>
    {
        use TrafficItem::*;
        use TrafficViewMode::*;
        use CompletionStatus::*;
        Ok(match parent {
            None => {
                (self.completion(), match view_mode {
                    Hierarchical => self.item_index.len(),
                    Transactions => self.transaction_index.len(),
                    Packets => self.packet_index.len(),
                })
            },
            Some(Transfer(transfer_id)) => {
                let entry = self.transfer_index.get(*transfer_id)?;
                if !entry.is_start() {
                    return Ok((Complete, 0));
                }
                let transaction_count = self.transfer_range(&entry)?.len();
                let ep_traf = self.endpoint_traffic(entry.endpoint_id())?;
                if entry.transfer_id().value >= ep_traf.end_index.len() {
                    (Ongoing, transaction_count)
                } else {
                    (Complete, transaction_count)
                }
            },
            Some(Transaction(_, transaction_id)) => {
                let packet_count = self.transaction_index.target_range(
                    *transaction_id, self.packet_index.len())?.len();
                if transaction_id.value < self.transaction_index.len() - 1 {
                    (Complete, packet_count)
                } else {
                    (Ongoing, packet_count)
                }
            },
            Some(Packet(..)) => (Complete, 0),
        })
    }

    fn description(&mut self, item: &TrafficItem, detail: bool)
        -> Result<String, Error>
    {
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
            Transaction(transfer_id_opt, transaction_id) => {
                let num_packets = self.packet_index.len();
                let packet_id_range = self.transaction_index.target_range(
                    *transaction_id, num_packets)?;
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
                    let endpoint_id = match transfer_id_opt {
                        Some(transfer_id) => {
                            let entry = self.transfer_index.get(*transfer_id)?;
                            entry.endpoint_id()
                        },
                        None => match self.shared.packet_endpoint(
                            pid, &start_packet)
                        {
                            Ok(endpoint_id) => endpoint_id,
                            Err(_) => INVALID_EP_ID
                        }
                    };
                    let endpoint = self.endpoints.get(endpoint_id)?;
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
            Transfer(transfer_id) => {
                use EndpointType::*;
                use usb::EndpointType::*;
                use TransactionResult::*;
                let entry = self.transfer_index.get(*transfer_id)?;
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id)?;
                let device_id = endpoint.device_id();
                let dev_data = self.device_data(device_id)?;
                let ep_addr = endpoint.address();
                let (ep_type, _) = dev_data.endpoint_details(ep_addr);
                let range = self.transfer_range(&entry)?;
                let count = range.len();
                if detail && entry.is_start() {
                    let ep_traf = self.endpoint_traffic(entry.endpoint_id())?;
                    let start_ep_transaction_id =
                        ep_traf.transfer_index.get(entry.transfer_id())?;
                    let start_transaction_id =
                        ep_traf.transaction_ids.get(start_ep_transaction_id)?;
                    let start_packet_id =
                        self.transaction_index.get(start_transaction_id)?;
                    if count == 1 {
                        writeln!(s, "Transaction group with 1 transaction")?;
                    } else {
                        writeln!(s, "Transaction group with {} transactions",
                            count)?;
                    }
                    writeln!(s, "Timestamp: {} ns from start of capture",
                        fmt_count(self.packet_time(start_packet_id)?))?;
                    writeln!(s, "First transaction #{}, first packet #{}",
                        start_transaction_id.value + 1,
                        start_packet_id.value + 1)?;
                }
                match (ep_type, entry.is_start()) {
                    (Invalid, true) => write!(s,
                        "{count} invalid groups"),
                    (Invalid, false) => write!(s,
                        "End of invalid groups"),
                    (Framing, true) => write!(s,
                        "{count} SOF groups"),
                    (Framing, false) => write!(s,
                        "End of SOF groups"),
                    (Normal(Control), true) => {
                        let addr = endpoint.device_address();
                        match self.control_transfer(addr, endpoint_id, range) {
                            Ok(transfer) if detail => write!(s,
                                "Control transfer on device {addr}\n{}",
                                transfer.summary()),
                            Ok(transfer) => write!(s,
                                "{}", transfer.summary()),
                            Err(_) => write!(s,
                                "Incomplete control transfer on device {addr}")
                        }
                    },
                    (Normal(Control), false) => {
                        let addr = endpoint.device_address();
                        write!(s, "End of control transfer on device {addr}")
                    },
                    (endpoint_type, starting) => {
                        let ep_transfer_id = entry.transfer_id();
                        let ep_traf = self.endpoint_traffic(endpoint_id)?;
                        let range = ep_traf.transfer_index.target_range(
                            ep_transfer_id, ep_traf.transaction_ids.len())?;
                        let first_transaction_id =
                            ep_traf.transaction_ids.get(range.start)?;
                        let first_transaction =
                            self.transaction(first_transaction_id)?;
                        let ep_type_string = format!("{endpoint_type}");
                        let ep_type_lower = ep_type_string.to_lowercase();
                        let count = if first_transaction.split.is_some() {
                            (count + 1) / 2
                        } else {
                            count
                        };
                        match (first_transaction.result(ep_type), starting) {
                            (Success, true) => {
                                let ep_traf =
                                    self.endpoint_traffic(endpoint_id)?;
                                let data_range =
                                    ep_traf.transfer_data_range(&range)?;
                                let length =
                                    ep_traf.transfer_data_length(&data_range)?;
                                let length_string = fmt_size(length);
                                let max = if detail { 1024 } else { 100 };
                                let display_length = min(length, max) as usize;
                                let transfer_bytes = self.transfer_bytes(
                                    endpoint_id, &data_range, display_length)?;
                                let display_bytes = Bytes {
                                    partial: length > display_length as u64,
                                    bytes: &transfer_bytes,
                                };
                                write!(s, "{ep_type_string} transfer ")?;
                                write!(s, "of {length_string} ")?;
                                write!(s, "on endpoint {endpoint}")?;
                                if detail {
                                    write!(s, "\nPayload: {display_bytes}")
                                } else {
                                    write!(s, ": {display_bytes}")
                                }
                            },
                            (Success, false) => write!(s,
                                "End of {ep_type_lower} transfer on endpoint {endpoint}"),
                            (Failure, true) => write!(s,
                                "Polling {count} times for {ep_type_lower} transfer on endpoint {endpoint}"),
                            (Failure, false) => write!(s,
                                "End polling for {ep_type_lower} transfer on endpoint {endpoint}"),
                            (Ambiguous, true) => {
                                write!(s, "{count} ambiguous transactions on endpoint {endpoint}")?;
                                if detail {
                                    write!(s, "\nThe result of these transactions is ambiguous because the endpoint type is not known.")?;
                                    write!(s, "\nTry starting the capture before this device is enumerated, so that its descriptors are captured.")?;
                                }
                                Ok(())
                            },
                            (Ambiguous, false) => write!(s,
                                "End of ambiguous transactions."),
                        }
                    }
                }?;
                s
            }
        })
    }

    fn connectors(&mut self, view_mode: TrafficViewMode, item: &TrafficItem)
        -> Result<String, Error>
    {
        use EndpointState::*;
        use TrafficItem::*;
        use TrafficViewMode::*;
        if view_mode == Packets {
            return Ok(String::from(""));
        }
        let last_packet = match item {
            Packet(_, Some(transaction_id), packet_id) => {
                let range = self.transaction_index.target_range(
                    *transaction_id, self.packet_index.len())?;
                *packet_id == range.end - 1
            }, _ => false
        };
        if view_mode == Transactions {
            return Ok(String::from(match (item, last_packet) {
                (Transfer(_), _)     => unreachable!(),
                (Transaction(..), _) => "○",
                (Packet(..), false)  => "├──",
                (Packet(..), true )  => "└──",
            }));
        }
        let endpoint_count = self.endpoints.len() as usize;
        let max_string_length = endpoint_count + "    └──".len();
        let mut connectors = String::with_capacity(max_string_length);
        let transfer_id = match item {
            Transfer(i) | Transaction(Some(i), _) | Packet(Some(i), ..) => *i,
            _ => unreachable!()
        };
        let entry = self.transfer_index.get(transfer_id)?;
        let endpoint_id = entry.endpoint_id();
        let endpoint_state = self.endpoint_state(transfer_id)?;
        let extended = self.transfer_extended(endpoint_id, transfer_id)?;
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let last_transaction = match item {
            Transaction(_, transaction_id) |
            Packet(_, Some(transaction_id), _) => {
                let range = ep_traf.transfer_index.target_range(
                    entry.transfer_id(), ep_traf.transaction_ids.len())?;
                let last_transaction_id =
                    ep_traf.transaction_ids.get(range.end - 1)?;
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
                (Transfer(..), Starting | Ending, _) => true,
                (Transaction(..) | Packet(..), _, true) => on_endpoint,
                _ => false,
            };
            connectors.push(match item {
                Transfer(..) => {
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
                Transfer(..)    => '─',
                Transaction(..) => '─',
                Packet(..)      => ' ',
            });
        }
        connectors.push_str(
            match (item, last_packet) {
                (Transfer(_), _) if entry.is_start() => "─",
                (Transfer(_), _)                     => "──□ ",
                (Transaction(..), _)                 => "───",
                (Packet(..), false)                  => "    ├──",
                (Packet(..), true)                   => "    └──",
            }
        );
        Ok(connectors)
    }

    fn timestamp(&mut self, item: &TrafficItem)
        -> Result<Timestamp, Error>
    {
        use TrafficItem::*;
        let packet_id = match item {
            Transfer(transfer_id) => {
                let entry = self.transfer_index.get(*transfer_id)?;
                let ep_traf = self.endpoint_traffic(entry.endpoint_id())?;
                let ep_transaction_id =
                    ep_traf.transfer_index.get(entry.transfer_id())?;
                let transaction_id =
                    ep_traf.transaction_ids.get(ep_transaction_id)?;
                self.transaction_index.get(transaction_id)?
            },
            Transaction(.., transaction_id) =>
                self.transaction_index.get(*transaction_id)?,
            Packet(.., packet_id) => *packet_id,
        };
        self.packet_time(packet_id)
    }
}

impl ItemSource<DeviceItem, DeviceViewMode> for CaptureReader {
    fn item(&mut self,
            parent: Option<&DeviceItem>,
            _view_mode: DeviceViewMode,
            index: u64)
        -> Result<DeviceItem, Error>
    {
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

    fn item_update(&mut self, item: &DeviceItem)
        -> Result<Option<DeviceItem>, Error>
    {
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

    fn child_item(&mut self, parent: &DeviceItem, index: u64)
        -> Result<DeviceItem, Error>
    {
        use DeviceItemContent::*;
        let data = self.device_data(parent.device_id)?;
        let content = match parent.content {
            Device(desc_opt) => match index {
                0 => DeviceDescriptor(desc_opt),
                n => {
                    let conf = ConfigNum(n.try_into()?);
                    let config = data.configuration(conf)?;
                    Configuration(conf, config.descriptor)
                }
            },
            DeviceDescriptor(desc_opt) => match desc_opt {
                Some(desc) =>
                    DeviceDescriptorField(desc,
                        DeviceField(index.try_into()?)),
                None => bail!("Device descriptor fields not available")
            },
            Configuration(conf, desc) => {
                let config = data.configuration(conf)?;
                let other_count = config.other_descriptors.len() as u64;
                let func_count = config.functions.len() as u64;
                match index {
                    0 => ConfigurationDescriptor(desc),
                    n if n < 1 + other_count =>
                        OtherDescriptor(config
                            .other_descriptor(
                                ConfigOtherNum((n - 1).try_into()?))?
                            .clone()),
                    n if n < 1 + other_count + func_count =>
                        Function(conf, config.function(
                            ConfigFuncNum((n - 1 - other_count).try_into()?))?
                                .descriptor),
                    n => Interface(conf, config
                            .unassociated_interfaces()
                            .nth((n - 1 - other_count - func_count).try_into()?)
                            .context("Failed to find unassociated interface")?
                            .descriptor)
                }
            },
            ConfigurationDescriptor(desc) =>
                ConfigurationDescriptorField(desc,
                    ConfigField(index.try_into()?)),
            Function(conf, desc) => {
                let config = data.configuration(conf)?;
                match index.try_into()? {
                    0 => FunctionDescriptor(desc),
                    n => match config.associated_interfaces(&desc).nth(n - 1) {
                        Some(interface) =>
                            Interface(conf, interface.descriptor),
                        None => bail!(
                            "Function has no interface with index {n}")
                    }
                }
            },
            FunctionDescriptor(desc) =>
                FunctionDescriptorField(desc,
                    IfaceAssocField(index.try_into()?)),
            Interface(conf, if_desc) => {
                let config = data.configuration(conf)?;
                let interface = config.interface(&if_desc)?;
                let desc_count = interface.other_descriptors.len() as u64;
                match index {
                    0 => InterfaceDescriptor(if_desc),
                    n if n < 1 + desc_count => {
                        let num = IfaceOtherNum((n - 1).try_into()?);
                        let desc = interface.other_descriptor(num)?.clone();
                        OtherDescriptor(desc)
                    },
                    n => {
                        let num = InterfaceEpNum((n - 1 - desc_count).try_into()?);
                        let ep_desc = *interface.endpoint_descriptor(num)?;
                        Endpoint(conf, if_desc.key(), ep_desc)
                    }
                }
            },
            Endpoint(_conf, _key, desc) => EndpointDescriptor(desc),
            InterfaceDescriptor(desc) =>
                InterfaceDescriptorField(desc,
                    InterfaceField(index.try_into()?)),
            EndpointDescriptor(desc) =>
                EndpointDescriptorField(desc,
                    EndpointField(index.try_into()?)),
            _ => bail!("This device item type cannot have children")
        };
        Ok(DeviceItem {
            device_id: parent.device_id,
            version: data.version(),
            content,
            indent: parent.indent + 1,
        })
    }

    fn item_children(&mut self,
                     parent: Option<&DeviceItem>,
                     _view_mode: DeviceViewMode)
        -> Result<(CompletionStatus, u64), Error>
    {
        use DeviceItemContent::*;
        use CompletionStatus::*;
        let (completion, children) = match parent {
            None =>
                (self.completion(),
                 self.devices.len().saturating_sub(1) as usize),
            Some(item) => {
                let data = self.device_data(item.device_id)?;
                match item.content {
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
                    Configuration(conf, _) => {
                        let config = data.configuration(conf)?;
                        (Ongoing,
                         1 + config.other_descriptors.len()
                           + config.functions.len()
                           + config.unassociated_interfaces().count())
                    }
                    ConfigurationDescriptor(_) =>
                        (Ongoing, usb::ConfigDescriptor::NUM_FIELDS),
                    Function(conf, desc) => {
                        let config = data.configuration(conf)?;
                        let interfaces = config.associated_interfaces(&desc);
                        (Complete, 1 + interfaces.count())
                    }
                    FunctionDescriptor(_) =>
                        (Complete,
                         usb::InterfaceAssociationDescriptor::NUM_FIELDS),
                    Interface(conf, desc) => {
                        let config = data.configuration(conf)?;
                        let interface = config.interface(&desc)?;
                        (Ongoing,
                         1 + interface.endpoint_descriptors.len()
                           + interface.other_descriptors.len())
                    },
                    Endpoint(..) => (Complete, 1),
                    InterfaceDescriptor(_) =>
                        (Ongoing, usb::InterfaceDescriptor::NUM_FIELDS),
                    EndpointDescriptor(_) =>
                        (Complete, usb::EndpointDescriptor::NUM_FIELDS),

                    // Other types have no children.
                    _ => (Complete, 0),
                }
            }
        };
        Ok((completion, children as u64))
    }

    fn description(&mut self, item: &DeviceItem, _detail: bool)
        -> Result<String, Error>
    {
        use DeviceItemContent::*;
        let data = self.device_data(item.device_id)?;
        Ok(match &item.content {
            Device(_) => {
                let device = self.devices.get(item.device_id)?;
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
            Configuration(conf, _) => format!(
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
                    usb_ids::Class::from_id(desc.function_class)
                        .map_or("Unknown", |c| c.name())
                )
            },
            FunctionDescriptor(_) =>
                "Interface association descriptor".to_string(),
            FunctionDescriptorField(desc, field) => desc.field_text(*field),
            Interface(_conf, desc) => {
                let num = desc.interface_number;
                match desc.alternate_setting {
                    InterfaceAlt(0) => format!(
                        "Interface {num}"),
                    InterfaceAlt(alt) => format!(
                        "Interface {num} (alternate {alt})"),
                }
            },
            InterfaceDescriptor(_) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(desc, field) => {
                let strings = data.strings.load();
                desc.field_text(*field, strings.as_ref())
            },
            Endpoint(.., desc) => {
                let addr = desc.endpoint_address;
                let attrs = desc.attributes;
                format!("Endpoint {} {} ({})", addr.number(),
                   addr.direction(), attrs.endpoint_type())
            },
            EndpointDescriptor(_) =>
                "Endpoint descriptor".to_string(),
            EndpointDescriptorField(desc, field) => desc.field_text(*field),
            OtherDescriptor(desc) => desc.description(),
        })
    }

    fn connectors(&mut self, _view_mode: (), item: &DeviceItem)
        -> Result<String, Error>
    {
        Ok("   ".repeat(item.indent as usize))
    }

    fn timestamp(&mut self, _item: &DeviceItem)
        -> Result<Timestamp, Error>
    {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, BufWriter, BufRead, Write};
    use std::path::PathBuf;
    use crate::decoder::Decoder;
    use crate::pcap::Loader;

    fn summarize_item<Item, ViewMode>(
        cap: &mut CaptureReader,
        item: &Item,
        mode: ViewMode,
        depth: usize
    ) -> String
        where CaptureReader: ItemSource<Item, ViewMode>,
              ViewMode: Copy
    {
        let mut summary = cap.description(item, false).unwrap();
        let (_completion, num_children) =
            cap.item_children(Some(item), mode).unwrap();
        let child_ids = 0..num_children;
        for (n, child_summary) in child_ids
            .map(|child_id| {
                let child = cap.child_item(item, child_id).unwrap();
                summarize_item(cap, &child, mode, depth + 1)
            })
            .dedup_with_count()
        {
            summary += "\n";
            summary += &" ".repeat(depth + 1);
            if n > 1 {
                summary += &format!("{} times: {}", n, &child_summary);
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
        depth: usize,
        writer: &mut dyn Write
    )
        where CaptureReader: ItemSource<Item, ViewMode>,
              ViewMode: Copy
    {
        let summary = summarize_item(cap, item, mode, depth);
        for _ in 0..depth {
            writer.write(b" ").unwrap();
        }
        writer.write(summary.as_bytes()).unwrap();
        writer.write(b"\n").unwrap();
    }

    #[test]
    fn test_captures() {
        let test_dir = PathBuf::from("./tests/");
        let mut list_path = test_dir.clone();
        list_path.push("tests.txt");
        let list_file = File::open(list_path).unwrap();
        let mode = TrafficViewMode::Hierarchical;
        for test_name in BufReader::new(list_file).lines() {
            let mut test_path = test_dir.clone();
            test_path.push(test_name.unwrap());
            let mut cap_path = test_path.clone();
            let mut traf_ref_path = test_path.clone();
            let mut traf_out_path = test_path.clone();
            let mut dev_ref_path = test_path.clone();
            let mut dev_out_path = test_path.clone();
            cap_path.push("capture.pcap");
            traf_ref_path.push("reference.txt");
            traf_out_path.push("output.txt");
            dev_ref_path.push("devices-reference.txt");
            dev_out_path.push("devices-output.txt");
            {
                let file = File::open(cap_path).unwrap();
                let mut loader = Loader::open(file).unwrap();
                let (writer, mut reader) = create_capture().unwrap();
                let mut decoder = Decoder::new(writer).unwrap();
                while let Some(result) = loader.next() {
                    let (packet, timestamp_ns) = result.unwrap();
                    decoder
                        .handle_raw_packet(&packet.data, timestamp_ns)
                        .unwrap();
                }
                decoder.finish().unwrap();
                let traf_out_file = File::create(traf_out_path.clone()).unwrap();
                let mut traf_out_writer = BufWriter::new(traf_out_file);
                let num_items = reader.item_index.len();
                for item_id in 0 .. num_items {
                    let item = reader.item(None, mode, item_id).unwrap();
                    write_item(&mut reader, &item, mode, 0, &mut traf_out_writer);
                }
                let dev_out_file = File::create(dev_out_path.clone()).unwrap();
                let mut dev_out_writer = BufWriter::new(dev_out_file);
                let num_devices = reader.devices.len() - 1;
                for device_id in 0 .. num_devices {
                    let item = reader.item(None, (), device_id).unwrap();
                    write_item(&mut reader, &item, (), 0, &mut dev_out_writer);
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

pub mod prelude {
    #[allow(unused_imports)]
    pub use super::{
        create_capture,
        create_endpoint,
        CaptureReader,
        CaptureWriter,
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
        EndpointTransferId,
        PacketId,
        TrafficItemId,
        TransactionId,
        TransferId,
        TransferIndexEntry,
        INVALID_EP_NUM,
        FRAMING_EP_NUM,
        INVALID_EP_ID,
        FRAMING_EP_ID,
    };
}
