use std::collections::BTreeMap;
use std::mem::size_of;
use std::ops::Range;

use bytemuck_derive::{Pod, Zeroable};
use bytemuck::pod_read_unaligned;
use crc::{Crc, CRC_16_USB};
use num_enum::{IntoPrimitive, FromPrimitive};
use derive_more::{From, Into, Display};
use usb_ids::FromId;

use crate::util::titlecase;
use crate::vec_map::VecMap;

fn crc16(bytes: &[u8]) -> u16 {
    const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_USB);
    CRC16.checksum(bytes)
}

// We can't use the CRC_5_USB implementation, because we need to
// compute the CRC over either 11 or 19 bits of data, rather than
// over an integer number of bytes.

pub fn crc5(mut input: u32, num_bits: u32) -> u8 {
    let mut state: u32 = 0x1f;
    for _ in 0..num_bits {
        let cmp = input & 1 != state & 1;
        input >>= 1;
        state >>= 1;
        if cmp {
            state ^= 0x14;
        }
    }
    (state ^ 0x1f) as u8
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Copy, Clone, Debug, Default, IntoPrimitive, FromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum PID {
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

impl std::fmt::Display for PID {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl From<&u8> for PID {
    fn from(byte: &u8) -> PID {
        PID::from(*byte)
    }
}

pub fn validate_packet(packet: &[u8]) -> Result<PID, Option<PID>> {
    use PID::*;

    match packet.first().map(PID::from) {
        // A zero-byte packet is always invalid, and has no PID.
        None => Err(None),

        // Otherwise, check validity according to PID.
        Some(pid) => {
            let len = packet.len();
            let valid = match pid {

                // SOF and tokens must be three bytes, with a valid CRC5.
                SOF | SETUP | IN | OUT | PING if len == 3 => {
                    let data = u32::from_le_bytes(
                        [packet[1], packet[2] & 0x07, 0, 0]);
                    let crc = packet[2] >> 3;
                    crc == crc5(data, 11)
                }

                // SPLIT packets must be four bytes, with a valid CRC5.
                SPLIT if len == 4 => {
                    let data = u32::from_le_bytes(
                        [packet[1], packet[2], packet[3] & 0x07, 0]);
                    let crc = packet[3] >> 3;
                    crc == crc5(data, 19)
                },

                // Data packets must be 3 to 1027 bytes, with a valid CRC16.
                DATA0 | DATA1 | DATA2 | MDATA if (3..=1027).contains(&len) => {
                    let data = &packet[1..(len - 2)];
                    let crc = u16::from_le_bytes([packet[len - 2], packet[len - 1]]);
                    crc == crc16(data)
                }

                // Handshake packets must be a single byte.
                ACK | NAK | NYET | STALL | ERR if len == 1 => true,

                // Anything else is invalid.
                _ => false
            };

            if valid {
                // Packet is valid.
                Ok(pid)
            } else {
                // Invalid, but has a (possibly wrong or malformed) PID byte.
                Err(Some(pid))
            }
        }
    }
}

macro_rules! byte_type {
    ($name: ident) => {
        #[derive(Copy, Clone, Debug, Default,
                 PartialEq, Eq, Hash, PartialOrd, Ord,
                 Pod, Zeroable, From, Into, Display)]
        #[repr(transparent)]
        pub struct $name(pub u8);
    }
}

byte_type!(DeviceAddr);
byte_type!(DeviceField);
byte_type!(StringId);
byte_type!(ConfigNum);
byte_type!(ConfigField);
byte_type!(InterfaceNum);
byte_type!(InterfaceAlt);
byte_type!(InterfaceField);
byte_type!(EndpointNum);
byte_type!(EndpointField);
byte_type!(EndpointAddr);
byte_type!(EndpointAttr);
byte_type!(IfaceAssocField);

pub type InterfaceKey = (InterfaceNum, InterfaceAlt);

impl EndpointAddr {
    pub fn number(&self) -> EndpointNum {
        EndpointNum(self.0 & 0x7F)
    }

    pub fn direction(&self) -> Direction {
        if self.0 & 0x80 == 0 {
            Direction::Out
        } else {
            Direction::In
        }
    }

    pub fn from_parts(number: EndpointNum, direction: Direction) -> Self {
        EndpointAddr((direction as u8) << 7 | number.0 & 0x7F)
    }
}

impl EndpointAttr {
    pub fn endpoint_type(&self) -> EndpointType {
        EndpointType::from(self.0 & 0x03)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u8)]
pub enum EndpointType {
    #[default]
    Control     = 0,
    Isochronous = 1,
    Bulk        = 2,
    Interrupt   = 3,
}

impl std::fmt::Display for EndpointType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Control => write!(f, "control"),
            Self::Isochronous => write!(f, "isochronous"),
            Self::Bulk => write!(f, "bulk"),
            Self::Interrupt => write!(f, "interrupt"),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct BCDVersion {
    pub minor: u8,
    pub major: u8,
}

impl std::fmt::Display for BCDVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:X}.{:02X}", self.major, self.minor)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u8)]
pub enum StartComplete {
    #[default]
    Start = 0,
    Complete = 1,
}

