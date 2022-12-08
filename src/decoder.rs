use crate::usb::{self, prelude::*};
use crate::capture::prelude::*;
use crate::hybrid_index::Number;
use crate::vec_map::{VecMap, Key};

use CaptureError::IndexError;

impl PID {
    fn from_packet(packet: &[u8]) -> Result<PID, CaptureError> {
        let first_byte = packet
            .first()
            .ok_or_else(||
                IndexError(String::from(
                    "Packet is empty, cannot retrieve PID")))?;
        Ok(PID::from(*first_byte))
    }
}

struct EndpointData {
    device_id: DeviceId,
    address: EndpointAddr,
    active: Option<TransferState>,
    ended: Option<EndpointTransferId>,
    last_success: bool,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
    pending_payload: Option<Vec<u8>>,
}

struct TransferState {
    id: EndpointTransferId,
    first: PID,
    last: Option<PID>,
}

#[derive(PartialEq, Eq)]
enum TransferStatus {
    Single,
    New,
    Continue,
    Retry,
    Done,
    Invalid
}

#[derive(PartialEq, Eq)]
enum TransactionStatus {
    New,
    Continue,
    Done,
    Fail,
    Invalid
}

impl EndpointData {
    fn new(device_id: DeviceId, address: EndpointAddr) -> EndpointData {
        EndpointData {
            address,
            device_id,
            active: None,
            ended: None,
            last_success: false,
            setup: None,
            payload: Vec::new(),
            pending_payload: None,
        }
    }
}

#[derive(Default)]
struct TransactionState {
    id: Option<TransactionId>,
    first: Option<PID>,
    last: Option<PID>,
    split_sc: Option<StartComplete>,
    split_ep_type: Option<usb::EndpointType>,
    split_pid: Option<PID>,
    count: u64,
    endpoint_id: Option<EndpointId>,
    setup: Option<SetupFields>,
    payload: Option<Vec<u8>>,
}

