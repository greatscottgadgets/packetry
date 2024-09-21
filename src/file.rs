use std::borrow::Cow;
use std::io::{BufReader, BufWriter, Read, Write};
use std::mem::size_of;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{SystemTime, Duration};

use anyhow::{Context, Error, anyhow};
use byteorder_slice::{
    ByteOrder, BigEndian, LittleEndian,
    result::ReadSlice
};
use pcap_file::{
    pcap::{PcapReader, PcapHeader, PcapWriter, RawPcapPacket},
    pcapng::{
        PcapNgReader, PcapNgWriter, PcapNgBlock,
        blocks::{
            Block, RawBlock,
            ENHANCED_PACKET_BLOCK,
            section_header:: {
                SectionHeaderBlock,
                SectionHeaderOption,
            },
            interface_description::{
                InterfaceDescriptionBlock,
                InterfaceDescriptionOption,
            },
            interface_statistics::{
                InterfaceStatisticsBlock,
                InterfaceStatisticsOption,
            },
            enhanced_packet::EnhancedPacketBlock,
        },
    },
    DataLink,
    Endianness,
    TsResolution,
};
use once_cell::sync::Lazy;

use crate::capture::CaptureMetadata;
use crate::usb::Speed;
use crate::version::version;

/// Item type generated by file loaders.
pub enum LoaderItem<PacketData> {
    Packet(PacketData),
    Metadata(Box<CaptureMetadata>),
    LoadError(Error),
    Ignore,
    End
}

/// Interface to packets from capture loaders.
pub trait GenericPacket {
    /// The bytes of the raw packet.
    fn bytes(&self) -> &[u8];

    /// Timestamp in nanoseconds since start of capture.
    fn timestamp_ns(&self) -> u64;

    /// Total bytes read from the input including this packet.
    fn total_bytes_read(&self) -> u64;
}

/// Interface to capture loaders.
pub trait GenericLoader<Source>
where Self: Sized, Source: Read
{
    type PacketData<'p>;

    /// Create a loader for a byte source.
    fn new(source: Source) -> Result<Self, Error>;

    /// Get the next item.
    fn next(&mut self) -> LoaderItem<impl GenericPacket>;
}

/// Interface to capture savers.
pub trait GenericSaver<Dest>
where Self: Sized, Dest: Write
{
    /// Create a saver for a byte sink.
    fn new(dest: Dest, meta: Arc<CaptureMetadata>) -> Result<Self, Error>;

    /// Add the next packet.
    fn add_packet(&mut self, bytes: &[u8], timestamp_ns: u64) -> Result<(), Error>;

    /// Finish saving after the last packet is added.
    fn close(self) -> Result<(), Error>;
}

/// Loader for pcap format.
pub struct PcapLoader<Source: Read> {
    pcap: PcapReader<BufReader<Source>>,
    bytes_read: u64,
    frac_ns: u64,
    start_time: Option<u64>,
}

/// Saver for pcap format.
pub struct PcapSaver<Dest: Write> {
    pcap: PcapWriter<BufWriter<Dest>>,
}

/// Helper type for wrapping existing packets.
pub struct PacketWrapper<PacketData> {
    packet_data: PacketData,
    timestamp_ns: u64,
    total_bytes_read: u64,
}

impl<Source> GenericLoader<Source>
for PcapLoader<Source>
where Source: Read
{
    type PacketData<'p> = PacketWrapper<RawPcapPacket<'p>>;

    fn new(source: Source) -> Result<Self, Error> {
        let reader = BufReader::new(source);
        let pcap = PcapReader::new(reader)?;
        let header = pcap.header();
        let bytes_read = size_of::<PcapHeader>() as u64;
        let frac_ns = match header.ts_resolution {
            TsResolution::MicroSecond => 1_000,
            TsResolution::NanoSecond => 1,
        };
        let start_time = None;
        Ok(PcapLoader{pcap, bytes_read, frac_ns, start_time})
    }

    fn next(&mut self) -> LoaderItem<impl GenericPacket> {
        use LoaderItem::*;
        match self.pcap.next_raw_packet() {
            None => End,
            Some(Err(e)) => LoadError(anyhow!(e)),
            Some(Ok(raw_packet)) => {
                let raw_timestamp =
                    raw_packet.ts_sec as u64 * 1_000_000_000 +
                    raw_packet.ts_frac as u64 * self.frac_ns;
                let timestamp = if let Some(start) = self.start_time {
                    raw_timestamp - start
                } else {
                    self.start_time = Some(raw_timestamp);
                    0
                };
                let size = 16 + raw_packet.data.len();
                self.bytes_read += size as u64;
                Packet(
                    PacketWrapper {
                        packet_data: raw_packet,
                        timestamp_ns: timestamp,
                        total_bytes_read: self.bytes_read
                    }
                )
            }
        }
    }
}

