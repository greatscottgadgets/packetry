use crate::backend::cynthion::{CynthionDevice, CynthionUsability::*, Speed};
use crate::pcap::Writer;
use anyhow::Result;
use ctrlc;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use argh::FromArgs;
#[derive(FromArgs, PartialEq, Debug)]
/// Start packetry in CLI mode
#[argh(subcommand, name = "capture")]
pub struct SubCommandCliCapture {
    /// device (default: auto)
    #[argh(
        option,
        short = 'd',
        long = "device",
        default = "String::from(\"auto\")"
    )]
    device_serial: String,
    /// usb speed (default: low)
    #[argh(option, short = 's', long = "speed", default = "Speed::Low")]
    usb_speed: Speed,
    /// output file (use '-' for stdout, default: cynthion.pcap)
    #[argh(
        option,
        short = 'o',
        long = "output",
        default = "String::from(\"cynthion.pcap\")"
    )]
    output_file: String,
}

pub fn headless_capture(options: SubCommandCliCapture) -> Result<()> {
    let device = select_device(&options.device_serial, options.usb_speed)?;

    let output_writer: Box<dyn Write> = if options.output_file == "-" {
        Box::new(io::stdout())
    } else {
        Box::new(File::create(&options.output_file)?)
    };
    let mut writer = Writer::open(output_writer)?;

    let cynthion = device.open()?;
    let (stream_handle, stop_handle) =
        cynthion.start(options.usb_speed, |e| eprintln!("{:?}", e))?;

    let stop_handle = Arc::new(Mutex::new(Some(stop_handle)));
    ctrlc::set_handler(move || {
        let mut handle_opt = stop_handle.lock().unwrap();
        if let Some(handle) = handle_opt.take() {
            if let Err(e) = handle.stop() {
                eprintln!("Failed to stop Cynthion: {}", e);
            }
        }
    })
    .expect("Error setting CTRL+C handler");

    for packet in stream_handle {
        writer.add_packet(&packet.bytes, packet.timestamp_ns)?;
    }

    writer.close()?;

    Ok(())
}

fn select_device(serial: &str, speed: Speed) -> Result<CynthionDevice> {
    let mut devices = CynthionDevice::scan()?;

    if devices.is_empty() {
        return Err(anyhow::anyhow!("No devices found"));
    }

    let device_index = if serial == "auto" {
        // Select the first device
        Some(0)
    } else {
        // Find the device with the requested serial number
        devices
            .iter()
            .position(|d| d.device_info.serial_number() == Some(&serial.to_string()))
    };

    // Check if the device was found
    let device_index = match device_index {
        Some(index) => index,
        None => return Err(anyhow::anyhow!("Device with serial {} not found", serial)),
    };

    // Remove the device from the list to get ownership
    let device = devices.remove(device_index);

    // Check if the device is usable and supports the requested speed
    match &device.usability {
        Usable(_, speeds) => {
            if !speeds.contains(&speed) {
                return Err(anyhow::anyhow!("Device does not support speed {:?}", speed));
            }
        }

        Unusable(reason) => {
            return Err(anyhow::anyhow!("Device is not usable: {}", reason));
        }
    }

    Ok(device)
}
