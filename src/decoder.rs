use std::sync::atomic::Ordering::Release;
use std::sync::Arc;

use anyhow::{Context, Error, bail};

use crate::capture::prelude::*;
use crate::rcu::SingleWriterRcu;
use crate::usb::{self, prelude::*};
use crate::vec_map::{VecMap, Key};

impl PID {
    fn from_packet(packet: &[u8]) -> Result<PID, Error> {
        let first_byte = packet
            .first()
            .context("Packet is empty, cannot retrieve PID")?;
        Ok(PID::from(*first_byte))
    }
}

struct EndpointData {
    device_id: DeviceId,
    address: EndpointAddr,
    writer: EndpointWriter,
    early_start: Option<EndpointTransferId>,
    active: Option<TransferState>,
    ended: Option<EndpointTransferId>,
    last_success: bool,
    setup: Option<SetupFields>,
    payload: Vec<u8>,
    pending_payload: Option<(Vec<u8>, EndpointTransactionId)>,
    total_data: u64,
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
    Retry,
    Done,
    Fail,
    Invalid
}

impl EndpointData {
    fn new(device_id: DeviceId,
           address: EndpointAddr,
           writer: EndpointWriter)
        -> EndpointData
    {
        EndpointData {
            address,
            device_id,
            writer,
            early_start: None,
            active: None,
            ended: None,
            last_success: false,
            setup: None,
            payload: Vec::new(),
            pending_payload: None,
            total_data: 0,
        }
    }
}

enum TransactionStyle {
    Simple(PID),
    Split(StartComplete, usb::EndpointType, Option<PID>),
}

struct TransactionState {
    style: TransactionStyle,
    id: TransactionId,
    last: PID,
    endpoint_id: Option<EndpointId>,
    ep_transaction_id: Option<EndpointTransactionId>,
    setup: Option<SetupFields>,
    payload: Option<Vec<u8>>,
}