impl<Dest> GenericSaver<Dest>
for PcapSaver<Dest>
where Self: Sized, Dest: Write
{
    fn new(dest: Dest, _meta: Arc<CaptureMetadata>) -> Result<Self, Error> {
        let writer = BufWriter::new(dest);
        let header = PcapHeader {
            datalink: DataLink::USB_2_0,
            ts_resolution: TsResolution::NanoSecond,
            .. PcapHeader::default()
        };
        Ok(PcapSaver { pcap: PcapWriter::with_header(writer, header)? })
    }

    fn add_packet(&mut self, bytes: &[u8], timestamp_ns: u64)
        -> Result<(), Error>
    {
        let length: u32 = bytes
            .len()
            .try_into()
            .context("Packet too large for pcap file")?;
        let packet = RawPcapPacket {
            ts_sec: (timestamp_ns / 1_000_000_000) as u32,
            ts_frac: (timestamp_ns % 1_000_000_000) as u32,
            incl_len: length,
            orig_len: length,
            data: Cow::from(bytes)
        };
        self.pcap.write_raw_packet(&packet)?;
        Ok(())
    }

    fn close(self) -> Result<(), Error> {
        self.pcap.into_writer().flush()?;
        Ok(())
    }
}

impl GenericPacket for PacketWrapper<RawPcapPacket<'_>> {
    fn bytes(&self) -> &[u8] { &self.packet_data.data }
    fn timestamp_ns(&self) -> u64 { self.timestamp_ns }
    fn total_bytes_read(&self) -> u64 { self.total_bytes_read }
}

/// Loader for pcap-ng format.
pub struct PcapNgLoader<Source: Read> {
    pcap: PcapNgReader<BufReader<Source>>,
    initial_metadata: Option<CaptureMetadata>,
    bytes_read: u64,
    endianness: Endianness,
    interface_ts_units: Vec<u64>,
    ts_start: Option<u64>,
}

/// Saver for pcap-ng format.
pub struct PcapNgSaver<Dest: Write> {
    pcap: PcapNgWriter<BufWriter<Dest>>,
    meta: Arc<CaptureMetadata>,
}

