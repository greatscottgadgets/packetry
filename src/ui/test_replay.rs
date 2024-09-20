//! Test the UI by replaying previously recorded interactions.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use gtk::prelude::*;

use itertools::assert_equal;
use serde_json::Deserializer;

use crate::decoder::Decoder;
use crate::item::TrafficViewMode;
use crate::file::{GenericPacket, GenericLoader, LoaderItem, PcapLoader};
use crate::ui::{
    model::GenericModel,
    row_data::{GenericRowData, TrafficRowData, DeviceRowData},
    record_ui::UiAction,
    UserInterface,
    activate,
    reset_capture,
    update_view,
    with_ui,
};

// On systems other than macOS we can run this as a normal unit test.
#[cfg(not(target_os="macos"))]
#[test]
fn test_replay() {
    run_test();
}

// On macOS, GTK must run on the main thread, so we spawn a new process.
#[cfg(target_os="macos")]
#[test]
fn spawn_test_replay() {
    procspawn::init();
    let process = procspawn::spawn((), |_| run_test());
    process.join().unwrap();
}

// We register this hook as a constructor, so that the child process - which
// is another copy of the test binary - can begin running our target code
// immediately, rather than going through the rest of the tests again first.
#[cfg(target_os="macos")]
#[ctor]
fn on_cargo_test_startup() {
    procspawn::init();
}

fn run_test() {
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
                    let writer = reset_capture()
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
                        let loader = PcapLoader::new(file)
                            .expect("Failed to create pcap loader");
                        let decoder = Decoder::new(writer)
                            .expect("Failed to create decoder");
                        replay = Some((loader, decoder, capture));
                    }
                },
                (Update(count),
                 Some((loader, decoder, capture))) => {
                    with_ui(|ui| {
                        ui.recording
                            .borrow_mut()
                            .log_update(count);
                        Ok(())
                    }).unwrap();
                    while capture.packet_index.len() < count {
                        use LoaderItem::*;
                        match loader.next() {
                            Packet(packet) => decoder
                                .handle_raw_packet(
                                    packet.bytes(), packet.timestamp_ns())
                                .expect("Failed to decode packet"),
                            Metadata(meta) => decoder.handle_metadata(meta),
                            LoadError(e) => panic!("Error in pcap reader: {e}"),
                            Ignore => continue,
                            End => panic!("No next loader item"),
                        };
                    }
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
        log_name => {
            let mode = TrafficViewMode::from_log_name(log_name);
            let model = ui.traffic_models
                .get(&mode)
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
    }
}
