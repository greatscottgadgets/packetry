use anyhow::Result;
use crate::backend::cynthion::{
	CynthionDevice,
	CynthionUsability::*,
	Speed
};
use crate::capture::{
	create_capture,
	PacketId
};
use crate::decoder::Decoder;
use crate::pcap::Writer;
use std::fs::File;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use ctrlc;

use argh::FromArgs;
#[derive(FromArgs, PartialEq, Debug)]
/// Start packetry in CLI mode
#[argh(subcommand, name = "capture")]
pub struct SubCommandCliCapture {
	/// device (default: auto)
	#[argh(option, short = 'd', long = "device", default = "String::from(\"auto\")")]
	device_serial: String,
	/// usb speed (default: low)
	#[argh(option, short = 's', long = "speed", default = "Speed::Low")]
	usb_speed: Speed,
	/// output file (use '-' for stdout, default: cynthion.pcap)
	#[argh(option, short = 'o', long = "output", default = "String::from(\"cynthion.pcap\")")]
	output_file: String,
}


pub fn headless_capture(options: SubCommandCliCapture) -> Result<()> {
	let device = select_device(&options.device_serial, options.usb_speed)?;

	let (capture_writer, mut capture_reader) = create_capture()?;
	let mut decoder = Decoder::new(capture_writer)?;

	// Open a writer to stdout if the output file is set to '-'
	let mut stdout_writer = if options.output_file == "-" {
			let stdout_file = std::io::stdout();
			Some(Writer::open(stdout_file)?)
		} else {
			None
		};

	let cynthion = device.open()?;
	let (stream_handle, stop_handle) = cynthion.start(options.usb_speed, |e| eprintln!("{:?}", e))?;
	
	// Stop the capture when CTRL+C is pressed
	// For this set up a watchdog thread that checks if CTRL+C was pressed
	let running = Arc::new(AtomicBool::new(true));
	let r = running.clone();
	let stop_handle = Arc::new(Mutex::new(Some(stop_handle)));
	let stop_handle_clone = Arc::clone(&stop_handle);
	
	ctrlc::set_handler(move || {
		r.store(false, Ordering::SeqCst);
	}).expect("Error setting CTRL+C handler");

	let ctrlc_watchdog = {
		let running = running.clone();
		thread::spawn(move || {
			while running.load(Ordering::SeqCst) {
				thread::sleep(std::time::Duration::from_millis(100));
			}

			if let Some(handle) = stop_handle_clone.lock().unwrap().take() {
				if let Err(e) = handle.stop() {
					eprintln!("Error stopping capture: {}", e);
				}
			}
		})
	};

	let mut counter = 0;
	for packet in stream_handle {
		decoder.handle_raw_packet(&packet.bytes, packet.timestamp_ns)?;

		// Write the packet to stdout
		if let Some(ref mut stdout_writer) = stdout_writer {
			let packet_id = PacketId::from(counter);
			let packet = capture_reader.packet(packet_id)?;
			let timestamp_ns = capture_reader.packet_time(packet_id)?;
			stdout_writer.add_packet(&packet, timestamp_ns)?;
		}

		counter += 1;
	}
	
	ctrlc_watchdog.join().expect("CTRL+C watchdog thread panicked");

	if let Some(stdout_writer) = stdout_writer {
		stdout_writer.close()?;
	}

	// Save the capture to disk
	if options.output_file != "-" {
		let pcap_file = File::create(&options.output_file)?;
		let mut pcap_writer = Writer::open(pcap_file)?;
	
		let packet_count = capture_reader.packet_index.len();
		for i in 0..packet_count {
			let packet_id = PacketId::from(i);
			let packet = capture_reader.packet(packet_id)?;
			let timestamp_ns = capture_reader.packet_time(packet_id)?;
			pcap_writer.add_packet(&packet, timestamp_ns)?;
		}
		pcap_writer.close()?;
		eprintln!("Capture saved to {}", options.output_file);
	}

	Ok(())
}

fn select_device(serial: &str, speed: Speed) -> Result<CynthionDevice> {
	let mut devices = CynthionDevice::scan()?;
	let count = devices.len();

	if count == 0 {
		return Err(anyhow::anyhow!("No devices found"));
	}

	let device_index = if serial == "auto" {
		 // Select the first device
		Some(0)
	} else {
		// Find the device with the requested serial number
		devices.iter().position(|d| d.device_info.serial_number() == Some(&serial.to_string()))
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
		},
		
		Unusable(reason) => {
			return Err(anyhow::anyhow!("Device is not usable: {}", reason));
		}
	}

	Ok(device)
}

