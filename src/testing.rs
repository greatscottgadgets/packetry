//! Hardware-in-the loop test using a Cynthion USB analyzer.

use crate::backend::{BackendHandle, TimestampedEvent, Speed};
use crate::backend::cynthion::{CynthionDevice, CynthionHandle, VID_PID};
use crate::capture::prelude::*;
use crate::decoder::Decoder;
use crate::file::{GenericSaver, PcapNgSaver};
use crate::item::{ItemSource, TrafficViewMode};

use anyhow::{Context, Error, bail, ensure};
use futures_lite::future::block_on;
use nusb::{transfer::{Interrupt, In}};
use portable_async_sleep::async_sleep;

use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const US: Duration = Duration::from_micros(1);
const MS: Duration = Duration::from_millis(1);

pub fn test_cynthion(save_captures: bool) {
    use Speed::*;

    let speeds = [
        (High, 0x81, 4096, Some((124*US,  126*US))),
        (Full, 0x82,  512, Some((995*US, 1005*US))),
        (Low,  0x83,   64, None),
    ];

    for (bus_speed, ep_addr, length, sof) in speeds {
        for speed_selection in [bus_speed, Auto] {
            if let Err(e) = block_on(test(
                save_captures,
                bus_speed,
                speed_selection,
                ep_addr,
                length,
                sof
            )) {
                eprintln!("\nTest failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn test(
    save_capture: bool,
    bus_speed: Speed,
    speed_selection: Speed,
    ep_addr: u8,
    length: usize,
    sof: Option<(Duration, Duration)>
) -> Result<(), Error> {
    use Speed::*;

    println!("\nTesting capture at {} with {} speed selected:\n",
             bus_speed.abbr(), speed_selection.abbr());

    // Create capture and decoder.
    let (writer, mut reader) = create_capture()
        .context("Failed to create capture")?;
    let mut decoder = Decoder::new(writer)
        .context("Failed to create decoder")?;

    // Open analyzer device.
    println!("Opening analyzer device");
    let mut candidates = nusb::list_devices()
        .await
        .context("Failed to list USB devices")?
        .filter(|info| (info.vendor_id(), info.product_id()) == VID_PID)
        .collect::<Vec<_>>();
    let device_info = match (candidates.len(), candidates.pop()) {
        (0, None) => bail!("No Cynthion devices found"),
        (1, Some(info)) => info,
        (..) => bail!("Multiple Cynthion devices found"),
    };
    let mut analyzer = CynthionDevice { device_info }
        .open()
        .await
        .context("Failed to open analyzer")?;

    // Tell analyzer to disconnect test device.
    println!("Disabling test device");
    analyzer.configure_test_device(None).await?;
    async_sleep(Duration::from_millis(100)).await;

    // Start capture.
    let analyzer_start_time = Instant::now();
    let (stream_handle, stop_handle) = analyzer
        .start(speed_selection, Box::new(|result|
            result.context("Failure in capture thread").unwrap()))
        .context("Failed to start analyzer")?;

    // Attempt to open and read data from the test device.
    let test_device_result = read_test_device(
        &mut analyzer, bus_speed, ep_addr, length).await;

    // Stop analyzer.
    stop_handle.stop()
        .context("Failed to stop analyzer")?;
    let analyzer_stop_time = Instant::now();

    // Now that capture is stopped, check result of reading test device.
    let bytes_read = test_device_result?;

    // Decode all packets that were received.
    for result in stream_handle {
        let event = result
            .context("Error decoding raw capture data")?;
        use TimestampedEvent::*;
        match event {
            Packet { timestamp_ns, bytes } =>
                decoder.handle_raw_packet(&bytes, timestamp_ns)
                    .context("Error decoding packet")?,
            Event { timestamp_ns, event_type } =>
                decoder.handle_event(event_type, timestamp_ns)
                    .context("Error handling event")?,
        }
    }

    if save_capture {
        // Write the capture to a file.
        let path = PathBuf::from(format!("./HITL-{}-{}.pcapng",
            bus_speed.abbr(), speed_selection.abbr()));
        let file = File::create(path)?;
        let meta = reader.shared.metadata.load_full();
        let mut saver = PcapNgSaver::new(file, meta)?;
        for result in reader.timestamped_packets_and_events()? {
            use PacketOrEvent::*;
            match result? {
                (timestamp, Packet(packet)) =>
                    saver.add_packet(&packet, timestamp)?,
                (timestamp, Event(event_type)) =>
                    saver.add_event(event_type, timestamp)?,
            };
        }
        saver.close()?;
    }

    // Accepted event sequences for each test case.
    let expected_descriptions = match (bus_speed, speed_selection) {
        (High, High) => vec![
            "Capture started at High Speed (480 Mbps)",
        ],
        (High, Auto) => vec![
            "Capture started at Full Speed (12 Mbps)",
            "Bus entered suspend",
            "SE0 line state detected",
            "Bus reset",
            "High Speed negotiation",
            "Speed changed to High Speed (480 Mbps)"
        ],
        (Full, _) => vec![
            "Capture started at Full Speed (12 Mbps)",
            "Bus entered suspend",
            "SE0 line state detected",
            "Bus reset",
            "Full Speed idle state detected",
        ],
        (Low, Low) => vec![
            "Capture started at Low Speed (1.5 Mbps)",
            "Bus entered suspend",
            "SE0 line state detected",
            "Bus reset",
            "Low Speed idle state detected",
        ],
        (Low, Auto) => vec![
            "Capture started at Low Speed (1.5 Mbps)",
            "Bus entered suspend",
            "SE0 line state detected",
            "Bus reset",
            "Low Speed idle state detected",
            "Device attached at Low Speed",
        ],
        _ => unreachable!(),
    };

    for (i, expected) in expected_descriptions.iter().enumerate() {
        let item = reader.item(
            None, TrafficViewMode::Hierarchical, i as u64)?;
        let description = reader.description(&item, false)?;
        if description == *expected {
            println!("Found event: {expected}");
        } else {
            bail!("Event did not have expected description: \
                   expected: '{expected}', found: '{description}')");
        }
    };

    // Look for the test device in the capture.
    let device_id = DeviceId::from(1);
    let device_data = reader.device_data(device_id)?;
    ensure!(device_data.description() == "USB Analyzer Test Device",
            "Device found did not have expected description");
    println!("Found test device in capture");

    // Check captured payload bytes match received ones.
    let bytes_captured = bytes_on_endpoint(&mut reader)
        .context("Error counting captured bytes on endpoint")?;
    println!("Captured {}/{} bytes of data read from test device",
             bytes_captured.len(), length);
    ensure!(bytes_captured.len() == length,
            "Not all data was captured");
    ensure!(bytes_captured == bytes_read,
            "Captured data did not match received data");

    if let Some((min_interval, max_interval)) = sof {
        println!("Checking SOF timestamp intervals");
        // Check SOF timestamps have the expected spacing.
        // SOF packets are assigned to a special endpoint.
        let endpoint_id = FRAMING_EP_ID;
        let ep_traf = reader.endpoint_traffic(endpoint_id)?;
        let count = ep_traf.transaction_count();
        let ep_transaction_ids =
            EndpointTransactionId::from(0)..EndpointTransactionId::from(count);
        let mut sof_count = 0;
        let mut last = None;
        let mut gaps = Vec::new();
        for transaction_id in ep_traf
            .transaction_id_range(&ep_transaction_ids)?
        {
            let range = reader.transaction_packet_range(transaction_id)?;
            for id in range.start.value..range.end.value {
                let packet_id = PacketId::from(id);
                let timestamp = Duration::from_nanos(
                    reader.packet_time(packet_id)?);
                if let Some(prev) = last.replace(timestamp) {
                    let interval = timestamp - prev;
                    if !(interval > min_interval && interval < max_interval) {
                        if interval > 10*MS {
                            // More than 10ms gap, looks like a bus reset.
                            gaps.push(interval);
                            println!("Found a gap of {} ms between SOF packets",
                                     interval.as_millis());
                            continue
                        } else {
                            panic!("SOF interval of {} us is out of range",
                                   interval.as_micros());
                        }
                    }
                }
                sof_count += 1;
            }
        }

        println!("Found {sof_count} SOF packets with expected interval range");

        ensure!(gaps.len() <= 1, "More than one gap in SOF packets seen");

        // Check how long we could have been capturing SOF packets for.
        let max_bus_time = analyzer_stop_time.duration_since(analyzer_start_time);

        // Allow for delays and time when the bus was in reset.
        let min_bus_time = max_bus_time - 500*MS;

        // Calculate how many SOF packets we should have seen.
        let min_count = min_bus_time.as_micros() / max_interval.as_micros();
        let max_count = max_bus_time.as_micros() / min_interval.as_micros();

        println!("Expected to see between {min_count} and {max_count} SOF packets");

        ensure!(sof_count >= min_count, "Not enough SOF packets captured");
        ensure!(sof_count <= max_count, "Too many SOF packets captured");
    }

    // Look for the stop event.
    let item_count = reader.item_count();
    let item = reader.item(
        None, TrafficViewMode::Hierarchical, item_count - 1)?;
    let description = reader.description(&item, false)?;
    assert_eq!(description, "Capture stopped by request");
    println!("Found stop event in capture");

    Ok(())
}

async fn read_test_device(
    analyzer: &mut CynthionHandle,
    speed: Speed,
    ep_addr: u8,
    length: usize)
-> Result<Vec<u8>, Error>
{
    // Tell analyzer to connect test device, then wait for it to enumerate.
    println!("Enabling test device");
    analyzer.configure_test_device(Some(speed)).await?;
    async_sleep(Duration::from_millis(2000)).await;

    // Open test device on AUX port.
    let test_device = nusb::list_devices()
        .await
        .context("Failed to list USB devices")?
        .find(|dev| dev.vendor_id() == 0x1209 && dev.product_id() == 0x000A)
        .context("Test device not found")?
        .open()
        .await
        .context("Failed to open test device")?;
    let test_interface = test_device
        .claim_interface(0)
        .await
        .context("Failed to claim interface 0 on test device")?;
    let mut test_endpoint = match test_interface
        .endpoint::<Interrupt, In>(ep_addr)
    {
        Ok(endpoint) => endpoint,
        Err(_) => bail!("Failed to claim endpoint {ep_addr}")
    };
    // Read some data from the test device.
    println!("Starting read from test device");
    let request = test_endpoint.allocate(length);
    test_endpoint.submit(request);
    let completion = test_endpoint.next_complete().await;
    completion.status.context("Transfer from test device failed")?;
    println!("Read {} bytes from test device", completion.buffer.len());
    ensure!(completion.buffer.len() == length, "Did not complete reading data");
    Ok(completion.buffer.into_vec())
}

fn bytes_on_endpoint(reader: &mut CaptureReader) -> Result<Vec<u8>, Error> {
    // The endpoints in the capture, in order, should be:
    // - The control endpoint for address zero
    // - The control endpoint for the test device
    // - The data endpoint for the test device: this is the one we want.
    let endpoint_id = FIRST_EP_ID + 2;
    // We're looking for the first and only transfer on the endpoint.
    let ep_group_id = EndpointGroupId::from(0);
    let ep_traf = reader.endpoint_traffic(endpoint_id)?;
    let ep_transaction_ids = ep_traf.group_range(ep_group_id)?;
    let data_range = ep_traf.transfer_data_range(&ep_transaction_ids)?;
    let data_length = ep_traf
        .transfer_data_length(&data_range)?
        .try_into()
        .unwrap();
    let data = reader.transfer_bytes(endpoint_id, &data_range, data_length)?;
    Ok(data)
}

impl Speed {
    pub fn abbr(&self) -> &'static str {
        use Speed::*;
        match self {
            Auto => "Auto",
            High => "HS",
            Full => "FS",
            Low => "LS",
        }
    }
}
