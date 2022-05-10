use std::ops::Range;
use std::mem::size_of;

use crate::file_vec::FileVec;
use crate::hybrid_index::HybridIndex;
use crate::usb::{
    PID,
    PacketFields,
    SetupFields,
    RequestType,
    Direction,
    Recipient,
    StandardRequest,
    DescriptorType,
    DeviceDescriptor,
    ConfigDescriptor,
    Configuration,
    ControlTransfer,
};

use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive};
use num_format::{Locale, ToFormattedString};
use humansize::{FileSize, file_size_opts as options};

#[derive(Clone)]
pub enum Item {
    Transfer(u64),
    Transaction(u64, u64),
    Packet(u64, u64, u64),
}

#[derive(Clone)]
pub enum DeviceItem {
    Device(u64),
    DeviceDescriptor(u64),
    DeviceDescriptorField(u64, u8),
    Configuration(u64, u8),
    ConfigurationDescriptor(u64, u8),
    ConfigurationDescriptorField(u64, u8, u8),
    Interface(u64, u8, u8),
    InterfaceDescriptor(u64, u8, u8),
    InterfaceDescriptorField(u64, u8, u8, u8),
    EndpointDescriptor(u64, u8, u8, u8),
    EndpointDescriptorField(u64, u8, u8, u8, u8),
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct Device {
    pub address: u8,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct Endpoint(u64);
    u64, device_id, set_device_id: 51, 0;
    u8, device_address, set_device_address: 58, 52;
    u8, number, set_number: 63, 59;
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct TransferIndexEntry(u64);
    u64, transfer_id, set_transfer_id: 51, 0;
    u16, endpoint_id, set_endpoint_id: 62, 52;
    u8, _is_start, _set_is_start: 63, 63;
}

impl TransferIndexEntry {
    fn is_start(&self) -> bool {
        self._is_start() != 0
    }
    fn set_is_start(&mut self, value: bool) {
        self._set_is_start(value as u8)
    }
}

#[derive(Default)]
struct TransactionState {
    first: PID,
    last: PID,
    start: u64,
    count: u64,
    endpoint_id: usize,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
}

#[derive(Copy, Clone, IntoPrimitive, FromPrimitive, PartialEq)]
#[repr(u8)]
enum EndpointState {
    #[default]
    Idle = 0,
    Starting = 1,
    Ongoing = 2,
    Ending = 3,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
enum EndpointType {
    Control       = 0x00,
    Isochronous   = 0x01,
    Bulk          = 0x02,
    Interrupt     = 0x03,
    #[default]
    Unidentified  = 0x04,
    Framing       = 0x10,
    Invalid       = 0x11,
}

struct EndpointData {
    device_id: usize,
    number: usize,
    transaction_ids: HybridIndex,
    transfer_index: HybridIndex,
    transaction_start: u64,
    transaction_count: u64,
    last: PID,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
}

struct DeviceData {
    device_descriptor: Option<DeviceDescriptor>,
    configurations: Vec<Option<Configuration>>,
    configuration_id: Option<usize>,
    endpoint_types: Vec<EndpointType>,
    strings: Vec<Option<Vec<u8>>>,
}

impl DeviceData {
    fn endpoint_type(&self, number: usize) -> EndpointType {
        use EndpointType::*;
        match number {
            0 => Control,
            0x10 => Framing,
            0x11 => Invalid,
            _ => self.endpoint_types[number],
        }
    }

