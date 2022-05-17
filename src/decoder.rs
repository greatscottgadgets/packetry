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
    CaptureError,
    Device,
    DeviceData,
    Endpoint,
    EndpointType,
    EndpointState,
    EndpointTraffic,
    TransferIndexEntry,
};

use crate::hybrid_index::HybridIndex;

use CaptureError::IndexError;

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
    pub fn status(&mut self, packet: &[u8])
        -> Result<DecodeStatus, CaptureError>
    {
        let next = PID::from(*packet.get(0).ok_or(IndexError)?);
        use PID::*;
        Ok(match (self.first, self.last, next) {

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
        })
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
    pub fn new(capture: &'cap mut Capture) -> Result<Self, CaptureError> {
        let mut decoder = Decoder {
            capture: capture,
            device_index: [-1; USB_MAX_DEVICES],
            endpoint_data: Vec::new(),
            endpoint_index: [[-1; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
            last_endpoint_state: Vec::new(),
            last_item_endpoint: -1,
            transaction_state: TransactionState::default(),
        };
        decoder.add_endpoint(0, EndpointType::Invalid as usize)?;
        decoder.add_endpoint(0, EndpointType::Framing as usize)?;
        Ok(decoder)
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8])
        -> Result<(), CaptureError>
    {
        self.transaction_update(packet)?;
        self.capture.packet_index.push(
            self.capture.packet_data.len())?;
        self.capture.packet_data.append(packet)?;
        Ok(())
    }

    fn transaction_update(&mut self, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let pid = PID::from(*packet.get(0).ok_or(IndexError)?);
        match self.transaction_state.status(packet)? {
            DecodeStatus::NEW => {
                self.transaction_end()?;
                self.transaction_start(packet)?;
            },
            DecodeStatus::CONTINUE => {
                self.transaction_append(pid);
            },
            DecodeStatus::DONE => {
                self.transaction_append(pid);
                self.transaction_end()?;
            },
            DecodeStatus::INVALID => {
                self.transaction_end()?;
                self.transaction_start(packet)?;
                self.transaction_end()?;
            },
        };
        Ok(())
    }

    fn transaction_start(&mut self, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let state = &mut self.transaction_state;
        state.start = self.capture.packet_index.len();
        state.count = 1;
        state.first = PID::from(*packet.get(0).ok_or(IndexError)?);
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
                    self.add_endpoint(addr, num)?;
                }
                self.transaction_state.endpoint_id =
                    self.endpoint_index[addr][num] as usize;
            },
            _ => {
                self.transaction_state.endpoint_id = 0;
            }
        };
        Ok(())
    }

    fn transaction_append(&mut self, pid: PID) {
        let state = &mut self.transaction_state;
        state.count += 1;
        state.last = pid;
    }

    fn transaction_end(&mut self)
        -> Result<(), CaptureError>
    {
        self.add_transaction()?;
        let state = &mut self.transaction_state;
        state.count = 0;
        state.first = PID::Malformed;
        state.last = PID::Malformed;
        state.setup = None;
        Ok(())
    }

    fn add_transaction(&mut self)
        -> Result<(), CaptureError>
    {
        if self.transaction_state.count == 0 { return Ok(()) }
        self.transfer_update()?;
        self.capture.transaction_index.push(
            self.transaction_state.start)?;
        Ok(())
    }

    fn add_endpoint(&mut self, addr: usize, num: usize)
        -> Result<(), CaptureError>
    {
        if self.device_index[addr] == -1 {
            self.device_index[addr] = self.capture.devices.size() as i8;
            let device = Device { address: addr as u8 };
            self.capture.devices.push(&device)?;
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
        self.capture.endpoints.push(&endpoint)?;
        let ep_traf = EndpointTraffic {
            transaction_ids: HybridIndex::new(1)?,
            transfer_index: HybridIndex::new(1)?,
        };
        self.capture.endpoint_traffic.push(ep_traf);
        self.last_endpoint_state.push(EndpointState::Idle as u8);
        Ok(())
    }

    fn current_endpoint_data(&self)
        -> Result<&EndpointData, CaptureError>
    {
        let endpoint_id = self.transaction_state.endpoint_id;
        Ok(self.endpoint_data.get(endpoint_id)
                             .ok_or(IndexError)?)
    }

    fn current_endpoint_data_mut(&mut self)
        -> Result<&mut EndpointData, CaptureError>
    {
        let endpoint_id = self.transaction_state.endpoint_id;
        Ok(self.endpoint_data.get_mut(endpoint_id)
                             .ok_or(IndexError)?)
    }

    fn current_device_data(&self)
        -> Result<&DeviceData, CaptureError>
    {
        let ep_data = self.current_endpoint_data()?;
        let device_id = ep_data.device_id as u64;
        Ok(self.capture.get_device_data(&device_id)?)
    }

    fn current_device_data_mut(&mut self)
        -> Result<&mut DeviceData, CaptureError>
    {
        let ep_data = self.current_endpoint_data()?;
        let device_id = ep_data.device_id as u64;
        Ok(self.capture.get_device_data_mut(&device_id)?)
    }

    fn decode_request(&mut self, fields: SetupFields)
        -> Result<(), CaptureError>
    {
        let req_type = fields.type_fields.request_type();
        let request = StandardRequest::from(fields.request);
        Ok(match (req_type, request) {
            (RequestType::Standard, StandardRequest::GetDescriptor)
                => self.decode_descriptor_read(&fields)?,
            (RequestType::Standard, StandardRequest::SetConfiguration)
                => self.decode_configuration_set(&fields)?,
            (..) => {}
        })
    }

    fn decode_descriptor_read(&mut self, fields: &SetupFields)
        -> Result<(), CaptureError>
    {
        let recipient = fields.type_fields.recipient();
        let desc_type = DescriptorType::from((fields.value >> 8) as u8);
        let payload = &self.current_endpoint_data()?.payload;
        let length = payload.len();
        Ok(match (recipient, desc_type) {
            (Recipient::Device, DescriptorType::Device) => {
                if length == size_of::<DeviceDescriptor>() {
                    let descriptor = DeviceDescriptor::from_bytes(payload);
                    let dev_data = self.current_device_data_mut()?;
                    dev_data.device_descriptor = Some(descriptor);
                }
            },
            (Recipient::Device, DescriptorType::Configuration) => {
                let size = size_of::<ConfigDescriptor>();
                if length >= size {
                    let configuration = Configuration::from_bytes(&payload);
                    let dev_data = self.current_device_data_mut()?;
                    let configurations = &mut dev_data.configurations;
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
                    let string = payload[2..length].to_vec();
                    let dev_data = self.current_device_data_mut()?;
                    let strings = &mut dev_data.strings;
                    let string_id = (fields.value & 0xFF) as usize;
                    while strings.len() <= string_id {
                        strings.push(None);
                    }
                    strings[string_id] = Some(string);
                }
            },
            _ => {}
        })
    }

    fn decode_configuration_set(&mut self, fields: &SetupFields)
        -> Result<(), CaptureError>
    {
        let dev_data = self.current_device_data_mut()?;
        let config_id = fields.value as usize;
        dev_data.configuration_id = Some(config_id);
        dev_data.update_endpoint_types();
        Ok(())
    }

    fn transfer_status(&mut self) -> Result<DecodeStatus, CaptureError> {
        let next = self.transaction_state.first;
        let ep_data = self.current_endpoint_data()?;
        let dev_data = self.current_device_data()?;
        let ep_type = &dev_data.endpoint_type(ep_data.number);
        use PID::*;
        use EndpointType::*;
        use Direction::*;
        Ok(match (ep_type, ep_data.last, next) {

            // A SETUP transaction starts a new control transfer.
            // Store the setup fields to interpret the request.
            (Control, _, SETUP) => {
                let setup = self.transaction_state.setup.take();
                let ep_data = self.current_endpoint_data_mut()?;
                ep_data.setup = setup;
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
                                let payload =
                                    self.transaction_state.payload.clone();
                                let ep_data = self.current_endpoint_data_mut()?;
                                ep_data.payload.extend(payload);
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
                            let fields_copy = fields.clone();
                            self.decode_request(fields_copy)?;
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
        })
    }

    fn transfer_update(&mut self)
        -> Result<(), CaptureError>
    {
        let status = self.transfer_status()?;
        let ep_data = self.current_endpoint_data()?;
        let retry_needed =
            ep_data.transaction_count > 0 &&
            status != DecodeStatus::INVALID &&
            !self.transaction_state.completed();
        if retry_needed {
            self.transfer_append(false)?;
            return Ok(());
        }
        match status {
            DecodeStatus::NEW => {
                self.transfer_end()?;
                self.transfer_start()?;
                self.transfer_append(true)?;
            },
            DecodeStatus::CONTINUE => {
                self.transfer_append(true)?;
            },
            DecodeStatus::DONE => {
                self.transfer_append(true)?;
                self.transfer_end()?;
            },
            DecodeStatus::INVALID => {
                self.transfer_end()?;
                self.transfer_start()?;
                self.transfer_append(false)?;
                self.transfer_end()?;
            }
        }
        Ok(())
    }

    fn transfer_start(&mut self)
        -> Result<(), CaptureError>
    {
        self.capture.item_index.push(
            self.capture.transfer_index.len())?;
        let endpoint_id = self.transaction_state.endpoint_id;
        self.last_item_endpoint = endpoint_id as i16;
        self.add_transfer_entry(endpoint_id, true)?;
        let ep_data = self.endpoint_data.get_mut(endpoint_id)
                                        .ok_or(IndexError)?;
        let ep_traf = self.capture.endpoint_traffic.get_mut(endpoint_id)
                                                   .ok_or(IndexError)?;
        ep_data.transaction_start = ep_traf.transaction_ids.len();
        ep_data.transaction_count = 0;
        ep_traf.transfer_index.push(ep_data.transaction_start)?;
        Ok(())
    }

    fn transfer_append(&mut self, success: bool)
        -> Result<(), CaptureError>
    {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = self.endpoint_data.get_mut(endpoint_id)
                                        .ok_or(IndexError)?;
        let ep_traf = self.capture.endpoint_traffic.get_mut(endpoint_id)
                                                   .ok_or(IndexError)?;
        ep_traf.transaction_ids.push(
            self.capture.transaction_index.len())?;
        ep_data.transaction_count += 1;
        if success {
            ep_data.last = self.transaction_state.first;
        }
        Ok(())
    }

    fn transfer_end(&mut self)
        -> Result<(), CaptureError>
    {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = self.current_endpoint_data()?;
        if ep_data.transaction_count > 0 {
            if self.last_item_endpoint != (endpoint_id as i16) {
                self.capture.item_index.push(
                    self.capture.transfer_index.len())?;
                self.last_item_endpoint = endpoint_id as i16;
            }
            self.add_transfer_entry(endpoint_id, false)?;
        }
        let ep_data = self.current_endpoint_data_mut()?;
        ep_data.transaction_count = 0;
        ep_data.last = PID::Malformed;
        ep_data.payload.clear();
        Ok(())
    }

    fn add_transfer_entry(&mut self, endpoint_id: usize, start: bool)
        -> Result<(), CaptureError>
    {
        let ep_traf = self.capture.endpoint_traffic.get_mut(endpoint_id)
                                                   .ok_or(IndexError)?;
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id as u16);
        entry.set_transfer_id(ep_traf.transfer_index.len());
        entry.set_is_start(start);
        self.capture.transfer_index.push(&entry)?;
        self.add_endpoint_state(endpoint_id, start)?;
        Ok(())
    }

    fn add_endpoint_state(&mut self, endpoint_id: usize, start: bool)
        -> Result<(), CaptureError>
    {
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
        self.capture.endpoint_states.append(last_state)?;
        self.capture.endpoint_state_index.push(state_offset)?;
        Ok(())
    }
}