#[derive(Copy, Clone, Debug)]
pub enum Speed {
    Low,
    Full,
}

bitfield! {
    #[derive(Debug)]
    pub struct SOFFields(u16);
    pub u16, frame_number, _: 10, 0;
    pub u8, crc, _: 15, 11;
}

bitfield! {
    #[derive(Debug)]
    pub struct TokenFields(u16);
    pub u8, into DeviceAddr, device_address, _: 6, 0;
    pub u8, into EndpointNum, endpoint_number, _: 10, 7;
    pub u8, crc, _: 15, 11;
}

#[derive(Debug)]
pub struct DataFields {
    pub crc: u16,
}

bitfield! {
    #[derive(Debug)]
    pub struct SplitFields(u32);
    pub u8, into DeviceAddr, hub_address, _: 6, 0;
    pub u8, into StartComplete, sc, _: 7, 7;
    pub u8, port, _: 14, 8;
    pub bool, start, _: 15;
    pub bool, end, _: 16;
    pub u8, into EndpointType, endpoint_type, _: 18, 17;
    pub u8, crc, _: 23, 19;
}

impl SplitFields {
    pub fn from_packet(packet: &[u8]) -> SplitFields {
        SplitFields(
            u32::from_le_bytes(
                [packet[1], packet[2], packet[3], 0]))
    }