/// Helper for parsing Enhanced Packet Blocks.
fn parse_epb<B: ByteOrder>(raw_block: &RawBlock<'_>)
    -> Result<(usize, u64, usize), Error>
{
    let mut slice = raw_block.body.deref();
    let interface_id = slice.read_u32::<B>()? as usize;
    let timestamp_hi = slice.read_u32::<B>()? as u64;
    let timestamp_lo = slice.read_u32::<B>()? as u64;
    let length = slice.read_u32::<B>()? as usize;
    let timestamp = (timestamp_hi << 32) + timestamp_lo;
    Ok((interface_id, timestamp, length))
}

impl<Source> GenericLoader<Source>
for PcapNgLoader<Source>
where Source: Read
{
    type PacketData<'p> = PacketWrapper<(RawBlock<'p>, usize)>;

    fn new(source: Source) -> Result<Self, Error> {
        let reader = BufReader::new(source);
        let pcap = PcapNgReader::new(reader)?;
        let section_header = pcap.section();
        let endianness = section_header.endianness;
        let header_length = {
            let mut tmp = Vec::<u8>::new();
            section_header.write_to::<LittleEndian, Vec<u8>>(&mut tmp)?;
            tmp.len()
        };
        let initial_metadata = Some({
            let mut meta = CaptureMetadata::default();
            for option in &section_header.options {
                use SectionHeaderOption::*;
                match option {
                    UserApplication(application) => {
                        meta.application.replace(application.to_string());
                    },
                    OS(os) => {
                        meta.os.replace(os.to_string());
                    },
                    Hardware(hardware) => {
                        meta.hardware.replace(hardware.to_string());
                    },
                    Comment(comment) => {
                        meta.comment.replace(comment.to_string());
                    },
                    _ => {}
                };
            }
            meta
        });
        Ok(PcapNgLoader{
            pcap,
            initial_metadata,
            endianness,
            interface_ts_units: vec![],
            bytes_read: header_length as u64,
            ts_start: None
        })
    }

    fn next(&mut self) -> LoaderItem<impl GenericPacket> {
        use Endianness::*;
        use DataLink::*;
        use LoaderItem::*;
        if let Some(meta) = self.initial_metadata.take() {
            return Metadata(Box::new(meta))
        }
        let raw_block = match self.pcap.next_raw_block() {
            None => return End,
            Some(Err(e)) => return LoadError(anyhow!(e)),
            Some(Ok(block)) => block
        };
        self.bytes_read += raw_block.initial_len as u64;
        if raw_block.type_ == ENHANCED_PACKET_BLOCK {
            let parse_result = match self.endianness {
                Big => parse_epb::<BigEndian>(&raw_block),
                Little => parse_epb::<LittleEndian>(&raw_block),
            };
            let (interface_id, ts_value, length) = match parse_result {
                Ok(values) => values,
                Err(e) => return LoadError(anyhow!(e)),
            };
            let ts_unit = match self.interface_ts_units.get(interface_id) {
                Some(unit) => unit,
                None => return LoadError(anyhow!(
                    "Missing block for interface {interface_id}"
                ))
            };
            let timestamp_ns = if let Some(ts_start) = self.ts_start {
                ts_unit * (ts_value - ts_start)
            } else {
                self.ts_start = Some(ts_value);
                0
            };
            return Packet(
                PacketWrapper {
                    packet_data: (raw_block, length),
                    timestamp_ns,
                    total_bytes_read: self.bytes_read,
                }
            )
        }
        let parsed_block = match self.endianness {
            Big => raw_block.try_into_block::<BigEndian>(),
            Little => raw_block.try_into_block::<LittleEndian>(),
        };
        match parsed_block {
            Err(e) => return LoadError(anyhow!(e)),
            Ok(Block::SectionHeader(_)) =>
                return LoadError(anyhow!(
                    "Multiple sections are not supported.")),
            Ok(Block::InterfaceDescription(interface)) => {
                use InterfaceDescriptionOption::*;
                use Speed::*;
                if !self.interface_ts_units.is_empty() {
                    return LoadError(anyhow!(
                        "Multiple interfaces are not supported"))
                }
                let mut meta = CaptureMetadata::default();
                match interface.linktype {
                    USB_2_0 => {},
                    USB_2_0_HIGH_SPEED => {
                        meta.iface_speed.replace(High);
                    },
                    USB_2_0_FULL_SPEED => {
                        meta.iface_speed.replace(Full);
                    },
                    USB_2_0_LOW_SPEED => {
                        meta.iface_speed.replace(Low);
                    },
                    _ => return LoadError(anyhow!(
                        "Link type {:?} is not supported.",
                        interface.linktype)),
                };
                meta.iface_snaplen = NonZeroU32::new(interface.snaplen);
                let mut ts_units_specified = false;
                for option in interface.options {
                    match option {
                        IfTsResol(res) => {
                            let ts_unit = 1_000_000_000 / match res {
                                0x00..=0x7f => 10u64.pow(res as u32),
                                0x80..=0xff => 2u64.pow((res & 0x7f) as u32)
                            };
                            self.interface_ts_units.push(ts_unit);
                            ts_units_specified = true;
                        },
                        IfDescription(desc) => {
                            meta.iface_desc.replace(desc.to_string());
                        },
                        IfHardware(hw) => {
                            meta.iface_hardware.replace(hw.to_string());
                        },
                        IfOs(os) => {
                            meta.iface_os.replace(os.to_string());
                        },
                        _ => {}
                    };
                }
                if !ts_units_specified {
                    self.interface_ts_units.push(1000);
                }
                return Metadata(Box::new(meta))
            },
            Ok(Block::InterfaceStatistics(stats)) => {
                use InterfaceStatisticsOption::*;
                let mut meta = CaptureMetadata::default();
                for option in stats.options {
                    match option {
                        IsbStartTime(time) => {
                            meta.start_time.replace(
                                SystemTime::UNIX_EPOCH + Duration::from_nanos(
                                    time * self.interface_ts_units[0]
                                )
                            );
                        },
                        IsbEndTime(time) => {
                            meta.end_time.replace(
                                SystemTime::UNIX_EPOCH + Duration::from_nanos(
                                    time * self.interface_ts_units[0]
                                )
                            );
                        },
                        IsbIfDrop(pkts) => {
                            meta.dropped.replace(pkts);
                        },
                        _ => {}
                    };
                }
                return Metadata(Box::new(meta))
            },
            _ => {}
        };
        Ignore
    }
}

fn string(string: &str) -> Option<Cow<'_, str>> {
    Some(Cow::from(string))
}

