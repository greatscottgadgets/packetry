use std::mem::size_of;

use crate::usb::{
    PID,
    PacketFields,
    SetupFields,
    Direction,
    StandardRequest,
    RequestType,
    Recipient,
    DescriptorType,
    DeviceDescriptor,
    ConfigDescriptor,
    Configuration,
};

use crate::capture::{
    Capture,
    Device,
    DeviceData,
    Endpoint,
    EndpointType,
    EndpointState,
    EndpointTraffic,
    TransferIndexEntry,
};

use crate::hybrid_index::HybridIndex;

#[derive(PartialEq)]
enum DecodeStatus {
    NEW,
    CONTINUE,
    DONE,
    INVALID
}

struct EndpointData {
    device_id: usize,
    number: usize,
    transaction_start: u64,
    transaction_count: u64,
    last: PID,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
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

const USB_MAX_DEVICES: usize = 128;
const USB_MAX_ENDPOINTS: usize = 16;

pub struct Decoder<'cap> {
    capture: &'cap mut Capture,
    device_index: [i8; USB_MAX_DEVICES],
    endpoint_index: [[i16; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
    endpoint_data: Vec<EndpointData>,
    last_endpoint_state: Vec<u8>,
    last_item_endpoint: i16,
    transaction_state: TransactionState,
}

impl<'cap> Decoder<'cap> {
    pub fn new(capture: &'cap mut Capture) -> Self {
        let mut decoder = Decoder {
            capture: capture,
            device_index: [-1; USB_MAX_DEVICES],
            endpoint_data: Vec::new(),
            endpoint_index: [[-1; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
            last_endpoint_state: Vec::new(),
            last_item_endpoint: -1,
            transaction_state: TransactionState::default(),
        };
        decoder.add_endpoint(0, EndpointType::Invalid as usize);
        decoder.add_endpoint(0, EndpointType::Framing as usize);
        decoder 
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        self.transaction_update(packet);
        self.capture.packet_index.push(
            self.capture.packet_data.len()).unwrap();
        self.capture.packet_data.append(packet).unwrap();
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
        state.start = self.capture.packet_index.len();
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
                    let endpoint_id = self.capture.endpoints.len() as i16;
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
        self.capture.transaction_index.push(
            self.transaction_state.start).unwrap();
    }

    fn add_endpoint(&mut self, addr: usize, num: usize) {
        if self.device_index[addr] == -1 {
            self.device_index[addr] = self.capture.devices.size() as i8;
            let device = Device { address: addr as u8 };
            self.capture.devices.push(&device).unwrap();
            let dev_data = DeviceData {
                device_descriptor: None,
                configurations: Vec::new(),
                configuration_id: None,
                endpoint_types: vec![
                    EndpointType::Unidentified; USB_MAX_ENDPOINTS],
                strings: Vec::new(),
            };
            self.capture.device_data.push(dev_data);
        }
        let ep_data = EndpointData {
            number: num as usize,
            device_id: self.device_index[addr] as usize,
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
        self.capture.endpoints.push(&endpoint).unwrap();
        let ep_traf = EndpointTraffic {
            transaction_ids: HybridIndex::new(1).unwrap(),
            transfer_index: HybridIndex::new(1).unwrap(),
        };
        self.capture.endpoint_traffic.push(ep_traf);
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
                    let dev_data = &mut self.capture.device_data[device_id];
                    dev_data.device_descriptor =
                        Some(DeviceDescriptor::from_bytes(payload));
                }
            },
            (Recipient::Device, DescriptorType::Configuration) => {
                let size = size_of::<ConfigDescriptor>();
                if length >= size {
                    let device_id = ep_data.device_id;
                    let dev_data = &mut self.capture.device_data[device_id];
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
                    let strings =
                        &mut self.capture.device_data[device_id].strings;
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
        let dev_data = &mut self.capture.device_data[device_id];
        let fields = ep_data.setup.as_ref().unwrap();
        let config_id = fields.value as usize;
        dev_data.configuration_id = Some(config_id);
        dev_data.update_endpoint_types();
    }

    fn transfer_status(&mut self) -> DecodeStatus {
        let next = self.transaction_state.first;
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let dev_data = &self.capture.device_data[ep_data.device_id];
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
        self.capture.item_index.push(
            self.capture.transfer_index.len()).unwrap();
        let endpoint_id = self.transaction_state.endpoint_id;
        self.last_item_endpoint = endpoint_id as i16;
        self.add_transfer_entry(endpoint_id, true);
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let ep_traf = &mut self.capture.endpoint_traffic[endpoint_id];
        ep_data.transaction_start = ep_traf.transaction_ids.len();
        ep_data.transaction_count = 0;
        ep_traf.transfer_index.push(ep_data.transaction_start).unwrap();
    }

    fn transfer_append(&mut self, success: bool) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let ep_traf = &mut self.capture.endpoint_traffic[endpoint_id];
        ep_traf.transaction_ids.push(
            self.capture.transaction_index.len()).unwrap();
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
                self.capture.item_index.push(
                    self.capture.transfer_index.len()).unwrap();
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
        let ep_traf = &mut self.capture.endpoint_traffic[endpoint_id];
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id as u16);
        entry.set_transfer_id(ep_traf.transfer_index.len());
        entry.set_is_start(start);
        self.capture.transfer_index.push(&entry).unwrap();
        self.add_endpoint_state(endpoint_id, start);
    }

    fn add_endpoint_state(&mut self, endpoint_id: usize, start: bool) {
        let endpoint_count = self.capture.endpoints.len() as usize;
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
        let state_offset = self.capture.endpoint_states.len();
        self.capture.endpoint_states.append(last_state).unwrap();
        self.capture.endpoint_state_index.push(state_offset).unwrap();
    }
}
