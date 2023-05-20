use packetry::backend::cynthion::{CynthionDevice, CynthionUsability, Speed};
use packetry::capture::{create_capture, CaptureReader, EndpointId, EndpointTransferId};
use packetry::decoder::Decoder;

use anyhow::{Context, Error};
use futures_lite::future::block_on;
use nusb::transfer::RequestBuffer;

const TRANSFER_LENGTH: usize = 0x1000;

fn main() {
    test().unwrap();
}

fn test() -> Result<(), Error> {
    // Create capture and decoder.
    let (writer, mut reader) = create_capture()
        .context("Failed to create capture")?;
    let mut decoder = Decoder::new(writer)
        .context("Failed to create decoder")?;

    // Open analyzer device.
    let analyzer = CynthionDevice::scan()
        .context("Failed to scan for analyzers")?
        .iter()
        .find(|dev| matches!(dev.usability, CynthionUsability::Usable(..)))
        .context("No usable analyzer found")?
        .open()
        .context("Failed to open analyzer")?;

    // Start capture.
    let (packets, stop_handle) = analyzer
        .start(Speed::High,
               |err| err.context("Failure in capture thread").unwrap())
        .context("Failed to start analyzer")?;

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
    let buf = RequestBuffer::new(TRANSFER_LENGTH);
    let transfer = test_interface.bulk_in(0x81, buf);
    let completion = block_on(transfer);
    completion.status.context("Transfer from test device failed")?;
    println!("Read {} bytes from test device", completion.data.len());
    assert_eq!(completion.data.len(), TRANSFER_LENGTH);

    // Stop analyzer.
    stop_handle.stop()
        .context("Failed to stop analyzer")?;

    // Decode all packets that were received.
    for packet in packets {
        decoder.handle_raw_packet(&packet)
            .context("Error decoding packet")?;
    }

    // Look up the endpoint we're interested in and count payload bytes.
    let bytes_captured = bytes_on_endpoint(&mut reader)
        .context("Error counting captured bytes on endpoint")?;
    println!("Captured {}/{} bytes of data read from test device",
             bytes_captured, TRANSFER_LENGTH);
    assert_eq!(bytes_captured, TRANSFER_LENGTH as u64);

    Ok(())
}

fn bytes_on_endpoint(reader: &mut CaptureReader) -> Result<u64, Error> {
    // Endpoint IDs 0 and 1 are special (used for invalid and framing packets).
    // The first normal endpoint in the capture will have endpoint ID 2.
    let endpoint_id = EndpointId::from(2);
    // We're looking for the first and only transfer on the endpoint.
    let ep_transfer_id = EndpointTransferId::from(0);
    let ep_traf = reader.endpoint_traffic(endpoint_id)?;
    let ep_transaction_ids = ep_traf.transfer_index.target_range(
        ep_transfer_id, ep_traf.transaction_ids.len())?;
    let data_range = ep_traf.transfer_data_range(&ep_transaction_ids)?;
    let data_length = ep_traf.transfer_data_length(&data_range)?;
    Ok(data_length)
}