impl TransactionState {
    pub fn status(&mut self, packet: &[u8])
        -> Result<TransactionStatus, CaptureError>
    {
        let next = PID::from_packet(packet)?;
        use PID::*;
        use TransactionStatus::*;
        Ok(match (self.first, self.last, next) {

            // SPLIT starts a new transaction.
            (_, _, SPLIT) => New,

            // Valid SPLIT transactions depend on SC flag and EP type.
            (Some(SPLIT), Some(last), _) => {
                let sc = self.split_sc
                    .ok_or_else(|| IndexError(String::from(
                        "SPLIT start/complete flag not set")))?;
                let ep_type = self.split_ep_type
                    .ok_or_else(|| IndexError(String::from(
                        "SPLIT endpoint type not set")))?;
                use StartComplete::*;
                use usb::EndpointType::*;
                match (ep_type, sc, last, next) {

                    // Valid split transactions for control/bulk endpoints:

                    // SSPLIT->SETUP/OUT->DATA0/1->ACK/NAK.
                    (Bulk,    Start, SPLIT, OUT      ) => Continue,
                    (Control, Start, SPLIT, SETUP|OUT) => Continue,
                    (Control, Start, SETUP, DATA0    )
                        if packet.len() == 11 => Continue,
                    (Bulk|Control, Start, OUT, DATA0|DATA1) => Continue,
                    (Bulk|Control, Start, DATA0|DATA1, ACK) => Done,
                    (Bulk|Control, Start, DATA0|DATA1, NAK) => Fail,
                    // CSPLIT->SETUP/OUT->ACK/NAK/NYET/STALL.
                    (Bulk,    Complete, SPLIT,     OUT      ) => Continue,
                    (Bulk,    Complete, SETUP,     ACK|NYET ) => Done,
                    (Bulk,    Complete, OUT,       NAK|STALL) => Fail,
                    (Control, Complete, SPLIT,     SETUP|OUT) => Continue,
                    (Control, Complete, SETUP|OUT, ACK|NYET ) => Done,
                    (Control, Complete, SETUP|OUT, NAK|STALL) => Fail,
                    // SSPLIT->IN->ACK/NAK.
                    (Control|Bulk, Start, SPLIT, IN ) => Continue,
                    (Control|Bulk, Start, IN,    ACK) => Done,
                    (Control|Bulk, Start, IN,    NAK) => Fail,
                    // CSPLIT->IN->DATA0/DATA1/NAK/NYET/STALL.
                    (Control|Bulk, Complete, SPLIT, IN) => Continue,
                    (Control|Bulk, Complete, IN,    DATA0|DATA1|NYET) => Done,
                    (Control|Bulk, Complete, IN,    NAK|STALL       ) => Fail,

                    // Valid split transactions for interrupt endpoints:

                    // SSPLIT->OUT->DATA0/1
                    (Interrupt, Start, SPLIT, OUT        ) => Continue,
                    (Interrupt, Start, OUT,   DATA0|DATA1) => Done,
                    // CSPLIT->OUT->ACK/NAK/NYET/STALL/ERR.
                    (Interrupt, Complete, SPLIT, OUT          ) => Continue,
                    (Interrupt, Complete, OUT,   ACK|NYET     ) => Done,
                    (Interrupt, Complete, OUT,   NAK|STALL|ERR) => Fail,
                    // SSPLIT->IN.
                    (Interrupt, Start, SPLIT, IN) => Done,
                    // CSPLIT->IN->DATA0/DATA1/MDATA/NAK/NYET/STALL/ERR.
                    (Interrupt, Complete, SPLIT, IN) => Continue,
                    (Interrupt, Complete, IN, DATA0|DATA1|MDATA|NYET) => Done,
                    (Interrupt, Complete, IN, NAK|STALL|ERR) => Fail,

                    // Valid split transactions for isochronous endpoints:

                    // SSPLIT->OUT->DATA0
                    (Isochronous, Start, SPLIT, OUT) => Continue,
                    (Isochronous, Start, OUT, DATA0) => Done,
                    // SSPLIT->IN.
                    (Isochronous, Start, SPLIT, IN) => Done,
                    // CSPLIT->IN->DATA0/MDATA/NYET/ERR.
                    (Isochronous, Complete, SPLIT, IN) => Continue,
                    (Isochronous, Complete, IN, DATA0|MDATA|NYET|ERR) => Done,

                    // Any other combination is invalid.
                    (..) => Invalid,
                }
            },

            // Unless handled above after SPLIT, SETUP/IN/OUT/PING always
            // start a new transaction.
            (_, _, SETUP | IN | OUT | PING) => New,

            // SOF when there is no existing transaction starts a new
            // "transaction" representing an idle period on the bus.
            (_, None, SOF) => New,
            // Additional SOFs extend this "transaction", more may follow.
            (_, Some(SOF), SOF) => Continue,

            // A malformed packet is grouped with previous malformed packets.
            (_, Some(Malformed), Malformed) => Continue,
            // If preceded by any other packet, it starts a new transaction.
            (_, _, Malformed) => New,

            // SETUP must be followed by DATA0 with setup data.
            (_, Some(SETUP), DATA0) if packet.len() == 11 => Continue,

            // ACK then completes the transaction.
            (Some(SETUP), Some(DATA0), ACK) => Done,

            // IN may be followed by NAK or STALL, completing transaction.
            (_, Some(IN), NAK | STALL) => Fail,
            // IN or OUT may be followed by DATA0 or DATA1, wait for status.
            (_, Some(IN | OUT), DATA0 | DATA1) if packet.len() >= 3 => Continue,
            // An ACK or NYET then completes the transaction.
            (Some(IN | OUT), Some(DATA0 | DATA1), ACK | NYET) => Done,
            // OUT may also be completed by NAK or STALL.
            (Some(OUT), Some(DATA0 | DATA1), NAK | STALL) => Fail,

            // PING may be followed by ACK, NAK or STALL.
            (Some(PING), Some(PING), ACK) => Done,
            (Some(PING), Some(PING), NAK | STALL) => Fail,

            // Any other case is not a valid part of a transaction.
            _ => Invalid,
        })
    }