    pub fn speed(&self) -> Speed {
        use Speed::*;
        if self.endpoint_type() == EndpointType::Isochronous {
            Full
        } else if self.start() {
            Low
        } else {
            Full
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug)]
pub enum PacketFields {
    SOF(SOFFields),
    Token(TokenFields),
    Data(DataFields),
    Split(SplitFields),
    None
}

impl PacketFields {
    pub fn from_packet(packet: &[u8]) -> Self {
        let end = packet.len();
        use PID::*;
        match PID::from(packet[0]) {
            SOF => PacketFields::SOF(
                SOFFields(
                    u16::from_le_bytes([packet[1], packet[2]]))),
            SETUP | IN | OUT | PING => PacketFields::Token(
                TokenFields(
                    u16::from_le_bytes([packet[1], packet[2]]))),
            DATA0 | DATA1 => PacketFields::Data(
                DataFields{
                    crc: u16::from_le_bytes(
                        [packet[end - 2], packet[end - 1]])}),
            SPLIT => PacketFields::Split(SplitFields::from_packet(packet)),
            _ => PacketFields::None
        }
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
pub enum RequestType {
    Standard = 0,
    Class = 1,
    Vendor = 2,
    #[default]
    Reserved = 3,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
pub enum Recipient {
    Device = 0,
    Interface = 1,
    Endpoint = 2,
    Other = 3,
    #[default]
    Reserved = 4,
}

#[derive(Copy, Clone, Debug, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Direction {
    #[default]
    Out = 0,
    In = 1,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", match self {
            Direction::In  => "IN",
            Direction::Out => "OUT"})
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
    #[repr(C)]
    pub struct RequestTypeFields(u8);
    pub u8, into Recipient, recipient, _: 4, 0;
    pub u8, into RequestType, request_type, _: 6, 5;
    pub u8, into Direction, direction, _: 7, 7;
}

#[derive(Copy, Clone)]
pub struct SetupFields {
    pub type_fields: RequestTypeFields,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupFields {
    pub fn from_data_packet(packet: &[u8]) -> Self {
        SetupFields {
            type_fields: RequestTypeFields(packet[1]),
            request: packet[2],
            value: u16::from_le_bytes([packet[3], packet[4]]),
            index: u16::from_le_bytes([packet[5], packet[6]]),
            length: u16::from_le_bytes([packet[7], packet[8]]),
        }
    }
}

#[derive(Debug, FromPrimitive)]
#[repr(u8)]
pub enum StandardRequest {
    GetStatus = 0,
    ClearFeature = 1,
    SetFeature = 3,
    SetAddress = 5,
    GetDescriptor = 6,
    SetDescriptor = 7,
    GetConfiguration = 8,
    SetConfiguration = 9,
    GetInterface = 10,
    SetInterface = 11,
    SynchFrame = 12,
    #[default]
    Unknown = 13,
}

#[allow(clippy::useless_format)]
impl StandardRequest {
    pub fn description(&self, fields: &SetupFields) -> String {
        use StandardRequest::*;
        match self {
            GetStatus => format!("Getting status"),
            ClearFeature | SetFeature => {
                let feature = StandardFeature::from(fields.value);
                format!("{} {}",
                    match self {
                        ClearFeature => "Clearing",
                        SetFeature => "Setting",
                        _ => ""
                    },
                    feature.description()
                )
            },
            SetAddress => format!("Setting address to {}", fields.value),
            GetDescriptor | SetDescriptor => {
                let descriptor_type =
                    DescriptorType::from((fields.value >> 8) as u8);
                format!(
                    "{} {} #{}{}",
                    match self {
                        GetDescriptor => "Getting",
                        SetDescriptor => "Setting",
                        _ => ""
                    },
                    descriptor_type.description(None),
                    fields.value & 0xFF,
                    match (descriptor_type, fields.index) {
                        (DescriptorType::String, language) if language > 0 =>
                            format!(", language 0x{language:04x}{}",
                                language_name(language)
                                    .map_or_else(
                                        String::new,
                                        |l| format!(" ({l})"))),
                        (..) => format!(""),
                    }
                )
            },
            GetConfiguration => format!("Getting configuration"),
            SetConfiguration => format!("Setting configuration {}", fields.value),
            GetInterface => format!("Getting interface setting"),
            SetInterface => format!("Setting alternate setting {}", fields.value),
            SynchFrame => format!("Synchronising frame"),
            Unknown => format!("Unknown standard request"),
        }
    }
}

fn language_name(code: u16) -> Option<String> {
    let language_id = code & 0x3ff;
    let dialect_id = (code >> 10) as u8;
    let language = usb_ids::Language::from_id(language_id);
    let dialect = usb_ids::Dialect::from_lid_did(language_id, dialect_id);
    match (language, dialect) {
        (Some(language), Some(dialect)) =>
            Some(format!("{}/{}", language.name(), dialect.name())),
        (Some(language), None) =>
            Some(language.name().to_string()),
        _ => None
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, Eq)]
#[repr(u8)]
pub enum DescriptorType {
    Invalid = 0x00,
    Device = 0x01,
    Configuration = 0x02,
    String = 0x03,
    Interface = 0x04,
    Endpoint = 0x05,
    DeviceQualifier = 0x06,
    OtherSpeedConfiguration = 0x07,
    InterfacePower = 0x08,
    OnTheGo = 0x09,
    Debug = 0x0A,
    InterfaceAssociation = 0x0B,
    BinaryObjectStore = 0x0F,
    DeviceCapability = 0x10,
    #[default]
    Unknown = 0xFF,
}

impl DescriptorType {
    fn expected_length(&self) -> Option<usize> {
        use DescriptorType::*;
        match self {
            Device =>
                Some(size_of::<DeviceDescriptor>()),
            Configuration =>
                Some(size_of::<ConfigDescriptor>()),
            InterfaceAssociation =>
                Some(size_of::<InterfaceAssociationDescriptor>()),
            Interface =>
                Some(size_of::<InterfaceDescriptor>()),
            Endpoint =>
                Some(size_of::<EndpointDescriptor>()),
            _ =>
                None
        }
    }

