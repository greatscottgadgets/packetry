use std::borrow::Cow;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::mem::size_of;
use std::path::PathBuf;

use pcap_file::{
    pcap::{PcapReader, PcapWriter, PcapHeader, RawPcapPacket},
    DataLink,
    TsResolution,
};

use anyhow::{Context, Error};

pub struct Loader {
    pcap: PcapReader<BufReader<File>>,
    pub file_size: u64,
    pub bytes_read: u64,
    frac_ns: u64,
    start_time: Option<u64>,
}

pub struct Writer {
    pcap: PcapWriter<BufWriter<File>>,
}

impl Loader {
    pub fn open(path: PathBuf) -> Result<Loader, Error> {
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let reader = BufReader::new(file);
        let pcap = PcapReader::new(reader)?;
        let header = pcap.header();
        let bytes_read = size_of::<PcapHeader>() as u64;
        let frac_ns = match header.ts_resolution {
            TsResolution::MicroSecond => 1_000,
            TsResolution::NanoSecond => 1,
        };
        let start_time = None;
        Ok(Loader{pcap, file_size, bytes_read, frac_ns, start_time})
    }

    pub fn next(&mut self) -> Option<Result<(RawPcapPacket, u64), Error>> {
        match self.pcap.next_raw_packet() {
            None => None,
            Some(Err(e)) => Some(Err(Error::from(e))),
            Some(Ok(packet)) => {
                let raw_timestamp =
                    packet.ts_sec as u64 * 1_000_000_000 +
                    packet.ts_frac as u64 * self.frac_ns;
                let timestamp = if let Some(start) = self.start_time {
                    raw_timestamp - start
                } else {
                    self.start_time = Some(raw_timestamp);
                    0
                };
                let size = 16 + packet.data.len();
                self.bytes_read += size as u64;
                Some(Ok((packet, timestamp)))
            }
        }
    }
}

impl Writer {
    pub fn open(path: PathBuf) -> Result<Writer, Error> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        let header = PcapHeader {
            datalink: DataLink::USB_2_0,
            ts_resolution: TsResolution::NanoSecond,
            .. PcapHeader::default()
        };
        Ok(Writer{pcap: PcapWriter::with_header(writer, header)?})
    }

    pub fn add_packet(&mut self, bytes: &[u8], timestamp_ns: u64) -> Result<(), Error> {
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

    pub fn close(self) -> Result<(), Error> {
        self.pcap.into_writer().flush()?;
        Ok(())
    }
}