fn speed_bps(speed: &Speed) -> Option<u64> {
    use Speed::*;
    match speed {
        Low  => Some(  1_500_000),
        Full => Some( 12_000_000),
        High => Some(480_000_000),
        Auto => None,
    }
}

fn time_nanos(time: &SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_nanos().try_into().ok())
}

macro_rules! option {
    ($src: ident,
     $dest: ident,
     $name: ident,
     $variant: ident,
     $converter: expr) => {
        if let Some($name) = &$src.$name {
            if let Some(value) = $converter($name) {
                $dest.push($variant(value))
            }
        }
    }
}

fn iface_options(meta: &CaptureMetadata)
    -> Vec<InterfaceDescriptionOption>
{
    use InterfaceDescriptionOption::*;
    // Always store nanosecond resolution.
    let mut opt = vec![IfTsResol(9)];
    option!(meta, opt, iface_desc, IfDescription, string);
    option!(meta, opt, iface_hardware, IfHardware, string);
    option!(meta, opt, iface_os, IfOs, string);
    option!(meta, opt, iface_speed, IfSpeed, speed_bps);
    opt
}

fn stats_options(meta: &CaptureMetadata)
    -> Vec<InterfaceStatisticsOption>
{
    use InterfaceStatisticsOption::*;
    let mut opt = Vec::new();
    option!(meta, opt, start_time, IsbStartTime, time_nanos);
    option!(meta, opt, end_time, IsbEndTime, time_nanos);
    if let Some(pkts) = meta.dropped {
        opt.push(IsbIfDrop(pkts));
    }
    opt
}

impl<Dest> GenericSaver<Dest>
for PcapNgSaver<Dest>
where Self: Sized, Dest: Write
{
    fn new(dest: Dest, meta: Arc<CaptureMetadata>) -> Result<Self, Error> {
        static APPLICATION: Lazy<String> = Lazy::new(||
            format!("Packetry {}", version())
        );
        static SECTION: Lazy<SectionHeaderBlock> = Lazy::new(||
            SectionHeaderBlock {
                options: vec![
                    SectionHeaderOption::UserApplication(
                        Cow::from(Lazy::force(&APPLICATION))
                    ),
                    SectionHeaderOption::OS(
                        Cow::from(std::env::consts::OS)
                    ),
                    SectionHeaderOption::Hardware(
                        Cow::from(std::env::consts::ARCH)
                    ),
                ],
                .. Default::default()
            }
        );
        let writer = BufWriter::new(dest);
        let section = Lazy::force(&SECTION).clone();
        let mut pcap = PcapNgWriter::with_section_header(writer, section)?;
        pcap.write_block(&Block::InterfaceDescription(
            InterfaceDescriptionBlock {
                linktype: DataLink::USB_2_0,
                snaplen: meta.iface_snaplen.map_or(0, NonZeroU32::get),
                options: iface_options(&meta),
            }
        ))?;
        Ok(PcapNgSaver { pcap, meta })
    }

    fn add_packet(&mut self, bytes: &[u8], timestamp_ns: u64)
        -> Result<(), Error>
    {
        let length: u32 = bytes
            .len()
            .try_into()
            .context("Packet too large for pcap file")?;
        let timestamp = Duration::from_nanos(timestamp_ns);
        self.pcap.write_block(&Block::EnhancedPacket(
            EnhancedPacketBlock {
                interface_id: 0,
                timestamp,
                original_len: length,
                data: Cow::from(bytes),
                options: vec![],
            }
        ))?;
        Ok(())
    }

    fn close(mut self) -> Result<(), Error> {
        self.pcap.write_block(&Block::InterfaceStatistics(
            InterfaceStatisticsBlock {
                interface_id: 0,
                timestamp: match self.meta.end_time {
                    Some(end) => end
                        .duration_since(SystemTime::UNIX_EPOCH)?
                        .as_nanos()
                        .try_into()
                        .context("Timestamp overflow")?,
                    None => 0
                },
                options: stats_options(&self.meta)
            }
        ))?;
        self.pcap.into_inner().flush()?;
        Ok(())
    }
}

impl GenericPacket for PacketWrapper<(RawBlock<'_>, usize)> {
    fn bytes(&self) -> &[u8] {
        let (raw_block, length) = &self.packet_data;
        &raw_block.body[20..][..*length]
    }
    fn timestamp_ns(&self) -> u64 { self.timestamp_ns }
    fn total_bytes_read(&self) -> u64 { self.total_bytes_read }
}