    fn description(&self, bytes: Option<&[u8]>) -> String {
        use DescriptorType::*;
        format!("{} descriptor", match self {
            Invalid => "invalid",
            Device => "device",
            Configuration => "configuration",
            String => "string",
            Interface => "interface",
            Endpoint => "endpoint",
            DeviceQualifier => "device qualifier",
            OtherSpeedConfiguration => "other speed",
            InterfacePower => "interface power",
            OnTheGo => "OTG",
            Debug => "debug",
            InterfaceAssociation => "interface association",
            BinaryObjectStore => "BOS",
            DeviceCapability => "device capability",
            Unknown => if let Some(type_code) = bytes.and_then(|b| b.get(1)) {
                let type_group = match type_code {
                    0x00..=0x1F => "standard",
                    0x20..=0x3F => "class",
                    0x40..=0x5F => "custom",
                    0x60..=0xFF => "reserved",
                };
                return format!("{} descriptor 0x{:02X}",
                               type_group, type_code)
            } else {
                "unknown"
            }
        })
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u16)]
pub enum StandardFeature {
    EndpointHalt = 0,
    DeviceRemoteWakeup = 1,
    TestMode = 2,
    #[default]
    Unknown = 3
}

impl StandardFeature {
    pub fn description(self) -> &'static str {
        const STRINGS: [&str; 4] = [
            "endpoint halt",
            "device remote wakeup",
            "test mode",
            "unknown standard feature",
        ];
        STRINGS[self as usize]
    }
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct DeviceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub usb_version: BCDVersion,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: BCDVersion,
    pub manufacturer_str_id: StringId,
    pub product_str_id: StringId,
    pub serial_str_id: StringId,
    pub num_configurations: u8
}

#[allow(clippy::useless_format)]
impl DeviceDescriptor {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        pod_read_unaligned::<DeviceDescriptor>(bytes)
    }

    pub fn field_text(&self, id: DeviceField,
                      strings: &VecMap<StringId, UTF16ByteVec>)
        -> String
    {
        match id.0 {
        0  => format!("Length: {} bytes", self.length),
        1  => format!("Type: 0x{:02X}", self.descriptor_type),
        2  => format!("USB Version: {}", self.usb_version),
        3  => format!("Class: 0x{:02X}{}", self.device_class,
            usb_ids::Class::from_id(self.device_class)
                .map_or_else(String::new, |c| format!(": {}", c.name()))),
        4  => format!("Subclass: 0x{:02X}{}", self.device_subclass,
            usb_ids::SubClass::from_cid_scid(
                    self.device_class, self.device_subclass)
                .map_or_else(String::new, |s| format!(": {}", s.name()))),
        5  => format!("Protocol: 0x{:02X}{}", self.device_protocol,
            usb_ids::Protocol::from_cid_scid_pid(
                    self.device_class, self.device_subclass,
                    self.device_protocol)
                .map_or_else(String::new, |p| format!(": {}", p.name()))),
        6  => format!("Max EP0 packet size: {} bytes", self.max_packet_size_0),
        7  => format!("Vendor ID: 0x{:04X}{}", self.vendor_id,
            usb_ids::Vendor::from_id(self.vendor_id)
                .map_or_else(String::new, |v| format!(": {}", v.name()))),
        8  => format!("Product ID: 0x{:04X}{}", self.product_id,
            usb_ids::Device::from_vid_pid(self.vendor_id, self.product_id)
                .map_or_else(String::new, |d| format!(": {}", d.name()))),
        9  => format!("Version: {}", self.device_version),
        10 => format!("Manufacturer string: {}",
                      fmt_str_id(strings, self.manufacturer_str_id)),
        11 => format!("Product string: {}",
                      fmt_str_id(strings, self.product_str_id)),
        12 => format!("Serial string: {}",
                      fmt_str_id(strings, self.serial_str_id)),
        i  => format!("Error: Invalid field ID {i}")
        }
    }

    pub const NUM_FIELDS: usize = 13;
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct ConfigDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub total_length: u16,
    pub num_interfaces: u8,
    pub config_value: u8,
    pub config_str_id: StringId,
    pub attributes: u8,
    pub max_power: u8
}

#[allow(clippy::useless_format)]
impl ConfigDescriptor {
    pub fn field_text(&self, id: ConfigField,
                      strings: &VecMap<StringId, UTF16ByteVec>)
        -> String
    {
        match id.0 {
        0 => format!("Length: {} bytes", self.length),
        1 => format!("Type: 0x{:02X}", self.descriptor_type),
        2 => format!("Total length: {} bytes", {
            let length: u16 = self.total_length; length }),
        3 => format!("Number of interfaces: {}", self.num_interfaces),
        4 => format!("Configuration number: {}", self.config_value),
        5 => format!("Configuration string: {}",
                      fmt_str_id(strings, self.config_str_id)),
        6 => format!("Attributes: 0x{:02X}", self.attributes),
        7 => format!("Max power: {}mA", self.max_power as u16 * 2),
        i => format!("Error: Invalid field ID {i}")
        }
    }

    pub const NUM_FIELDS: usize = 8;
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct InterfaceAssociationDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub first_interface: u8,
    pub interface_count: u8,
    pub function_class: u8,
    pub function_subclass: u8,
    pub function_protocol: u8,
    pub function: u8,
}

