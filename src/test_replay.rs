use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use gtk::prelude::*;

use itertools::assert_equal;
use pcap_file::pcap::PcapReader;
use serde_json::Deserializer;

use packetry::decoder::Decoder;
use packetry::model::GenericModel;
use packetry::row_data::{GenericRowData, TrafficRowData, DeviceRowData};
use packetry::record_ui::UiAction;
use packetry::ui::{
    UserInterface,
    activate,
    reset_capture,
    update_view,
    with_ui,
};

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry.test"),
        Default::default(),
    );
    application.connect_activate(|app| {
        activate(app)
            .expect("Failed to activate UI");
        check_replays();
        app.quit();
    });
    application.run_with_args::<&str>(&[]);
}

fn check_replays() {
    let test_dir = PathBuf::from("./tests/ui/");
    let mut list_path = test_dir.clone();
    list_path.push("tests.txt");
    let list_file = File::open(list_path)
        .expect("Failed to open list of replay tests");
    let mut comparisons = Vec::new();
    for result in BufReader::new(list_file).lines() {
        let test_name = result
            .expect("Failed to read next replay test from file");
        let mut test_path = test_dir.clone();
        test_path.push(test_name);
        let mut act_path = test_path.clone();
        let mut ref_path = test_path.clone();
        let mut out_path = test_path.clone();
        act_path.push("actions.json");
        ref_path.push("reference.txt");
        out_path.push("output.txt");
        let action_file = File::open(act_path)
            .expect("Failed to open replay test action file");
        let output_file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(out_path.clone())
            .expect("Failed to open output file for writing");
        with_ui(|ui| {
            ui.recording
                .borrow_mut()
                .set_output(output_file);
            Ok(())
        }).unwrap();
        comparisons.push((ref_path, out_path));
        let mut action_reader = BufReader::new(action_file);
        let deserializer = Deserializer::from_reader(&mut action_reader);
        let mut replay = None;
        for result in deserializer.into_iter::<UiAction>() {
            use UiAction::*;
            let action = result
                .expect("Failed to deserialize action");
            match (action, &mut replay) {
                (Open(path), _) => {
                    reset_capture()
                        .expect("Resetting capture failed");
                    let mut capture = None;
                    with_ui(|ui| {
                        capture = Some(ui.capture.clone());
                        ui.recording
                            .borrow_mut()
                            .log_open_file(&path, &ui.capture);
                        Ok(())
                    }).unwrap();
                    if let Some(capture) = capture {
                        let file = File::open(path)
                            .expect("Failed to open pcap file");
                        let reader = BufReader::new(file);
                        let pcap = PcapReader::new(reader)
                            .expect("Failed to read pcap file");
                        let decoder = Decoder::default();
                        replay = Some((pcap, decoder, capture));
                    }
                },
                (Update(count),
                 Some((pcap, decoder, capture))) => {
                    with_ui(|ui| {
                        ui.recording
                            .borrow_mut()
                            .log_update(count);
                        Ok(())
                    }).unwrap();
                    let mut cap = capture
                        .lock()
                        .expect("Failed to lock capture");
                    while cap.packet_index.len() < count {
                        let packet = pcap
                            .next_raw_packet()
                            .expect("No next pcap packet")
                            .expect("Error in pcap reader");
                        decoder
                            .handle_raw_packet(&mut cap, &packet.data)
                            .expect("Failed to decode packet");
                    }
                    drop(cap);
                    update_view()
                        .expect("Failed to update view");
                },
                (SetExpanded(name, position, expanded),
                 Some(..)) => {
                    with_ui(|ui| {
                        ui.recording
                            .borrow_mut()
                            .log_item_expanded(
                                &name, position, expanded);
                        set_expanded(ui, &name, position, expanded);
                        Ok(())
                    }).unwrap();
                },
                (..) => panic!("Unsupported action")
            }
        }
    }
    for (ref_path, out_path) in comparisons {
        let ref_file = File::open(ref_path)
            .expect("Failed to open reference file");
        let out_file = File::open(out_path)
            .expect("Failed to open output file for reading");
        let ref_reader = BufReader::new(ref_file);
        let out_reader = BufReader::new(out_file);
        assert_equal(
            ref_reader
                .lines()
                .map(|result| result.expect("Failed to read line")),
            out_reader
                .lines()
                .map(|result| result.expect("Failed to read line"))
        );
    }
}

fn set_expanded(ui: &mut UserInterface,
                name: &str,
                position: u32,
                expanded: bool)
{
    match name {
        "traffic" => {
            let model = ui.traffic_model
                .as_ref()
                .expect("UI has no traffic model");
            let node = model.item(position)
                .expect("Failed to retrieve list item")
                .downcast::<TrafficRowData>()
                .expect("List item is not TrafficRowData")
                .node()
                .expect("Failed to get node from TrafficRowData");
            model.set_expanded(&node, position, expanded)
                .expect("Failed to expand/collapse item");
        },
        "devices" => {
            let model = ui.device_model
                .as_ref()
                .expect("UI has no device model");
            let node = model.item(position)
                .expect("Failed to retrieve list item")
                .downcast::<DeviceRowData>()
                .expect("List item is not DeviceRowData")
                .node()
                .expect("Failed to get node from DeviceRowData");
            model.set_expanded(&node, position, expanded)
                .expect("Failed to expand/collapse item");
        },
        _ => panic!("Unknown model name")
    }
}
