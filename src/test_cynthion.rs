use packetry::backend::cynthion::{CynthionDevice, CynthionUsability, Speed};
use packetry::capture::{create_capture, CaptureReader, DeviceId, EndpointId, EndpointTransferId};
use packetry::decoder::Decoder;

use anyhow::{Context, Error};
use futures_lite::future::block_on;
use nusb::transfer::RequestBuffer;

use std::thread::sleep;
use std::time::Duration;

fn main() {
    for (speed, ep_addr, length) in [
        (Speed::High, 0x81, 4096),
        (Speed::Full, 0x82, 512),
        (Speed::Low,  0x83, 64)]
    {
        test(speed, ep_addr, length).unwrap();
    }
}

fn test(speed: Speed, ep_addr: u8, length: usize) -> Result<(), Error> {
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
        decoder.handle_raw_packet(&packet)
            .context("Error decoding packet")?;
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
    assert_eq!(bytes_captured, completion.data[0..bytes_captured.len()],
                   "Captured data did not match received data");

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
