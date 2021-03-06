use std::mem::size_of;

use crate::usb::{self, prelude::*};
use crate::capture::{prelude::*, INVALID_EP_NUM, FRAMING_EP_NUM};
use crate::hybrid_index::HybridIndex;
use crate::vec_map::{VecMap, Key};

use CaptureError::IndexError;

impl PID {
    fn from_packet(packet: &[u8]) -> Result<PID, CaptureError> {
        Ok(PID::from(*packet.get(0).ok_or(IndexError)?))
    }
}

#[derive(PartialEq)]
enum DecodeStatus {
    Single,
    New,
    Continue,
    Retry,
    Done,
    Invalid
}

struct EndpointData {
    device_id: DeviceId,
    address: EndpointAddr,
    transfer_id: Option<EndpointTransferId>,
    transaction_count: u64,
    last: Option<PID>,
    last_success: bool,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
}

#[derive(Default)]
struct TransactionState {
    first: Option<PID>,
    last: Option<PID>,
    start: Option<PacketId>,
    count: u64,
    endpoint_id: Option<EndpointId>,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
}

impl TransactionState {
    pub fn status(&mut self, packet: &[u8])
        -> Result<DecodeStatus, CaptureError>
    {
        let next = PID::from_packet(packet)?;
        use PID::*;
        Ok(match (self.first, self.last, next) {

            // SETUP, IN or OUT always start a new transaction.
            (_, _, SETUP | IN | OUT) => DecodeStatus::New,

            // SOF when there is no existing transaction starts a new
            // "transaction" representing an idle period on the bus.
            (_, None, SOF) => DecodeStatus::New,
            // Additional SOFs extend this "transaction", more may follow.
            (_, Some(SOF), SOF) => DecodeStatus::Continue,

            // A malformed packet is grouped with previous malformed packets.
            (_, Some(Malformed), Malformed) => DecodeStatus::Continue,
            // If preceded by any other packet, it starts a new transaction.
            (_, _, Malformed) => DecodeStatus::New,

            // SETUP must be followed by DATA0.
            (_, Some(SETUP), DATA0) => {
                // The packet must have the correct size.
                match packet.len() {
                    11 => {
                        self.setup = Some(
                            SetupFields::from_data_packet(packet));
                        // Wait for ACK.
                        DecodeStatus::Continue
                    },
                    _ => DecodeStatus::Invalid
                }
            }
            // ACK then completes the transaction.
            (Some(SETUP), Some(DATA0), ACK) => DecodeStatus::Done,

            // IN may be followed by NAK or STALL, completing transaction.
            (_, Some(IN), NAK | STALL) => DecodeStatus::Done,
            // IN or OUT may be followed by DATA0 or DATA1, wait for status.
            (_, Some(IN | OUT), DATA0 | DATA1) => {
                if packet.len() >= 3 {
                    let range = 1 .. (packet.len() - 2);
                    self.payload = packet[range].to_vec();
                    DecodeStatus::Continue
                } else {
                    DecodeStatus::Invalid
                }
            },
            // An ACK or NYET then completes the transaction.
            (Some(IN | OUT), Some(DATA0 | DATA1), ACK | NYET) => DecodeStatus::Done,
            // OUT may also be completed by NAK or STALL.
            (Some(OUT), Some(DATA0 | DATA1), NAK | STALL) => DecodeStatus::Done,

            // Any other case is not a valid part of a transaction.
            _ => DecodeStatus::Invalid,
        })
    }

    fn completed(&self) -> bool {
        use PID::*;
        // A transaction is completed if it has 3 valid packets and is
        // acknowledged with an ACK or NYET handshake.
        match (self.count, self.last) {
            (3, Some(ACK | NYET)) => true,
            (..)                  => false
        }
    }
}

#[derive(Copy, Clone)]
struct EndpointKey {
    dev_addr: DeviceAddr,
    direction: Direction,
    ep_num: EndpointNum,
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

pub struct Decoder<'cap> {
    capture: &'cap mut Capture,
    device_index: VecMap<DeviceAddr, DeviceId>,
    endpoint_index: VecMap<EndpointKey, EndpointId>,
    endpoint_data: VecMap<EndpointId, EndpointData>,
    last_endpoint_state: Vec<u8>,
    last_item_endpoint: Option<EndpointId>,
    transaction_state: TransactionState,
}