fn transaction_status(state: &Option<TransactionState>, packet: &[u8])
    -> Result<TransactionStatus, Error>
{
    let next = PID::from_packet(packet)?;
    use PID::*;
    use TransactionStatus::*;
    use TransactionStyle::*;
    use StartComplete::*;
    use usb::EndpointType::*;
    Ok(match state {
        None => match next {
            // Tokens may start a new transaction.
            SOF | SETUP | IN | OUT | PING | SPLIT => New,
            // Malformed packets start a group.
            Malformed => New,
            // Others are not valid as the start of a transaction.
            _ => Invalid,
        },
        Some(TransactionState {
            style: Simple(first),
            last, ..}) =>
        {
            match (first, last, next) {
                // These tokens always start a new transaction.
                (.., SETUP | IN | OUT | PING | SPLIT) => New,

                // SOFs and malformed packets attach to existing groups.
                (_, SOF, SOF) => Continue,
                (_, Malformed, Malformed) => Continue,

                // If not after an existing group they start a new group.
                (.., SOF | Malformed) => New,

                // SETUP must be followed by DATA0 with setup data.
                (_, SETUP, DATA0) if packet.len() == 11 => Continue,

                // ACK then completes the transaction.
                (SETUP, DATA0, ACK) => Done,

                // IN may be followed by NAK or STALL, failing transaction.
                (_, IN, NAK | STALL) => Fail,
                // IN or OUT may be followed by DATA0 or DATA1.
                (_, IN | OUT, DATA0 | DATA1) if packet.len() >= 3 => Continue,
                // An ACK or NYET then completes the transaction.
                (IN | OUT, DATA0 | DATA1, ACK | NYET) => Done,
                // OUT may also be completed by NAK or STALL.
                (OUT, DATA0 | DATA1, NAK | STALL) => Fail,

                // PING may be followed by ACK, NAK or STALL.
                (_, PING, ACK) => Done,
                (_, PING, NAK | STALL) => Fail,

                // Any other case is not a valid part of a transaction.
                _ => Invalid,
            }
        },
        Some(TransactionState {
            style: Split(sc, ep_type, ..),
            last, .. }) =>
        {
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
                (Bulk,    Complete, SETUP,     ACK      ) => Done,
                (Bulk,    Complete, SETUP,     NYET     ) => Retry,
                (Bulk,    Complete, OUT,       NAK|STALL) => Fail,
                (Control, Complete, SPLIT,     SETUP|OUT) => Continue,
                (Control, Complete, SETUP|OUT, ACK      ) => Done,
                (Control, Complete, SETUP|OUT, NYET     ) => Retry,
                (Control, Complete, SETUP|OUT, NAK|STALL) => Fail,
                // SSPLIT->IN->ACK/NAK.
                (Control|Bulk, Start, SPLIT, IN ) => Continue,
                (Control|Bulk, Start, IN,    ACK) => Done,
                (Control|Bulk, Start, IN,    NAK) => Fail,
                // CSPLIT->IN->DATA0/DATA1/NAK/NYET/STALL.
                (Control|Bulk, Complete, SPLIT, IN) => Continue,
                (Control|Bulk, Complete, IN,    DATA0|DATA1) => Done,
                (Control|Bulk, Complete, IN,    NYET       ) => Retry,
                (Control|Bulk, Complete, IN,    NAK|STALL  ) => Fail,

                // Valid split transactions for interrupt endpoints:

                // SSPLIT->OUT->DATA0/1
                (Interrupt, Start, SPLIT, OUT        ) => Continue,
                (Interrupt, Start, OUT,   DATA0|DATA1) => Done,
                // CSPLIT->OUT->ACK/NAK/NYET/STALL/ERR.
                (Interrupt, Complete, SPLIT, OUT          ) => Continue,
                (Interrupt, Complete, OUT,   ACK          ) => Done,
                (Interrupt, Complete, OUT,   NYET         ) => Retry,
                (Interrupt, Complete, OUT,   NAK|STALL|ERR) => Fail,
                // SSPLIT->IN.
                (Interrupt, Start, SPLIT, IN) => Done,
                // CSPLIT->IN->DATA0/DATA1/MDATA/NAK/NYET/STALL/ERR.
                (Interrupt, Complete, SPLIT, IN) => Continue,
                (Interrupt, Complete, IN, DATA0|DATA1|MDATA) => Done,
                (Interrupt, Complete, IN, NYET             ) => Retry,
                (Interrupt, Complete, IN, NAK|STALL|ERR    ) => Fail,

                // Valid split transactions for isochronous endpoints:

                // SSPLIT->OUT->DATA0
                (Isochronous, Start, SPLIT, OUT) => Continue,
                (Isochronous, Start, OUT, DATA0) => Done,
                // SSPLIT->IN.
                (Isochronous, Start, SPLIT, IN) => Done,
                // CSPLIT->IN->DATA0/MDATA/NYET/ERR.
                (Isochronous, Complete, SPLIT, IN) => Continue,
                (Isochronous, Complete, IN, DATA0|MDATA) => Done,
                (Isochronous, Complete, IN, NYET       ) => Retry,
                (Isochronous, Complete, IN, ERR        ) => Fail,

                // Any other combination is invalid.
                (..) => Invalid,
            }
        },
    })
}

impl TransactionState {
    fn start_pid(&self) -> Result<PID, Error> {
        use TransactionStyle::*;
        match self.style {
            Simple(pid) | Split(.., Some(pid)) => Ok(pid),
            _ => bail!("Transaction state has no token PID")
        }
    }

    fn endpoint_id(&self) -> Result<EndpointId, Error> {
        self.endpoint_id.context("Transaction state has no endpoint ID")
    }

    fn extract_payload(&mut self, packet: &[u8]) {
        use PID::*;
        use TransactionStyle::*;
        use usb::EndpointType::*;
        use StartComplete::*;
        match (&self.style, PID::from(packet[0])) {
            (Simple(SETUP), DATA0) |
            (Split(Start, Control, Some(SETUP)), DATA0) => {
                self.setup = Some(SetupFields::from_data_packet(packet));
            },
            (_, DATA0 | DATA1) => {
                let range = 1 .. (packet.len() - 2);
                self.payload = Some(packet[range].to_vec());
            }
            (..) => {},
        }
    }
}

