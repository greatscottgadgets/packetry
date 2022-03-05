use std::ops::Range;

use crate::file_vec::FileVec;
use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive, TryFromPrimitive};

#[derive(Copy, Clone, Debug, IntoPrimitive, FromPrimitive, PartialEq)]
#[repr(u8)]
enum PID {
    RSVD  = 0xF0,
    OUT   = 0xE1,
    ACK   = 0xD2,
    DATA0 = 0xC3,
    PING  = 0xB4,
    SOF   = 0xA5,
    NYET  = 0x96,
    DATA2 = 0x87,
    SPLIT = 0x78,
    IN    = 0x69,
    NAK   = 0x5A,
    DATA1 = 0x4B,
    ERR   = 0x3C,
    SETUP = 0x2D,
    STALL = 0x1E,
    MDATA = 0x0F,
    #[default]
    Malformed = 0,
}

impl Default for PID {
    fn default() -> Self {
        PID::Malformed
    }
}

impl std::fmt::Display for PID {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Copy, Clone, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum ItemType {
    Packet = 0,
    Transaction = 1,
    Transfer = 2,
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct Item {
    index: u64,
    item_type: u8,
}

bitfield! {
    #[derive(Debug)]
    pub struct SOFFields(u16);
    u16, frame_number, _: 10, 0;
    u8, crc, _: 15, 11;
}

bitfield! {
    #[derive(Debug)]
    pub struct TokenFields(u16);
    u8, device_address, _: 6, 0;
    u8, endpoint_number, _: 10, 7;
    u8, crc, _: 15, 11;
}

#[derive(Debug)]
pub struct DataFields {
    pub crc: u16,
}

#[derive(Debug)]
pub enum PacketFields {
    SOF(SOFFields),
    Token(TokenFields),
    Data(DataFields),
    None
}

impl PacketFields {
    fn from_packet(packet: &Vec<u8>) -> Self {
        let end = packet.len();
        use PID::*;
        match PID::from(packet[0]) {
            SOF => PacketFields::SOF(
                SOFFields(
                    u16::from_le_bytes([packet[1], packet[2]]))),
            SETUP | IN | OUT => PacketFields::Token(
                TokenFields(
                    u16::from_le_bytes([packet[1], packet[2]]))),
            DATA0 | DATA1 => PacketFields::Data(
                DataFields{
                    crc: u16::from_le_bytes(
                        [packet[end - 2], packet[end - 1]])}),
            _ => PacketFields::None
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct Endpoint {
    pub device_address: u8,
    pub endpoint_number: u8,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct TransferIndexEntry(u64);
    u64, transfer_id, set_transfer_id: 52, 0;
    u16, endpoint_id, set_endpoint_id: 63, 53;
}

#[derive(Copy, Clone, Debug, Default)]
struct TransactionState {
    first: PID,
    last: PID,
    start: u64,
    count: u64,
    endpoint_id: usize,
}

enum EndpointType {
    Control,
    Normal,
}

struct EndpointData {
    ep_type: EndpointType,
    transaction_ids: FileVec<u64>,
    transfer_index: FileVec<u64>,
    transaction_start: u64,
    transaction_count: u64,
    last: PID,
}

impl EndpointData {
    fn status(&self, next: PID) -> DecodeStatus {
        use PID::*;
        use EndpointType::*;
        match (&self.ep_type, self.last, next) {

            // A SETUP transaction starts a new control transfer.
            (Control, _, SETUP) => DecodeStatus::NEW,

            // SETUP may be followed by IN or OUT at data stage.
            (Control, SETUP, IN | OUT) => DecodeStatus::CONTINUE,

            // IN or OUT may then be repeated during data stage.
            (Control, IN, IN) => DecodeStatus::CONTINUE,
            (Control, OUT, OUT) => DecodeStatus::CONTINUE,

            // The opposite direction at status stage ends the transfer.
            (Control, IN, OUT) => DecodeStatus::DONE,
            (Control, OUT, IN) => DecodeStatus::DONE,

            // An IN or OUT transaction on a non-control endpoint,
            // with no transfer in progress, starts a bulk transfer.
            (Normal, Malformed, IN | OUT) => DecodeStatus::NEW,

            // IN or OUT may then be repeated.
            (Normal, IN, IN) => DecodeStatus::CONTINUE,
            (Normal, OUT, OUT) => DecodeStatus::CONTINUE,

            // Any other case is not a valid part of a transfer.
            _ => DecodeStatus::INVALID
        }
    }

    fn finish(&mut self) {
        if self.transaction_count > 0 {
            self.transfer_index.push(&self.transaction_start).unwrap();
        }
    }
}

const USB_MAX_DEVICES: usize = 128;
const USB_MAX_ENDPOINTS: usize = 16;

pub struct Capture {
    items: FileVec<Item>,
    packet_index: FileVec<u64>,
    packet_data: FileVec<u8>,
    transaction_index: FileVec<u64>,
    transfer_index: FileVec<TransferIndexEntry>,
    endpoint_index: [[i16; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
    endpoints: FileVec<Endpoint>,
    endpoint_data: Vec<EndpointData>,
    transaction_state: TransactionState,
}

impl Default for Capture {
    fn default() -> Self {
        Capture::new()
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
    pub fn status(&self, next: PID) -> DecodeStatus {
        use PID::*;
        match (self.first, self.last, next) {

            // SETUP, IN or OUT always start a new transaction.
            (_, _, SETUP | IN | OUT) => DecodeStatus::NEW,

            // SOF when there is no existing transaction starts a new
            // "transaction" representing an idle period on the bus.
            (_, Malformed, SOF) => DecodeStatus::NEW,
            // Additional SOFs extend this "transaction", more may follow.
            (_, SOF, SOF) => DecodeStatus::CONTINUE,

            // SETUP must be followed by DATA0, wait for ACK to follow.
            (_, SETUP, DATA0) => DecodeStatus::CONTINUE,
            // ACK then completes the transaction.
            (SETUP, DATA0, ACK) => DecodeStatus::DONE,

            // IN may be followed by NAK or STALL, completing transaction.
            (_, IN, NAK | STALL) => DecodeStatus::DONE,
            // IN or OUT may be followed by DATA0 or DATA1, wait for status.
            (_, IN | OUT, DATA0 | DATA1) => DecodeStatus::CONTINUE,
            // An ACK then completes the transaction.
            (IN | OUT, DATA0 | DATA1, ACK) => DecodeStatus::DONE,
            // OUT may also be completed by NAK or STALL.
            (OUT, DATA0 | DATA1, NAK | STALL) => DecodeStatus::DONE,

            // Any other case is not a valid part of a transaction.
            _ => DecodeStatus::INVALID,
        }
    }
}

fn get_index_range<T>(index: &mut FileVec<u64>,
                      target: &FileVec<T>,
                      idx: u64) -> Range<u64>
    where T: bytemuck::Pod + Default
{
    match index.get_range(idx..(idx + 2)) {
        Ok(vec) => {
            let start = vec[0];
            let end = vec[1];
            start..end
        }
        Err(_) => {
            let start = index.get(idx).unwrap();
            let end = target.len();
            start..end
        }
    }
}

impl Capture {
    pub fn new() -> Self {
        Capture {
            items: FileVec::new().unwrap(),
            packet_index: FileVec::new().unwrap(),
            packet_data: FileVec::new().unwrap(),
            transaction_index: FileVec::new().unwrap(),
            transfer_index: FileVec::new().unwrap(),
            endpoints: FileVec::new().unwrap(),
            endpoint_data: Vec::new(),
            endpoint_index: [[-1; USB_MAX_ENDPOINTS]; USB_MAX_DEVICES],
            transaction_state: TransactionState::default(),
        }
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        self.transaction_update(packet);
        self.packet_index.push(&self.packet_data.len()).unwrap();
        self.packet_data.append(packet).unwrap();
    }

    pub fn finish(&mut self) {
        for i in 0..self.endpoints.len() as usize {
            self.endpoint_data[i].finish()
        }
    }

    fn transaction_update(&mut self, packet: &[u8]) {
        let pid = PID::from(packet[0]);
        match self.transaction_state.status(pid) {
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
                self.add_item(ItemType::Packet);
            },
        };
    }

    fn transaction_start(&mut self, packet: &[u8]) {
        let state = &mut self.transaction_state;
        state.start = self.packet_index.len();
        state.count = 1;
        state.first = PID::from(packet[0]);
        state.last = state.first;
        match PacketFields::from_packet(&packet.to_vec()) {
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
            _ => {}
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
    }

    fn add_transaction(&mut self) {
        if self.transaction_state.count == 0 { return }
        use PID::*;
        match self.transaction_state.first {
            SETUP | IN | OUT => self.transfer_update(),
            SOF => self.add_item(ItemType::Transaction),
            _ => {}
        };
        self.transaction_index.push(&self.transaction_state.start).unwrap();
    }

    fn add_endpoint(&mut self, addr: usize, num: usize) {
        use EndpointType::*;
        let ep_data = EndpointData {
            ep_type: if num == 0 { Control } else { Normal },
            transaction_ids: FileVec::new().unwrap(),
            transfer_index: FileVec::new().unwrap(),
            transaction_start: 0,
            transaction_count: 0,
            last: PID::Malformed,
        };
        self.endpoint_data.push(ep_data);
        let endpoint = Endpoint {
            device_address: addr as u8,
            endpoint_number: num as u8,
        };
        self.endpoints.push(&endpoint).unwrap();
    }

    fn transfer_update(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let status = ep_data.status(self.transaction_state.first);
        let completed =
            self.transaction_state.count == 3 &&
            self.transaction_state.last == PID::ACK;
        let retry_needed =
            ep_data.transaction_count > 0 &&
            status != DecodeStatus::INVALID &&
            !completed;
        if retry_needed {
            self.transfer_append(false);
            return
        }
        match status {
            DecodeStatus::NEW => {
                self.transfer_end();
                self.add_item(ItemType::Transfer);
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
                self.add_item(ItemType::Transaction);
            }
        }
    }

    fn transfer_start(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        let mut entry = TransferIndexEntry::default();
        entry.set_endpoint_id(self.transaction_state.endpoint_id as u16);
        entry.set_transfer_id(ep_data.transfer_index.len());
        self.transfer_index.push(&entry).unwrap();
        ep_data.transaction_start = ep_data.transaction_ids.len();
        ep_data.transaction_count = 0;
    }

    fn transfer_append(&mut self, success: bool) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        ep_data.transaction_ids.push(&self.transaction_index.len()).unwrap();
        ep_data.transaction_count += 1;
        if success {
            ep_data.last = self.transaction_state.first;
        }
    }

    fn transfer_end(&mut self) {
        let endpoint_id = self.transaction_state.endpoint_id;
        let ep_data = &mut self.endpoint_data[endpoint_id];
        if ep_data.transaction_count > 0 {
            ep_data.transfer_index.push(
                &ep_data.transaction_start).unwrap();
        }
        ep_data.transaction_count = 0;
        ep_data.last = PID::Malformed;
    }

    fn add_item(&mut self, item_type: ItemType) {
        let item = Item {
            item_type: item_type as u8,
            index: match item_type {
                ItemType::Packet => self.packet_index.len(),
                ItemType::Transaction => self.transaction_index.len(),
                ItemType::Transfer => self.transfer_index.len(),
            }
        };
        self.items.push(&item).unwrap();
    }

    pub fn get_item(&mut self, parent: &Option<Item>, index: u64) -> Item {
        match parent {
            None => self.items.get(index).unwrap(),
            Some(parent) => match ItemType::try_from(parent.item_type).unwrap() {
                ItemType::Transaction => {
                    let packet_start =
                        self.transaction_index.get(parent.index).unwrap();
                    Item {
                        item_type: ItemType::Packet as u8,
                        index: packet_start + index,
                    }
                },
                ItemType::Transfer => {
                    let entry = self.transfer_index.get(parent.index).unwrap();
                    let endpoint_id = entry.endpoint_id() as usize;
                    let ep_data = &mut self.endpoint_data[endpoint_id];
                    let range = get_index_range(
                        &mut ep_data.transfer_index,
                        &ep_data.transaction_ids,
                        entry.transfer_id());
                    let transaction_id =
                        ep_data.transaction_ids.get(range.start + index).unwrap();
                    Item {
                        item_type: ItemType::Transaction as u8,
                        index: transaction_id,
                    }
                }
                _ => panic!("not supported yet"),
            }
        }
    }

    pub fn item_count(&mut self, parent: &Option<Item>) -> u64 {
        use ItemType::*;
        match parent {
            None => self.items.len(),
            Some(parent) => match ItemType::try_from(parent.item_type).unwrap() {
                Packet => 0,
                Transaction => {
                    let range = get_index_range(&mut self.transaction_index,
                                                &self.packet_index, parent.index);
                    range.end - range.start
                },
                Transfer => {
                    let entry = self.transfer_index.get(parent.index).unwrap();
                    let endpoint_id = entry.endpoint_id() as usize;
                    let ep_data = &mut self.endpoint_data[endpoint_id];
                    let range = get_index_range(
                        &mut ep_data.transfer_index,
                        &ep_data.transaction_ids,
                        entry.transfer_id());
                    range.end - range.start
                }
            }
        }
    }

    pub fn get_summary(&mut self, item: &Item) -> String {
        match ItemType::try_from(item.item_type).unwrap() {
            ItemType::Packet => {
                let packet = self.get_packet(item.index);
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
                            " with CRC {:04X}",
                            data.crc),
                        PacketFields::None => "".to_string()
                    },
                    packet)
            },
            ItemType::Transaction => {
                let range = get_index_range(&mut self.transaction_index,
                                            &self.packet_index, item.index);
                let count = range.end - range.start;
                let pid = self.get_packet_pid(range.start);
                format!("{} transaction, {} packets", pid, count)
            },
            ItemType::Transfer => {
                let entry = self.transfer_index.get(item.index).unwrap();
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id as u64).unwrap();
                let ep_data = &mut self.endpoint_data[endpoint_id as usize];
                let range = get_index_range(
                    &mut ep_data.transfer_index,
                    &ep_data.transaction_ids,
                    entry.transfer_id());
                let count = range.end - range.start;
                format!("{} transfer on {}.{}, {} transactions",
                        match ep_data.ep_type {
                            EndpointType::Control => "Control",
                            EndpointType::Normal => "Bulk",
                        },
                        endpoint.device_address,
                        endpoint.endpoint_number,
                        count)
            },
        }
    }

    fn get_packet(&mut self, index: u64) -> Vec<u8> {
        let range = get_index_range(&mut self.packet_index,
                                    &self.packet_data, index);
        self.packet_data.get_range(range).unwrap()
    }

    fn get_packet_pid(&mut self, index: u64) -> PID {
        let offset = self.packet_index.get(index).unwrap();
        PID::from(self.packet_data.get(offset).unwrap())
    }
}

mod tests {
    use super::*;

    #[test]
    fn test_parse_sof() {
        let p = PacketFields::from_packet(&vec![0xa5, 0xde, 0x1e]);
        if let PacketFields::SOF(sof) = p {
            assert!(sof.frame_number() == 1758);
            assert!(sof.crc() == 0x03);
        } else {
            panic!("Expected SOF but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_setup() {
        let p = PacketFields::from_packet(&vec![0x2d, 0x02, 0xa8]);
        if let PacketFields::Token(tok) = p {
            assert!(tok.device_address() == 2);
            assert!(tok.endpoint_number() == 0);
            assert!(tok.crc() == 0x15);
        } else {
            panic!("Expected Token but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_in() {
        let p = PacketFields::from_packet(&vec![0x69, 0x82, 0x18]);
        if let PacketFields::Token(tok) = p {
            assert!(tok.device_address() == 2);
            assert!(tok.endpoint_number() == 1);
            assert!(tok.crc() == 0x03);
        } else {
            panic!("Expected Token but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_data() {
        let p = PacketFields::from_packet(&vec![0xc3, 0x40, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xd5]);
        if let PacketFields::Data(data) = p {
            assert!(data.crc == 0xd5aa);
        } else {
            panic!("Expected Data but got {:?}", p);
        }

    }
}