impl<'cap> Decoder<'cap> {
    pub fn new(capture: &'cap mut Capture) -> Result<Self, CaptureError> {
        let mut decoder = Decoder {
            capture,
            device_index: VecMap::new(),
            endpoint_index: VecMap::new(),
            endpoint_data: VecMap::new(),
            last_endpoint_state: Vec::new(),
            last_item_endpoint: None,
            transaction_state: TransactionState::default(),
        };
        let invalid_id = decoder.add_endpoint(
            DeviceAddr(0), EndpointNum(INVALID_EP_NUM), Direction::Out)?;
        let framing_id = decoder.add_endpoint(
            DeviceAddr(0), EndpointNum(FRAMING_EP_NUM), Direction::Out)?;
        assert!(invalid_id == Decoder::INVALID_EP_ID);
        assert!(framing_id == Decoder::FRAMING_EP_ID);
        Ok(decoder)
    }

    const INVALID_EP_ID: EndpointId = EndpointId::constant(0);
    const FRAMING_EP_ID: EndpointId = EndpointId::constant(1);

    pub fn handle_raw_packet(&mut self, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let data_id = self.capture.packet_data.append(packet)?;
        let packet_id = self.capture.packet_index.push(data_id)?;
        self.transaction_update(packet_id, packet)?;
        Ok(())
    }

    pub fn token_endpoint(&mut self, pid: PID, token: &TokenFields)
        -> Result<EndpointId, CaptureError>
    {
        let dev_addr = token.device_address();
        let ep_num = token.endpoint_number();
        let direction = match (ep_num.0, pid) {
            (0, _)        => Direction::Out,
            (_, PID::IN)  => Direction::In,
            (_, PID::OUT) => Direction::Out,
            _ => return Err(IndexError)
        };
        let key = EndpointKey {
            dev_addr,
            ep_num,
            direction
        };
        Ok(match self.endpoint_index.get(key) {
            Some(id) => *id,
            None => {
                let id = self.add_endpoint(
                    key.dev_addr, key.ep_num, key.direction)?;
                self.endpoint_index.set(key, id);
                id
            }
        })
    }

    fn transaction_update(&mut self, packet_id: PacketId, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let pid = PID::from_packet(packet)?;
        match self.transaction_state.status(packet)? {
            DecodeStatus::Single => {
                self.transaction_start(packet_id, packet)?;
                self.transaction_end()?;
            },
            DecodeStatus::New => {
                self.transaction_start(packet_id, packet)?;
            },
            DecodeStatus::Continue | DecodeStatus::Retry => {
                self.transaction_append(pid);
            },
            DecodeStatus::Done => {
                self.transaction_append(pid);
                self.transaction_end()?;
            },
            DecodeStatus::Invalid => {
                self.transaction_start(packet_id, packet)?;
                self.transaction_end()?;
            },
        };
        Ok(())
    }

    fn transaction_start(&mut self, packet_id: PacketId, packet: &[u8])
        -> Result<(), CaptureError>
    {
        self.add_transaction()?;
        self.transaction_state = TransactionState::default();
        let pid = PID::from_packet(packet)?;
        let state = &mut self.transaction_state;
        state.start = Some(packet_id);
        state.count = 1;
        state.first = Some(pid);
        state.last = state.first;
        self.transaction_state.endpoint_id = Some(
            match PacketFields::from_packet(packet) {
                PacketFields::SOF(_) => Decoder::FRAMING_EP_ID,
                PacketFields::Token(token) =>
                    self.token_endpoint(pid, &token)?,
                _ => Decoder::INVALID_EP_ID,
            }
        );
        Ok(())
    }

    fn transaction_append(&mut self, pid: PID) {
        let state = &mut self.transaction_state;
        state.count += 1;
        state.last = Some(pid);
    }

    fn transaction_end(&mut self)
        -> Result<(), CaptureError>
    {
        self.add_transaction()?;
        self.transaction_state = TransactionState::default();
        Ok(())
    }

    fn add_transaction(&mut self)
        -> Result<(), CaptureError>
    {
        if self.transaction_state.count == 0 { return Ok(()) }
        let start_packet_id =
            self.transaction_state.start.ok_or(IndexError)?;
        let transaction_id =
            self.capture.transaction_index.push(start_packet_id)?;
        self.transfer_update(transaction_id)?;
        Ok(())
    }

    fn add_device(&mut self, address: DeviceAddr)
        -> Result<DeviceId, CaptureError>
    {
        let device = Device { address };
        let device_id = self.capture.devices.push(&device)?;
        self.device_index.set(address, device_id);
        self.capture.device_data.set(device_id, DeviceData {
            device_descriptor: None,
            configurations: VecMap::new(),
            config_number: None,
            endpoint_details: VecMap::new(),
            strings: VecMap::new(),
        });
        Ok(device_id)
    }

