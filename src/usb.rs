use std::mem::size_of;

use bytemuck_derive::{Pod, Zeroable};
use bytemuck::pod_read_unaligned;
use num_enum::{IntoPrimitive, FromPrimitive};
use derive_more::{From, Into, Display};

#[allow(clippy::upper_case_acronyms)]
#[derive(Copy, Clone, Debug, IntoPrimitive, FromPrimitive, PartialEq)]
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

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct DeviceAddr(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct DeviceField(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct StringId(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct ConfigNum(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct ConfigField(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct InterfaceNum(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct InterfaceField(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct EndpointNum(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct EndpointField(pub u8);

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct EndpointAddr(pub u8);

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
}

#[derive(Copy, Clone, Debug, PartialEq, Default,
         Pod, Zeroable, From, Into, Display)]
#[repr(transparent)]
pub struct EndpointAttr(pub u8);

impl EndpointAttr {
    pub fn endpoint_type(&self) -> EndpointType {
        EndpointType::from(self.0 & 0x03)
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
#[repr(u8)]
pub enum EndpointType {
    #[default]
    Control     = 0,
    Isochronous = 1,
    Bulk        = 2,
    Interrupt   = 3,
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

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug)]
pub enum PacketFields {
    SOF(SOFFields),
    Token(TokenFields),
    Data(DataFields),
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

#[derive(Copy, Clone, Debug, Display, FromPrimitive)]
#[repr(u8)]
pub enum Direction {
    #[default]
    Out = 0,
    In = 1,
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
                    "{} {} descriptor #{}{}",
                    match self {
                        GetDescriptor => "Getting",
                        SetDescriptor => "Setting",
                        _ => ""
                    },
                    descriptor_type.description(),
                    fields.value & 0xFF,
                    match (descriptor_type, fields.index) {
                        (DescriptorType::String, language) if language > 0 =>
                            format!(", language 0x{:04x}", language),
                        (..) => format!(""),
                    }
                )
            },
            GetConfiguration => format!("Getting configuration"),
            SetConfiguration => format!("Setting configuration {}", fields.value),
            GetInterface => format!("Getting interface {}", fields.index),
            SetInterface => format!("Setting interface {} to {}",
                                    fields.index, fields.value),
            SynchFrame => format!("Synchronising frame"),
            Unknown => format!("Unknown standard request"),
        }
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum DescriptorType {
    Device = 1,
    Configuration = 2,
    String = 3,
    Interface = 4,
    Endpoint = 5,
    DeviceQualifier = 6,
    OtherSpeedConfiguration = 7,
    InterfacePower = 8,
    #[default]
    Unknown = 9
}

impl DescriptorType {
    pub fn description(self) -> &'static str {
        const STRINGS: [&str; 10] = [
            "invalid",
            "device",
            "configuration",
            "string",
            "interface",
            "endpoint",
            "device qualifier",
            "other speed configuration",
            "interface power",
            "unknown",
        ];
        STRINGS[self as usize]
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
    pub usb: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
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

    pub fn field_text(&self, id: DeviceField, strings: &[Option<Vec<u8>>])
        -> String
    {
        match id.0 {
        0  => format!("Length: {} bytes", self.length),
        1  => format!("Type: 0x{:02X}", self.descriptor_type),
        2  => format!("USB Version: {:X}.{:02X}",
                      self.usb >> 8, self.usb & 0xFF),
        3  => format!("Class: 0x{:02X}", self.device_class),
        4  => format!("Subclass: 0x{:02X}", self.device_subclass),
        5  => format!("Protocol: 0x{:02X}", self.device_protocol),
        6  => format!("Max EP0 packet size: {} bytes", self.max_packet_size_0),
        7  => format!("Vendor ID: 0x{:04X}", self.vendor_id),
        8  => format!("Product ID: 0x{:04X}", self.product_id),
        9  => format!("Version: {:X}.{:02X}",
                      self.device_version >> 8, self.device_version & 0xFF),
        10 => format!("Manufacturer string: {}",
                      fmt_str_id(strings, self.manufacturer_str_id)),
        11 => format!("Product string: {}",
                      fmt_str_id(strings, self.product_str_id)),
        12 => format!("Serial string: {}",
                      fmt_str_id(strings, self.serial_str_id)),
        i  => format!("Error: Invalid field ID {}", i)
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
    pub fn field_text(&self, id: ConfigField, strings: &[Option<Vec<u8>>])
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
        i => format!("Error: Invalid field ID {}", i)
        }
    }

    pub const NUM_FIELDS: usize = 8;
}

#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C, packed)]
pub struct InterfaceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub num_endpoints: u8,
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub interface_str_id: StringId,
}

#[allow(clippy::useless_format)]
impl InterfaceDescriptor {
    pub fn field_text(&self, id: InterfaceField, strings: &[Option<Vec<u8>>])
        -> String
    {
        match id.0 {
        0 => format!("Length: {} bytes", self.length),
        1 => format!("Type: 0x{:02X}", self.descriptor_type),
        2 => format!("Interface number: {}", self.interface_number),
        3 => format!("Alternate setting: {}", self.alternate_setting),
        4 => format!("Number of endpoints: {}", self.num_endpoints),
        5 => format!("Class: 0x{:02X}", self.interface_class),
        6 => format!("Subclass: 0x{:02X}", self.interface_subclass),
        7 => format!("Protocol: 0x{:02X}", self.interface_protocol),
        8 => format!("Interface string: {}",
                      fmt_str_id(strings, self.interface_str_id)),
        i => format!("Error: Invalid field ID {}", i)
        }
    }

    pub const NUM_FIELDS: usize = 9;
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
        i => format!("Error: Invalid field ID {}", i)
        }
    }

    pub const NUM_FIELDS: usize = 6;
}

pub struct Interface {
    pub descriptor: InterfaceDescriptor,
    pub endpoint_descriptors: Vec<EndpointDescriptor>
}

pub struct Configuration {
    pub descriptor: ConfigDescriptor,
    pub interfaces: Vec<Interface>,
}

impl Configuration {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let config_size = size_of::<ConfigDescriptor>();
        let iface_size = size_of::<InterfaceDescriptor>();
        let ep_size = size_of::<EndpointDescriptor>();
        if bytes.len() < config_size {
            return None;
        }
        let config_bytes = &bytes[0 .. config_size];
        let config_desc =
            pod_read_unaligned::<ConfigDescriptor>(config_bytes);
        if config_desc.descriptor_type != DescriptorType::Configuration as u8 {
            return None;
        }
        let mut config = Configuration {
            descriptor: config_desc,
            interfaces:
                Vec::with_capacity(config_desc.num_interfaces as usize),
        };
        let mut offset = config_size;
        for _ in 0 .. config.descriptor.num_interfaces {
            if offset + iface_size > bytes.len() {
                break;
            }
            let iface_bytes = &bytes[offset .. offset + iface_size];
            let iface_desc =
                pod_read_unaligned::<InterfaceDescriptor>(iface_bytes);
            offset += iface_size;
            if iface_desc.descriptor_type != DescriptorType::Interface as u8{
                break;
            }
            let mut iface = Interface {
                descriptor: iface_desc,
                endpoint_descriptors:
                    Vec::with_capacity(iface_desc.num_endpoints as usize),
            };
            while iface.endpoint_descriptors.len() <
                iface.descriptor.num_endpoints as usize
            {
                if offset + ep_size > bytes.len() {
                    break;
                }
                let ep_bytes = &bytes[offset .. offset + ep_size];
                let ep_desc =
                    pod_read_unaligned::<EndpointDescriptor>(ep_bytes);
                offset += ep_desc.length as usize;
                if ep_desc.descriptor_type != DescriptorType::Endpoint as u8 {
                    // Could be HID or other class descriptor; skip over it.
                    continue;
                }
                iface.endpoint_descriptors.push(ep_desc);
            };
            config.interfaces.push(iface);
        };
        Some(config)
    }
}

pub struct ControlTransfer {
    pub address: DeviceAddr,
    pub fields: SetupFields,
    pub data: Vec<u8>,
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
            "{} for {}",
            match request_type {
                RequestType::Standard => std_req.description(&self.fields),
                _ => format!(
                    "{:?} request #{}, index {}, value {}",
                    request_type, request,
                    self.fields.index, self.fields.value)
            },
            match self.fields.type_fields.recipient() {
                Recipient::Device => format!(
                    "device {}", self.address),
                Recipient::Interface => format!(
                    "interface {}.{}", self.address, self.fields.index),
                Recipient::Endpoint => {
                    let ep_addr = EndpointAddr(self.fields.index as u8);
                    format!("endpoint {}.{} {}",
                            self.address, ep_addr.number(), ep_addr.direction())
                }
                _ => format!(
                    "device {}, index {}", self.address, self.fields.index)
            }
        )];
        match (self.fields.length, size) {
            (0, 0) => {}
            (len, _) if size == len as usize => {
                parts.push(format!(", {} {} bytes", action, len));
            },
            (len, _) => {
                parts.push(format!(", {} {} of {} requested bytes",
                                   action, size, len));
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
                    format!(": {}", fmt_utf16(&self.data[2..size])));
            },
            (..) => {}
        };
        parts.concat()
    }
}