#[allow(dead_code)]
impl InterfaceAssociationDescriptor {
    pub fn field_text(&self, id: IfaceAssocField) -> String
    {
        match id.0 {
        0 => format!("Length: {} bytes", self.length),
        1 => format!("Type: 0x{:02X}", self.descriptor_type),
        2 => format!("First interface: {}", self.first_interface),
        3 => format!("Interface count: {}", self.interface_count),
        4 => format!("Function class: 0x{:02X}{}", self.function_class,
            usb_ids::Class::from_id(self.function_class)
                .map_or_else(String::new, |c| format!(": {}", c.name()))),
        5  => format!("Function subclass: 0x{:02X}{}", self.function_subclass,
            usb_ids::SubClass::from_cid_scid(
                    self.function_class, self.function_subclass)
                .map_or_else(String::new, |s| format!(": {}", s.name()))),
        6  => format!("Function protocol: 0x{:02X}{}", self.function_protocol,
            usb_ids::Protocol::from_cid_scid_pid(
                    self.function_class, self.function_subclass,
                    self.function_protocol)
                .map_or_else(String::new, |p| format!(": {}", p.name()))),
        7 => format!("Function number: {}", self.function),
        i => format!("Error: Invalid field ID {i}")
        }
    }

    pub const NUM_FIELDS: usize = 8;

    pub fn interface_range(&self) -> Range<InterfaceKey> {
        let start = self.first_interface;
        let count = self.interface_count;
        let start_key = (InterfaceNum(start), InterfaceAlt(0));
        let end_key = (InterfaceNum(start + count), InterfaceAlt(0));
        start_key..end_key
    }
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct InterfaceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub interface_number: InterfaceNum,
    pub alternate_setting: InterfaceAlt,
    pub num_endpoints: u8,
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub interface_str_id: StringId,
}

#[allow(clippy::useless_format)]
impl InterfaceDescriptor {
    pub fn field_text(&self, id: InterfaceField,
                      strings: &VecMap<StringId, UTF16ByteVec>)
        -> String
    {
        match id.0 {
        0 => format!("Length: {} bytes", self.length),
        1 => format!("Type: 0x{:02X}", self.descriptor_type),
        2 => format!("Interface number: {}", self.interface_number),
        3 => format!("Alternate setting: {}", self.alternate_setting),
        4 => format!("Number of endpoints: {}", self.num_endpoints),
        5 => format!("Class: 0x{:02X}{}", self.interface_class,
            usb_ids::Class::from_id(self.interface_class)
                .map_or_else(String::new, |c| format!(": {}", c.name()))),
        6  => format!("Subclass: 0x{:02X}{}", self.interface_subclass,
            usb_ids::SubClass::from_cid_scid(
                    self.interface_class, self.interface_subclass)
                .map_or_else(String::new, |s| format!(": {}", s.name()))),
        7  => format!("Protocol: 0x{:02X}{}", self.interface_protocol,
            usb_ids::Protocol::from_cid_scid_pid(
                    self.interface_class, self.interface_subclass,
                    self.interface_protocol)
                .map_or_else(String::new, |p| format!(": {}", p.name()))),
        8 => format!("Interface string: {}",
                      fmt_str_id(strings, self.interface_str_id)),
        i => format!("Error: Invalid field ID {i}")
        }
    }

    pub const NUM_FIELDS: usize = 9;

    pub fn key(&self) -> InterfaceKey {
        (self.interface_number, self.alternate_setting)
    }
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct EndpointDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub endpoint_address: EndpointAddr,
    pub attributes: EndpointAttr,
    pub max_packet_size: u16,
    pub interval: u8,
}

#[allow(clippy::useless_format)]
impl EndpointDescriptor {
    pub fn field_text(&self, id: EndpointField) -> String {
        match id.0 {
        0 => format!("Length: {} bytes", self.length),
        1 => format!("Type: 0x{:02X}", self.descriptor_type),
        2 => format!("Endpoint address: 0x{:02X}", self.endpoint_address.0),
        3 => format!("Attributes: 0x{:02X}", self.attributes.0),
        4 => format!("Max packet size: {} bytes", {
            let size: u16 = self.max_packet_size; size }),
        5 => format!("Interval: 0x{:02X}", self.interval),
        i => format!("Error: Invalid field ID {i}")
        }
    }