    fn add_endpoint(&mut self,
                    dev_addr: DeviceAddr,
                    number: EndpointNum,
                    direction: Direction)
        -> Result<EndpointId, CaptureError>
    {
        let device_id = match self.device_index.get(dev_addr) {
            Some(id) => *id,
            None => self.add_device(dev_addr)?
        };
        let mut endpoint = Endpoint::default();
        endpoint.set_device_id(device_id);
        endpoint.set_device_address(dev_addr);
        endpoint.set_number(number);
        endpoint.set_direction(direction);
        let endpoint_id = self.capture.endpoints.push(&endpoint)?;
        let address = EndpointAddr::from_parts(number, direction);
        self.endpoint_data.set(endpoint_id, EndpointData {
            address,
            device_id,
            transfer_id: None,
            transaction_count: 0,
            last: None,
            last_success: false,
            setup: None,
            payload: Vec::new(),
        });
        self.capture.endpoint_traffic.set(endpoint_id, EndpointTraffic {
            transaction_ids: HybridIndex::new(1)?,
            transfer_index: HybridIndex::new(1)?,
            data_index: HybridIndex::new(1)?,
            total_data: 0,
        });
        let ep_state = EndpointState::Idle as u8;
        self.last_endpoint_state.push(ep_state);
        Ok(endpoint_id)
    }

    fn current_endpoint_id(&self) -> Result<EndpointId, CaptureError> {
        self.transaction_state.endpoint_id.ok_or(IndexError)
    }

    fn current_endpoint_data(&self) -> Result<&EndpointData, CaptureError> {
        let endpoint_id = self.current_endpoint_id()?;
        self.endpoint_data.get(endpoint_id).ok_or(IndexError)
    }

    fn current_endpoint_data_mut(&mut self)
        -> Result<&mut EndpointData, CaptureError>
    {
        let endpoint_id = self.current_endpoint_id()?;
        self.endpoint_data.get_mut(endpoint_id).ok_or(IndexError)
    }

    fn current_device_data(&self)
        -> Result<&DeviceData, CaptureError>
    {
        let ep_data = self.current_endpoint_data()?;
        self.capture.device_data(&ep_data.device_id)
    }

    fn current_device_data_mut(&mut self)
        -> Result<&mut DeviceData, CaptureError>
    {
        let ep_data = self.current_endpoint_data()?;
        let device_id = ep_data.device_id;
        self.capture.device_data_mut(&device_id)
    }

    fn decode_request(&mut self, fields: SetupFields)
        -> Result<(), CaptureError>
    {
        let req_type = fields.type_fields.request_type();
        let request = StandardRequest::from(fields.request);
        match (req_type, request) {
            (RequestType::Standard, StandardRequest::GetDescriptor)
                => self.decode_descriptor_read(&fields)?,
            (RequestType::Standard, StandardRequest::SetConfiguration)
                => self.decode_configuration_set(&fields)?,
            _ => ()
        }
        Ok(())
    }

