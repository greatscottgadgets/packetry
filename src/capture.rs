use std::cmp::min;
use std::fmt::Debug;
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
use crate::vec_map::VecMap;
use crate::usb::{self, prelude::*};
use crate::util::{fmt_count, fmt_size};

use anyhow::{Context, Error, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive};

// Use 2MB block size for packet data, which is a large page size on x86_64.
const PACKET_DATA_BLOCK_SIZE: usize = 0x200000;

/// Capture state shared between readers and writers.
pub struct CaptureShared {
    pub device_data: ArcSwap<VecMap<DeviceId, Arc<DeviceData>>>,
    pub endpoint_readers: ArcSwap<VecMap<EndpointId, Arc<EndpointReader>>>,
    pub complete: AtomicBool,
}

/// Unique handle for write access to a capture.
pub struct CaptureWriter {
    pub shared: Arc<CaptureShared>,
    pub packet_data: DataWriter<u8, PACKET_DATA_BLOCK_SIZE>,
    pub packet_index: CompactWriter<PacketId, PacketByteId, 2>,
    pub packet_times: CompactWriter<PacketId, Timestamp, 2>,
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

#[derive(Copy, Clone, Debug)]
pub enum TrafficItem {
    Transfer(TransferId),
    Transaction(TransferId, TransactionId),
    Packet(TransferId, TransactionId, PacketId),
}

#[derive(Copy, Clone, Debug)]
pub enum DeviceItem {
    Device(DeviceId, DeviceVersion),
    DeviceDescriptor(DeviceId),
    DeviceDescriptorField(DeviceId, DeviceField, DeviceVersion),
    Configuration(DeviceId, ConfigNum),
    ConfigurationDescriptor(DeviceId, ConfigNum),
    ConfigurationDescriptorField(DeviceId, ConfigNum,
                                 ConfigField, DeviceVersion),
    Interface(DeviceId, ConfigNum, InterfaceNum),
    InterfaceDescriptor(DeviceId, ConfigNum, InterfaceNum),
    InterfaceDescriptorField(DeviceId, ConfigNum,
                             InterfaceNum, InterfaceField, DeviceVersion),
    EndpointDescriptor(DeviceId, ConfigNum, InterfaceNum, InterfaceEpNum),
    EndpointDescriptorField(DeviceId, ConfigNum, InterfaceNum,
                            InterfaceEpNum, EndpointField, DeviceVersion),
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

#[derive(Copy, Clone, Debug)]
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

#[derive(Default)]
pub struct DeviceData {
    pub device_descriptor: ArcSwapOption<DeviceDescriptor>,
    pub configurations: ArcSwap<VecMap<ConfigNum, Arc<Configuration>>>,
    pub config_number: ArcSwapOption<ConfigNum>,
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