    pub const NUM_FIELDS: usize = 6;
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Descriptor {
    Device(DeviceDescriptor),
    Configuration(ConfigDescriptor),
    InterfaceAssociation(InterfaceAssociationDescriptor),
    Interface(InterfaceDescriptor),
    Endpoint(EndpointDescriptor),
    Other(DescriptorType, Vec<u8>),
    Truncated(DescriptorType, Vec<u8>),
}

impl Descriptor {
    pub fn description(&self) -> String {
        use Descriptor::*;
        match self {
            Device(_) => "Device descriptor".to_string(),
            Configuration(_) => "Configuration descriptor".to_string(),
            Interface(_) => "Interface descriptor".to_string(),
            Endpoint(_) => "Endpoint descriptor".to_string(),
            InterfaceAssociation(_) =>
                "Interface association descriptor".to_string(),
            Other(desc_type, bytes) => format!("{}, {} bytes",
                titlecase(&desc_type.description(Some(bytes))), bytes.len()),
            Truncated(desc_type, bytes) => {
                let description = desc_type.description(Some(bytes));
                let desc_length = bytes[0] as usize;
                let length = bytes.len();
                let expected = desc_type
                    .expected_length()
                    .unwrap_or(desc_length);
                format!("Truncated {} ({} of {} bytes)",
                    description, length, expected)
            }
        }
    }
}

pub struct DescriptorIterator<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> DescriptorIterator<'bytes> {
    fn from(bytes: &'bytes [u8]) -> Self {
        DescriptorIterator {
            bytes,
            offset: 0
        }
    }

    fn decode_descriptor(
        &mut self,
        desc_type: DescriptorType,
        desc_bytes: &[u8],
    ) -> Descriptor {
        // Decide how many bytes to decode.
        let bytes = match desc_type.expected_length() {
            // There aren't enough bytes for this descriptor type.
            Some(expected) if desc_bytes.len() < expected =>
                return Descriptor::Truncated(desc_type, desc_bytes.to_vec()),
            // We have an expected length for this descriptor type.
            // We'll only decode the part we're expecting.
            Some(expected) => &desc_bytes[0 .. expected],
            // We don't have an expected length for this descriptor type.
            // We'll decode all the bytes as a generic descriptor.
            None => desc_bytes,
        };
        match desc_type {
            DescriptorType::Device =>
                Descriptor::Device(
                    DeviceDescriptor::from_bytes(bytes)),
            DescriptorType::Configuration =>
                Descriptor::Configuration(
                    pod_read_unaligned::<ConfigDescriptor>(bytes)),
            DescriptorType::Interface =>
                Descriptor::Interface(
                    pod_read_unaligned::<InterfaceDescriptor>(bytes)),
            DescriptorType::Endpoint =>
                Descriptor::Endpoint(
                    pod_read_unaligned::<EndpointDescriptor>(bytes)),
            DescriptorType::InterfaceAssociation =>
                Descriptor::InterfaceAssociation(
                    pod_read_unaligned::<InterfaceAssociationDescriptor>(bytes)),
            _ => Descriptor::Other(desc_type, bytes.to_vec())
        }
    }
}

impl Iterator for DescriptorIterator<'_> {
    type Item = Descriptor;

    fn next(&mut self) -> Option<Descriptor> {
        use Descriptor::Truncated;
        use DescriptorType::Unknown;
        let remaining = self.bytes.len() - self.offset;
        let (descriptor, bytes_consumed) = match remaining {
            // All bytes consumed by descriptors, none left over.
            0 => return None,
            // Not enough bytes for type and length.
            1 => (Truncated(Unknown, self.bytes[self.offset..].to_vec()), 1),
            _ => {
                let remaining_bytes = &self.bytes[self.offset..];
                let desc_length = remaining_bytes[0] as usize;
                let desc_type = DescriptorType::from(remaining_bytes[1]);
                if desc_length > remaining {
                    // We don't have all the bytes of this descriptor.
                    (Truncated(desc_type, remaining_bytes.to_vec()), remaining)
                } else {
                    // This looks like a valid descriptor, decode it.
                    let bytes = &remaining_bytes[0 .. desc_length];
                    (self.decode_descriptor(desc_type, bytes), desc_length)
                }
            }
        };
        self.offset += bytes_consumed;
        Some(descriptor)
    }
}

pub struct Function {
    pub descriptor: InterfaceAssociationDescriptor,
}

pub struct Endpoint {
    pub descriptor: EndpointDescriptor,
    pub other_descriptors: Vec<Descriptor>,
}

pub struct Interface {
    pub descriptor: InterfaceDescriptor,
    pub endpoints: Vec<Endpoint>,
    pub other_descriptors: Vec<Descriptor>,
}

