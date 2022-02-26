use std::ops::Range;

use crate::file_vec::FileVec;
use bytemuck_derive::{Pod, Zeroable};

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum PID {
	NONE  = 0x00,
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
    Malformed,
}

impl From<u8> for PID {
    fn from(num: u8) -> Self {
        use PID::*;
        match num {
            0xF0 => RSVD,
            0xE1 => OUT,
            0xD2 => ACK,
            0xC3 => DATA0,
            0xB4 => PING,
            0xA5 => SOF,
            0x96 => NYET,
            0x87 => DATA2,
            0x78 => SPLIT,
            0x69 => IN,
            0x5A => NAK,
            0x4B => DATA1,
            0x3C => ERR,
            0x2D => SETUP,
            0x1E => STALL,
            0x0F => MDATA,
            _ => Malformed,
        }
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
enum ItemType {
    Packet,
}
impl From<u8> for ItemType {
    fn from(num: u8) -> Self {
        use ItemType::*;
        match num {
            0 => Packet,
            _ => panic!("Cannot convert {} to ItemType", num),
        }
    }
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

impl std::fmt::Display for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?} packet", PID::from(self.pid))
    }
}

pub struct Capture {
    items: FileVec<Item>,
    packets: FileVec<Packet>,
    packet_data: FileVec<u8>,
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
        }
    }

    pub fn handle_raw_packet(&mut self, packet: &[u8]) {
        let usb_packet = Packet{
            data_start: self.packet_data.len(),
            data_end: self.packet_data.len() + packet.len() as u64,
            pid: packet[0].into(),
        };
        let item = Item {
            item_type: ItemType::Packet as u8,
            index: self.packets.len(),
        };
        self.packet_data.append(packet).unwrap();
        self.packets.push(&usb_packet).unwrap();
        self.items.push(&item).unwrap();
    }

    pub fn get_item(&mut self, parent: &Option<Item>, index: u64) -> Item {
        match parent {
            None => self.items.get(index).unwrap(),
            _ => panic!("not supported yet"),
        }
    }

    pub fn item_count(&mut self, parent: &Option<Item>) -> u64 {
        match parent {
            None => self.items.len(),
            _ => 0,
        }
    }

    pub fn get_summary(&mut self, item: &Item) -> String {
        match ItemType::from(item.item_type) {
            ItemType::Packet => {
                let packet = self.packets.get(item.index).unwrap();
                let data = self.get_packet_data(packet.data_start..packet.data_end);
                format!("{}\t{:02X?}", packet, data)
            },
        }
    }

    fn get_packet_data(&mut self, range: Range<u64>) -> Vec<u8> {
        self.packet_data.get_range(range).unwrap()
    }
}