fn fmt_str_id(strings: &[Option<Vec<u8>>], id: StringId) -> String {
    match id.0 {
        0 => "(none)".to_string(),
        n => match &strings[n as usize] {
            Some(bytes) => format!("#{} {}", id, fmt_utf16(bytes)),
            None => format!("#{} (not seen)", id)
        }
    }
}

fn fmt_utf16(bytes: &[u8]) -> String {
        let chars: Vec<u16> =
            bytes.chunks_exact(2)
                 .into_iter()
                 .map(|a| u16::from_le_bytes([a[0], a[1]]))
                 .collect();
        match String::from_utf16(&chars) {
            Ok(string) => format!("'{}'", string),
            Err(_) => format!(
                "invalid UTF16, partial decode: '{}'",
                String::from_utf16_lossy(&chars))
        }
}

#[cfg(test)]
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
            assert!(tok.device_address() == DeviceAddr(2));
            assert!(tok.endpoint_number() == EndpointNum(0));
            assert!(tok.crc() == 0x15);
        } else {
            panic!("Expected Token but got {:?}", p);
        }

    }

    #[test]
    fn test_parse_in() {
        let p = PacketFields::from_packet(&vec![0x69, 0x82, 0x18]);
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
        let p = PacketFields::from_packet(&vec![0xc3, 0x40, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xd5]);
        if let PacketFields::Data(data) = p {
            assert!(data.crc == 0xd5aa);
        } else {
            panic!("Expected Data but got {:?}", p);
        }
    }
}
