use std::ops::Range;

use crate::file_vec::FileVec;
use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive, TryFromPrimitive};

#[derive(Copy, Clone, Debug, IntoPrimitive, FromPrimitive, PartialEq)]
#[repr(u8)]
enum PID {
	RSVD  = 0xF0,
	OUT	  = 0xE1,
	ACK	  = 0xD2,
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

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct Packet {
    pub data_start: u64,
    pub data_end: u64,
    pub pid: u8,
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct Transaction {
    pub packet_start: u64,
    pub packet_count: u64,
}

#[derive(Copy, Clone, Debug, Default)]
struct TransactionState {
    first: PID,
    last: PID,
}

pub struct Capture {
    items: FileVec<Item>,
    packets: FileVec<Packet>,
    packet_data: FileVec<u8>,
    transactions: FileVec<Transaction>,
    transaction_state: TransactionState,
    current_packet: Packet,
    current_transaction: Transaction,
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

impl Capture {
    pub fn new() -> Self {
        Capture{
            items: FileVec::new().unwrap(),
            packets: FileVec::new().unwrap(),
            packet_data: FileVec::new().unwrap(),
            transactions: FileVec::new().unwrap(),
            transaction_state: TransactionState::default(),
            current_packet: Packet::default(),
            current_transaction: Transaction::default(),
        }
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        self.current_packet.data_start = self.packet_data.len();
        self.current_packet.data_end = self.packet_data.len() + packet.len() as u64;
        self.current_packet.pid = packet[0];
        self.transaction_update();
        self.packet_data.append(packet).unwrap();
        self.packets.push(&self.current_packet).unwrap();
    }

    fn transaction_update(&mut self) {
        match self.transaction_state.status(PID::from(self.current_packet.pid)) {
            DecodeStatus::NEW => {
                self.transaction_end();
                self.transaction_start();
            },
            DecodeStatus::CONTINUE => {
                self.transaction_append();
            },
            DecodeStatus::DONE => {
                self.transaction_append();
                self.transaction_end();
            },
            DecodeStatus::INVALID => {
                self.transaction_end();
                self.add_item(ItemType::Packet, self.packets.len());
            },
        };
    }

    fn transaction_start(&mut self) {
        self.current_transaction.packet_start = self.packets.len();
        self.current_transaction.packet_count = 1;
        let pid = PID::from(self.current_packet.pid);
        self.transaction_state.first = pid;
        self.transaction_state.last = pid;
    }

    fn transaction_append(&mut self) {
        self.current_transaction.packet_count += 1;
        let pid = PID::from(self.current_packet.pid);
        self.transaction_state.last = pid;
    }

    fn transaction_end(&mut self) {
        if self.current_transaction.packet_count > 0 {
            if !(self.transaction_state.first == PID::SOF &&
                 self.current_transaction.packet_count == 1)
            {
                self.add_item(ItemType::Transaction, self.transactions.len());
                self.transactions.push(&self.current_transaction).unwrap();
            }
        }
        self.current_transaction.packet_count = 0;
        self.transaction_state.first = PID::Malformed;
        self.transaction_state.last = PID::Malformed;
    }

    fn add_item(&mut self, item_type: ItemType, index: u64) {
        let item = Item {
            item_type: item_type as u8,
            index: index,
        };
        self.items.push(&item).unwrap();
    }

    pub fn get_item(&mut self, parent: &Option<Item>, index: u64) -> Item {
        match parent {
            None => self.items.get(index).unwrap(),
            Some(parent) => match ItemType::try_from(parent.item_type).unwrap() {
                ItemType::Transaction => {
                    let packet_start = self.transactions.get(parent.index).unwrap().packet_start;
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
                    self.transactions.get(parent.index).unwrap().packet_count
                }
            }
        }
    }

    pub fn get_summary(&mut self, item: &Item) -> String {
        match ItemType::try_from(item.item_type).unwrap() {
            ItemType::Packet => {
                let packet = self.packets.get(item.index).unwrap();
                let data = self.get_packet_data(packet.data_start..packet.data_end);
                format!("{} packet \t{:02X?}", PID::from(packet.pid), data)
            },
            ItemType::Transaction => {
                let transaction = self.transactions.get(item.index).unwrap();
                let packet = self.packets.get(transaction.packet_start).unwrap();
                let count = transaction.packet_count;
                format!("{} transaction, {} packets", PID::from(packet.pid), count)
            },
        }
    }

    fn get_packet_data(&mut self, range: Range<u64>) -> Vec<u8> {
        self.packet_data.get_range(range).unwrap()
    }
}