pub struct Configuration {
    pub descriptor: ConfigDescriptor,
    pub functions: BTreeMap<u8, Function>,
    pub interfaces: BTreeMap<InterfaceKey, Interface>,
    pub other_descriptors: Vec<Descriptor>,
}

impl Configuration {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut result: Option<Configuration> = None;
        let mut iface_key: Option<InterfaceKey> = None;
        let mut ep_index: Option<usize> = None;
        for descriptor in DescriptorIterator::from(bytes) {
            match descriptor {
                Descriptor::Configuration(config_desc) => {
                    result = Some(Configuration {
                        descriptor: config_desc,
                        functions: BTreeMap::new(),
                        interfaces: BTreeMap::new(),
                        other_descriptors: Vec::new(),
                    });
                },
                Descriptor::InterfaceAssociation(assoc_desc) => {
                    if let Some(config) = result.as_mut() {
                        config.functions.insert(
                            assoc_desc.function,
                            Function {
                                descriptor: assoc_desc,
                            }
                        );
                    }
                },
                Descriptor::Interface(iface_desc) => {
                    if let Some(config) = result.as_mut() {
                        let iface_num = iface_desc.interface_number;
                        let iface_alt = iface_desc.alternate_setting;
                        let key = (iface_num, iface_alt);
                        iface_key = Some(key);
                        ep_index = None;
                        config.interfaces.insert(
                            key,
                            Interface {
                                descriptor: iface_desc,
                                endpoints:
                                    Vec::with_capacity(
                                        iface_desc.num_endpoints as usize),
                                other_descriptors: Vec::new(),
                            }
                        );
                    }
                },
                _ => match (result.as_mut(), iface_key) {
                    (Some(config), Some(key)) => {
                        if let Some(iface) = config.interfaces.get_mut(&key) {
                            if let Descriptor::Endpoint(ep_desc) = descriptor {
                                ep_index = Some(iface.endpoints.len());
                                iface.endpoints.push(
                                    Endpoint {
                                        descriptor: ep_desc,
                                        other_descriptors: Vec::new()
                                    }
                                );
                            } else if let Some(i) = ep_index {
                                iface
                                    .endpoints[i]
                                    .other_descriptors
                                    .push(descriptor);
                            } else {
                                iface.other_descriptors.push(descriptor);
                            }
                        }
                    }
                    (Some(config), None) => {
                        config.other_descriptors.push(descriptor);
                    }
                    _ => {}
                },
            };
        }
        result
    }
}

pub enum ControlResult {
    Completed,
    Incomplete,
    Stalled,
}

pub struct ControlTransfer {
    pub address: DeviceAddr,
    pub fields: SetupFields,
    pub data: Vec<u8>,
    pub result: ControlResult,
}

impl ControlTransfer {
    pub fn summary(&self) -> String {
        let request_type = self.fields.type_fields.request_type();
        let direction = self.fields.type_fields.direction();
        let request = self.fields.request;
        let std_req = StandardRequest::from(request);
        let descriptor_type =
            DescriptorType::from((self.fields.value >> 8) as u8);
        let action = match direction {
            Direction::In => "reading",
            Direction::Out => "writing"
        };
        let size = self.data.len();
        let mut parts = vec![format!(
            "{} {}",
            match request_type {
                RequestType::Standard => std_req.description(&self.fields),
                _ => format!(
                    "{:?} request #{}, index {}, value {}",
                    request_type, request,
                    match self.fields.type_fields.recipient() {
                        Recipient::Interface | Recipient::Endpoint =>
                            self.fields.index >> 8,
                        _ => self.fields.index
                    },
                    self.fields.value)
            },
            match self.fields.type_fields.recipient() {
                Recipient::Device => format!(
                    "for device {}", self.address),
                Recipient::Interface => format!(
                    "for interface {}.{}",
                    self.address, self.fields.index as u8),
                Recipient::Endpoint => {
                    let ep_addr = EndpointAddr(self.fields.index as u8);
                    format!("for endpoint {}.{} {}",
                            self.address, ep_addr.number(), ep_addr.direction())
                }
                _ => format!("on device {}", self.address)
            }
        )];
        match (self.fields.length, size) {
            (0, 0) => {}
            (len, _) if size == len as usize => {
                parts.push(format!(", {action} {len} bytes"));
            },
            (len, _) => {
                parts.push(
                    format!(", {action} {size} of {len} requested bytes"));
            }
        };
        match (request_type, std_req, descriptor_type) {
            (RequestType::Standard,
             StandardRequest::GetDescriptor,
             DescriptorType::String)
                if size >= 4 &&
                self.fields.index != 0 =>
            {
                parts.push(
                    format!(": {}", UTF16Bytes(&self.data[2..size])));
            },
            (..) => {}
        };
        let summary = parts.concat();
        match self.result {
            ControlResult::Completed => summary,
            ControlResult::Incomplete => format!("{}, incomplete", summary),
            ControlResult::Stalled => format!("{}, stalled", summary),
        }
    }
}

