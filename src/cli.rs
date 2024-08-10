use std::process::ExitCode;

#[macro_use]
extern crate bitfield;

mod backend;
mod pcap;

use crate::backend::cynthion::{CynthionDevice, CynthionUsability::*, Speed};

use crate::pcap::Writer;

use anyhow::Error;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use tabled::{Table, Tabled};

use argh::FromArgs;
#[derive(FromArgs, PartialEq, Debug)]
/// packetry - a fast, intuitive USB 2.0 protocol analysis application for use with Cynthion
struct Args {
    #[argh(subcommand)]
    sub_commands: Option<SubcommandEnum>,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubcommandEnum {
    One(SubCommandCliCapture),
    Two(SubCommandDevices),
}

#[derive(FromArgs, PartialEq, Debug)]
/// Start packetry in CLI mode
#[argh(subcommand, name = "capture")]
struct SubCommandCliCapture {
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

#[derive(FromArgs, PartialEq, Debug)]
/// List capture devices
#[argh(subcommand, name = "devices")]
struct SubCommandDevices {}

#[derive(Tabled)]
pub struct DeviceInfo {
    name: String,
    serial: String,
    useable: String,
    bus: String,
    address: String,
    speeds: String,
}

fn main() -> ExitCode {
    let args: Args = argh::from_env();

    let exit_code = if let Some(subcmd) = args.sub_commands {
        match subcmd {
            SubcommandEnum::One(captureoptions) => {
                if let Err(e) = headless_capture(captureoptions) {
                    eprintln!("Error capturing packets: {}", e);
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }

            SubcommandEnum::Two(_) => {
                if let Err(e) = list_devices() {
                    eprintln!("Error listing devices: {}", e);
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
        }
    } else {
        ExitCode::SUCCESS
    };

    exit_code
}

fn headless_capture(options: SubCommandCliCapture) -> Result<(), Error> {
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

fn select_device(serial: &str, speed: Speed) -> Result<CynthionDevice, Error> {
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

pub fn list_devices() -> Result<(), Error> {
    let devices = CynthionDevice::scan()?;

    if devices.is_empty() {
        println!("No devices found.");
        return Ok(());
    }

    let device_table: Vec<DeviceInfo> = devices
        .iter()
        .map(|device| {
            let info = &device.device_info;

            // Maybe in the future there are more devices to support. Hardcode the name for now.
            let name = "Cynthion".to_string();

            let serial = info
                .serial_number()
                .map_or("None".to_string(), |s| s.to_string());
            let bus = info.bus_number().to_string();
            let address = info.device_address().to_string();

            let (useable, speeds) = match &device.usability {
                Usable(_, speeds) => (
                    "Yes".to_string(),
                    speeds
                        .iter()
                        .map(|speed| {
                            let desc = speed.description();
                            desc.split(' ').next().unwrap_or("").to_string()
                        })
                        .collect::<Vec<String>>()
                        .join(", "),
                ),
                Unusable(reason) => (reason.to_string(), String::new()),
            };

            DeviceInfo {
                name,
                serial,
                useable,
                bus,
                address,
                speeds,
            }
        })
        .collect();

    let table = Table::new(device_table).to_string();
    println!("{}", table);

    Ok(())
}
