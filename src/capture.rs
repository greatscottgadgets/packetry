use std::ops::Range;
use std::num::TryFromIntError;
use std::mem::size_of;

use crate::id::{Id, HasLength};
use crate::file_vec::{FileVec, FileVecError};
use crate::hybrid_index::{HybridIndex, HybridIndexError, Number};
use crate::vec_map::VecMap;
use crate::usb::{self, prelude::*};

use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive};
use num_format::{Locale, ToFormattedString};
use humansize::{FileSize, file_size_opts as options};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error(transparent)]
    FileVecError(#[from] FileVecError),
    #[error(transparent)]
    HybridIndexError(#[from] HybridIndexError),
    #[error(transparent)]
    RangeError(#[from] TryFromIntError),
    #[error("Descriptor missing")]
    DescriptorMissing,
    #[error("Indexing error: {0}")]
    IndexError(String),
}

use CaptureError::{DescriptorMissing, IndexError};

pub type PacketByteId = Id<u8>;
pub type PacketId = Id<PacketByteId>;
pub type TransactionId = Id<PacketId>;
pub type TransferId = Id<TransferIndexEntry>;
pub type EndpointTransactionId = Id<TransactionId>;
pub type EndpointTransferId = Id<EndpointTransactionId>;
pub type TrafficItemId = Id<TransferId>;
pub type DeviceId = Id<Device>;
pub type EndpointId = Id<Endpoint>;
pub type EndpointByteCount = u64;
pub type DeviceVersion = u32;

#[derive(Copy, Clone)]
pub enum TrafficItem {
    Transfer(TransferId),
    Transaction(TransferId, TransactionId),
    Packet(TransferId, TransactionId, PacketId),
}

#[derive(Copy, Clone)]
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

pub struct EndpointTraffic {
    pub transaction_ids: HybridIndex<EndpointTransactionId, TransactionId>,
    pub transfer_index: HybridIndex<EndpointTransferId, EndpointTransactionId>,
    pub data_index: HybridIndex<EndpointTransactionId, EndpointByteCount>,
    pub total_data: EndpointByteCount,
    pub end_index: HybridIndex<EndpointTransferId, TrafficItemId>,
}

impl EndpointTraffic {
    pub fn new() -> Result<Self, CaptureError> {
        Ok(EndpointTraffic {
            transaction_ids: HybridIndex::new(1)?,
            transfer_index: HybridIndex::new(1)?,
            data_index: HybridIndex::new(1)?,
            total_data: 0,
            end_index: HybridIndex::new(1)?,
        })
    }
}

#[derive(Default)]
pub struct DeviceData {
    pub device_descriptor: Option<DeviceDescriptor>,
    pub configurations: VecMap<ConfigNum, Configuration>,
    pub config_number: Option<ConfigNum>,
    pub endpoint_details:
        VecMap<EndpointAddr, (usb::EndpointType, Option<usize>)>,
    pub strings: VecMap<StringId, UTF16ByteVec>,
    pub version: DeviceVersion,
}

impl DeviceData {
    fn description(&self) -> String {
        match self.device_descriptor {
            None => "Unknown".to_string(),
            Some(descriptor) => {
                let str_id = descriptor.product_str_id;
                if let Some(utf16) = self.strings.get(str_id) {
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
        -> Result<&Configuration, CaptureError>
    {
        match self.configurations.get(*number) {
            Some(config) => Ok(config),
            None => Err(DescriptorMissing)
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
                self.device_descriptor.map(|desc| {
                    desc.max_packet_size_0 as usize
                })
            ),
            _ => match self.endpoint_details.get(addr) {
                Some((ep_type, ep_max)) => (Normal(*ep_type), *ep_max),
                None => (Unidentified, None)
            }
        }
    }

    pub fn update_endpoint_details(&mut self) {
        if let Some(number) = self.config_number {
            if let Some(config) = &self.configurations.get(number) {
                for iface in &config.interfaces {
                    for ep_desc in &iface.endpoint_descriptors {
                        let ep_addr = ep_desc.endpoint_address;
                        let ep_type = ep_desc.attributes.endpoint_type();
                        let ep_max = ep_desc.max_packet_size as usize;
                        self.endpoint_details.set(ep_addr,
                            (ep_type, Some(ep_max))
                        );
                    }
                }
            }
        }
    }

    pub fn set_endpoint_type(&mut self,
                             addr: EndpointAddr,
                             ep_type: usb::EndpointType)
    {
        if self.endpoint_details.get(addr).is_none() {
            self.endpoint_details.set(addr, (ep_type, None));
        }
    }

    pub fn decode_request(&mut self, fields: &SetupFields, payload: &[u8])
        -> Result<(), CaptureError>
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

    pub fn decode_descriptor_read(&mut self,
                                  fields: &SetupFields,
                                  payload: &[u8])
        -> Result<(), CaptureError>
    {
        let recipient = fields.type_fields.recipient();
        let desc_type = DescriptorType::from((fields.value >> 8) as u8);
        let length = payload.len();
        match (recipient, desc_type) {
            (Recipient::Device, DescriptorType::Device) => {
                if length == size_of::<DeviceDescriptor>() {
                    let descriptor = DeviceDescriptor::from_bytes(payload);
                    self.device_descriptor = Some(descriptor);
                    self.version += 1;
                }
            },
            (Recipient::Device, DescriptorType::Configuration) => {
                let size = size_of::<ConfigDescriptor>();
                if length >= size {
                    let configuration = Configuration::from_bytes(payload);
                    if let Some(config) = configuration {
                        let configurations = &mut self.configurations;
                        let config_num = ConfigNum::from(
                            config.descriptor.config_value);
                        configurations.set(config_num, config);
                        self.update_endpoint_details();
                        self.version += 1;
                    }
                }
            },
            (Recipient::Device, DescriptorType::String) => {
                if length >= 2 {
                    let string = UTF16ByteVec(payload[2..length].to_vec());
                    let string_id =
                        StringId::from((fields.value & 0xFF) as u8);
                    self.strings.set(string_id, string);
                    self.version += 1;
                }
            },
            _ => {}
        };
        Ok(())
    }

    fn decode_configuration_set(&mut self, fields: &SetupFields)
        -> Result<(), CaptureError>
    {
        self.config_number = Some(ConfigNum(fields.value.try_into()?));
        self.update_endpoint_details();
        self.version += 1;
        Ok(())
    }
}

impl Configuration {
    pub fn interface(&self, number: &InterfaceNum)
        -> Result<&Interface, CaptureError>
    {
        match self.interfaces.get(*number) {
            Some(iface) => Ok(iface),
            _ => Err(IndexError(format!(
                "Configuration has no interface {number}")))
        }
    }
}

impl Interface {
    pub fn endpoint_descriptor(&self, number: &InterfaceEpNum)
        -> Result<&EndpointDescriptor, CaptureError>
    {
        match self.endpoint_descriptors.get(*number) {
            Some(desc) => Ok(desc),
            _ => Err(IndexError(format!(
                "Interface has no endpoint descriptor {number}")))
        }
    }
}

pub struct Transaction {
    start_pid: PID,
    end_pid: PID,
    split: Option<(SplitFields, PID)>,
    packet_id_range: Range<PacketId>,
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
}

pub fn fmt_count(count: u64) -> String {
    count.to_formatted_string(&Locale::en)
}

pub fn fmt_size(size: u64) -> String {
    if size == 1 {
        "1 byte".to_string()
    } else if size < 1024 {
        format!("{size} bytes")
    } else {
        match size.file_size(options::BINARY) {
            Ok(string) => string,
            Err(e) => format!("<Error: {e}>")
        }
    }
}

pub fn fmt_vec<T>(vec: &FileVec<T>) -> String
    where T: bytemuck::Pod + Default
{
    format!("{} entries, {}", fmt_count(vec.len()), fmt_size(vec.size()))
}

pub fn fmt_index<I, T>(idx: &HybridIndex<I, T>) -> String
    where I: Number, T: Number + Copy
{
    format!("{} values in {} entries, {}",
            fmt_count(idx.len()),
            fmt_count(idx.entry_count()),
            fmt_size(idx.size()))
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

pub struct CompletedTransactions {
    transaction_ids: Vec<TransactionId>,
    index: usize,
}

impl CompletedTransactions {
    fn next(&mut self, capture: &mut Capture) -> Option<Transaction> {
        while self.index < self.transaction_ids.len() {
            let transaction_id = self.transaction_ids[self.index];
            let transaction = capture.transaction(transaction_id).ok()?;
            self.index += 1;
            if transaction.successful() {
                return Some(transaction);
            }
        }
        None
    }
}

pub struct Capture {
    pub packet_data: FileVec<u8>,
    pub packet_index: HybridIndex<PacketId, PacketByteId>,
    pub transaction_index: HybridIndex<TransactionId, PacketId>,
    pub transfer_index: FileVec<TransferIndexEntry>,
    pub item_index: HybridIndex<TrafficItemId, TransferId>,
    pub devices: FileVec<Device>,
    pub device_data: VecMap<DeviceId, DeviceData>,
    pub endpoints: FileVec<Endpoint>,
    pub endpoint_traffic: VecMap<EndpointId, EndpointTraffic>,
    pub endpoint_states: FileVec<u8>,
    pub endpoint_state_index: HybridIndex<TransferId, Id<u8>>,
    pub end_index: HybridIndex<TransferId, TrafficItemId>,
    pub completion: CompletionStatus,
}

impl Capture {
    pub fn new() -> Result<Self, CaptureError> {
        let mut capture = Capture {
            packet_data: FileVec::new()?,
            packet_index: HybridIndex::new(2)?,
            transaction_index: HybridIndex::new(1)?,
            transfer_index: FileVec::new()?,
            item_index: HybridIndex::new(1)?,
            devices: FileVec::new()?,
            device_data: VecMap::new(),
            endpoints: FileVec::new()?,
            endpoint_traffic: VecMap::new(),
            endpoint_states: FileVec::new()?,
            endpoint_state_index: HybridIndex::new(1)?,
            end_index: HybridIndex::new(1)?,
            completion: CompletionStatus::Ongoing,
        };
        let default_addr = DeviceAddr(0);
        let default_device = Device { address: default_addr };
        let default_id = capture.devices.push(&default_device)?;
        capture.device_data.set(default_id, DeviceData::default());
        for number in [INVALID_EP_NUM, FRAMING_EP_NUM] {
            let mut endpoint = Endpoint::default();
            endpoint.set_device_id(default_id);
            endpoint.set_device_address(default_addr);
            endpoint.set_number(number);
            endpoint.set_direction(Direction::Out);
            let endpoint_id = capture.endpoints.push(&endpoint)?;
            capture.endpoint_traffic.set(endpoint_id, EndpointTraffic::new()?);
        }
        Ok(capture)
    }

    pub fn print_storage_summary(&self) {
        let mut overhead: u64 =
            self.packet_index.size() +
            self.transaction_index.size() +
            self.transfer_index.size() +
            self.endpoint_states.size() +
            self.endpoint_state_index.size();
        let mut trx_count = 0;
        let mut trx_entries = 0;
        let mut trx_size = 0;
        let mut xfr_count = 0;
        let mut xfr_entries = 0;
        let mut xfr_size = 0;
        for ep_traf in &self.endpoint_traffic {
            trx_count += ep_traf.transaction_ids.len();
            trx_entries += ep_traf.transaction_ids.entry_count();
            trx_size += ep_traf.transaction_ids.size();
            xfr_count += ep_traf.transfer_index.len();
            xfr_entries += ep_traf.transfer_index.entry_count();
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
            "  Endpoint transaction indices: {} values in {} entries, {}\n",
            "  Endpoint transfer indices: {} values in {} entries, {}\n",
            "Total overhead: {:.1}% ({})\n"),
            fmt_size(self.packet_data.size()),
            fmt_index(&self.packet_index),
            fmt_index(&self.transaction_index),
            fmt_vec(&self.transfer_index),
            fmt_vec(&self.endpoint_states),
            fmt_index(&self.endpoint_state_index),
            fmt_count(trx_count), fmt_count(trx_entries), fmt_size(trx_size),
            fmt_count(xfr_count), fmt_count(xfr_entries), fmt_size(xfr_size),
            percentage, fmt_size(overhead),
        )
    }

    pub fn endpoint_traffic(&mut self, endpoint_id: EndpointId)
        -> Result<&mut EndpointTraffic, CaptureError>
    {
        self.endpoint_traffic.get_mut(endpoint_id).ok_or_else(||
            IndexError(format!("Capture has no endpoint ID {endpoint_id}")))
    }

    fn transfer_range(&mut self, entry: &TransferIndexEntry)
        -> Result<Range<EndpointTransactionId>, CaptureError>
    {
        let endpoint_id = entry.endpoint_id();
        let ep_transfer_id = entry.transfer_id();
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        Ok(ep_traf.transfer_index.target_range(
            ep_transfer_id, ep_traf.transaction_ids.len())?)
    }

    fn transfer_byte_range(&mut self,
                           endpoint_id: EndpointId,
                           range: &Range<EndpointTransactionId>)
        -> Result<Range<u64>, CaptureError>
    {
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let index = &mut ep_traf.data_index;
        let start = index.get(range.start)?;
        let end = if range.end.value >= index.len() {
            ep_traf.total_data
        } else {
            index.get(range.end)?
        };
        Ok(start .. end)
    }

    fn transaction_bytes(&mut self, transaction: &Transaction)
        -> Result<Vec<u8>, CaptureError>
    {
        let data_packet_id = transaction.data_packet_id
            .ok_or_else(||IndexError(String::from(
                "Transaction has no data packet")))?;
        let packet_byte_range = self.packet_index.target_range(
            data_packet_id, self.packet_data.len())?;
        let data_byte_range =
            packet_byte_range.start + 1 .. packet_byte_range.end - 2;
        Ok(self.packet_data.get_range(data_byte_range)?)
    }

    fn transfer_bytes(&mut self,
                      endpoint_id: EndpointId,
                      transaction_range: &Range<EndpointTransactionId>,
                      max_length: usize)
        -> Result<Vec<u8>, CaptureError>
    {
        let transaction_ids = self.endpoint_traffic(endpoint_id)?
                                  .transaction_ids
                                  .get_range(transaction_range)?;
        let mut transactions = self.completed_transactions(transaction_ids);
        let mut result = Vec::new();
        while let Some(transaction) = transactions.next(self) {
            let data = self.transaction_bytes(&transaction)?;
            result.extend_from_slice(&data);
            if result.len() >= max_length {
                result.truncate(max_length);
                break
            }
        }
        Ok(result)
    }

    fn endpoint_state(&mut self, transfer_id: TransferId)
        -> Result<Vec<u8>, CaptureError>
    {
        let range = self.endpoint_state_index.target_range(
            transfer_id, self.endpoint_states.len())?;
        Ok(self.endpoint_states.get_range(range)?)
    }

    pub fn packet(&mut self, id: PacketId)
        -> Result<Vec<u8>, CaptureError>
    {
        let range = self.packet_index.target_range(
            id, self.packet_data.len())?;
        Ok(self.packet_data.get_range(range)?)
    }

    fn packet_pid(&mut self, id: PacketId)
        -> Result<PID, CaptureError>
    {
        let offset: Id<u8> = self.packet_index.get(id)?;
        Ok(PID::from(self.packet_data.get(offset)?))
    }

    fn transaction(&mut self, id: TransactionId)
        -> Result<Transaction, CaptureError>
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

    fn completed_transactions(&mut self,
                              transaction_ids: Vec<TransactionId>)
        -> CompletedTransactions
    {
        CompletedTransactions {
            transaction_ids,
            index: 0
        }
    }

    fn control_transfer(&mut self,
                        address: DeviceAddr,
                        endpoint_id: EndpointId,
                        range: Range<EndpointTransactionId>)
        -> Result<ControlTransfer, CaptureError>
    {
        use PID::*;
        let transaction_ids = self.endpoint_traffic(endpoint_id)?
                                  .transaction_ids
                                  .get_range(&range)?;
        let mut transactions = self.completed_transactions(transaction_ids);
        let fields = match transactions.next(self) {
            Some(transaction) => {
                let data_packet_id = transaction.data_packet_id
                    .ok_or_else(||IndexError(String::from(
                        "Transaction has no data packet")))?;
                let data_packet = self.packet(data_packet_id)?;
                let data_pid = PID::from(
                    *data_packet.first().ok_or_else(||
                        IndexError(String::from(
                            "Found empty packet instead of setup data")))?);
                if data_pid != DATA0 {
                    return Err(IndexError(format!(
                        "Found {data_pid} packet instead of setup data")));
                } else if data_packet.len() != 11 {
                    return Err(IndexError(format!(
                        "Found DATA0 with packet length {} instead of setup data",
                        data_packet.len())));
                } else {
                    SetupFields::from_data_packet(&data_packet)
                }
            },
            _ => {
                return Err(IndexError(String::from(
                    "Control transfer has no completed transactions")))
            }
        };
        let direction = fields.type_fields.direction();
        let mut data: Vec<u8> = Vec::new();
        while let Some(transaction) = transactions.next(self) {
            use PID::*;
            use Direction::*;
            match (direction,
                   transaction.start_pid,
                   transaction.split,
                   transaction.payload_byte_range)
            {
                (In,  IN,    None,           Some(range)) |
                (Out, OUT,   None,           Some(range)) |
                (In,  SPLIT, Some((_, IN)),  Some(range)) |
                (Out, SPLIT, Some((_, OUT)), Some(range)) => {
                    data.extend_from_slice(
                        &self.packet_data.get_range(range)?);
                },
                (..) => {}
            };
        }
        Ok(ControlTransfer {
            address,
            fields,
            data,
        })
    }

    pub fn device_data(&self, id: &DeviceId)
        -> Result<&DeviceData, CaptureError>
    {
        self.device_data.get(*id).ok_or_else(||
            IndexError(format!("Capture has no device with ID {id}")))
    }

    fn device_version(&self, id: &DeviceId) -> Result<u32, CaptureError> {
        Ok(self.device_data(id)?.version)
    }

    pub fn device_data_mut(&mut self, id: &DeviceId)
        -> Result<&mut DeviceData, CaptureError>
    {
        self.device_data.get_mut(*id).ok_or_else(||
            IndexError(format!("Capture has no device with ID {id}")))
    }

    pub fn try_configuration(&self, dev: &DeviceId, conf: &ConfigNum)
        -> Option<&Configuration>
    {
        self.device_data(dev).ok()?.configurations.get(*conf)
    }

    fn transfer_extended(&mut self,
                         endpoint_id: EndpointId,
                         transfer_id: TransferId)
        -> Result<bool, CaptureError>
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

    pub fn finish(&mut self) {
        self.completion = CompletionStatus::Complete;
    }
}

#[derive(Copy, Clone)]
pub enum CompletionStatus {
    Complete,
    Ongoing
}

pub trait ItemSource<Item> {
    fn item(&mut self, parent: Option<&Item>, index: u64)
        -> Result<Item, CaptureError>;
    fn item_update(&mut self, item: &Item)
        -> Result<Option<Item>, CaptureError>;
    fn child_item(&mut self, parent: &Item, index: u64)
        -> Result<Item, CaptureError>;
    fn item_children(&mut self, parent: Option<&Item>)
        -> Result<(CompletionStatus, u64), CaptureError>;
    fn summary(&mut self, item: &Item) -> Result<String, CaptureError>;
    fn connectors(&mut self, item: &Item) -> Result<String, CaptureError>;
}

impl ItemSource<TrafficItem> for Capture {
    fn item(&mut self, parent: Option<&TrafficItem>, index: u64)
        -> Result<TrafficItem, CaptureError>
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
        -> Result<Option<TrafficItem>, CaptureError>
    {
        Ok(None)
    }

    fn child_item(&mut self, parent: &TrafficItem, index: u64)
        -> Result<TrafficItem, CaptureError>
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
            Packet(..) => return Err(IndexError(String::from(
                "Packets have no child items")))
        })
    }

    fn item_children(&mut self, parent: Option<&TrafficItem>)
        -> Result<(CompletionStatus, u64), CaptureError>
    {
        use TrafficItem::*;
        use CompletionStatus::*;
        Ok(match parent {
            None => {
                (self.completion, self.item_index.len())
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
        -> Result<String, CaptureError>
    {
        use TrafficItem::*;
        use usb::StartComplete::*;
        Ok(match item {
            Packet(.., packet_id) => {
                let packet = self.packet(*packet_id)?;
                let first_byte = *packet.first().ok_or_else(||
                    IndexError(format!(
                        "Packet {packet_id} is empty, cannot retrieve PID")))?;
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
            Transaction(_, transaction_id) => {
                let transaction = self.transaction(*transaction_id)?;
                let end_pid = transaction.end_pid;
                use PID::*;
                use StartComplete::*;
                match (transaction.start_pid,
                       &transaction.split,
                       transaction.payload_size())
                {
                    (SOF, ..) => format!(
                        "{} SOF packets", transaction.packet_count()),
                    (Malformed, ..) => format!(
                        "{} malformed packets", transaction.packet_count()),
                    (SPLIT, Some((split_fields, token_pid)), payload_size) => {
                        format!("{} {token_pid} transaction{}",
                            match split_fields.sc() {
                                Start => "Starting",
                                Complete => "Completing",
                            },
                            match payload_size {
                                None => format!(", {}", end_pid),
                                Some(size) if size == 0 => format!(
                                    " with no data, {}", end_pid),
                                Some(size) => format!(
                                    " with {} data bytes, {}: {}",
                                    size, end_pid, Bytes::first(
                                        100,
                                        &self.transaction_bytes(&transaction)?))
                            }
                        )
                    },
                    (pid, _, None) => format!(
                        "{pid} transaction, {end_pid}"),
                    (pid, _, Some(size)) if size == 0 => format!(
                        "{pid} transaction with no data, {end_pid}"),
                    (pid, _, Some(size)) => format!(
                        "{pid} transaction with {size} data bytes, {end_pid}: {}",
                        Bytes::first(100, &self.transaction_bytes(&transaction)?))
                }
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
                        let transfer = self.control_transfer(
                            endpoint.device_address(), endpoint_id, range)?;
                        transfer.summary()
                    },
                    (endpoint_type, starting) => {
                        let ep_transfer_id = entry.transfer_id();
                        let ep_traf = self.endpoint_traffic(endpoint_id)?;
                        let ep_transaction_id =
                            ep_traf.transfer_index.get(ep_transfer_id)?;
                        let transaction_id =
                            ep_traf.transaction_ids.get(ep_transaction_id)?;
                        let transaction = self.transaction(transaction_id)?;
                        let ep_type_string = format!("{endpoint_type}");
                        let ep_type_lower = ep_type_string.to_lowercase();
                        let count = if transaction.split.is_some() {
                            (count + 1) / 2
                        } else {
                            count
                        };
                        match (transaction.successful(), starting) {
                            (true, true) => {
                                let byte_range =
                                    self.transfer_byte_range(endpoint_id,
                                                             &range)?;
                                let length = byte_range.len();
                                let length_string = fmt_size(length);
                                let bytes = self.transfer_bytes(endpoint_id,
                                                                &range, 100)?;
                                let display_bytes = Bytes {
                                    partial: length > 100,
                                    bytes: &bytes,
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
        -> Result<String, CaptureError>
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
}

impl ItemSource<DeviceItem> for Capture {
    fn item(&mut self, parent: Option<&DeviceItem>, index: u64)
        -> Result<DeviceItem, CaptureError>
    {
        match parent {
            None => {
                let device_id = DeviceId::from(index + 1);
                let data = self.device_data(&device_id)?;
                Ok(DeviceItem::Device(device_id, data.version))
            },
            Some(item) => self.child_item(item, index)
        }
    }

    fn item_update(&mut self, item: &DeviceItem)
        -> Result<Option<DeviceItem>, CaptureError>
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
        -> Result<DeviceItem, CaptureError>
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
            _ => return Err(IndexError(String::from(
                "This device item type cannot have children")))
        })
    }

    fn item_children(&mut self, parent: Option<&DeviceItem>)
        -> Result<(CompletionStatus, u64), CaptureError>
    {
        use DeviceItem::*;
        use CompletionStatus::*;
        let (completion, children) = match parent {
            None =>
                (self.completion, self.device_data.len() - 1),
            Some(Device(dev, _version)) =>
                (Ongoing, {
                    let configs = self.device_data(dev)?.configurations.len();
                    if configs == 0 { 1 } else { configs }
                }),
            Some(DeviceDescriptor(dev)) =>
                match self.device_data(dev)?.device_descriptor {
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
        -> Result<String, CaptureError>
    {
        use DeviceItem::*;
        Ok(match item {
            Device(dev, _version) => {
                let device = self.devices.get(*dev)?;
                let data = self.device_data(dev)?;
                format!("Device {}: {}", device.address, data.description())
            },
            DeviceDescriptor(dev) => {
                match self.device_data(dev)?.device_descriptor {
                    Some(_) => "Device descriptor",
                    None => "No device descriptor"
                }.to_string()
            },
            DeviceDescriptorField(dev, field, _ver) => {
                let data = self.device_data(dev)?;
                match data.device_descriptor {
                    Some(desc) => desc.field_text(*field, &data.strings),
                    None => return Err(DescriptorMissing)
                }
            },
            Configuration(_, conf) => format!(
                "Configuration {conf}"),
            ConfigurationDescriptor(..) =>
                "Configuration descriptor".to_string(),
            ConfigurationDescriptorField(dev, conf, field, _ver) => {
                let data = self.device_data(dev)?;
                data.configuration(conf)?
                    .descriptor
                    .field_text(*field, &data.strings)
            },
            Interface(_, _, iface) => format!(
                "Interface {iface}"),
            InterfaceDescriptor(..) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(dev, conf, iface, field, _ver) => {
                let data = self.device_data(dev)?;
                data.configuration(conf)?
                    .interface(iface)?
                    .descriptor
                    .field_text(*field, &data.strings)
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

    fn connectors(&mut self, item: &DeviceItem) -> Result<String, CaptureError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, BufWriter, BufRead, Write};
    use crate::decoder::Decoder;
    use pcap_file::pcap::PcapReader;

    fn write_item(cap: &mut Capture, item: &TrafficItem, depth: u8,
                  writer: &mut dyn Write)
    {
        let summary = cap.summary(item).unwrap();
        for _ in 0..depth {
            writer.write(b" ").unwrap();
        }
        writer.write(summary.as_bytes()).unwrap();
        writer.write(b"\n").unwrap();
        let (_completion, num_children) = cap.item_children(Some(item)).unwrap();
        for child_id in 0..num_children {
            let child = cap.child_item(item, child_id).unwrap();
            write_item(cap, &child, depth + 1, writer);
        }
    }

    #[test]
    fn test_captures() {
        let test_dir = "./tests/";
        for result in std::fs::read_dir(test_dir).unwrap() {
            let entry = result.unwrap();
            if entry.file_type().unwrap().is_dir() {
                let path = entry.path();
                let mut cap_path = path.clone();
                let mut ref_path = path.clone();
                let mut out_path = path.clone();
                cap_path.push("capture.pcap");
                ref_path.push("reference.txt");
                out_path.push("output.txt");
                {
                    let pcap_file = File::open(cap_path).unwrap();
                    let mut pcap_reader = PcapReader::new(pcap_file).unwrap();
                    let mut cap = Capture::new().unwrap();
                    let mut decoder = Decoder::default();
                    while let Some(result) = pcap_reader.next_raw_packet() {
                        let packet = result.unwrap().data;
                        decoder.handle_raw_packet(&mut cap, &packet).unwrap();
                    }
                    decoder.finish(&mut cap).unwrap();
                    let out_file = File::create(out_path.clone()).unwrap();
                    let mut out_writer = BufWriter::new(out_file);
                    let num_items = cap.item_index.len();
                    for item_id in 0 .. num_items {
                        let item = cap.item(None, item_id).unwrap();
                        write_item(&mut cap, &item, 0, &mut out_writer);
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
                    assert!(actual == expected);
                }
            }
        }
    }
}

pub mod prelude {
    pub use super::{
        Capture,
        CaptureError,
        Device,
        DeviceId,
        DeviceData,
        Endpoint,
        EndpointId,
        EndpointType,
        EndpointState,
        EndpointTraffic,
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