enum TransactionSideEffect {
    NoEffect,
    PendingData(Vec<u8>),
    IndexData(usize, Option<EndpointTransactionId>)
}

impl EndpointData {
    fn transfer_status(&mut self,
                       dev_data: &DeviceData,
                       transaction: &mut TransactionState,
                       success: bool,
                       complete: bool)
        -> Result<(TransferStatus, TransactionSideEffect), Error>
    {
        use TransactionStyle::*;
        let (ep_type, ep_max) = dev_data.endpoint_details(self.address);
        let split_sc = match transaction.style {
            Simple(..) => None,
            Split(sc, ..) => Some(sc),
        };
        let next = transaction.start_pid()?;
        let pending_payload = self.pending_payload.take();
        let (payload, id) = match transaction.payload.take() {
            Some(payload) => (Some(payload), None),
            None => match pending_payload {
                Some((payload, id)) => (Some(payload), Some(id)),
                None => (None, None)
            }
        };
        let length = payload.as_ref().map_or(0, |vec| vec.len());
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
        use TransactionSideEffect::*;
        let mut effect = NoEffect;
        let status = match (ep_type, &self.active, next) {

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
                                if let Some(data) = payload {
                                    if (split_sc, next) ==
                                        (Some(Start), OUT)
                                    {
                                        effect = PendingData(data);
                                    } else {
                                        self.payload.extend(data);
                                        effect = IndexData(length, id);
                                    }
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
                if success {
                    if let Some(data) = payload {
                        if split_sc == Some(Start) && next == OUT {
                            effect = PendingData(data);
                        } else {
                            effect = IndexData(length, id);
                        }
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
                if success {
                    if let Some(data) = payload {
                        if split_sc == Some(Start) && next == OUT {
                            effect = PendingData(data);
                        } else if complete {
                            effect = IndexData(length, id);
                        }
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
        };
        Ok((status, effect))
    }

    fn apply_effect(&mut self,
                    transaction: &TransactionState,
                    effect: TransactionSideEffect)
        -> Result<(), Error>
    {
        use TransactionSideEffect::*;
        match effect {
            NoEffect => {},
            PendingData(data) => {
                let ep_transaction_id = transaction.ep_transaction_id
                    .context("Pending data but no endpoint transaction ID set")?;
                self.pending_payload = Some((data, ep_transaction_id));
            },
            IndexData(length, ep_transaction_id) => {
                let ep_transaction_id = ep_transaction_id
                    .or(transaction.ep_transaction_id)
                    .context("Data to index but no endpoint transaction ID set")?;
                self.writer.data_transactions.push(ep_transaction_id)?;
                self.writer.data_byte_counts.push(self.total_data)?;
                self.total_data += length as u64;
                self.writer.shared.total_data.store(self.total_data, Release);
            }
        };
        Ok(())
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
    capture: CaptureWriter,
    device_index: VecMap<DeviceAddr, DeviceId>,
    endpoint_index: VecMap<EndpointKey, EndpointId>,
    endpoint_data: VecMap<EndpointId, EndpointData>,
    last_endpoint_state: Vec<u8>,
    last_item_endpoint: Option<EndpointId>,
    transaction_state: Option<TransactionState>,
}

impl Decoder {
    pub fn new(capture: CaptureWriter) -> Result<Decoder, Error> {
        // Create the decoder.
        let mut decoder = Decoder {
            capture,
            device_index: VecMap::new(),
            endpoint_index: VecMap::new(),
            endpoint_data: VecMap::new(),
            last_endpoint_state: Vec::new(),
            last_item_endpoint: None,
            transaction_state: None,
        };

        // Add the default device.
        let default_addr = DeviceAddr(0);
        let default_device = Device { address: default_addr };
        let default_id = decoder.capture.devices.push(&default_device)?;
        let mut device_data = VecMap::new();
        device_data.set(default_id, Arc::new(DeviceData::default()));
        decoder.device_index.set(default_addr, default_id);

        // Add the special endpoints for invalid and framing packets.
        let mut endpoint_readers = VecMap::new();
        for ep_number in [INVALID_EP_NUM, FRAMING_EP_NUM] {
            let (writer, reader) = create_endpoint()?;
            let mut endpoint = Endpoint::default();
            endpoint.set_device_id(default_id);
            endpoint.set_device_address(default_addr);
            endpoint.set_number(ep_number);
            endpoint.set_direction(Direction::Out);
            let endpoint_id = decoder.capture.endpoints.push(&endpoint)?;
            let endpoint_addr =
                EndpointAddr::from_parts(ep_number, Direction::Out);
            decoder.endpoint_data.set(
                endpoint_id,
                EndpointData::new(default_id, endpoint_addr, writer)
            );
            let ep_state = EndpointState::Idle as u8;
            decoder.last_endpoint_state.push(ep_state);
            endpoint_readers.set(endpoint_id, Arc::new(reader));
        }

        // Push changes to shared state.
        decoder.capture.shared.device_data
            .swap(Arc::new(device_data));
        decoder.capture.shared.endpoint_readers
            .swap(Arc::new(endpoint_readers));

        Ok(decoder)
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8], timestamp_ns: u64)
        -> Result<(), Error>
    {
        let data_range = self.capture.packet_data.append(packet)?;
        let packet_id = self.capture.packet_index.push(data_range.start)?;
        self.capture.packet_times.push(timestamp_ns)?;
        self.transaction_update(packet_id, packet)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<CaptureWriter, Error> {
        self.transaction_end(false, false)?;
        self.capture.shared.complete.store(true, Release);
        Ok(self.capture)
    }

    pub fn token_endpoint(&mut self, pid: PID, token: &TokenFields)
        -> Result<EndpointId, Error>
    {
        let dev_addr = token.device_address();
        let ep_num = token.endpoint_number();
        let direction = match (ep_num.0, pid) {
            (0, _)         => Direction::Out,
            (_, PID::IN)   => Direction::In,
            (_, PID::OUT)  => Direction::Out,
            (_, PID::PING) => Direction::Out,
            _ => bail!("PID {pid} does not indicate a direction")
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

    fn packet_endpoint(&mut self, packet: &[u8])
        -> Result<EndpointId, Error>
    {
        let pid = PID::from_packet(packet)?;
        Ok(match PacketFields::from_packet(packet) {
            PacketFields::SOF(_) => FRAMING_EP_ID,
            PacketFields::Token(token) =>
                self.token_endpoint(pid, &token)?,
            _ => INVALID_EP_ID,
        })
    }

    fn transaction_update(&mut self, packet_id: PacketId, packet: &[u8])
        -> Result<(), Error>
    {
        use TransactionStatus::*;
        use TransactionStyle::*;
        use StartComplete::*;
        let status = transaction_status(&self.transaction_state, packet)?;
        let success = status != Fail;
        let complete = match &self.transaction_state {
            None => false,
            Some(TransactionState { style: Simple(..), .. }) => true,
            Some(TransactionState { style: Split(Start, ..), .. }) => false,
            Some(TransactionState { style: Split(Complete, ..), .. }) =>
                status != Retry,
        };
        if status != Invalid {
            if let Some(state) = &mut self.transaction_state {
                state.extract_payload(packet);
            }
        }
        match status {
            New => {
                self.transaction_end(false, false)?;
                self.transaction_start(packet_id, packet)?;
                self.transfer_early_append()?;
            },
            Continue => {
                self.transaction_append(packet)?;
                self.transfer_early_append()?;
            },
            Done | Retry | Fail => {
                self.transaction_append(packet)?;
                self.transaction_end(success, complete)?;
            },
            Invalid => {
                self.transaction_start(packet_id, packet)?;
                self.transaction_end(false, false)?;
            },
        };
        Ok(())
    }

    fn transaction_start(&mut self, packet_id: PacketId, packet: &[u8])
        -> Result<(), Error>
    {
        use PID::*;
        use TransactionStyle::*;
        let pid = PID::from_packet(packet)?;
        let transaction_id = self.capture.transaction_index.push(packet_id)?;
        let (style, endpoint_id) = if pid == SPLIT {
            let split = SplitFields::from_packet(packet);
            (Split(split.sc(), split.endpoint_type(), None), None)
        } else {
            (Simple(pid), Some(self.packet_endpoint(packet)?))
        };
        let mut state = TransactionState {
            style,
            id: transaction_id,
            last: pid,
            endpoint_id,
            ep_transaction_id: None,
            setup: None,
            payload: None,
        };
        // Some packets start a new transfer immediately.
        self.transfer_early_start(&mut state, pid)?;
        self.transaction_state = Some(state);
        Ok(())
    }

    fn transaction_append(&mut self, packet: &[u8])
        -> Result<(), Error>
    {
        use TransactionStyle::*;
        let pid = PID::from_packet(packet)?;
        let update = match &self.transaction_state {
            Some(TransactionState { style: Split(sc, ep_type, None), ..}) => {
                let (sc, ep_type) = (*sc, *ep_type);
                let endpoint_id = self.packet_endpoint(packet)?;
                let ep_data = &self.endpoint_data[endpoint_id];
                let ep_addr = ep_data.address;
                let dev_data = self.capture.device_data(ep_data.device_id)?;
                dev_data.set_endpoint_type(ep_addr, ep_type);
                Some((sc, ep_type, endpoint_id))
            },
            _ => None,
        };
        if let Some(state) = &mut self.transaction_state {
            state.last = pid;
            if let Some((sc, ep_type, endpoint_id)) = update {
                state.style = Split(sc, ep_type, Some(pid));
                state.endpoint_id = Some(endpoint_id);
            }
            Ok(())
        } else {
            bail!("No current transaction to append to")
        }
    }

    fn transaction_end(&mut self, success: bool, complete: bool)
        -> Result<(), Error>
    {
        if let Some(mut state) = self.transaction_state.take() {
            if state.endpoint_id.is_some() {
                self.transfer_update(&mut state, success, complete)?;
            }
        }
        Ok(())
    }

    fn add_device(&mut self, address: DeviceAddr)
        -> Result<DeviceId, Error>
    {
        let device = Device { address };
        let device_id = self.capture.devices.push(&device)?;
        self.device_index.set(address, device_id);
        self.capture.shared.device_data.update(|device_data| {
            device_data.set(device_id, Arc::new(DeviceData::default()));
        });
        Ok(device_id)
    }

    fn add_endpoint(&mut self,
                    dev_addr: DeviceAddr,
                    number: EndpointNum,
                    direction: Direction)
        -> Result<EndpointId, Error>
    {
        let device_id = match self.device_index.get(dev_addr) {
            Some(id) => *id,
            None => self.add_device(dev_addr)?
        };
        let (writer, reader) = create_endpoint()?;
        let mut endpoint = Endpoint::default();
        endpoint.set_device_id(device_id);
        endpoint.set_device_address(dev_addr);
        endpoint.set_number(number);
        endpoint.set_direction(direction);
        let endpoint_id = self.capture.endpoints.push(&endpoint)?;
        let endpoint_addr = EndpointAddr::from_parts(number, direction);
        let endpoint_data = EndpointData::new(device_id, endpoint_addr, writer);
        let endpoint_state = EndpointState::Idle as u8;
        self.last_endpoint_state.push(endpoint_state);
        self.endpoint_data.set(endpoint_id, endpoint_data);
        self.capture.shared.endpoint_readers.update(|endpoint_readers| {
            endpoint_readers.set(endpoint_id, Arc::new(reader));
        });
        Ok(endpoint_id)
    }

    fn transfer_early_start(&mut self,
                            transaction: &mut TransactionState,
                            start: PID)
        -> Result<(), Error>
    {
        use PID::*;
        let start_early = match (start, transaction.endpoint_id) {
            // SETUP always starts a new transfer.
            (SETUP, Some(endpoint_id)) => Some(endpoint_id),
            // Other PIDs always start a new transfer if there
            // is no existing one on their endpoint.
            (IN | OUT | SOF | Malformed, Some(endpoint_id)) => {
                let ep_data = &self.endpoint_data[endpoint_id];
                if ep_data.active.is_none() {
                    Some(endpoint_id)
                } else {
                    None
                }
            }
            // For all other cases, wait for transaction progress.
            _ => None,
        };

        if let Some(endpoint_id) = start_early {
            let ep_transfer_id =
                self.add_transfer(endpoint_id, transaction)?;
            let ep_data = &mut self.endpoint_data[endpoint_id];
            ep_data.early_start = Some(ep_transfer_id);
        }

        Ok(())
    }

    fn transfer_early_append(&mut self) -> Result<(), Error> {
        use PID::*;
        use TransactionStyle::*;
        // Decide whether to index this transaction now.
        // If this transaction might change the transfer sequence
        // and we can't tell yet, we can't index it yet.
        let to_index = if let
            Some(TransactionState {
                 style: Simple(_pid) | Split(.., Some(_pid)),
                 id: transaction_id,
                 endpoint_id: Some(endpoint_id),
                 ep_transaction_id: None,
                 ..
            }) = &self.transaction_state
        {
            let ep_data = &self.endpoint_data[*endpoint_id];
            match ep_data.active {
                // IN and OUT transfers may start and end depending on
                // transaction success and whether a packet is short.
                Some(TransferState { first: IN | OUT, .. }) => None,
                // In all other transfer states, it should be safe to index
                // the current transaction immediately.
                _ => Some((*endpoint_id, *transaction_id))
            }
        } else {
            // We can't index this transaction yet as we don't know
            // what endpoint it needs to be attached to.
            None
        };
        if let (Some(state), Some((endpoint_id, transaction_id))) =
            (&mut self.transaction_state, to_index)
        {
            let ep_data = &mut self.endpoint_data[endpoint_id];
            let ep_transaction_id =
                ep_data.writer.transaction_ids.push(transaction_id)?;
            state.ep_transaction_id = Some(ep_transaction_id);
        };
        Ok(())
    }

    fn transfer_update(&mut self,
                       transaction: &mut TransactionState,
                       success: bool,
                       complete: bool)
        -> Result<(), Error>
    {
        use TransferStatus::*;
        let endpoint_id = transaction.endpoint_id()?;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let dev_data = self.capture.device_data(ep_data.device_id)?;
        let (status, effect) = ep_data.transfer_status(
            dev_data.as_ref(), transaction, success, complete)?;
        match status {
            Single => {
                self.transfer_start(transaction, true)?;
                self.transfer_end(transaction)?;
            },
            New => {
                self.transfer_start(transaction, true)?;
            },
            Continue => {
                self.transfer_append(transaction, true)?;
            },
            Retry => {
                self.transfer_append(transaction, false)?;
            },
            Done => {
                self.transfer_append(transaction, true)?;
                self.transfer_end(transaction)?;
            },
            Invalid => {
                self.transfer_start(transaction, false)?;
                self.transfer_end(transaction)?;
            }
        }
        self.endpoint_data[endpoint_id].apply_effect(transaction, effect)?;
        Ok(())
    }

    fn transfer_start(&mut self,
                      transaction: &mut TransactionState,
                      done: bool)
        -> Result<(), Error>
    {
        let endpoint_id = transaction.endpoint_id()?;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let ep_transfer_id =
            if let Some(ep_transfer_id) = ep_data.early_start.take() {
                ep_transfer_id
            } else {
                self.add_transfer(endpoint_id, transaction)?
            };
        let transaction_type = transaction.start_pid()?;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.active = Some(
            TransferState {
                id: ep_transfer_id,
                first: transaction_type,
                last: if done { Some(transaction_type) } else { None },
            }
        );
        ep_data.payload.clear();
        Ok(())
    }

    fn transfer_append(&mut self,
                       transaction: &mut TransactionState,
                       done: bool)
        -> Result<(), Error>
    {
        let endpoint_id = transaction.endpoint_id()?;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        if let Some(transfer) = &mut ep_data.active {
            if transaction.ep_transaction_id.is_none() {
                let ep_transaction_id =
                    ep_data.writer.transaction_ids.push(transaction.id)?;
                transaction.ep_transaction_id = Some(ep_transaction_id);
            }
            if done {
                transfer.last = Some(transaction.start_pid()?);
            }
        } else {
            self.transfer_start(transaction, done)?;
        }
        Ok(())
    }

    fn transfer_end(&mut self, transaction: &TransactionState)
        -> Result<(), Error>
    {
        let endpoint_id = transaction.endpoint_id()?;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.payload.clear();
        if let Some(transfer) = ep_data.active.take() {
            let ep_transfer_id = transfer.id;
            ep_data.ended = Some(ep_transfer_id);
            let transfer_end_id =
                self.add_transfer_entry(endpoint_id, ep_transfer_id, false)?;
            if self.last_item_endpoint != Some(endpoint_id) {
                self.add_item(endpoint_id, transfer_end_id)?;
            }
        }
        Ok(())
    }

    fn add_transfer(&mut self,
                    endpoint_id: EndpointId,
                    transaction: &mut TransactionState)
        -> Result<EndpointTransferId, Error>
    {
        let ep_data = &mut self.endpoint_data[endpoint_id];
        if let Some(transfer) = ep_data.active.take() {
            ep_data.ended = Some(transfer.id);
            self.add_transfer_entry(endpoint_id, transfer.id, false)?;
        }
        let ep_transaction_id =
            if let Some(ep_transaction_id) = transaction.ep_transaction_id {
                ep_transaction_id
            } else {
                let ep_data = &mut self.endpoint_data[endpoint_id];
                let ep_transaction_id =
                    ep_data.writer.transaction_ids.push(transaction.id)?;
                transaction.ep_transaction_id = Some(ep_transaction_id);
                ep_transaction_id
            };
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let ep_transfer_id =
            ep_data.writer.transfer_index.push(ep_transaction_id)?;
        let transfer_start_id =
            self.add_transfer_entry(endpoint_id, ep_transfer_id, true)?;
        self.add_item(endpoint_id, transfer_start_id)?;
        Ok(ep_transfer_id)
    }

    fn add_transfer_entry(&mut self,
                          endpoint_id: EndpointId,
                          ep_transfer_id: EndpointTransferId,
                          start: bool)
        -> Result<TransferId, Error>
    {
        self.add_endpoint_state(endpoint_id, start)?;
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(endpoint_id);
        entry.set_transfer_id(ep_transfer_id);
        entry.set_is_start(start);
        let transfer_id = self.capture.transfer_index.push(&entry)?;
        Ok(transfer_id)
    }

    fn add_endpoint_state(&mut self,
                          endpoint_id: EndpointId,
                          start: bool)
        -> Result<TransferId, Error>
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
        let range = self.capture.endpoint_states.append(last_state)?;
        let state_id = self.capture.endpoint_state_index.push(range.start)?;
        Ok(state_id)
    }

    fn add_item(&mut self,
                item_endpoint_id: EndpointId,
                transfer_id: TransferId)
        -> Result<TrafficItemId, Error>
    {
        let item_id = self.capture.item_index.push(transfer_id)?;
        self.last_item_endpoint = Some(item_endpoint_id);

        // Look for ended transfers which still need to be linked to an item.
        let endpoint_count = self.capture.endpoints.len();
        for i in 0..endpoint_count {
            let endpoint_id = EndpointId::from(i);
            let ep_data = &mut self.endpoint_data[endpoint_id];
            if let Some(ep_transfer_id) = ep_data.ended.take() {
                // This transfer has ended and is not yet linked to an item.
                let end_id = ep_data.writer.end_index.push(item_id)?;
                assert!(end_id == ep_transfer_id);
            }
        }

        Ok(item_id)
    }
}