    fn extract_payload(&mut self, packet: &[u8]) {
        use PID::*;
        match (self.first, self.split_pid, PID::from(packet[0])) {
            (Some(SPLIT), Some(SETUP), DATA0) |
            (Some(SETUP), None, DATA0) => {
                self.setup = Some(SetupFields::from_data_packet(packet));
            },
            (_, _, DATA0 | DATA1) => {
                let range = 1 .. (packet.len() - 2);
                self.payload = Some(packet[range].to_vec());
            }
            (..) => {},
        }
    }
}

impl EndpointData {
    fn transfer_status(&mut self,
                       capture: &mut Capture,
                       endpoint_id: EndpointId,
                       transaction: &mut TransactionState,
                       next: PID,
                       success: bool,
                       complete: bool)
        -> Result<TransferStatus, CaptureError>
    {
        let dev_data = capture.device_data_mut(&self.device_id)?;
        let (ep_type, ep_max) = dev_data.endpoint_details(self.address);
        let split_sc = transaction.split_sc;
        let pending_payload = self.pending_payload.take();
        let payload = transaction.payload.take().or(pending_payload);
        let length = payload.as_ref().map_or(0, |vec| vec.len()) as u64;
        let short = match (&payload, ep_max) {
            (Some(payload), Some(max)) => payload.len() < max,
            (..) => false,
        };
        use PID::*;
        use EndpointType::{Normal, Framing};
        use usb::EndpointType::*;
        use Direction::*;
        use TransferStatus::*;
        use StartComplete::*;
        Ok(match (ep_type, &self.active, next) {

            // A SETUP transaction starts a new control transfer.
            // Store the setup fields to interpret the request.
            (Normal(Control), _, SETUP) => {
                match split_sc {
                    None | Some(Start) => {
                        self.setup = transaction.setup;
                        New
                    },
                    Some(Complete) => Continue,
                }
            },

            (Normal(Control),
             Some(TransferState {
                last: Some(last), ..}), _) => match &self.setup
            {
                // No control transaction is valid unless setup was done.
                None => Invalid,
                // If setup was done then valid transactions depend on the
                // contents of the setup data packet.
                Some(fields) => {
                    let with_data = fields.length != 0;
                    let direction = fields.type_fields.direction();
                    match (direction, with_data, last, next) {

                        // If there is data to transfer, setup stage is
                        // followed by IN/OUT at data stage in the direction
                        // of the request. IN/OUT may then be repeated.
                        (In,  true, SETUP, IN ) |
                        (Out, true, SETUP, OUT) |
                        (In,  true, IN,    IN ) |
                        (Out, true, OUT,   OUT) => {
                            if success {
                                if (split_sc, next) == (Some(Complete), OUT) {
                                    self.pending_payload = payload;
                                } else if let Some(data) = payload {
                                    self.payload.extend(data);
                                }
                                // Await status stage.
                                Continue
                            } else {
                                // Retry data stage.
                                Retry
                            }
                        },

                        // If there is no data to transfer, setup stage is
                        // followed by IN/OUT at status stage in the opposite
                        // direction to the request. If there is data, then
                        // the status stage follows the data stage.
                        (In,  false, SETUP, OUT) |
                        (Out, false, SETUP, IN ) |
                        (In,  true,  IN,    OUT) |
                        (Out, true,  OUT,   IN ) => {
                            if success && complete {
                                dev_data.decode_request(
                                    fields, &self.payload)?;
                                // Status stage complete.
                                Done
                            } else {
                                // Retry status stage, or await completion.
                                Retry
                            }
                        },

                        // PING is valid at any time that OUT would be.
                        (Out, true,  SETUP, PING) |
                        (Out, true,  OUT,   PING) |
                        (In,  false, SETUP, PING) |
                        (In,  true,  IN,    PING) => Retry,

                        // Any other sequence is invalid.
                        (..) => Invalid
                    }
                }
            },

            // An IN or OUT transaction on a non-control endpoint,
            // with no transfer in progress, starts a new transfer.
            // This can be either an actual transfer, or a polling
            // group used to collect NAKed transactions.
            (_, None, IN | OUT) => {
                let ep_traf = capture.endpoint_traffic(endpoint_id)?;
                ep_traf.data_index.push(ep_traf.total_data)?;
                if success {
                    if split_sc == Some(Complete) && next == OUT {
                        self.pending_payload = payload;
                    } else {
                        ep_traf.total_data += length;
                    }
                }
                if complete {
                    self.last_success = success;
                    if success && short {
                        // New transfer, ended immediately by a short packet.
                        Single
                    } else {
                        // Either a new transfer or a new polling group.
                        New
                    }
                } else {
                    // Wait for split completion.
                    New
                }
            },

            // IN or OUT may then be repeated.
            (_, Some(TransferState { first: IN,  ..}), IN) |
            (_, Some(TransferState { first: OUT, ..}), OUT) => {
                let ep_traf = capture.endpoint_traffic(endpoint_id)?;
                ep_traf.data_index.push(ep_traf.total_data)?;
                if success {
                    if split_sc == Some(Complete) && next == OUT {
                        self.pending_payload = payload;
                    } else if complete {
                        ep_traf.total_data += length;
                    }
                }
                if complete {
                    let success_changed = success != self.last_success;
                    self.last_success = success;
                    if success_changed {
                        if success && short {
                            // New transfer, ended immediately by a short packet.
                            Single
                        } else {
                            // Either a new transfer or a new polling group.
                            New
                        }
                    } else if success {
                        // Continuing an ongoing transfer.
                        if short {
                            // A short packet ends the transfer.
                            Done
                        } else {
                            // A full-length packet continues the transfer.
                            Continue
                        }
                    } else {
                        // Continuing a polling group.
                        Retry
                    }
                } else {
                    // Wait for split completion.
                    Retry
                }
            },

            // OUT may also be followed by PING.
            (_, Some(TransferState { first: OUT, .. }), PING) => Retry,

            // A SOF group starts a special transfer, unless
            // one is already in progress.
            (Framing, None, SOF) => New,

            // Further SOF groups continue this transfer.
            (Framing, _, SOF) => Continue,

            // Any other case is not a valid part of a transfer.
            _ => Invalid
        })
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

pub struct Decoder {
    device_index: VecMap<DeviceAddr, DeviceId>,
    endpoint_index: VecMap<EndpointKey, EndpointId>,
    endpoint_data: VecMap<EndpointId, EndpointData>,
    last_endpoint_state: Vec<u8>,
    last_item_endpoint: Option<EndpointId>,
    transaction_state: TransactionState,
}

impl Default for Decoder {
    fn default() -> Decoder {
        let mut decoder = Decoder {
            device_index: VecMap::new(),
            endpoint_index: VecMap::new(),
            endpoint_data: VecMap::new(),
            last_endpoint_state: Vec::new(),
            last_item_endpoint: None,
            transaction_state: TransactionState::default(),
        };
        let default_addr = DeviceAddr(0);
        let default_id = DeviceId::from(0);
        decoder.device_index.set(default_addr, default_id);
        for (ep_id, ep_num) in [
            (INVALID_EP_ID, INVALID_EP_NUM),
            (FRAMING_EP_ID, FRAMING_EP_NUM)]
        {
            decoder.endpoint_data.set(
                ep_id,
                EndpointData::new(
                    default_id,
                    EndpointAddr::from_parts(ep_num, Direction::Out)
                )
            );
            let ep_state = EndpointState::Idle as u8;
            decoder.last_endpoint_state.push(ep_state);
        }
        decoder
    }
}

impl Decoder {
    pub fn handle_raw_packet(&mut self, capture: &mut Capture, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let data_id = capture.packet_data.append(packet)?;
        let packet_id = capture.packet_index.push(data_id)?;
        self.transaction_update(capture, packet_id, packet)?;
        Ok(())
    }

    pub fn finish(&mut self, capture: &mut Capture)
        -> Result<(), CaptureError>
    {
        self.transaction_end(capture, false, false)?;
        capture.finish();
        Ok(())
    }

    pub fn token_endpoint(&mut self, capture: &mut Capture, pid: PID, token: &TokenFields)
        -> Result<EndpointId, CaptureError>
    {
        let dev_addr = token.device_address();
        let ep_num = token.endpoint_number();
        let direction = match (ep_num.0, pid) {
            (0, _)         => Direction::Out,
            (_, PID::IN)   => Direction::In,
            (_, PID::OUT)  => Direction::Out,
            (_, PID::PING) => Direction::Out,
            _ => return Err(IndexError(format!(
                "PID {pid} does not indicate a direction")))
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
                    capture, key.dev_addr, key.ep_num, key.direction)?;
                self.endpoint_index.set(key, id);
                id
            }
        })
    }

    fn packet_endpoint(&mut self, capture: &mut Capture, packet: &[u8])
        -> Result<EndpointId, CaptureError>
    {
        let pid = PID::from_packet(packet)?;
        Ok(match PacketFields::from_packet(packet) {
            PacketFields::SOF(_) => FRAMING_EP_ID,
            PacketFields::Token(token) =>
                self.token_endpoint(capture, pid, &token)?,
            _ => INVALID_EP_ID,
        })
    }

    fn transaction_update(&mut self, capture: &mut Capture, packet_id: PacketId, packet: &[u8])
        -> Result<(), CaptureError>
    {
        use TransactionStatus::*;
        use StartComplete::*;
        let status = self.transaction_state.status(packet)?;
        let success = status != Fail;
        let complete = match self.transaction_state.split_sc {
            None => true,
            Some(Start) => false,
            Some(Complete) => true,
        };
        if status != Invalid {
            self.transaction_state.extract_payload(packet);
        }
        match status {
            New => {
                self.transaction_start(capture, packet_id, packet)?;
            },
            Continue => {
                self.transaction_append(capture, packet)?;
            },
            Done | Fail => {
                self.transaction_append(capture, packet)?;
                self.transaction_end(capture, success, complete)?;
            },
            Invalid => {
                self.transaction_start(capture, packet_id, packet)?;
                self.transaction_end(capture, false, false)?;
            },
        };
        Ok(())
    }

    fn transaction_start(&mut self, capture: &mut Capture, packet_id: PacketId, packet: &[u8])
        -> Result<(), CaptureError>
    {
        self.transaction_end(capture, false, false)?;
        self.transaction_state = TransactionState::default();
        let pid = PID::from_packet(packet)?;
        let state = &mut self.transaction_state;
        state.id = Some(capture.transaction_index.push(packet_id)?);
        state.count = 1;
        state.first = Some(pid);
        state.last = state.first;
        if pid == PID::SPLIT {
            let split = SplitFields::from_packet(packet);
            self.transaction_state.split_sc = Some(split.sc());
            self.transaction_state.split_ep_type =
                Some(split.endpoint_type());
        } else {
            self.transaction_state.endpoint_id =
                Some(self.packet_endpoint(capture, packet)?);
        }
        Ok(())
    }

    fn transaction_append(&mut self, capture: &mut Capture, packet: &[u8])
        -> Result<(), CaptureError>
    {
        let pid = PID::from_packet(packet)?;
        self.transaction_state.count += 1;
        if self.transaction_state.last == Some(PID::SPLIT) {
            let endpoint_id = self.packet_endpoint(capture, packet)?;
            self.transaction_state.endpoint_id = Some(endpoint_id);
            self.transaction_state.split_pid = Some(pid);
            let ep_data = self.endpoint_data(endpoint_id)?;
            if let Some(ep_type) = self.transaction_state.split_ep_type {
                let dev_data = capture.device_data_mut(&ep_data.device_id)?;
                dev_data.set_endpoint_type(ep_data.address, ep_type);
            }
        }
        self.transaction_state.last = Some(pid);
        Ok(())
    }

    fn transaction_end(&mut self,
                       capture: &mut Capture,
                       success: bool,
                       complete: bool)
        -> Result<(), CaptureError>
    {
        if let Some(transaction_id) = self.transaction_state.id.take() {
            self.transfer_update(capture, transaction_id, success, complete)?;
        }
        Ok(())
    }

    fn add_device(&mut self, capture: &mut Capture, address: DeviceAddr)
        -> Result<DeviceId, CaptureError>
    {
        let device = Device { address };
        let device_id = capture.devices.push(&device)?;
        self.device_index.set(address, device_id);
        capture.device_data.set(device_id, DeviceData::default());
        Ok(device_id)
    }

    fn add_endpoint(&mut self,
                    capture: &mut Capture,
                    dev_addr: DeviceAddr,
                    number: EndpointNum,
                    direction: Direction)
        -> Result<EndpointId, CaptureError>
    {
        let device_id = match self.device_index.get(dev_addr) {
            Some(id) => *id,
            None => self.add_device(capture, dev_addr)?
        };
        let mut endpoint = Endpoint::default();
        endpoint.set_device_id(device_id);
        endpoint.set_device_address(dev_addr);
        endpoint.set_number(number);
        endpoint.set_direction(direction);
        let endpoint_id = capture.endpoints.push(&endpoint)?;
        let address = EndpointAddr::from_parts(number, direction);
        self.endpoint_data.set(
            endpoint_id,
            EndpointData::new(device_id, address));
        capture.endpoint_traffic.set(endpoint_id, EndpointTraffic::new()?);
        let ep_state = EndpointState::Idle as u8;
        self.last_endpoint_state.push(ep_state);
        Ok(endpoint_id)
    }

    fn endpoint_data(&self, endpoint_id: EndpointId)
        -> Result<&EndpointData, CaptureError> {
        self.endpoint_data.get(endpoint_id).ok_or_else(||
            IndexError(format!(
                "Decoder has no data for current endpoint ID {endpoint_id}")))
    }

    fn endpoint_data_mut(&mut self, endpoint_id: EndpointId)
        -> Result<&mut EndpointData, CaptureError>
    {
        self.endpoint_data.get_mut(endpoint_id).ok_or_else(||
            IndexError(format!(
                "Decoder has no data for current endpoint ID {endpoint_id}")))
    }

    fn transfer_update(&mut self,
                       capture: &mut Capture,
                       transaction_id: TransactionId,
                       success: bool,
                       complete: bool)
        -> Result<(), CaptureError>
    {
        use TransferStatus::*;
        let mut next = self.transaction_state.first.ok_or_else(||
            IndexError(String::from(
                "Transaction state has no first PID set")))?;
        if next == PID::SPLIT {
            next = self.transaction_state.split_pid.ok_or_else(||
                IndexError(String::from(
                    "Transaction state has no split PID set")))?;
        }
        let endpoint_id = self.transaction_state.endpoint_id.ok_or_else(||
            IndexError(String::from(
                "Transaction state has endpoint ID set")))?;
        let ep_data = self.endpoint_data.get_mut(endpoint_id).ok_or_else(||
            IndexError(format!(
                "Decoder has no data for endpoint ID {endpoint_id}")))?;
        match ep_data.transfer_status(capture, endpoint_id,
                                      &mut self.transaction_state,
                                      next, success, complete)?
        {
            Single => {
                self.transfer_start(capture, endpoint_id,
                                    transaction_id, next, true)?;
                self.transfer_end(capture, endpoint_id)?;
            },
            New => {
                self.transfer_start(capture, endpoint_id,
                                    transaction_id, next, true)?;
            },
            Continue => {
                self.transfer_append(capture, endpoint_id,
                                     transaction_id, next, true)?;
            },
            Retry => {
                self.transfer_append(capture, endpoint_id,
                                     transaction_id, next, false)?;
            },
            Done => {
                self.transfer_append(capture, endpoint_id,
                                     transaction_id, next, true)?;
                self.transfer_end(capture, endpoint_id)?;
            },
            Invalid => {
                self.transfer_start(capture, endpoint_id,
                                    transaction_id, next, false)?;
                self.transfer_end(capture, endpoint_id)?;
            }
        }
        Ok(())
    }

    fn transfer_start(&mut self,
                      capture: &mut Capture,
                      endpoint_id: EndpointId,
                      transaction_id: TransactionId,
                      transaction_type: PID,
                      done: bool)
        -> Result<(), CaptureError>
    {
        let ep_data = self.endpoint_data_mut(endpoint_id)?;
        if let Some(transfer) = ep_data.active.take() {
            ep_data.ended = Some(transfer.id);
            self.add_transfer_entry(capture, endpoint_id, transfer.id, false)?;
        }
        let ep_traf = capture.endpoint_traffic(endpoint_id)?;
        let ep_transaction_id = ep_traf.transaction_ids.push(transaction_id)?;
        let ep_transfer_id = ep_traf.transfer_index.push(ep_transaction_id)?;
        let ep_data = self.endpoint_data_mut(endpoint_id)?;
        ep_data.active = Some(
            TransferState {
                id: ep_transfer_id,
                first: transaction_type,
                last: if done { Some(transaction_type) } else { None },
            }
        );
        ep_data.payload.clear();
        let transfer_start_id = self.add_transfer_entry(capture, endpoint_id,
                                                        ep_transfer_id, true)?;
        self.add_item(capture, endpoint_id, transfer_start_id)?;
        Ok(())
    }

    fn transfer_append(&mut self,
                       capture: &mut Capture,
                       endpoint_id: EndpointId,
                       transaction_id: TransactionId,
                       transaction_type: PID,
                       done: bool)
        -> Result<(), CaptureError>
    {
        let ep_data = self.endpoint_data_mut(endpoint_id)?;
        if let Some(transfer) = &mut ep_data.active {
            let ep_traf = capture.endpoint_traffic(endpoint_id)?;
            ep_traf.transaction_ids.push(transaction_id)?;
            if done {
                transfer.last = Some(transaction_type);
            }
        } else {
            self.transfer_start(capture, endpoint_id, transaction_id,
                                transaction_type, done)?;
        }
        Ok(())
    }

    fn transfer_end(&mut self,
                    capture: &mut Capture,
                    endpoint_id: EndpointId)
        -> Result<(), CaptureError>
    {
        let ep_data = self.endpoint_data_mut(endpoint_id)?;
        if let Some(transfer) = ep_data.active.take() {
            ep_data.ended = Some(transfer.id);
            let transfer_end_id =
                self.add_transfer_entry(capture, endpoint_id,
                                        transfer.id, false)?;
            if self.last_item_endpoint != Some(endpoint_id) {
                self.add_item(capture, endpoint_id, transfer_end_id)?;
            }
        }
        Ok(())
    }

    fn add_transfer_entry(&mut self,
                          capture: &mut Capture,
                          endpoint_id: EndpointId,
                          ep_transfer_id: EndpointTransferId,
                          start: bool)
        -> Result<TransferId, CaptureError>
    {
        self.add_endpoint_state(capture, endpoint_id, start)?;
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id);
        entry.set_transfer_id(ep_transfer_id);
        entry.set_is_start(start);
        let transfer_id = capture.transfer_index.push(&entry)?;
        Ok(transfer_id)
    }

    fn add_endpoint_state(&mut self,
                          capture: &mut Capture,
                          endpoint_id: EndpointId,
                          start: bool)
        -> Result<TransferId, CaptureError>
    {
        let endpoint_count = capture.endpoints.len() as usize;
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
        let state_offset = capture.endpoint_states.append(last_state)?;
        let state_id = capture.endpoint_state_index.push(state_offset)?;
        Ok(state_id)
    }

    fn add_item(&mut self, capture: &mut Capture, endpoint_id: EndpointId, transfer_id: TransferId)
        -> Result<TrafficItemId, CaptureError>
    {
        let item_id = capture.item_index.push(transfer_id)?;
        self.last_item_endpoint = Some(endpoint_id);

        // Look for ended transfers which still need to be linked to an item.
        let endpoint_count = capture.endpoints.len();
        for i in 0..endpoint_count {
            let endpoint_id = EndpointId::from_u64(i);
            let ep_data = self.endpoint_data
                .get_mut(endpoint_id)
                .ok_or_else(|| IndexError(
                    format!("Endpoint {i} has no associated data")))?;
            if let Some(ep_transfer_id) = ep_data.ended.take() {
                // This transfer has ended and is not yet linked to an item.
                let ep_traf = capture.endpoint_traffic(endpoint_id)?;
                assert!(ep_traf.end_index.push(item_id)? == ep_transfer_id);
            }
        }

        Ok(item_id)
    }
}
