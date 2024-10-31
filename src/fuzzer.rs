//! Fuzz the USB packet decoder.

#![allow(dead_code)]
#![no_main]

#[macro_use]
extern crate bitfield;

use libfuzzer_sys::{arbitrary::{Arbitrary, Unstructured}, fuzz_target};

mod capture;
mod database;
mod decoder;
mod pcap;
mod usb;
mod util;

use capture::create_capture;
use decoder::Decoder;

fuzz_target!(|data: &[u8]| {
    let mut input = Unstructured::new(data);
    let packets = Vec::<(Vec::<u8>, u32)>::arbitrary(&mut input).unwrap();
    let mut timestamp = u32::arbitrary(&mut input).unwrap() as u64;
    let (writer, _reader) = create_capture().unwrap();
    let mut decoder = Decoder::new(writer).unwrap();
    for (packet, time_delta) in packets {
        timestamp += time_delta as u64;
        decoder.handle_raw_packet(&packet, timestamp).unwrap();
    }
});