    fn decode_descriptor_read(&mut self, fields: &SetupFields)
        -> Result<(), CaptureError>
    {
        let recipient = fields.type_fields.recipient();
        let desc_type = DescriptorType::from((fields.value >> 8) as u8);
        let payload = &self.current_endpoint_data()?.payload;
        let length = payload.len();
        match (recipient, desc_type) {
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
                    let configuration = Configuration::from_bytes(payload);
                    let dev_data = self.current_device_data_mut()?;
                    if let Some(config) = configuration {
                        let configurations = &mut dev_data.configurations;
                        let config_num = ConfigNum::from(
                            config.descriptor.config_value);
                        configurations.set(config_num, config);
                        dev_data.update_endpoint_details();
                    }
                }
            },
            (Recipient::Device, DescriptorType::String) => {
                if length >= 2 {
                    let string = UTF16ByteVec(payload[2..length].to_vec());
                    let dev_data = self.current_device_data_mut()?;
                    let strings = &mut dev_data.strings;
                    let string_id =
                        StringId::from((fields.value & 0xFF) as u8);
                    strings.set(string_id, string);
                }
            },
            _ => {}
        };
        Ok(())
    }

    fn decode_configuration_set(&mut self, fields: &SetupFields)
        -> Result<(), CaptureError>
    {
        let dev_data = self.current_device_data_mut()?;
        dev_data.config_number = Some(ConfigNum(fields.value.try_into()?));
        dev_data.update_endpoint_details();
        Ok(())
    }

    fn transfer_status(&mut self) -> Result<DecodeStatus, CaptureError> {
        let next = self.transaction_state.first.ok_or(IndexError)?;
        let endpoint_id = self.current_endpoint_id()?;
        let ep_data = self.current_endpoint_data()?;
        let dev_data = self.current_device_data()?;
        let (ep_type, ep_max) = dev_data.endpoint_details(ep_data.address);
        let success = self.transaction_state.completed();
        let length = self.transaction_state.payload.len() as u64;
        let short = match ep_max {
            Some(max) => length < max as u64,
            None      => false
        };
        use PID::*;
        use EndpointType::*;
        use usb::EndpointType::*;
        use Direction::*;
        Ok(match (ep_type, ep_data.last, next) {

            // A SETUP transaction starts a new control transfer.
            // Store the setup fields to interpret the request.
            (Normal(Control), _, SETUP) => {
                let setup = self.transaction_state.setup.take();
                let ep_data = self.current_endpoint_data_mut()?;
                ep_data.setup = setup;
                DecodeStatus::New
            },

            (Normal(Control), _, _) => match &ep_data.setup {
                // No control transaction is valid unless setup was done.
                None => DecodeStatus::Invalid,
                // If setup was done then valid transactions depend on the
                // contents of the setup data packet.
                Some(fields) => {
                    let with_data = fields.length != 0;
                    let direction = fields.type_fields.direction();
                    match (direction, with_data, ep_data.last, next) {

                        // If there is data to transfer, setup stage is
                        // followed by IN/OUT at data stage in the direction
                        // of the request. IN/OUT may then be repeated.
                        (In,  true, Some(SETUP), IN ) |
                        (Out, true, Some(SETUP), OUT) |
                        (In,  true, Some(IN),    IN ) |
                        (Out, true, Some(OUT),   OUT) => {
                            if success {
                                let payload =
                                    self.transaction_state.payload.clone();
                                let ep_data = self.current_endpoint_data_mut()?;
                                ep_data.payload.extend(payload);
                                // Await status stage.
                                DecodeStatus::Continue
                            } else {
                                // Retry data stage.
                                DecodeStatus::Retry
                            }
                        },

                        // If there is no data to transfer, setup stage is
                        // followed by IN/OUT at status stage in the opposite
                        // direction to the request. If there is data, then
                        // the status stage follows the data stage.
                        (In,  false, Some(SETUP), OUT) |
                        (Out, false, Some(SETUP), IN ) |
                        (In,  true,  Some(IN),    OUT) |
                        (Out, true,  Some(OUT),   IN ) => {
                            if success {
                                let fields_copy = *fields;
                                self.decode_request(fields_copy)?;
                                // Status stage complete.
                                DecodeStatus::Done
                            } else {
                                // Retry status stage.
                                DecodeStatus::Retry
                            }
                        },
                        // Any other sequence is invalid.
                        (..) => DecodeStatus::Invalid
                    }
                }
            },

            // An IN or OUT transaction on a non-control endpoint,
            // with no transfer in progress, starts a new transfer.
            // This can be either an actual transfer, or a polling
            // group used to collect NAKed transactions.
            (_, None, IN | OUT) => {
                let ep_traf = self.capture.endpoint_traffic(endpoint_id)?;
                ep_traf.data_index.push(ep_traf.total_data)?;
                if success {
                    ep_traf.total_data += length;
                }
                let ep_data = self.current_endpoint_data_mut()?;
                ep_data.last_success = success;
                if success && short {
                    // New transfer, ended immediately by a short packet.
                    DecodeStatus::Single
                } else {
                    // Either a new transfer or a new polling group.
                    DecodeStatus::New
                }
            },

            // IN or OUT may then be repeated.
            (_, Some(IN),  IN ) |
            (_, Some(OUT), OUT) => {
                let success_changed = success != ep_data.last_success;
                let ep_traf = self.capture.endpoint_traffic(endpoint_id)?;
                ep_traf.data_index.push(ep_traf.total_data)?;
                if success {
                    ep_traf.total_data += length;
                }
                if success_changed {
                    let ep_data = self.current_endpoint_data_mut()?;
                    ep_data.last_success = success;
                    if success && short {
                        // New transfer, ended immediately by a short packet.
                        DecodeStatus::Single
                    } else {
                        // Either a new transfer or a new polling group.
                        DecodeStatus::New
                    }
                } else if success {
                    // Continuing an ongoing transfer.
                    if short {
                        // A short packet ends the transfer.
                        DecodeStatus::Done
                    } else {
                        // A full-length packet continues the transfer.
                        DecodeStatus::Continue
                    }
                } else {
                    // Continuing a polling group.
                    DecodeStatus::Retry
                }
            },

            // A SOF group starts a special transfer, unless
            // one is already in progress.
            (Framing, None, SOF) => DecodeStatus::New,

            // Further SOF groups continue this transfer.
            (Framing, Some(SOF), SOF) => DecodeStatus::Continue,

            // Any other case is not a valid part of a transfer.
            _ => DecodeStatus::Invalid
        })
    }

    fn transfer_update(&mut self, transaction_id: TransactionId)
        -> Result<(), CaptureError>
    {
        match self.transfer_status()? {
            DecodeStatus::Single => {
                self.transfer_start(transaction_id, true)?;
                self.transfer_end()?;
            },
            DecodeStatus::New => {
                self.transfer_start(transaction_id, true)?;
            },
            DecodeStatus::Continue => {
                self.transfer_append(transaction_id, true)?;
            },
            DecodeStatus::Retry => {
                self.transfer_append(transaction_id, false)?;
            },
            DecodeStatus::Done => {
                self.transfer_append(transaction_id, true)?;
                self.transfer_end()?;
            },
            DecodeStatus::Invalid => {
                self.transfer_start(transaction_id, false)?;
                self.transfer_end()?;
            }
        }
        Ok(())
    }

    fn transfer_start(&mut self,
                      transaction_id: TransactionId,
                      success: bool)
        -> Result<(), CaptureError>
    {
        let endpoint_id = self.current_endpoint_id()?;
        let ep_data = self.current_endpoint_data()?;
        if ep_data.transaction_count > 0 {
            self.add_transfer_entry(endpoint_id, false)?;
        }
        let transaction_type = self.transaction_state.first;
        let ep_traf = self.capture.endpoint_traffic(endpoint_id)?;
        let ep_transaction_id = ep_traf.transaction_ids.push(transaction_id)?;
        let ep_transfer_id = ep_traf.transfer_index.push(ep_transaction_id)?;
        let ep_data = self.current_endpoint_data_mut()?;
        ep_data.transfer_id = Some(ep_transfer_id);
        ep_data.transaction_count = 1;
        if success {
            ep_data.last = transaction_type;
        } else {
            ep_data.last = None
        }
        ep_data.payload.clear();
        let transfer_start_id = self.add_transfer_entry(endpoint_id, true)?;
        self.capture.item_index.push(transfer_start_id)?;
        self.last_item_endpoint = Some(endpoint_id);
        Ok(())
    }

    fn transfer_append(&mut self,
                       transaction_id: TransactionId,
                       success: bool)
        -> Result<(), CaptureError>
    {
        let transaction_type = self.transaction_state.first;
        let endpoint_id = self.current_endpoint_id()?;
        let ep_traf = self.capture.endpoint_traffic(endpoint_id)?;
        ep_traf.transaction_ids.push(transaction_id)?;
        let ep_data = self.current_endpoint_data_mut()?;
        ep_data.transaction_count += 1;
        if success {
            ep_data.last = transaction_type;
        }
        Ok(())
    }

    fn transfer_end(&mut self)
        -> Result<(), CaptureError>
    {
        let endpoint_id = self.current_endpoint_id()?;
        let ep_data = self.current_endpoint_data()?;
        if ep_data.transaction_count > 0 {
            let transfer_end_id =
                self.add_transfer_entry(endpoint_id, false)?;
            if self.last_item_endpoint != Some(endpoint_id) {
                self.capture.item_index.push(transfer_end_id)?;
                self.last_item_endpoint = Some(endpoint_id);
            }
        }
        let ep_data = self.current_endpoint_data_mut()?;
        ep_data.transaction_count = 0;
        ep_data.last = None;
        ep_data.payload.clear();
        Ok(())
    }

    fn add_transfer_entry(&mut self,
                          endpoint_id: EndpointId,
                          start: bool)
        -> Result<TransferId, CaptureError>
    {
        let ep_data = self.current_endpoint_data()?;
        let ep_transfer_id = ep_data.transfer_id.ok_or(IndexError)?;
        self.add_endpoint_state(endpoint_id, start)?;
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id);
        entry.set_transfer_id(ep_transfer_id);
        entry.set_is_start(start);
        let transfer_id = self.capture.transfer_index.push(&entry)?;
        Ok(transfer_id)
    }

    fn add_endpoint_state(&mut self, endpoint_id: EndpointId, start: bool)
        -> Result<EndpointStateId, CaptureError>
    {
        let endpoint_count = self.capture.endpoints.len() as usize;
        for i in 0..endpoint_count {
            use EndpointState::*;
            self.last_endpoint_state[i] = {
                let same = i == endpoint_id.value as usize;
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
        let state_offset = self.capture.endpoint_states.append(last_state)?;
        let state_id = self.capture.endpoint_state_index.push(state_offset)?;
        Ok(state_id)
    }
}
