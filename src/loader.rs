use std::fs::File;
use std::io::BufReader;
use std::mem::size_of;
use std::path::PathBuf;

use pcap_file::{
    pcap::{PcapReader, PcapHeader, RawPcapPacket},
    TsResolution,
};

use anyhow::Error;

pub struct Loader {
    pcap: PcapReader<BufReader<File>>,
    pub file_size: u64,
    pub bytes_read: u64,
    frac_ns: u64,
    start_time: Option<u64>,
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
