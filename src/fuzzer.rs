//! Fuzz the USB packet decoder.

#![allow(dead_code)]
#![no_main]

#[macro_use]
extern crate bitfield;

use std::sync::Arc;
use std::fs::File;

use anyhow::Error;
use libfuzzer_sys::{arbitrary::{Arbitrary, Unstructured}, fuzz_target};

pub mod built { include!(concat!(env!("OUT_DIR"), "/built.rs")); }
mod capture;
mod database;
mod decoder;
mod event;
mod file;
mod usb;
mod util;
mod version;

use capture::{CaptureMetadata, create_capture};
use decoder::Decoder;
use file::{GenericSaver, PcapSaver};

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let packets = Vec::<(Vec::<u8>, u32)>::arbitrary(&mut input).unwrap();
    let mut timestamp = u32::arbitrary(&mut input).unwrap() as u64;
    let (writer, _reader) = create_capture().unwrap();
    let mut decoder = Decoder::new(writer).unwrap();
    let metadata = Arc::new(CaptureMetadata::default());
    let file = File::create("fuzz.pcap").unwrap();
    let mut saver = PcapSaver::new(file, metadata).unwrap();
    let handle_packets = || {
        for (packet, time_delta) in packets {
            timestamp += time_delta as u64;
            saver.add_packet(&packet, timestamp)?;
            decoder.handle_raw_packet(&packet, timestamp)?;
        }
        Ok(())
    };
    let result: Result<(), Error> = handle_packets();
    saver.close().unwrap();
    result.unwrap();
});

