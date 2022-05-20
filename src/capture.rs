use std::ops::Range;
use std::num::TryFromIntError;

use crate::id::{Id, HasLength};
use crate::file_vec::{FileVec, FileVecError};
use crate::hybrid_index::{HybridIndex, HybridIndexError, Number};
use crate::usb::{
    self,
    PID,
    PacketFields,
    SetupFields,
    Direction,
    DeviceDescriptor,
    Configuration,
    Interface,
    EndpointDescriptor,
    ControlTransfer,
    DeviceAddr,
    DeviceField,
    ConfigNum,
    ConfigField,
    InterfaceNum,
    InterfaceField,
    EndpointNum,
    EndpointField,
};

use bytemuck_derive::{Pod, Zeroable};
use num_enum::{IntoPrimitive, FromPrimitive};
use num_format::{Locale, ToFormattedString};
use humansize::{FileSize, file_size_opts as options};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaptureError {
    #[error(transparent)]
    FileVecError(#[from] FileVecError),
    #[error(transparent)]
    HybridIndexError(#[from] HybridIndexError),
    #[error(transparent)]
    RangeError(#[from] TryFromIntError),
    #[error("Descriptor missing")]
    DescriptorMissing,
    #[error("Invalid index")]
    IndexError,
}

use CaptureError::{DescriptorMissing, IndexError};

pub type PacketByteId = Id<u8>;
pub type PacketId = Id<PacketByteId>;
pub type TransactionId = Id<PacketId>;
pub type TransferId = Id<TransferIndexEntry>;
pub type EndpointTransactionId = Id<TransactionId>;
pub type EndpointTransferId = Id<EndpointTransactionId>;
pub type ItemId = Id<TransferId>;
pub type DeviceId = Id<Device>;
pub type EndpointId = Id<Endpoint>;
pub type EndpointStateId = Id<Id<u8>>;

#[derive(Clone)]
pub enum Item {
    Transfer(TransferId),
    Transaction(TransferId, TransactionId),
    Packet(TransferId, TransactionId, PacketId),
}

#[derive(Clone)]
pub enum DeviceItem {
    Device(DeviceId),
    DeviceDescriptor(DeviceId),
    DeviceDescriptorField(DeviceId, DeviceField),
    Configuration(DeviceId, ConfigNum),
    ConfigurationDescriptor(DeviceId, ConfigNum),
    ConfigurationDescriptorField(DeviceId, ConfigNum, ConfigField),
    Interface(DeviceId, ConfigNum, InterfaceNum),
    InterfaceDescriptor(DeviceId, ConfigNum, InterfaceNum),
    InterfaceDescriptorField(DeviceId, ConfigNum,
                             InterfaceNum, InterfaceField),
    EndpointDescriptor(DeviceId, ConfigNum, InterfaceNum, EndpointNum),
    EndpointDescriptorField(DeviceId, ConfigNum, InterfaceNum,
                            EndpointNum, EndpointField),
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct Device {
    pub address: DeviceAddr,
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct Endpoint(u64);
    pub u64, from into DeviceId, device_id, set_device_id: 51, 0;
    pub u8, from into DeviceAddr, device_address, set_device_address: 58, 52;
    pub u8, from into EndpointNum, number, set_number: 63, 59;
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct TransferIndexEntry(u64);
    pub u64, from into EndpointTransferId, transfer_id, set_transfer_id: 51, 0;
    pub u64, from into EndpointId, endpoint_id, set_endpoint_id: 62, 52;
    pub u8, _is_start, _set_is_start: 63, 63;
}

impl TransferIndexEntry {
    pub fn is_start(&self) -> bool {
        self._is_start() != 0
    }
    pub fn set_is_start(&mut self, value: bool) {
        self._set_is_start(value as u8)
    }
}

#[derive(Copy, Clone, IntoPrimitive, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum EndpointState {
    #[default]
    Idle = 0,
    Starting = 1,
    Ongoing = 2,
    Ending = 3,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
pub enum EndpointType {
    Control       = 0x00,
    Isochronous   = 0x01,
    Bulk          = 0x02,
    Interrupt     = 0x03,
    #[default]
    Unidentified  = 0x04,
    Framing       = 0x10,
    Invalid       = 0x11,
}

pub struct EndpointTraffic {
    pub transaction_ids: HybridIndex<TransactionId>,
    pub transfer_index: HybridIndex<EndpointTransactionId>,
}

pub struct DeviceData {
    pub device_descriptor: Option<DeviceDescriptor>,
    pub configurations: Vec<Option<Configuration>>,
    pub config_number: Option<ConfigNum>,
    pub endpoint_types: Vec<EndpointType>,
    pub strings: Vec<Option<Vec<u8>>>,
}

impl DeviceData {
    pub fn try_get_configuration(&self, number: &ConfigNum)
        -> Option<&Configuration>
    {
        match self.configurations.get(number.0 as usize) {
            Some(Some(config)) => Some(config),
            _ => None
        }
    }

    pub fn get_configuration(&self, number: &ConfigNum)
        -> Result<&Configuration, CaptureError>
    {
        match self.try_get_configuration(number) {
            Some(config) => Ok(config),
            None => Err(DescriptorMissing)
        }
    }

    pub fn endpoint_type(&self, number: EndpointNum) -> EndpointType {
        use EndpointType::*;
        match number.0 {
            0 => Control,
            0x10 => Framing,
            0x11 => Invalid,
            _ => match self.endpoint_types.get(number.0 as usize) {
                Some(ep_type) => *ep_type,
                None => Unidentified
            }
        }
    }

    pub fn update_endpoint_types(&mut self) {
        if let Some(number) = self.config_number {
            let idx = number.0 as usize;
            if let Some(Some(config)) = &self.configurations.get(idx) {
                for iface in &config.interfaces {
                    for ep_desc in &iface.endpoint_descriptors {
                        let number = ep_desc.endpoint_address & 0x0F;
                        let index = number as usize;
                        if let Some(ep_type) =
                            self.endpoint_types.get_mut(index)
                        {
                            *ep_type =
                                EndpointType::from(ep_desc.attributes & 0x03);
                        }
                    }
                }
            }
        }
    }
}

impl Configuration {
    pub fn get_interface(&self, number: &InterfaceNum)
        -> Result<&Interface, CaptureError>
    {
        match self.interfaces.get(number.0 as usize) {
            Some(iface) => Ok(iface),
            _ => Err(IndexError)
        }
    }
}

impl Interface {
    pub fn get_endpoint_descriptor(&self, number: &EndpointNum)
        -> Result<&EndpointDescriptor, CaptureError>
    {
        match self.endpoint_descriptors.get(number.0 as usize) {
            Some(desc) => Ok(desc),
            _ => Err(IndexError)
        }
    }
}

pub struct Capture {
    pub item_index: HybridIndex<TransferId>,
    pub packet_index: HybridIndex<PacketByteId>,
    pub packet_data: FileVec<u8>,
    pub transaction_index: HybridIndex<PacketId>,
    pub transfer_index: FileVec<TransferIndexEntry>,
    pub devices: FileVec<Device>,
    pub device_data: Vec<DeviceData>,
    pub endpoints: FileVec<Endpoint>,
    pub endpoint_traffic: Vec<EndpointTraffic>,
    pub endpoint_states: FileVec<u8>,
    pub endpoint_state_index: HybridIndex<Id<u8>>,
}

pub struct Transaction {
    pid: PID,
    packet_id_range: Range<PacketId>,
    payload_byte_range: Option<Range<Id<u8>>>,
}

impl Transaction {
    fn packet_count(&self) -> u64 {
        self.packet_id_range.len()
    }

    fn payload_size(&self) -> Option<u64> {
        self.payload_byte_range.as_ref().map(|range| range.len())
    }
}

fn get_index_range<T>(
        index: &mut HybridIndex<T>,
        length: u64,
        id: Id<T>)
    -> Result<Range<T>, CaptureError>
    where T: Number + Copy
{
    Ok(if id.value + 2 > index.len() {
        let start = index.get(id)?;
        let end = <T>::from_u64(length);
        start..end
    } else {
        let limit = Id::<T>::from(id.value + 2);
        let vec = index.get_range(id .. limit)?;
        let start = vec[0];
        let end = vec[1];
        start..end
    })
}

pub fn fmt_count(count: u64) -> String {
    count.to_formatted_string(&Locale::en)
}

pub fn fmt_size(size: u64) -> String {
    match size.file_size(options::BINARY) {
        Ok(string) => string,
        Err(e) => format!("<Error: {}>", e)
    }
}

pub fn fmt_vec<T>(vec: &FileVec<T>) -> String
    where T: bytemuck::Pod + Default
{
    format!("{} entries, {}", fmt_count(vec.len()), fmt_size(vec.size()))
}

pub fn fmt_index<T>(idx: &HybridIndex<T>) -> String
    where T: Number
{
    format!("{} values in {} entries, {}",
            fmt_count(idx.len()),
            fmt_count(idx.entry_count()),
            fmt_size(idx.size()))
}

impl Capture {
    pub fn new() -> Result<Self, CaptureError> {
        Ok(Capture {
            item_index: HybridIndex::new(1)?,
            packet_index: HybridIndex::new(2)?,
            packet_data: FileVec::new()?,
            transaction_index: HybridIndex::new(1)?,
            transfer_index: FileVec::new()?,
            devices: FileVec::new()?,
            device_data: Vec::new(),
            endpoints: FileVec::new()?,
            endpoint_traffic: Vec::new(),
            endpoint_states: FileVec::new()?,
            endpoint_state_index: HybridIndex::new(1)?,
        })
    }

    pub fn print_storage_summary(&self) {
        let mut overhead: u64 =
            self.packet_index.size() +
            self.transaction_index.size() +
            self.transfer_index.size() +
            self.endpoint_states.size() +
            self.endpoint_state_index.size();
        let mut trx_count = 0;
        let mut trx_entries = 0;
        let mut trx_size = 0;
        let mut xfr_count = 0;
        let mut xfr_entries = 0;
        let mut xfr_size = 0;
        for ep_traf in &self.endpoint_traffic {
            trx_count += ep_traf.transaction_ids.len();
            trx_entries += ep_traf.transaction_ids.entry_count();
            trx_size += ep_traf.transaction_ids.size();
            xfr_count += ep_traf.transfer_index.len();
            xfr_entries += ep_traf.transfer_index.entry_count();
            xfr_size += ep_traf.transfer_index.size();
            overhead += trx_size + xfr_size;
        }
        let ratio = (overhead as f32) / (self.packet_data.size() as f32);
        let percentage = ratio * 100.0;
        print!(concat!(
            "Storage summary:\n",
            "  Packet data: {}\n",
            "  Packet index: {}\n",
            "  Transaction index: {}\n",
            "  Transfer index: {}\n",
            "  Endpoint states: {}\n",
            "  Endpoint state index: {}\n",
            "  Endpoint transaction indices: {} values in {} entries, {}\n",
            "  Endpoint transfer indices: {} values in {} entries, {}\n",
            "Total overhead: {:.1}% ({})\n"),
            fmt_size(self.packet_data.size()),
            fmt_index(&self.packet_index),
            fmt_index(&self.transaction_index),
            fmt_vec(&self.transfer_index),
            fmt_vec(&self.endpoint_states),
            fmt_index(&self.endpoint_state_index),
            fmt_count(trx_count), fmt_count(trx_entries), fmt_size(trx_size),
            fmt_count(xfr_count), fmt_count(xfr_entries), fmt_size(xfr_size),
            percentage, fmt_size(overhead),
        )
    }

    pub fn endpoint_traffic(&mut self, endpoint_id: EndpointId)
        -> Result<&mut EndpointTraffic, CaptureError>
    {
        let idx = endpoint_id.value as usize;
        self.endpoint_traffic.get_mut(idx).ok_or(IndexError)
    }

    pub fn get_item(&mut self, parent: &Option<Item>, index: u64)
        -> Result<Item, CaptureError>
    {
        match parent {
            None => {
                let item_id = ItemId::from(index);
                let transfer_id = self.item_index.get(item_id)?;
                Ok(Item::Transfer(transfer_id))
            },
            Some(item) => self.get_child(item, index)
        }
    }

    pub fn get_child(&mut self, parent: &Item, index: u64)
        -> Result<Item, CaptureError>
    {
        use Item::*;
        Ok(match parent {
            Transfer(transfer_id) =>
                Transaction(*transfer_id, {
                    let entry = self.transfer_index.get(*transfer_id)?;
                    let endpoint_id = entry.endpoint_id();
                    let ep_transfer_id = entry.transfer_id();
                    let ep_traf = self.endpoint_traffic(endpoint_id)?;
                    let offset = ep_traf.transfer_index.get(ep_transfer_id)?;
                    ep_traf.transaction_ids.get(offset + index)?
                }),
            Transaction(transfer_id, transaction_id) =>
                Packet(*transfer_id, *transaction_id, {
                    self.transaction_index.get(*transaction_id)? + index}),
            Packet(..) => return Err(IndexError)
        })
    }

    fn transfer_range(&mut self, entry: &TransferIndexEntry)
        -> Result<Range<EndpointTransactionId>, CaptureError>
    {
        let endpoint_id = entry.endpoint_id();
        let ep_transfer_id = entry.transfer_id();
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        get_index_range(&mut ep_traf.transfer_index,
                        ep_traf.transaction_ids.len(), ep_transfer_id)
    }

    pub fn item_count(&mut self, parent: &Option<Item>)
        -> Result<u64, CaptureError>
    {
        match parent {
            None => Ok(self.item_index.len()),
            Some(item) => self.child_count(item)
        }
    }

    pub fn child_count(&mut self, parent: &Item)
        -> Result<u64, CaptureError>
    {
        use Item::*;
        Ok(match parent {
            Transfer(transfer_id) => {
                let entry = self.transfer_index.get(*transfer_id)?;
                if entry.is_start() {
                    self.transfer_range(&entry)?.len()
                } else {
                    0
                }
            },
            Transaction(_, transaction_id) => {
                get_index_range(&mut self.transaction_index,
                    self.packet_index.len(), *transaction_id)?.len()
            },
            Packet(..) => 0,
        })
    }

    pub fn get_summary(&mut self, item: &Item)
        -> Result<String, CaptureError>
    {
        use Item::*;
        Ok(match item {
            Packet(.., packet_id) => {
                let packet = self.get_packet(*packet_id)?;
                let pid = PID::from(*packet.get(0).ok_or(IndexError)?);
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
                            " with {} data bytes and CRC {:04X}",
                            packet.len() - 3,
                            data.crc),
                        PacketFields::None => "".to_string()
                    },
                    packet)
            },
            Transaction(_, transaction_id) => {
                let transaction = self.get_transaction(*transaction_id)?;
                let count = transaction.packet_count();
                match (transaction.pid, transaction.payload_size()) {
                    (PID::SOF, _) => format!(
                        "{} SOF packets", count),
                    (pid, None) => format!(
                        "{} transaction, {} packets", pid, count),
                    (pid, Some(size)) => format!(
                        "{} transaction, {} packets with {} data bytes",
                        pid, count, size)
                }
            },
            Transfer(transfer_id) => {
                let entry = self.transfer_index.get(*transfer_id)?;
                let endpoint_id = entry.endpoint_id();
                let endpoint = self.endpoints.get(endpoint_id)?;
                let device_id = endpoint.device_id();
                let dev_data = &self.get_device_data(&device_id)?;
                let num = endpoint.number();
                let ep_type = dev_data.endpoint_type(num);
                if !entry.is_start() {
                    return Ok(match ep_type {
                        EndpointType::Invalid =>
                            "End of invalid groups".to_string(),
                        EndpointType::Framing =>
                            "End of SOF groups".to_string(),
                        endpoint_type => format!(
                            "{:?} transfer ending on endpoint {}.{}",
                            endpoint_type, endpoint.device_address(), num)
                    })
                }
                let range = self.transfer_range(&entry)?;
                let count = range.len();
                match ep_type {
                    EndpointType::Invalid => format!(
                        "{} invalid groups", count),
                    EndpointType::Framing => format!(
                        "{} SOF groups", count),
                    EndpointType::Control => {
                        let transfer = self.get_control_transfer(
                            endpoint.device_address(), endpoint_id, range)?;
                        transfer.summary()
                    },
                    endpoint_type => format!(
                        "{:?} transfer with {} transactions on endpoint {}.{}",
                        endpoint_type, count,
                        endpoint.device_address(), endpoint.number())
                }
            }
        })
    }

    pub fn get_connectors(&mut self, item: &Item)
        -> Result<String, CaptureError>
    {
        use EndpointState::*;
        use Item::*;
        let endpoint_count = self.endpoints.len() as usize;
        const MIN_LEN: usize = " └─".len();
        let string_length = MIN_LEN + endpoint_count;
        let mut connectors = String::with_capacity(string_length);
        let transfer_id = match item {
            Transfer(i) | Transaction(i, _) | Packet(i, ..) => *i
        };
        let entry = self.transfer_index.get(transfer_id)?;
        let endpoint_id = entry.endpoint_id();
        let endpoint_state = self.get_endpoint_state(transfer_id)?;
        let extended = self.transfer_extended(endpoint_id, transfer_id)?;
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let last_transaction = match item {
            Transaction(_, transaction_id) | Packet(_, transaction_id, _) => {
                let range = get_index_range(&mut ep_traf.transfer_index,
                    ep_traf.transaction_ids.len(), entry.transfer_id())?;
                let last_transaction_id =
                    ep_traf.transaction_ids.get(range.end - 1)?;
                *transaction_id == last_transaction_id
            }, _ => false
        };
        let last_packet = match item {
            Packet(_, transaction_id, packet_id) => {
                let range = get_index_range(&mut self.transaction_index,
                    self.packet_index.len(), *transaction_id)?;
                *packet_id == range.end - 1
            }, _ => false
        };
        let last = last_transaction && !extended;
        let mut thru = false;
        for (i, &state) in endpoint_state.iter().enumerate() {
            let state = EndpointState::from(state);
            let active = state != Idle;
            let on_endpoint = i == endpoint_id.value as usize;
            thru |= match (item, state, on_endpoint) {
                (Transfer(..), Starting | Ending, _) => true,
                (Transaction(..) | Packet(..), _, true) => on_endpoint,
                _ => false,
            };
            connectors.push(match item {
                Transfer(..) => {
                    match (state, thru) {
                        (Idle,     _    ) => ' ',
                        (Starting, _    ) => '○',
                        (Ongoing,  false) => '│',
                        (Ongoing,  true ) => '┼',
                        (Ending,   _    ) => '└',
                    }
                },
                Transaction(..) => {
                    match (on_endpoint, active, thru, last) {
                        (false, false, false, _    ) => ' ',
                        (false, false, true,  _    ) => '─',
                        (false, true,  false, _    ) => '│',
                        (false, true,  true,  _    ) => '┼',
                        (true,  _,     _,     false) => '├',
                        (true,  _,     _,     true ) => '└',
                    }
                },
                Packet(..) => {
                    match (on_endpoint, active, last) {
                        (false, false, _    ) => ' ',
                        (false, true,  _    ) => '│',
                        (true,  _,     false) => '│',
                        (true,  _,     true ) => ' ',
                    }
                }
            });
        };
        let state_length = endpoint_state.len();
        for _ in state_length..endpoint_count {
            connectors.push(match item {
                Transfer(..)    => '─',
                Transaction(..) => '─',
                Packet(..)      => ' ',
            });
        }
        connectors.push_str(
            match (item, last_packet) {
                (Transfer(_), _) if entry.is_start() => "─",
                (Transfer(_), _)                     => "──□ ",
                (Transaction(..), _)                 => "───",
                (Packet(..), false)                  => "    ├──",
                (Packet(..), true)                   => "    └──",
            }
        );
        Ok(connectors)
    }

    fn transfer_extended(&mut self,
                         endpoint_id: EndpointId,
                         transfer_id: TransferId)
        -> Result<bool, CaptureError>
    {
        use EndpointState::*;
        let count = self.transfer_index.len();
        if transfer_id.value + 1 >= count {
            return Ok(false);
        };
        let state = self.get_endpoint_state(transfer_id + 1)?;
        Ok(match state.get(endpoint_id.value as usize) {
            Some(ep_state) => EndpointState::from(*ep_state) == Ongoing,
            None => false
        })
    }

    fn get_endpoint_state(&mut self, transfer_id: TransferId)
        -> Result<Vec<u8>, CaptureError>
    {
        let endpoint_state_id = EndpointStateId::from(transfer_id.value);
        let range = get_index_range(
            &mut self.endpoint_state_index,
            self.endpoint_states.len(), endpoint_state_id)?;
        Ok(self.endpoint_states.get_range(range)?)
    }

    fn get_packet(&mut self, id: PacketId)
        -> Result<Vec<u8>, CaptureError>
    {
        let range: Range<Id<u8>> = get_index_range(&mut self.packet_index,
                                                   self.packet_data.len(),
                                                   id)?;
        Ok(self.packet_data.get_range(range)?)
    }

    fn get_packet_pid(&mut self, id: PacketId)
        -> Result<PID, CaptureError>
    {
        let offset: Id<u8> = self.packet_index.get(id)?;
        Ok(PID::from(self.packet_data.get(offset)?))
    }

    fn get_transaction(&mut self, id: TransactionId)
        -> Result<Transaction, CaptureError>
    {
        let packet_id_range = get_index_range(&mut self.transaction_index,
                                              self.packet_index.len(),
                                              id)?;
        let packet_count = packet_id_range.len();
        let pid = self.get_packet_pid(packet_id_range.start)?;
        use PID::*;
        let payload_byte_range = match pid {
            IN | OUT if packet_count >= 2 => {
                let data_packet_id = packet_id_range.start + 1;
                let packet_byte_range = get_index_range(
                    &mut self.packet_index,
                    self.packet_data.len(), data_packet_id)?;
                let pid = self.packet_data.get(packet_byte_range.start)?;
                match PID::from(pid) {
                    DATA0 | DATA1 => Some({
                        packet_byte_range.start + 1 .. packet_byte_range.end - 2
                    }),
                    _ => None
                }
            },
            _ => None
        };
        Ok(Transaction {
            pid,
            packet_id_range,
            payload_byte_range,
        })
    }

    fn get_control_transfer(&mut self,
                            address: DeviceAddr,
                            endpoint_id: EndpointId,
                            range: Range<EndpointTransactionId>)
        -> Result<ControlTransfer, CaptureError>
    {
        let ep_traf = self.endpoint_traffic(endpoint_id)?;
        let transaction_ids =
            ep_traf.transaction_ids.get_range(range)?;
        let setup_transaction_id = transaction_ids.get(0).ok_or(IndexError)?;
        let setup_packet_id =
            self.transaction_index.get(*setup_transaction_id)?;
        let data_packet_id = setup_packet_id + 1;
        let data_packet = self.get_packet(data_packet_id)?;
        let fields = SetupFields::from_data_packet(&data_packet);
        let direction = fields.type_fields.direction();
        let mut data: Vec<u8> = Vec::new();
        for id in transaction_ids {
            let transaction = self.get_transaction(id)?;
            match (direction,
                   transaction.pid,
                   transaction.payload_byte_range)
            {
                (Direction::In,  PID::IN,  Some(range)) |
                (Direction::Out, PID::OUT, Some(range)) => {
                    data.extend_from_slice(
                        &self.packet_data.get_range(range)?);
                },
                (..) => {}
            };
        }
        Ok(ControlTransfer {
            address,
            fields,
            data,
        })
    }

    pub fn get_device_item(&mut self, parent: &Option<DeviceItem>, index: u64)
        -> Result<DeviceItem, CaptureError>
    {
        match parent {
            None => Ok(DeviceItem::Device(DeviceId::from(index + 1))),
            Some(item) => self.device_child(item, index)
        }
    }

    fn device_child(&self, item: &DeviceItem, index: u64)
        -> Result<DeviceItem, CaptureError>
    {
        use DeviceItem::*;
        Ok(match item {
            Device(dev) => match index {
                0 => DeviceDescriptor(*dev),
                conf => Configuration(*dev,
                    ConfigNum(conf.try_into()?)),
            },
            DeviceDescriptor(dev) =>
                DeviceDescriptorField(*dev,
                    DeviceField(index.try_into()?)),
            Configuration(dev, conf) => match index {
                0 => ConfigurationDescriptor(*dev, *conf),
                n => Interface(*dev, *conf,
                    InterfaceNum((n - 1).try_into()?)),
            },
            ConfigurationDescriptor(dev, conf) =>
                ConfigurationDescriptorField(*dev, *conf,
                    ConfigField(index.try_into()?)),
            Interface(dev, conf, iface) => match index {
                0 => InterfaceDescriptor(*dev, *conf, *iface),
                n => EndpointDescriptor(*dev, *conf, *iface,
                    EndpointNum((n - 1).try_into()?))
            },
            InterfaceDescriptor(dev, conf, iface) =>
                InterfaceDescriptorField(*dev, *conf, *iface,
                    InterfaceField(index.try_into()?)),
            EndpointDescriptor(dev, conf, iface, ep) =>
                EndpointDescriptorField(*dev, *conf, *iface, *ep,
                    EndpointField(index.try_into()?)),
            _ => return Err(IndexError)
        })
    }

    pub fn device_item_count(&mut self, parent: &Option<DeviceItem>)
        -> Result<u64, CaptureError>
    {
        Ok(match parent {
            None => (self.device_data.len() - 1) as u64,
            Some(item) => self.device_child_count(item)?,
        })
    }

    pub fn get_device_data(&self, id: &DeviceId)
        -> Result<&DeviceData, CaptureError>
    {
        self.device_data.get(id.value as usize).ok_or(IndexError)
    }

    pub fn get_device_data_mut(&mut self, id: &DeviceId)
        -> Result<&mut DeviceData, CaptureError>
    {
        self.device_data.get_mut(id.value as usize).ok_or(IndexError)
    }

    fn device_child_count(&self, item: &DeviceItem)
        -> Result<u64, CaptureError>
    {
        use DeviceItem::*;
        Ok((match item {
            Device(dev) =>
                self.get_device_data(dev)?.configurations.len(),
            DeviceDescriptor(dev) =>
                match self.get_device_data(dev)?.device_descriptor {
                    Some(_) => usb::DeviceDescriptor::NUM_FIELDS,
                    None => 0,
                },
            Configuration(dev, conf) =>
                match self.get_device_data(dev)?.try_get_configuration(conf) {
                    Some(conf) => 1 + conf.interfaces.len(),
                    None => 0
                },
            ConfigurationDescriptor(dev, conf) =>
                match self.get_device_data(dev)?.try_get_configuration(conf) {
                    Some(_) => usb::ConfigDescriptor::NUM_FIELDS,
                    None => 0
                },
            Interface(dev, conf, iface) =>
                match self.get_device_data(dev)?.try_get_configuration(conf) {
                    Some(conf) =>
                        conf.get_interface(iface)?.endpoint_descriptors.len(),
                    None => 0
                },
            InterfaceDescriptor(..) => usb::InterfaceDescriptor::NUM_FIELDS,
            EndpointDescriptor(..) => usb::EndpointDescriptor::NUM_FIELDS,
            _ => 0
        }) as u64)
    }

    pub fn get_device_summary(&mut self, item: &DeviceItem)
        -> Result<String, CaptureError>
    {
        use DeviceItem::*;
        Ok(match item {
            Device(dev) => {
                let device = self.devices.get(*dev)?;
                let data = self.get_device_data(dev)?;
                format!("Device {}: {}", device.address,
                    match data.device_descriptor {
                        Some(descriptor) => format!(
                            "{:04X}:{:04X}",
                            descriptor.vendor_id,
                            descriptor.product_id
                        ),
                        None => "Unknown".to_string(),
                    }
                )
            },
            DeviceDescriptor(dev) => {
                let data = self.get_device_data(dev)?;
                match data.device_descriptor {
                    Some(_) => "Device descriptor",
                    None => "No device descriptor"
                }.to_string()
            },
            DeviceDescriptorField(dev, field) => {
                let data = self.get_device_data(dev)?;
                match data.device_descriptor {
                    Some(desc) => desc.field_text(*field, &data.strings),
                    None => return Err(DescriptorMissing)
                }
            },
            Configuration(_, conf) => format!(
                "Configuration {}", conf),
            ConfigurationDescriptor(..) =>
                "Configuration descriptor".to_string(),
            ConfigurationDescriptorField(dev, conf, field) => {
                let data = self.get_device_data(dev)?;
                let config = data.get_configuration(conf)?;
                config.descriptor.field_text(*field, &data.strings)
            },
            Interface(_, _, iface) => format!(
                "Interface {}", iface),
            InterfaceDescriptor(..) =>
                "Interface descriptor".to_string(),
            InterfaceDescriptorField(dev, conf, iface, field) => {
                let data = self.get_device_data(dev)?;
                let config = data.get_configuration(conf)?;
                let iface = config.get_interface(iface)?;
                iface.descriptor.field_text(*field, &data.strings)
            },
            EndpointDescriptor(dev, conf, iface, ep) => {
                let data = self.get_device_data(dev)?;
                let config = data.get_configuration(conf)?;
                let iface = config.get_interface(iface)?;
                let desc = iface.get_endpoint_descriptor(ep)?;
                format!("Endpoint {} {}",
                    desc.endpoint_address & 0x7F,
                    if desc.endpoint_address & 0x80 != 0 {"IN"} else {"OUT"}
                )
            },
            EndpointDescriptorField(dev, conf, iface, ep, field) => {
                let data = self.get_device_data(dev)?;
                let config = data.get_configuration(conf)?;
                let iface = config.get_interface(iface)?;
                let desc = iface.get_endpoint_descriptor(ep)?;
                desc.field_text(*field)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{BufReader, BufWriter, BufRead, Write};
    use crate::decoder::Decoder;

    fn write_item(cap: &mut Capture, item: &Item, depth: u8,
                  writer: &mut dyn Write)
    {
        let summary = cap.get_summary(&item).unwrap();
        for _ in 0..depth {
            writer.write(b" ").unwrap();
        }
        writer.write(summary.as_bytes()).unwrap();
        writer.write(b"\n").unwrap();
        let num_children = cap.child_count(&item).unwrap();
        for child_id in 0..num_children {
            let child = cap.get_child(&item, child_id).unwrap();
            write_item(cap, &child, depth + 1, writer);
        }
    }

    #[test]
    fn test_captures() {
        let test_dir = "./tests/";
        for result in std::fs::read_dir(test_dir).unwrap() {
            let entry = result.unwrap();
            if entry.file_type().unwrap().is_dir() {
                let path = entry.path();
                let mut cap_path = path.clone();
                let mut ref_path = path.clone();
                let mut out_path = path.clone();
                cap_path.push("capture.pcap");
                ref_path.push("reference.txt");
                out_path.push("output.txt");
                {
                    let mut pcap = pcap::Capture::from_file(cap_path).unwrap();
                    let mut cap = Capture::new().unwrap();
                    let mut decoder = Decoder::new(&mut cap).unwrap();
                    while let Ok(packet) = pcap.next() {
                        decoder.handle_raw_packet(&packet).unwrap();
                    }
                    let out_file = File::create(out_path.clone()).unwrap();
                    let mut out_writer = BufWriter::new(out_file);
                    let num_items = cap.item_index.len();
                    for item_id in 0 .. num_items {
                        let item = cap.get_item(&None, item_id).unwrap();
                        write_item(&mut cap, &item, 0, &mut out_writer);
                    }
                }
                let ref_file = File::open(ref_path).unwrap();
                let out_file = File::open(out_path.clone()).unwrap();
                let ref_reader = BufReader::new(ref_file);
                let out_reader = BufReader::new(out_file);
                let mut out_lines = out_reader.lines();
                for line in ref_reader.lines() {
                    let expected = line.unwrap();
                    let actual = out_lines.next().unwrap().unwrap();
                    assert!(actual == expected);
                }
            }
        }
    }
}