fn fmt_str_id(strings: &VecMap<StringId, UTF16ByteVec>, id: StringId)
    -> String
{
    match id.0 {
        0 => "(none)".to_string(),
        _ => match &strings.get(id) {
            Some(utf16) => format!("#{id} {utf16}"),
            None => format!("#{id} (not seen)")
        }
    }
}

pub struct UTF16Bytes<'b>(&'b [u8]);

impl<'b> UTF16Bytes<'b> {
    fn chars(&self) -> Vec<u16> {
        self.0.chunks_exact(2)
              .map(|a| u16::from_le_bytes([a[0], a[1]]))
              .collect()
    }
}

impl std::fmt::Display for UTF16Bytes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let chars = self.chars();
        match String::from_utf16(&chars) {
            Ok(string) => write!(f, "'{}'", string.escape_default()),
            Err(_) => write!(f,
                "invalid UTF16, partial decode: '{}'",
                String::from_utf16_lossy(&chars).escape_default())
        }
    }
}

#[derive(Clone)]
pub struct UTF16ByteVec(pub Vec<u8>);

impl UTF16ByteVec {
    pub fn chars(&self) -> Vec<u16> {
        UTF16Bytes(self.0.as_slice()).chars()
    }
}

impl std::fmt::Display for UTF16ByteVec {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        UTF16Bytes(self.0.as_slice()).fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sof() {
        let packet = vec![0xa5, 0xde, 0x1e];
        let p = PacketFields::from_packet(&packet);
        if let PacketFields::SOF(sof) = p {
            assert!(sof.frame_number() == 1758);
            assert!(sof.crc() == 0x03);
        } else {
            panic!("Expected SOF but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_setup() {
        let packet = vec![0x2d, 0x02, 0xa8];
        assert_eq!(validate_packet(&packet), Ok(PID::SETUP));
        let p = PacketFields::from_packet(&packet);
        if let PacketFields::Token(tok) = p {
            assert!(tok.device_address() == DeviceAddr(2));
            assert!(tok.endpoint_number() == EndpointNum(0));
            assert!(tok.crc() == 0x15);
        } else {
            panic!("Expected Token but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_in() {
        let packet = vec![0x69, 0x82, 0x18];
        assert_eq!(validate_packet(&packet), Ok(PID::IN));
        let p = PacketFields::from_packet(&packet);
        if let PacketFields::Token(tok) = p {
            assert!(tok.device_address() == DeviceAddr(2));
            assert!(tok.endpoint_number() == EndpointNum(1));
            assert!(tok.crc() == 0x03);
        } else {
            panic!("Expected Token but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_data() {
        let packet = &vec![0xc3, 0x40, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xd5];
        assert_eq!(validate_packet(&packet), Ok(PID::DATA0));
        let p = PacketFields::from_packet(&packet);
        if let PacketFields::Data(data) = p {
            assert!(data.crc == 0xd5aa);
        } else {
            panic!("Expected Data but got {:?}", p);
        }
    }
}

pub mod prelude {
    #[allow(unused_imports)]
    pub use super::{
        PID,
        PacketFields,
        TokenFields,
        SetupFields,
        SplitFields,
        StartComplete,
        Speed,
        Direction,
        EndpointAddr,
        StandardRequest,
        RequestType,
        Recipient,
        Descriptor,
        DescriptorType,
        DeviceDescriptor,
        ConfigDescriptor,
        InterfaceAssociationDescriptor,
        InterfaceDescriptor,
        EndpointDescriptor,
        Configuration,
        Function,
        Interface,
        ControlTransfer,
        ControlResult,
        DeviceAddr,
        DeviceField,
        StringId,
        ConfigNum,
        ConfigField,
        IfaceAssocField,
        InterfaceNum,
        InterfaceAlt,
        InterfaceKey,
        InterfaceField,
        EndpointNum,
        EndpointField,
        UTF16ByteVec,
    };
}
