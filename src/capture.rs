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
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct Item {
    index: u64,
    item_type: u8,
}

bitfield! {
    pub struct SOFFields(u16);
    u16, frame_number, _: 10, 0;
    u8, crc, _: 15, 11;
}

bitfield! {
    pub struct TokenFields(u16);
    u8, device_address, _: 6, 0;
    u8, endpoint_number, _: 10, 7;
    u8, crc, _: 15, 11;
}

pub struct DataFields {
    pub crc: u16,
}

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

#[derive(Copy, Clone, Debug, Default)]
struct TransactionState {
    first: PID,
    last: PID,
    start: u64,
    count: u64,
}

pub struct Capture {
    items: FileVec<Item>,
    packet_index: FileVec<u64>,
    packet_data: FileVec<u8>,
    transaction_index: FileVec<u64>,
    transaction_state: TransactionState,
}

impl Default for Capture {
    fn default() -> Self {
        Capture::new()
    }
}

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
        Capture{
            items: FileVec::new().unwrap(),
            packet_index: FileVec::new().unwrap(),
            packet_data: FileVec::new().unwrap(),
            transaction_index: FileVec::new().unwrap(),
            transaction_state: TransactionState::default(),
        }
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        self.transaction_update(packet);
        self.packet_index.push(&self.packet_data.len()).unwrap();
        self.packet_data.append(packet).unwrap();
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
        self.add_item(ItemType::Transaction);
        self.transaction_index.push(&self.transaction_state.start).unwrap();
    }

    fn add_item(&mut self, item_type: ItemType) {
        let item = Item {
            item_type: item_type as u8,
            index: match item_type {
                ItemType::Packet => self.packet_index.len(),
                ItemType::Transaction => self.transaction_index.len(),
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