    pub fn configuration(&self, number: &ConfigNum)
        -> Result<Arc<Configuration>, Error>
    {
        match self.configurations.load().get(*number) {
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
                self.endpoint_details.update(|endpoint_details| {
                    for iface in &config.interfaces {
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
    pub fn interface(&self, number: &InterfaceNum)
        -> Result<&Interface, Error>
    {
        match self.interfaces.get(*number) {
            Some(iface) => Ok(iface),
            _ => bail!("Configuration has no interface {number}")
        }
    }
}

impl Interface {
    pub fn endpoint_descriptor(&self, number: &InterfaceEpNum)
        -> Result<&EndpointDescriptor, Error>
    {
        match self.endpoint_descriptors.get(*number) {
            Some(desc) => Ok(desc),
            _ => bail!("Interface has no endpoint descriptor {number}")
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

impl Transaction {
    fn packet_count(&self) -> u64 {
        self.packet_id_range.len()
    }

    fn payload_size(&self) -> Option<u64> {
        self.payload_byte_range.as_ref().map(|range| range.len())
    }

    fn successful(&self) -> bool {
        use PID::*;
        match (self.start_pid, self.end_pid) {

            // SPLIT is successful if it ends with DATA0/DATA1/ACK/NYET.
            (SPLIT, DATA0 | DATA1 | ACK | NYET) => true,

            // SETUP/IN/OUT is successful if it ends with ACK/NYET.
            (SETUP | IN | OUT, ACK | NYET) => true,

            (..) => false
        }
    }

    fn control_result(&self, direction: Direction) -> ControlResult {
        use ControlResult::*;
        use StartComplete::*;
        use Direction::*;
        use PID::*;
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
        } else if self.successful() {
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
                   endpoint: &Endpoint)
        -> Result<String, Error>
    {
        use PID::*;
        use StartComplete::*;
        Ok(match (self.start_pid, &self.split) {
            (SOF, _) => format!(
                "{} SOF packets", self.packet_count()),
            (Malformed, _) => format!(
                "{} malformed packets", self.packet_count()),
            (SPLIT, Some((split_fields, token_pid))) => format!(
                "{} {}",
                match split_fields.sc() {
                    Start => "Starting",
                    Complete => "Completing",
                },
                self.inner_description(capture, endpoint, *token_pid)?
            ),
            (pid, _) => self.inner_description(capture, endpoint, pid)?
        })
    }

    fn inner_description(&self,
                         capture: &mut CaptureReader,
                         endpoint: &Endpoint,
                         pid: PID)
        -> Result<String, Error>
    {
        Ok(format!(
            "{} transaction on {}.{}{}",
            pid,
            endpoint.device_address(),
            endpoint.number(),
            match (self.payload_size(), self.outcome()) {
                (None, None) =>
                    String::from(""),
                (None, Some(outcome)) =>
                    format!(", {outcome}"),
                (Some(0), None) =>
                    String::from(" with no data"),
                (Some(0), Some(outcome)) =>
                    format!(" with no data, {outcome}"),
                (Some(size), None) => format!(
                    " with {size} data bytes: {}",
                    Bytes::first(100, &capture.transaction_bytes(self)?)),
                (Some(size), Some(outcome)) => format!(
                    " with {size} data bytes, {outcome}: {}",
                    Bytes::first(100, &capture.transaction_bytes(self)?)),
            }
        ))
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
                        let pid = PID::from(*byte);
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

    pub fn device_data(&self, id: &DeviceId)
        -> Result<Arc<DeviceData>, Error>
    {
        Ok(self.shared.device_data
            .load()
            .get(*id)
            .with_context(|| format!("Capture has no device with ID {id}"))?
            .clone())
    }

    fn device_version(&self, id: &DeviceId) -> Result<u32, Error> {
        Ok(self.device_data(id)?.version())
    }

    pub fn try_configuration(&self, dev: &DeviceId, conf: &ConfigNum)
        -> Option<Arc<Configuration>>
    {
        self.device_data(dev)
            .ok()?
            .configurations
            .load()
            .get(*conf)
            .cloned()
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

pub trait ItemSource<Item> {
    fn item(&mut self, parent: Option<&Item>, index: u64)
        -> Result<Item, Error>;
    fn item_update(&mut self, item: &Item)
        -> Result<Option<Item>, Error>;
    fn child_item(&mut self, parent: &Item, index: u64)
        -> Result<Item, Error>;
    fn item_children(&mut self, parent: Option<&Item>)
        -> Result<(CompletionStatus, u64), Error>;
    fn summary(&mut self, item: &Item) -> Result<String, Error>;
    fn connectors(&mut self, item: &Item) -> Result<String, Error>;
    fn timestamp(&mut self, item: &Item) -> Result<Timestamp, Error>;
}

impl ItemSource<TrafficItem> for CaptureReader {
    fn item(&mut self, parent: Option<&TrafficItem>, index: u64)
        -> Result<TrafficItem, Error>
    {
        match parent {
            None => {
                let item_id = TrafficItemId::from(index);
                let transfer_id = self.item_index.get(item_id)?;
                Ok(TrafficItem::Transfer(transfer_id))
            },
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
                Transaction(*transfer_id, {
                    let entry = self.transfer_index.get(*transfer_id)?;
                    let endpoint_id = entry.endpoint_id();
                    let ep_transfer_id = entry.transfer_id();
                    let ep_traf = self.endpoint_traffic(endpoint_id)?;
                    let offset = ep_traf.transfer_index.get(ep_transfer_id)?;
                    ep_traf.transaction_ids.get(offset + index)?
                }),
            Transaction(transfer_id, transaction_id) =>
                Packet(*transfer_id, *transaction_id, {
                    self.transaction_index.get(*transaction_id)? + index}),
            Packet(..) => bail!("Packets have no child items")
        })
    }

    fn item_children(&mut self, parent: Option<&TrafficItem>)
        -> Result<(CompletionStatus, u64), Error>
    {
        use TrafficItem::*;
        use CompletionStatus::*;
        Ok(match parent {
            None => {
                (self.completion(), self.item_index.len())
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

    fn summary(&mut self, item: &TrafficItem)
        -> Result<String, Error>
    {
        use TrafficItem::*;
        use usb::StartComplete::*;
        Ok(match item {
            Packet(.., packet_id) => {
                let packet = self.packet(*packet_id)?;
                let first_byte = *packet.first().with_context(|| format!(
                    "Packet {packet_id} is empty, cannot retrieve PID"))?;
                let pid = PID::from(first_byte);
                format!("{pid} packet{}",
                    match PacketFields::from_packet(&packet) {
                        PacketFields::SOF(sof) => format!(
                            " with frame number {}, CRC {:02X}",
                            sof.frame_number(),
                            sof.crc()),
                        PacketFields::Token(token) => format!(
                            " on {}.{}, CRC {:02X}",
                            token.device_address(),
                            token.endpoint_number(),
                            token.crc()),
                        PacketFields::Data(data) if packet.len() <= 3 => format!(
                            " with CRC {:04X} and no data",
                            data.crc),
                        PacketFields::Data(data) => format!(
                            " with CRC {:04X} and {} data bytes: {}",
                            data.crc,
                            packet.len() - 3,
                            Bytes::first(100, &packet[1 .. packet.len() - 2])),
                        PacketFields::Split(split) => format!(
                            " {} {} speed {} transaction on hub {} port {}",
                            match split.sc() {
                                Start => "starting",
                                Complete => "completing",
                            },
                            format!("{:?}", split.speed()).to_lowercase(),
                            format!("{:?}", split.endpoint_type()).to_lowercase(),
                            split.hub_address(),
                            split.port()),
                        PacketFields::None => match pid {
                            PID::Malformed => format!(": {packet:02X?}"),
                            _ => "".to_string()
                        }
                    })
            },
            Transaction(transfer_id, transaction_id) => {
                let entry = self.transfer_index.get(*transfer_id)?;
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id)?;
                let transaction = self.transaction(*transaction_id)?;
                transaction.description(self, &endpoint)?
            },
            Transfer(transfer_id) => {
                use EndpointType::*;
                use usb::EndpointType::*;
                let entry = self.transfer_index.get(*transfer_id)?;
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id)?;
                let device_id = endpoint.device_id();
                let dev_data = self.device_data(&device_id)?;
                let ep_addr = endpoint.address();
                let (ep_type, _) = dev_data.endpoint_details(ep_addr);
                let range = self.transfer_range(&entry)?;
                let count = range.len();
                match (ep_type, entry.is_start()) {
                    (Invalid, true) => format!(
                        "{count} invalid groups"),
                    (Invalid, false) =>
                        "End of invalid groups".to_string(),
                    (Framing, true) => format!(
                        "{count} SOF groups"),
                    (Framing, false) =>
                        "End of SOF groups".to_string(),
                    (Normal(Control), true) => {
                        let addr = endpoint.device_address();
                        match self.control_transfer(addr, endpoint_id, range) {
                            Ok(transfer) => transfer.summary(),
                            Err(_) => format!(
                                "Incomplete control transfer on device {addr}")
                        }
                    },
                    (Normal(Control), false) => {
                        let addr = endpoint.device_address();
                        format!("End of control transfer on device {addr}")
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
                        match (first_transaction.successful(), starting) {
                            (true, true) => {
                                let ep_traf =
                                    self.endpoint_traffic(endpoint_id)?;
                                let data_range =
                                    ep_traf.transfer_data_range(&range)?;
                                let length =
                                    ep_traf.transfer_data_length(&data_range)?;
                                let length_string = fmt_size(length);
                                let display_length = min(length, 100) as usize;
                                let transfer_bytes = self.transfer_bytes(
                                    endpoint_id, &data_range, display_length)?;
                                let display_bytes = Bytes {
                                    partial: length > display_length as u64,
                                    bytes: &transfer_bytes,
                                };
                                format!(
                                    "{ep_type_string} transfer of {length_string} on endpoint {endpoint}: {display_bytes}")
                            },
                            (true, false) => format!(
                                "End of {ep_type_lower} transfer on endpoint {endpoint}"),
                            (false, true) => format!(
                                "Polling {count} times for {ep_type_lower} transfer on endpoint {endpoint}"),
                            (false, false) => format!(
                                "End polling for {ep_type_lower} transfer on endpoint {endpoint}"),
                        }
                    }
                }
            }
        })
    }

    fn connectors(&mut self, item: &TrafficItem)
        -> Result<String, Error>
    {
        use EndpointState::*;
        use TrafficItem::*;
        let endpoint_count = self.endpoints.len() as usize;
        let max_string_length = endpoint_count + "    └──".len();
        let mut connectors = String::with_capacity(max_string_length);
        let transfer_id = match item {
            Transfer(i) | Transaction(i, _) | Packet(i, ..) => *i
        };
        let entry = self.transfer_index.get(transfer_id)?;
        let endpoint_id = entry.endpoint_id();
        let endpoint_state = self.endpoint_state(transfer_id)?;
        let extended = self.transfer_extended(endpoint_id, transfer_id)?;
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let last_transaction = match item {
            Transaction(_, transaction_id) | Packet(_, transaction_id, _) => {
                let range = ep_traf.transfer_index.target_range(
                    entry.transfer_id(), ep_traf.transaction_ids.len())?;
                let last_transaction_id =
                    ep_traf.transaction_ids.get(range.end - 1)?;
                *transaction_id == last_transaction_id
            }, _ => false
        };
        let last_packet = match item {
            Packet(_, transaction_id, packet_id) => {
                let range = self.transaction_index.target_range(
                    *transaction_id, self.packet_index.len())?;
                *packet_id == range.end - 1
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

impl ItemSource<DeviceItem> for CaptureReader {
    fn item(&mut self, parent: Option<&DeviceItem>, index: u64)
        -> Result<DeviceItem, Error>
    {
        match parent {
            None => {
                let device_id = DeviceId::from(index + 1);
                let data = self.device_data(&device_id)?;
                Ok(DeviceItem::Device(device_id, data.version()))
            },
            Some(item) => self.child_item(item, index)
        }
    }

    fn item_update(&mut self, item: &DeviceItem)
        -> Result<Option<DeviceItem>, Error>
    {
        use DeviceItem::*;
        Ok(match item {
            Device(dev, version) |
            DeviceDescriptorField(dev, .., version) |
            ConfigurationDescriptorField(dev, .., version) |
            InterfaceDescriptorField(dev, .., version) |
            EndpointDescriptorField(dev, .., version) => {
                let new = self.device_version(dev)?;
                if *version != new {
                    Some(match *item {
                        Device(dev, _) =>
                            Device(dev, new),
                        DeviceDescriptorField(dev, field, _) =>
                            DeviceDescriptorField(dev, field, new),
                        ConfigurationDescriptorField(dev, conf, field, _) =>
                            ConfigurationDescriptorField(dev, conf, field, new),
                        InterfaceDescriptorField(dev, conf, iface, field, _) =>
                            InterfaceDescriptorField(dev, conf, iface, field, new),
                        EndpointDescriptorField(dev, conf, iface, ep, field, _) =>
                            EndpointDescriptorField(dev, conf, iface, ep, field, new),
                        _ => unreachable!()
                    })
                } else {
                    None
                }
            },
            _ => None
        })
    }

    fn child_item(&mut self, parent: &DeviceItem, index: u64)
        -> Result<DeviceItem, Error>
    {
        use DeviceItem::*;
        Ok(match parent {
            Device(dev, _version) => match index {
                0 => DeviceDescriptor(*dev),
                conf => Configuration(*dev,
                    ConfigNum(conf.try_into()?)),
            },
            DeviceDescriptor(dev) =>
                DeviceDescriptorField(*dev,
                    DeviceField(index.try_into()?),
                    self.device_version(dev)?),
            Configuration(dev, conf) => match index {
                0 => ConfigurationDescriptor(*dev, *conf),
                n => Interface(*dev, *conf,
                    InterfaceNum((n - 1).try_into()?)),
            },
            ConfigurationDescriptor(dev, conf) =>
                ConfigurationDescriptorField(*dev, *conf,
                    ConfigField(index.try_into()?),
                    self.device_version(dev)?),
            Interface(dev, conf, iface) => match index {
                0 => InterfaceDescriptor(*dev, *conf, *iface),
                n => EndpointDescriptor(*dev, *conf, *iface,
                    InterfaceEpNum((n - 1).try_into()?))
            },
            InterfaceDescriptor(dev, conf, iface) =>
                InterfaceDescriptorField(*dev, *conf, *iface,
                    InterfaceField(index.try_into()?),
                    self.device_version(dev)?),
            EndpointDescriptor(dev, conf, iface, ep) =>
                EndpointDescriptorField(*dev, *conf, *iface, *ep,
                    EndpointField(index.try_into()?),
                    self.device_version(dev)?),
            _ => bail!("This device item type cannot have children")
        })
    }

    fn item_children(&mut self, parent: Option<&DeviceItem>)
        -> Result<(CompletionStatus, u64), Error>
    {
        use DeviceItem::*;
        use CompletionStatus::*;
        let (completion, children) = match parent {
            None =>
                (self.completion(),
                 self.devices.len().saturating_sub(1) as usize),
            Some(Device(dev, _version)) =>
                (Ongoing, {
                    let configs = &self.device_data(dev)?.configurations;
                    let count = configs.load().len();
                    if count == 0 { 1 } else { count }
                }),
            Some(DeviceDescriptor(dev)) =>
                match self.device_data(dev)?.device_descriptor.load().as_ref() {
                    Some(_) => (Ongoing, usb::DeviceDescriptor::NUM_FIELDS),
                    None => (Ongoing, 0),
                },
            Some(Configuration(dev, conf)) =>
                match self.try_configuration(dev, conf) {
                    Some(conf) => (Ongoing, 1 + conf.interfaces.len()),
                    None => (Ongoing, 0)
                },
            Some(ConfigurationDescriptor(dev, conf)) =>
                match self.try_configuration(dev, conf) {
                    Some(_) => (Ongoing, usb::ConfigDescriptor::NUM_FIELDS),
                    None => (Ongoing, 0)
                },
            Some(Interface(dev, conf, iface)) =>
                match self.try_configuration(dev, conf) {
                    Some(conf) =>
                        (Ongoing,
                         1 + conf.interface(iface)?.endpoint_descriptors.len()),
                    None => (Ongoing, 0)
                },
            Some(InterfaceDescriptor(..)) =>
                (Ongoing, usb::InterfaceDescriptor::NUM_FIELDS),
            Some(EndpointDescriptor(..)) =>
                (Complete, usb::EndpointDescriptor::NUM_FIELDS),
            _ => (Ongoing, 0)
        };
        Ok((completion, children as u64))
    }

    fn summary(&mut self, item: &DeviceItem)
        -> Result<String, Error>
    {
        use DeviceItem::*;
        Ok(match item {
            Device(dev, _version) => {
                let device = self.devices.get(*dev)?;
                let data = self.device_data(dev)?;
                format!("Device {}: {}", device.address, data.description())
            },
            DeviceDescriptor(dev) => {
                match self.device_data(dev)?.device_descriptor.load().as_ref() {
                    Some(_) => "Device descriptor",
                    None => "No device descriptor"
                }.to_string()
            },
            DeviceDescriptorField(dev, field, _ver) => {
                let data = self.device_data(dev)?;
                let device_descriptor = data.device_descriptor.load();
                match device_descriptor.as_ref() {
                    Some(descriptor) => {
                        let strings = data.strings.load();
                        descriptor.field_text(*field, strings.as_ref())
                    },
                    None => bail!("Device descriptor missing")
                }
            },
            Configuration(_, conf) => format!(
                "Configuration {conf}"),
            ConfigurationDescriptor(..) =>
                "Configuration descriptor".to_string(),
            ConfigurationDescriptorField(dev, conf, field, _ver) => {
                let data = self.device_data(dev)?;
                let config_descriptor = data.configuration(conf)?.descriptor;
                let strings = data.strings.load();
                config_descriptor.field_text(*field, strings.as_ref())
            },
            Interface(_, _, iface) => format!(
                "Interface {iface}"),
            InterfaceDescriptor(..) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(dev, conf, iface, field, _ver) => {
                let data = self.device_data(dev)?;
                let config = data.configuration(conf)?;
                let interface = config.interface(iface)?;
                let strings = data.strings.load();
                interface.descriptor.field_text(*field, strings.as_ref())
            },
            EndpointDescriptor(dev, conf, iface, ep) => {
                let addr = self.device_data(dev)?
                               .configuration(conf)?
                               .interface(iface)?
                               .endpoint_descriptor(ep)?
                               .endpoint_address;
                format!("Endpoint {} {}", addr.number(), addr.direction())
            },
            EndpointDescriptorField(dev, conf, iface, ep, field, _ver) => {
                self.device_data(dev)?
                    .configuration(conf)?
                    .interface(iface)?
                    .endpoint_descriptor(ep)?
                    .field_text(*field)
            }
        })
    }

    fn connectors(&mut self, item: &DeviceItem) -> Result<String, Error> {
        use DeviceItem::*;
        let depth = match item {
            Device(..) => 0,
            DeviceDescriptor(..) => 1,
            DeviceDescriptorField(..) => 2,
            Configuration(..) => 1,
            ConfigurationDescriptor(..) => 2,
            ConfigurationDescriptorField(..) => 3,
            Interface(..) => 2,
            InterfaceDescriptor(..) => 3,
            InterfaceDescriptorField(..) => 4,
            EndpointDescriptor(..) => 3,
            EndpointDescriptorField(..) => 4,
        };
        Ok("   ".repeat(depth))
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
    use itertools::Itertools;

    fn summarize_item(cap: &mut CaptureReader, item: &TrafficItem, depth: usize)
        -> String
    {
        let mut summary = cap.summary(item).unwrap();
        let (_completion, num_children) =
            cap.item_children(Some(item)).unwrap();
        let child_ids = 0..num_children;
        for (n, child_summary) in child_ids
            .map(|child_id| {
                let child = cap.child_item(item, child_id).unwrap();
                summarize_item(cap, &child, depth + 1)
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

    fn write_item(cap: &mut CaptureReader, item: &TrafficItem, depth: usize,
                  writer: &mut dyn Write)
    {
        let summary = summarize_item(cap, item, depth);
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
        for test_name in BufReader::new(list_file).lines() {
            let mut test_path = test_dir.clone();
            test_path.push(test_name.unwrap());
            let mut cap_path = test_path.clone();
            let mut ref_path = test_path.clone();
            let mut out_path = test_path.clone();
            cap_path.push("capture.pcap");
            ref_path.push("reference.txt");
            out_path.push("output.txt");
            {
                let mut loader = Loader::open(cap_path).unwrap();
                let (writer, mut reader) = create_capture().unwrap();
                let mut decoder = Decoder::new(writer).unwrap();
                while let Some(result) = loader.next() {
                    let (packet, timestamp_ns) = result.unwrap();
                    decoder
                        .handle_raw_packet(&packet.data, timestamp_ns)
                        .unwrap();
                }
                decoder.finish().unwrap();
                let out_file = File::create(out_path.clone()).unwrap();
                let mut out_writer = BufWriter::new(out_file);
                let num_items = reader.item_index.len();
                for item_id in 0 .. num_items {
                    let item = reader.item(None, item_id).unwrap();
                    write_item(&mut reader, &item, 0, &mut out_writer);
                }
            }
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
