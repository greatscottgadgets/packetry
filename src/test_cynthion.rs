use crate::backend::cynthion::{CynthionDevice, CynthionUsability, Speed};
use crate::capture::{
    create_capture,
    CaptureReader,
    DeviceId,
    EndpointId,
    EndpointTransferId,
    PacketId,
};
use crate::decoder::Decoder;
use crate::pcap::Writer;

use anyhow::{Context, Error};
use futures_lite::future::block_on;
use nusb::transfer::RequestBuffer;

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

pub fn run_test(save_captures: bool) {
    for (name, speed, ep_addr, length, sof) in [
        ("HS", Speed::High, 0x81, 4096, Some((124500,  125500, 500))),
        ("FS", Speed::Full, 0x82,  512, Some((995000, 1005000,  50))),
        ("LS", Speed::Low,  0x83,   64, None)]
    {
        test(save_captures, name, speed, ep_addr, length, sof).unwrap();
    }
}

fn test(save_capture: bool,
        name: &str,
        speed: Speed,
        ep_addr: u8,
        length: usize,
        sof: Option<(u64, u64, u64)>)
    -> Result<(), Error>
{
    let desc = speed.description();
    println!("\nTesting at {desc}:\n");

    // Create capture and decoder.
    let (writer, mut reader) = create_capture()
        .context("Failed to create capture")?;
    let mut decoder = Decoder::new(writer)
        .context("Failed to create decoder")?;

    // Open analyzer device.
    println!("Opening analyzer device");
    let mut analyzer = CynthionDevice::scan()
        .context("Failed to scan for analyzers")?
        .iter()
        .find(|dev| matches!(dev.usability, CynthionUsability::Usable(..)))
        .context("No usable analyzer found")?
        .open()
        .context("Failed to open analyzer")?;

    // Tell analyzer to disconnect test device.
    println!("Disabling test device");
    analyzer.configure_test_device(None)?;
    sleep(Duration::from_millis(100));

    // Start capture.
    let (packets, stop_handle) = analyzer
        .start(speed,
               |err| err.context("Failure in capture thread").unwrap())
        .context("Failed to start analyzer")?;

    // Tell analyzer to connect test device, then wait for it to enumerate.
    println!("Enabling test device");
    analyzer.configure_test_device(Some(speed))?;
    sleep(Duration::from_millis(2000));

    // Open test device on AUX port.
    let test_device = nusb::list_devices()
        .context("Failed to list USB devices")?
        .find(|dev| dev.vendor_id() == 0x1209 && dev.product_id() == 0x000A)
        .context("Test device not found")?
        .open()
        .context("Failed to open test device")?;
    let test_interface = test_device.claim_interface(0)
        .context("Failed to claim interface 0 on test device")?;

    // Read some data from the test device.
    println!("Starting read from test device");
    let buf = RequestBuffer::new(length);
    let transfer = test_interface.interrupt_in(ep_addr, buf);
    let completion = block_on(transfer);
    completion.status.context("Transfer from test device failed")?;
    println!("Read {} bytes from test device", completion.data.len());
    assert_eq!(completion.data.len(), length);

    // Stop analyzer.
    stop_handle.stop()
        .context("Failed to stop analyzer")?;

    // Decode all packets that were received.
    for packet in packets {
        decoder.handle_raw_packet(&packet.bytes, packet.timestamp_ns)
            .context("Error decoding packet")?;
    }

    if save_capture {
        // Write the capture to a file.
        let path = PathBuf::from(format!("./HITL-{name}.pcap"));
        let mut writer = Writer::open(path)?;
        for i in 0..reader.packet_index.len() {
            let packet_id = PacketId::from(i);
            let packet = reader.packet(packet_id)?;
            let timestamp_ns = reader.packet_time(packet_id)?;
            writer.add_packet(&packet, timestamp_ns)?;
        }
        writer.close()?;
    }

    // Look for the test device in the capture.
    let device_id = DeviceId::from(1);
    let device_data = reader.device_data(&device_id)?;
    assert_eq!(device_data.description(), "USB Analyzer Test Device");
    println!("Found test device in capture");

    // Check captured payload bytes match received ones.
    let bytes_captured = bytes_on_endpoint(&mut reader)
        .context("Error counting captured bytes on endpoint")?;
    println!("Captured {}/{} bytes of data read from test device",
             bytes_captured.len(), length);
    assert_eq!(bytes_captured.len(), length,
               "Not all data was captured");
    assert_eq!(bytes_captured, completion.data,
               "Captured data did not match received data");

    if let Some((min_interval, max_interval, min_count)) = sof {
        println!("Checking SOF timestamp intervals");
        // Check SOF timestamps have the expected spacing.
        // SOF packets are assigned to endpoint ID 1.
        // We're looking for the first and only transfer on the endpoint.
        let endpoint_id = EndpointId::from(1);
        let ep_transfer_id = EndpointTransferId::from(0);
        let ep_traf = reader.endpoint_traffic(endpoint_id)?;
        let ep_transaction_ids = ep_traf.transfer_index
            .target_range(ep_transfer_id, ep_traf.transaction_ids.len())?;
        let mut sof_count = 0;
        let mut last = None;
        for transaction_id in ep_traf.transaction_ids
            .get_range(&ep_transaction_ids)?
        {
            let range = reader.transaction_index
                .target_range(transaction_id, reader.packet_index.len())?;
            for id in range.start.value..range.end.value {
                let packet_id = PacketId::from(id);
                let timestamp = reader.packet_times.get(packet_id)?;
                if let Some(prev) = last {
                    let interval = timestamp - prev;
                    if !(interval > min_interval && interval < max_interval) {
                        if interval > 10000000 {
                            // More than 10ms gap, assume host stopped sending.
                            continue
                        } else {
                            panic!("SOF interval of {}ns is out of range",
                                   interval);
                        }
                    }
                }
                sof_count += 1;
                last = Some(timestamp);
            }
        }
        println!("Found {} SOF packets with expected interval range", sof_count);
        assert!(sof_count > min_count, "Not enough SOF packets captured");
    }

    Ok(())
}

fn bytes_on_endpoint(reader: &mut CaptureReader) -> Result<Vec<u8>, Error> {
    // Endpoint IDs 0 and 1 are special (used for invalid and framing packets).
    // Endpoint 2 will be the control endpoint for device zero.
    // Endpoint 3 wil be the control endpoint for the test device.
    
    // The first normal endpoint in the capture will have endpoint ID 4.
    let endpoint_id = EndpointId::from(4);
    // We're looking for the first and only transfer on the endpoint.
    let ep_transfer_id = EndpointTransferId::from(0);
    let ep_traf = reader.endpoint_traffic(endpoint_id)?;
    let ep_transaction_ids = ep_traf.transfer_index.target_range(
        ep_transfer_id, ep_traf.transaction_ids.len())?;
    let data_range = ep_traf.transfer_data_range(&ep_transaction_ids)?;
    let data_length = ep_traf
        .transfer_data_length(&data_range)?
        .try_into()
        .unwrap();
    let data = reader.transfer_bytes(endpoint_id, &data_range, data_length)?;
    Ok(data)
}
