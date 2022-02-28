use std::ops::Range;

use crate::file_vec::FileVec;
use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive, TryFromPrimitive};

#[derive(Copy, Clone, Debug, IntoPrimitive, FromPrimitive)]
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

#[derive(Copy, Clone, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum TransactionType {
    SOF,
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct Transaction {
    pub packet_start: u64,
    pub packet_count: u64,
    pub transaction_type: u8,
}

impl std::fmt::Display for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?} packet", PID::from(self.pid))
    }
}

pub struct Capture {
    items: FileVec<Item>,
    packets: FileVec<Packet>,
    packet_data: FileVec<u8>,
    transactions: FileVec<Transaction>,
    sof_transaction_in_progress: bool,
}

impl Default for Capture {
    fn default() -> Self {
        Capture::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        Capture{
            items: FileVec::new().unwrap(),
            packets: FileVec::new().unwrap(),
            packet_data: FileVec::new().unwrap(),
            transactions: FileVec::new().unwrap(),
            sof_transaction_in_progress: false,
        }
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        let usb_packet = Packet{
            data_start: self.packet_data.len(),
            data_end: self.packet_data.len() + packet.len() as u64,
            pid: packet[0],
        };

        match PID::from(usb_packet.pid) {
            // Group consecutive SOF packets into a transaction
            PID::SOF => {
                if !self.sof_transaction_in_progress {
                    // If this is the first SOF, create the Transaction & the top-level Item
                    self.sof_transaction_in_progress = true;
                    let item = Item {
                        item_type: ItemType::Transaction as u8,
                        index: self.transactions.len(),
                    };
                    let transaction = Transaction {
                        transaction_type: TransactionType::SOF as u8,
                        packet_start: self.packets.len(),
                        packet_count: 1,
                    };
                    self.transactions.push(&transaction).unwrap();
                    self.items.push(&item).unwrap();
                } else {
                    // Otheriwse, update the packet count
                    let index = self.transactions.len()-1;
                    let mut transaction = self.transactions.get(index).unwrap();
                    transaction.packet_count += 1;
                    self.transactions.insert(&transaction, index).unwrap();
                }
            },
            // For other packets, add them to the top-level
            _ => {
                self.sof_transaction_in_progress = false;
                let item = Item {
                    item_type: ItemType::Packet as u8,
                    index: self.packets.len(),
                };
                self.items.push(&item).unwrap();
            }
        };
        self.packet_data.append(packet).unwrap();
        self.packets.push(&usb_packet).unwrap();
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
                format!("{}\t{:02X?}", packet, data)
            },
            ItemType::Transaction => {
                format!("{} SOFs", self.item_count(&Some(*item)))
            },
        }
    }

    fn get_packet_data(&mut self, range: Range<u64>) -> Vec<u8> {
        self.packet_data.get_range(range).unwrap()
    }
}