    fn update_endpoint_types(&mut self) {
        match self.configuration_id {
            Some(id) => match &self.configurations[id] {
                Some(config) => {
                    for iface in &config.interfaces {
                        for ep_desc in &iface.endpoint_descriptors {
                            let number = ep_desc.endpoint_address & 0x0F;
                            let index = number as usize;
                            self.endpoint_types[index] =
                                EndpointType::from(ep_desc.attributes & 0x03);
                        }
                    }
                },
                None => {},
            },
            None => {},
        }
    }
}

const USB_MAX_DEVICES: usize = 128;
const USB_MAX_ENDPOINTS: usize = 16;

pub struct Capture {
    item_index: HybridIndex,
    packet_index: HybridIndex,
    packet_data: FileVec<u8>,
    transaction_index: HybridIndex,
    transfer_index: FileVec<TransferIndexEntry>,
    device_index: [i8; USB_MAX_DEVICES],
    devices: FileVec<Device>,
    device_data: Vec<DeviceData>,
    endpoint_index: [[i16; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
    endpoints: FileVec<Endpoint>,
    endpoint_data: Vec<EndpointData>,
    endpoint_states: FileVec<u8>,
    endpoint_state_index: HybridIndex,
    last_endpoint_state: Vec<u8>,
    last_item_endpoint: i16,
    transaction_state: TransactionState,
}

impl Default for Capture {
    fn default() -> Self {
        Capture::new()
    }
}

pub struct Transaction {
    pid: PID,
    packet_id_range: Range<u64>,
    payload_byte_range: Option<Range<u64>>,
}

impl Transaction {
    fn packet_count(&self) -> u64 {
        self.packet_id_range.end - self.packet_id_range.start
    }

    fn payload_size(&self) -> Option<u64> {
        match &self.payload_byte_range {
            Some(range) => Some(range.end - range.start),
            None => None
        }
    }
}

#[derive(PartialEq)]
enum DecodeStatus {
    NEW,
    CONTINUE,
    DONE,
    INVALID
}

impl TransactionState {
    pub fn status(&mut self, packet: &[u8]) -> DecodeStatus {
        let next = PID::from(packet[0]);
        use PID::*;
        match (self.first, self.last, next) {

            // SETUP, IN or OUT always start a new transaction.
            (_, _, SETUP | IN | OUT) => DecodeStatus::NEW,

            // SOF when there is no existing transaction starts a new
            // "transaction" representing an idle period on the bus.
            (_, Malformed, SOF) => DecodeStatus::NEW,
            // Additional SOFs extend this "transaction", more may follow.
            (_, SOF, SOF) => DecodeStatus::CONTINUE,

            // SETUP must be followed by DATA0.
            (_, SETUP, DATA0) => {
                // The packet must have the correct size.
                match packet.len() {
                    11 => {
                        self.setup = Some(
                            SetupFields::from_data_packet(packet));
                        // Wait for ACK.
                        DecodeStatus::CONTINUE
                    },
                    _ => DecodeStatus::INVALID
                }
            }
            // ACK then completes the transaction.
            (SETUP, DATA0, ACK) => DecodeStatus::DONE,

            // IN may be followed by NAK or STALL, completing transaction.
            (_, IN, NAK | STALL) => DecodeStatus::DONE,
            // IN or OUT may be followed by DATA0 or DATA1, wait for status.
            (_, IN | OUT, DATA0 | DATA1) => {
                if packet.len() >= 3 {
                    let range = 1 .. (packet.len() - 2);
                    self.payload = packet[range].to_vec();
                    DecodeStatus::CONTINUE
                } else {
                    DecodeStatus::INVALID
                }
            },
            // An ACK or NYET then completes the transaction.
            (IN | OUT, DATA0 | DATA1, ACK | NYET) => DecodeStatus::DONE,
            // OUT may also be completed by NAK or STALL.
            (OUT, DATA0 | DATA1, NAK | STALL) => DecodeStatus::DONE,

            // Any other case is not a valid part of a transaction.
            _ => DecodeStatus::INVALID,
        }
    }

    fn completed(&self) -> bool {
        use PID::*;
        // A transaction is completed if it has 3 valid packets and is
        // acknowledged with an ACK or NYET handshake.
        match (self.count, self.last) {
            (3, ACK | NYET) => true,
            (..)            => false
        }
    }
}

fn get_index_range(index: &mut HybridIndex,
                      length: u64,
                      id: u64) -> Range<u64>
{
    if id + 2 > index.len() {
        let start = index.get(id).unwrap();
        let end = length;
        start..end
    } else {
        let vec = index.get_range(id..(id + 2)).unwrap();
        let start = vec[0];
        let end = vec[1];
        start..end
    }
}

pub fn fmt_count(count: u64) -> String {
    count.to_formatted_string(&Locale::en)
}

pub fn fmt_size(size: u64) -> String {
    size.file_size(options::BINARY).unwrap()
}

pub fn fmt_vec<T>(vec: &FileVec<T>) -> String
    where T: bytemuck::Pod + Default
{
    format!("{} entries, {}", fmt_count(vec.len()), fmt_size(vec.size()))
}

pub fn fmt_index(idx: &HybridIndex) -> String {
    format!("{} values in {} entries, {}",
            fmt_count(idx.len()),
            fmt_count(idx.entry_count()),
            fmt_size(idx.size()))
}

impl Capture {
    pub fn new() -> Self {
        let mut capture = Capture {
            item_index: HybridIndex::new(1).unwrap(),
            packet_index: HybridIndex::new(2).unwrap(),
            packet_data: FileVec::new().unwrap(),
            transaction_index: HybridIndex::new(1).unwrap(),
            transfer_index: FileVec::new().unwrap(),
            device_index: [-1; USB_MAX_DEVICES],
            devices: FileVec::new().unwrap(),
            device_data: Vec::new(),
            endpoints: FileVec::new().unwrap(),
            endpoint_data: Vec::new(),
            endpoint_index: [[-1; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
            endpoint_states: FileVec::new().unwrap(),
            endpoint_state_index: HybridIndex::new(1).unwrap(),
            last_endpoint_state: Vec::new(),
            last_item_endpoint: -1,
            transaction_state: TransactionState::default(),
        };
        capture.add_endpoint(0, EndpointType::Invalid as usize);
        capture.add_endpoint(0, EndpointType::Framing as usize);
        capture
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        self.transaction_update(packet);
        self.packet_index.push(self.packet_data.len()).unwrap();
        self.packet_data.append(packet).unwrap();
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
        for ep_data in &self.endpoint_data {
            trx_count += ep_data.transaction_ids.len();
            trx_entries += ep_data.transaction_ids.entry_count();
            trx_size += ep_data.transaction_ids.size();
            xfr_count += ep_data.transfer_index.len();
            xfr_entries += ep_data.transfer_index.entry_count();
            xfr_size += ep_data.transfer_index.size();
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

    fn transaction_update(&mut self, packet: &[u8]) {
        let pid = PID::from(packet[0]);
        match self.transaction_state.status(packet) {
            DecodeStatus::NEW => {
                self.transaction_end();
                self.transaction_start(packet);
            },
            DecodeStatus::CONTINUE => {
                self.transaction_append(pid);
            },
            DecodeStatus::DONE => {
                self.transaction_append(pid);
                self.transaction_end();
            },
            DecodeStatus::INVALID => {
                self.transaction_end();
                self.transaction_start(packet);
                self.transaction_end();
            },
        };
    }

    fn transaction_start(&mut self, packet: &[u8]) {
        let state = &mut self.transaction_state;
        state.start = self.packet_index.len();
        state.count = 1;
        state.first = PID::from(packet[0]);
        state.last = state.first;
        match PacketFields::from_packet(&packet) {
            PacketFields::SOF(_) => {
                self.transaction_state.endpoint_id = 1;
            },
            PacketFields::Token(token) => {
                let addr = token.device_address() as usize;
                let num = token.endpoint_number() as usize;
                if self.endpoint_index[addr][num] < 0 {
                    let endpoint_id = self.endpoints.len() as i16;
                    self.endpoint_index[addr][num] = endpoint_id;
                    self.add_endpoint(addr, num);
                }
                self.transaction_state.endpoint_id =
                    self.endpoint_index[addr][num] as usize;
            },
            _ => {
                self.transaction_state.endpoint_id = 0;
            }
        }
    }

    fn transaction_append(&mut self, pid: PID) {
        let state = &mut self.transaction_state;
        state.count += 1;
        state.last = pid;
    }

    fn transaction_end(&mut self) {
        self.add_transaction();
        let state = &mut self.transaction_state;
        state.count = 0;
        state.first = PID::Malformed;
        state.last = PID::Malformed;
        state.setup = None;
    }

    fn add_transaction(&mut self) {
        if self.transaction_state.count == 0 { return }
        self.transfer_update();
        self.transaction_index.push(self.transaction_state.start).unwrap();
    }

    fn add_endpoint(&mut self, addr: usize, num: usize) {
        if self.device_index[addr] == -1 {
            self.device_index[addr] = self.devices.size() as i8;
            let device = Device { address: addr as u8 };
            self.devices.push(&device).unwrap();
            let dev_data = DeviceData {
                device_descriptor: None,
                configurations: Vec::new(),
                configuration_id: None,
                endpoint_types: vec![
                    EndpointType::Unidentified; USB_MAX_ENDPOINTS],
                strings: Vec::new(),
            };
            self.device_data.push(dev_data);
        }
        let ep_data = EndpointData {
            number: num as usize,
            device_id: self.device_index[addr] as usize,
            transaction_ids: HybridIndex::new(1).unwrap(),
            transfer_index: HybridIndex::new(1).unwrap(),
            transaction_start: 0,
            transaction_count: 0,
            last: PID::Malformed,
            setup: None,
            payload: Vec::new(),
        };
        self.endpoint_data.push(ep_data);
        let mut endpoint = Endpoint::default();
        endpoint.set_device_id(self.device_index[addr] as u64);
        endpoint.set_device_address(addr as u8);
        endpoint.set_number(num as u8);
        self.endpoints.push(&endpoint).unwrap();
        self.last_endpoint_state.push(EndpointState::Idle as u8);
    }

    fn decode_request(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &self.endpoint_data[endpoint_id];
        let fields = ep_data.setup.as_ref().unwrap();
        let req_type = fields.type_fields.request_type();
        let request = StandardRequest::from(fields.request);
        match (req_type, request) {
            (RequestType::Standard, StandardRequest::GetDescriptor)
                => self.decode_descriptor_read(),
            (RequestType::Standard, StandardRequest::SetConfiguration)
                => self.decode_configuration_set(),
            (..) => {}
        }
    }

    fn decode_descriptor_read(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let fields = ep_data.setup.as_ref().unwrap();
        let recipient = fields.type_fields.recipient();
        let desc_type = DescriptorType::from((fields.value >> 8) as u8);
        let payload = &ep_data.payload;
        let length = payload.len();
        match (recipient, desc_type) {
            (Recipient::Device, DescriptorType::Device) => {
                if length == size_of::<DeviceDescriptor>() {
                    let device_id = ep_data.device_id;
                    let dev_data = &mut self.device_data[device_id];
                    dev_data.device_descriptor =
                        Some(DeviceDescriptor::from_bytes(payload));
                }
            },
            (Recipient::Device, DescriptorType::Configuration) => {
                let size = size_of::<ConfigDescriptor>();
                if length >= size {
                    let device_id = ep_data.device_id;
                    let dev_data = &mut self.device_data[device_id];
                    let configurations = &mut dev_data.configurations;
                    let configuration = Configuration::from_bytes(&payload);
                    if let Some(config) = configuration {
                        let config_id =
                            config.descriptor.config_value as usize;
                        while configurations.len() <= config_id {
                            configurations.push(None);
                        }
                        configurations[config_id] = Some(config);
                        dev_data.update_endpoint_types();
                    }
                }
            },
            (Recipient::Device, DescriptorType::String) => {
                if length >= 2 {
                    let device_id = ep_data.device_id;
                    let strings = &mut self.device_data[device_id].strings;
                    let string_id = (fields.value & 0xFF) as usize;
                    while strings.len() <= string_id {
                        strings.push(None);
                    }
                    strings[string_id] = Some(payload[2..length].to_vec());
                }
            },
            _ => {}
        }
    }

    fn decode_configuration_set(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let device_id = ep_data.device_id;
        let dev_data = &mut self.device_data[device_id];
        let fields = ep_data.setup.as_ref().unwrap();
        let config_id = fields.value as usize;
        dev_data.configuration_id = Some(config_id);
        dev_data.update_endpoint_types();
    }

    fn transfer_status(&mut self) -> DecodeStatus {
        let next = self.transaction_state.first;
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let dev_data = &self.device_data[ep_data.device_id];
        let ep_type = &dev_data.endpoint_type(ep_data.number);
        use PID::*;
        use EndpointType::*;
        use Direction::*;
        match (ep_type, ep_data.last, next) {

            // A SETUP transaction starts a new control transfer.
            // Store the setup fields to interpret the request.
            (Control, _, SETUP) => {
                ep_data.setup = self.transaction_state.setup.take();
                DecodeStatus::NEW
            },

            (Control, _, _) => match &ep_data.setup {
                // No control transaction is valid unless setup was done.
                None => DecodeStatus::INVALID,
                // If setup was done then valid transactions depend on the
                // contents of the setup data packet.
                Some(fields) => {
                    let with_data = fields.length != 0;
                    let direction = fields.type_fields.direction();
                    match (direction, with_data, ep_data.last, next) {

                        // If there is data to transfer, setup stage is
                        // followed by IN/OUT at data stage in the direction
                        // of the request. IN/OUT may then be repeated.
                        (In,  true, SETUP, IN ) |
                        (Out, true, SETUP, OUT) |
                        (In,  true, IN,    IN ) |
                        (Out, true, OUT,   OUT) => {
                            if self.transaction_state.completed() {
                                ep_data.payload.extend(
                                    &self.transaction_state.payload);
                            }
                            // Await status stage.
                            DecodeStatus::CONTINUE
                        },

                        // If there is no data to transfer, setup stage is
                        // followed by IN/OUT at status stage in the opposite
                        // direction to the request. If there is data, then
                        // the status stage follows the data stage.
                        (In,  false, SETUP, OUT) |
                        (Out, false, SETUP, IN ) |
                        (In,  true,  IN,    OUT) |
                        (Out, true,  OUT,   IN ) => {
                            self.decode_request();
                            DecodeStatus::DONE
                        },
                        // Any other sequence is invalid.
                        (..) => DecodeStatus::INVALID
                    }
                }
            },

            // An IN or OUT transaction on a non-control endpoint,
            // with no transfer in progress, starts a new transfer.
            (_, Malformed, IN | OUT) => DecodeStatus::NEW,

            // IN or OUT may then be repeated.
            (_, IN, IN) => DecodeStatus::CONTINUE,
            (_, OUT, OUT) => DecodeStatus::CONTINUE,

            // A SOF group starts a special transfer, unless
            // one is already in progress.
            (Framing, Malformed, SOF) => DecodeStatus::NEW,

            // Further SOF groups continue this transfer.
            (Framing, SOF, SOF) => DecodeStatus::CONTINUE,

            // Any other case is not a valid part of a transfer.
            _ => DecodeStatus::INVALID
        }
    }

    fn transfer_update(&mut self) {
        let status = self.transfer_status();
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let retry_needed =
            ep_data.transaction_count > 0 &&
            status != DecodeStatus::INVALID &&
            !self.transaction_state.completed();
        if retry_needed {
            self.transfer_append(false);
            return
        }
        match status {
            DecodeStatus::NEW => {
                self.transfer_end();
                self.transfer_start();
                self.transfer_append(true);
            },
            DecodeStatus::CONTINUE => {
                self.transfer_append(true);
            },
            DecodeStatus::DONE => {
                self.transfer_append(true);
                self.transfer_end();
            },
            DecodeStatus::INVALID => {
                self.transfer_end();
                self.transfer_start();
                self.transfer_append(false);
                self.transfer_end();
            }
        }
    }

    fn transfer_start(&mut self) {
        self.item_index.push(self.transfer_index.len()).unwrap();
        let endpoint_id = self.transaction_state.endpoint_id;
        self.last_item_endpoint = endpoint_id as i16;
        self.add_transfer_entry(endpoint_id, true);
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.transaction_start = ep_data.transaction_ids.len();
        ep_data.transaction_count = 0;
        ep_data.transfer_index.push(ep_data.transaction_start).unwrap();
    }

    fn transfer_append(&mut self, success: bool) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.transaction_ids.push(self.transaction_index.len()).unwrap();
        ep_data.transaction_count += 1;
        if success {
            ep_data.last = self.transaction_state.first;
        }
    }

    fn transfer_end(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &self.endpoint_data[endpoint_id];
        if ep_data.transaction_count > 0 {
            if self.last_item_endpoint != (endpoint_id as i16) {
                self.item_index.push(self.transfer_index.len()).unwrap();
                self.last_item_endpoint = endpoint_id as i16;
            }
            self.add_transfer_entry(endpoint_id, false);
        }
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.transaction_count = 0;
        ep_data.last = PID::Malformed;
        ep_data.payload.clear();
    }

    fn add_transfer_entry(&mut self, endpoint_id: usize, start: bool) {
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id as u16);
        entry.set_transfer_id(ep_data.transfer_index.len());
        entry.set_is_start(start);
        self.transfer_index.push(&entry).unwrap();
        self.add_endpoint_state(endpoint_id, start);
    }

    fn add_endpoint_state(&mut self, endpoint_id: usize, start: bool) {
        let endpoint_count = self.endpoints.len() as usize;
        for i in 0..endpoint_count {
            use EndpointState::*;
            self.last_endpoint_state[i] = {
                let same = endpoint_id == i;
                let last = EndpointState::from(self.last_endpoint_state[i]);
                match (same, start, last) {
                    (true, true,  _)               => Starting,
                    (true, false, _)               => Ending,
                    (false, _, Starting | Ongoing) => Ongoing,
                    (false, _, Ending | Idle)      => Idle,
                }
            } as u8;
        }
        let last_state = self.last_endpoint_state.as_slice();
        let state_offset = self.endpoint_states.len();
        self.endpoint_states.append(last_state).unwrap();
        self.endpoint_state_index.push(state_offset).unwrap();
    }

    pub fn get_item(&mut self, parent: &Option<Item>, index: u64) -> Item {
        match parent {
            None => Item::Transfer(self.item_index.get(index).unwrap()),
            Some(item) => self.get_child(item, index)
        }
    }

    pub fn get_child(&mut self, parent: &Item, index: u64) -> Item {
        use Item::*;
        match parent {
            Transfer(transfer_index_id) =>
                Transaction(*transfer_index_id, {
                    let entry = self.transfer_index.get(*transfer_index_id).unwrap();
                    let endpoint_id = entry.endpoint_id() as usize;
                    let transfer_id = entry.transfer_id();
                    let ep_data = &mut self.endpoint_data[endpoint_id];
                    let offset = ep_data.transfer_index.get(transfer_id).unwrap();
                    ep_data.transaction_ids.get(offset + index).unwrap()
                }),
            Transaction(transfer_index_id, transaction_id) =>
                Packet(*transfer_index_id, *transaction_id, {
                    self.transaction_index.get(*transaction_id).unwrap() + index}),
            Packet(..) => panic!("packets do not have children"),
        }
    }

    fn item_range(&mut self, item: &Item) -> Range<u64> {
        use Item::*;
        match item {
            Transfer(transfer_index_id) => {
                let entry = self.transfer_index.get(*transfer_index_id).unwrap();
                let endpoint_id = entry.endpoint_id() as usize;
                let transfer_id = entry.transfer_id();
                let ep_data = &mut self.endpoint_data[endpoint_id];
                get_index_range(&mut ep_data.transfer_index,
                    ep_data.transaction_ids.len(), transfer_id)
            },
            Transaction(_, transaction_id) => {
                get_index_range(&mut self.transaction_index,
                    self.packet_index.len(), *transaction_id)
            },
            Packet(.., packet_id) => {
                get_index_range(&mut self.packet_index,
                    self.packet_data.len(), *packet_id)
            },
        }
    }

    pub fn item_count(&mut self, parent: &Option<Item>) -> u64 {
        match parent {
            None => self.item_index.len(),
            Some(item) => self.child_count(item)
        }
    }

    pub fn child_count(&mut self, parent: &Item) -> u64 {
        use Item::*;
        match parent {
            Transfer(id) => {
                let entry = self.transfer_index.get(*id).unwrap();
                if entry.is_start() {
                    let range = self.item_range(parent);
                    range.end - range.start
                } else {
                    0
                }
            },
            Transaction(..) => {
                let range = self.item_range(parent);
                range.end - range.start
            },
            Packet(..) => 0,
        }
    }

    pub fn get_summary(&mut self, item: &Item) -> String {
        use Item::*;
        match item {
            Packet(.., packet_id) => {
                let packet = self.get_packet(*packet_id);
                let pid = PID::from(packet[0]);
                format!("{} packet{}: {:02X?}",
                    pid,
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
                        PacketFields::Data(data) => format!(
                            " with {} data bytes and CRC {:04X}",
                            packet.len() - 3,
                            data.crc),
                        PacketFields::None => "".to_string()
                    },
                    packet)
            },
            Transaction(_, transaction_id) => {
                let transaction = self.get_transaction(transaction_id);
                let count = transaction.packet_count();
                match (transaction.pid, transaction.payload_size()) {
                    (PID::SOF, _) => format!(
                        "{} SOF packets", count),
                    (pid, None) => format!(
                        "{} transaction, {} packets", pid, count),
                    (pid, Some(size)) => format!(
                        "{} transaction, {} packets with {} data bytes",
                        pid, count, size)
                }
            },
            Transfer(transfer_index_id) => {
                let entry = self.transfer_index.get(*transfer_index_id).unwrap();
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id as u64).unwrap();
                let device_id = endpoint.device_id() as usize;
                let dev_data = &self.device_data[device_id];
                let num = endpoint.number() as usize;
                let ep_type = dev_data.endpoint_type(num);
                if !entry.is_start() {
                    return match ep_type {
                        EndpointType::Invalid =>
                            "End of invalid groups".to_string(),
                        EndpointType::Framing =>
                            "End of SOF groups".to_string(),
                        endpoint_type => format!(
                            "{:?} transfer ending on endpoint {}.{}",
                            endpoint_type, endpoint.device_address(), num)
                    }
                }
                let range = self.item_range(&item);
                let count = range.end - range.start;
                match ep_type {
                    EndpointType::Invalid => format!(
                        "{} invalid groups", count),
                    EndpointType::Framing => format!(
                        "{} SOF groups", count),
                    EndpointType::Control => {
                        let transfer = self.get_control_transfer(
                            endpoint.device_address(), endpoint_id, range);
                        transfer.summary()
                    },
                    endpoint_type => format!(
                        "{:?} transfer with {} transactions on endpoint {}.{}",
                        endpoint_type, count,
                        endpoint.device_address(), endpoint.number())
                }
            }
        }
    }

    pub fn get_connectors(&mut self, item: &Item) -> String {
        use EndpointState::*;
        use Item::*;
        let endpoint_count = self.endpoints.len() as usize;
        const MIN_LEN: usize = " └─".len();
        let string_length = MIN_LEN + endpoint_count;
        let mut connectors = String::with_capacity(string_length);
        let transfer_index_id = match item {
            Transfer(i) | Transaction(i, _) | Packet(i, ..) => i
        };
        let entry = self.transfer_index.get(*transfer_index_id).unwrap();
        let endpoint_id = entry.endpoint_id() as usize;
        let endpoint_state = self.get_endpoint_state(*transfer_index_id);
        let state_length = endpoint_state.len();
        let extended = self.transfer_extended(endpoint_id, *transfer_index_id);
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let last_transaction = match item {
            Transaction(_, transaction_id) | Packet(_, transaction_id, _) => {
                let range = get_index_range(&mut ep_data.transfer_index,
                    ep_data.transaction_ids.len(), entry.transfer_id());
                let last_transaction_id =
                    ep_data.transaction_ids.get(range.end - 1).unwrap();
                *transaction_id == last_transaction_id
            }, _ => false
        };
        let last_packet = match item {
            Packet(_, transaction_id, packet_id) => {
                let range = get_index_range(&mut self.transaction_index,
                    self.packet_index.len(), *transaction_id);
                *packet_id == range.end - 1
            }, _ => false
        };
        let last = last_transaction && !extended;
        let mut thru = false;
        for i in 0..state_length {
            let state = EndpointState::from(endpoint_state[i]);
            let active = state != Idle;
            let on_endpoint = i == endpoint_id;
            thru |= match (item, state, on_endpoint) {
                (Transfer(..), Starting | Ending, _) => true,
                (Transaction(..) | Packet(..), _, true) => on_endpoint,
                _ => false,
            };
            connectors.push(match item {
                Transfer(..) => {
                    match (state, thru) {
                        (Idle,     _    ) => ' ',
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
        connectors
    }

    fn transfer_extended(&mut self, endpoint_id: usize, index: u64) -> bool {
        use EndpointState::*;
        let count = self.transfer_index.len();
        if index + 1 >= count {
            return false;
        };
        let state = self.get_endpoint_state(index + 1);
        if endpoint_id >= state.len() {
            false
        } else {
            match EndpointState::from(state[endpoint_id]) {
                Ongoing => true,
                _ => false,
            }
        }
    }

    fn get_endpoint_state(&mut self, index: u64) -> Vec<u8> {
        let range = get_index_range(
            &mut self.endpoint_state_index,
            self.endpoint_states.len(), index);
        self.endpoint_states.get_range(range).unwrap()
    }

    fn get_packet(&mut self, index: u64) -> Vec<u8> {
        let range = get_index_range(&mut self.packet_index,
                                    self.packet_data.len(), index);
        self.packet_data.get_range(range).unwrap()
    }

    fn get_packet_pid(&mut self, index: u64) -> PID {
        let offset = self.packet_index.get(index).unwrap();
        PID::from(self.packet_data.get(offset).unwrap())
    }

    fn get_transaction(&mut self, index: &u64) -> Transaction {
        let packet_id_range = get_index_range(&mut self.transaction_index,
                                              self.packet_index.len(), *index);
        let packet_count = packet_id_range.end - packet_id_range.start;
        let pid = self.get_packet_pid(packet_id_range.start);
        use PID::*;
        let payload_byte_range = match pid {
            IN | OUT if packet_count >= 2 => {
                let data_packet_id = packet_id_range.start + 1;
                let packet_byte_range = get_index_range(
                    &mut self.packet_index,
                    self.packet_data.len(), data_packet_id);
                let pid = self.packet_data.get(packet_byte_range.start).unwrap();
                match PID::from(pid) {
                    DATA0 | DATA1 => Some({
                        packet_byte_range.start + 1 .. packet_byte_range.end - 2
                    }),
                    _ => None
                }
            },
            _ => None
        };
        Transaction {
            pid: pid,
            packet_id_range: packet_id_range,
            payload_byte_range: payload_byte_range,
        }
    }

    fn get_control_transfer(&mut self,
                            address: u8,
                            endpoint_id: u16,
                            range: Range<u64>) -> ControlTransfer
    {
        let ep_data = &mut self.endpoint_data[endpoint_id as usize];
        let transaction_ids =
            ep_data.transaction_ids.get_range(range).unwrap();
        let setup_transaction_id = transaction_ids[0];
        let setup_packet_id =
            self.transaction_index.get(setup_transaction_id)
                                  .unwrap();
        let data_packet_id = setup_packet_id + 1;
        let data_packet = self.get_packet(data_packet_id);
        let fields = SetupFields::from_data_packet(&data_packet);
        let direction = fields.type_fields.direction();
        let mut data: Vec<u8> = Vec::new();
        for id in transaction_ids {
            let transaction = self.get_transaction(&id);
            match (direction,
                   transaction.pid,
                   transaction.payload_byte_range)
            {
                (Direction::In,  PID::IN,  Some(range)) |
                (Direction::Out, PID::OUT, Some(range)) => {
                    data.extend_from_slice(
                        &self.packet_data.get_range(range).unwrap());
                },
                (..) => {}
            };
        }
        ControlTransfer {
            address: address,
            fields: fields,
            data: data,
        }
    }

    pub fn get_device_item(&mut self, parent: &Option<DeviceItem>, index: u64)
        -> DeviceItem
    {
        match parent {
            None => DeviceItem::Device(index + 1),
            Some(item) => self.device_child(item, index)
        }
    }

    fn device_child(&self, item: &DeviceItem, index: u64) -> DeviceItem {
        use DeviceItem::*;
        match item {
            Device(dev) => match index {
                0 => DeviceDescriptor(*dev),
                conf => Configuration(*dev, conf as u8),
            },
            DeviceDescriptor(dev) =>
                DeviceDescriptorField(*dev, index as u8),
            Configuration(dev, conf) => match index {
                0 => ConfigurationDescriptor(*dev, *conf),
                n => Interface(*dev, *conf, (n - 1).try_into().unwrap()),
            },
            ConfigurationDescriptor(dev, conf) =>
                ConfigurationDescriptorField(*dev, *conf, index as u8),
            Interface(dev, conf, iface) => match index {
                0 => InterfaceDescriptor(*dev, *conf, *iface),
                n => EndpointDescriptor(*dev, *conf, *iface,
                                        (n - 1).try_into().unwrap())
            },
            InterfaceDescriptor(dev, conf, iface) =>
                InterfaceDescriptorField(*dev, *conf, *iface, index as u8),
            EndpointDescriptor(dev, conf, iface, ep) =>
                 EndpointDescriptorField(*dev, *conf, *iface,
                                         *ep, index as u8),
            _ => panic!("Item does not have children")
        }
    }

    pub fn device_item_count(&mut self, parent: &Option<DeviceItem>) -> u64 {
        match parent {
            None => (self.device_data.len() - 1) as u64,
            Some(item) => self.device_child_count(item),
        }
    }

    fn device_child_count(&self, item: &DeviceItem) -> u64 {
        use DeviceItem::*;
        let data = &self.device_data;
        (match item {
            Device(dev) =>
                data[*dev as usize].configurations.len(),
            DeviceDescriptor(dev) =>
                match data[*dev as usize].device_descriptor {
                    Some(_) => 13,
                    None => 0,
                },
            Configuration(dev, conf) =>
                match data[*dev as usize]
                    .configurations[*conf as usize].as_ref()
                {
                    Some(conf) => 1 + conf.interfaces.len(),
                    None => 0
                },
            ConfigurationDescriptor(dev, conf) =>
                match data[*dev as usize]
                    .configurations[*conf as usize]
                {
                    Some(_) => 8,
                    None => 0
                },
            Interface(dev, conf, iface) =>
                match data[*dev as usize]
                    .configurations[*conf as usize].as_ref()
                {
                    Some(conf) => 1 + conf.interfaces[*iface as usize]
                        .endpoint_descriptors.len(),
                    None => 0
                },
            InterfaceDescriptor(..) => 9,
            EndpointDescriptor(..) => 6,
            _ => 0
        }) as u64
    }

    pub fn get_device_summary(&mut self, item: &DeviceItem) -> String {
        use DeviceItem::*;
        match item {
            Device(dev) => {
                let data = &self.device_data[*dev as usize];
                let device = self.devices.get(*dev).unwrap();
                format!("Device {}: {}", device.address,
                    match data.device_descriptor {
                        Some(descriptor) => format!(
                            "{:04X}:{:04X}",
                            descriptor.vendor_id,
                            descriptor.product_id
                        ),
                        None => format!("Unknown"),
                    }
                )
            },
            DeviceDescriptor(dev) => {
                let data = &self.device_data[*dev as usize];
                match data.device_descriptor {
                    Some(_) => "Device descriptor",
                    None => "No device descriptor"
                }.to_string()
            },
            DeviceDescriptorField(dev, field) => {
                let data = &self.device_data[*dev as usize];
                let desc = data.device_descriptor.unwrap();
                desc.field_text(*field, &data.strings)
            },
            Configuration(_, conf) => format!(
                "Configuration {}", conf),
            ConfigurationDescriptor(..) =>
                "Configuration descriptor".to_string(),
            ConfigurationDescriptorField(dev, conf, field) => {
                let data = &self.device_data[*dev as usize];
                let config = &data.configurations[*conf as usize];
                let config = config.as_ref().unwrap();
                config.descriptor.field_text(*field, &data.strings)
            },
            Interface(_, _, iface) => format!(
                "Interface {}", iface),
            InterfaceDescriptor(..) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(dev, conf, iface, field) => {
                let data = &self.device_data[*dev as usize];
                let config = &data.configurations[*conf as usize];
                let config = config.as_ref().unwrap();
                let iface = &config.interfaces[*iface as usize];
                iface.descriptor.field_text(*field, &data.strings)
            },
            EndpointDescriptor(dev, conf, iface, ep) => {
                let data = &self.device_data[*dev as usize];
                let config = &data.configurations[*conf as usize];
                let config = config.as_ref().unwrap();
                let iface = &config.interfaces[*iface as usize];
                let desc = iface.endpoint_descriptors[*ep as usize];
                format!("Endpoint {} {}",
                    desc.endpoint_address & 0x7F,
                    if desc.endpoint_address & 0x80 != 0 {"IN"} else {"OUT"}
                )
            },
            EndpointDescriptorField(dev, conf, iface, ep, field) => {
                let data = &self.device_data[*dev as usize];
                let config = &data.configurations[*conf as usize];
                let config = config.as_ref().unwrap();
                let iface = &config.interfaces[*iface as usize];
                let desc = iface.endpoint_descriptors[*ep as usize];
                desc.field_text(*field)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, BufWriter, BufRead, Write};

    fn write_item(cap: &mut Capture, item: &Item, depth: u8,
                  writer: &mut dyn Write)
    {
        let summary = cap.get_summary(&item);
        for _ in 0..depth {
            writer.write(b" ").unwrap();
        }
        writer.write(summary.as_bytes()).unwrap();
        writer.write(b"\n").unwrap();
        let num_children = cap.child_count(&item);
        for child_id in 0..num_children {
            let child = cap.get_child(&item, child_id);
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
                    let mut pcap = pcap::Capture::from_file(cap_path).unwrap();
                    let mut cap = Capture::new();
                    while let Ok(packet) = pcap.next() {
                        cap.handle_raw_packet(&packet);
                    }
                    let out_file = File::create(out_path.clone()).unwrap();
                    let mut out_writer = BufWriter::new(out_file);
                    let num_items = cap.item_index.len();
                    for item_id in 0 .. num_items {
                        let item = cap.get_item(&None, item_id);
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

