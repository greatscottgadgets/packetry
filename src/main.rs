// On Windows, produce a GUI app rather than a console one.
#![windows_subsystem = "windows"]

// We need the bitfield macro.
#[macro_use]
extern crate bitfield;

// We need the ctor macro for the replay test on macOS.
#[cfg(all(test, target_os = "macos"))]
#[allow(unused_imports)]
#[macro_use]
extern crate ctor;

// Include build-time info.
pub mod built {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

// Declare all modules used.
mod backend;
mod capture;
mod cli_capture;
mod compact_index;
mod data_stream;
mod decoder;
mod devices;
mod expander;
mod id;
mod index_stream;
mod model;
mod pcap;
mod rcu;
mod row_data;
mod stream;
mod test_cynthion;
mod tree_list_model;
mod ui;
mod usb;
mod util;
mod vec_map;
mod version;

// Declare optional modules.
#[cfg(any(test, feature = "record-ui-test"))]
mod record_ui;
#[cfg(test)]
mod test_replay;

use gtk::gio::ApplicationFlags;
use gtk::prelude::*;

use cli_capture::{headless_capture, SubCommandCliCapture};
use devices::list_devices;
use ui::{activate, display_error, open, stop_cynthion};
use version::{version, version_info};

use argh::FromArgs;
#[derive(FromArgs, PartialEq, Debug)]
/// packetry - a fast, intuitive USB 2.0 protocol analysis application for use with Cynthion
struct Args {
    #[argh(subcommand)]
    sub_commands: Option<SubcommandEnum>,

    /// recording (pcap) to open in the GUI
    #[argh(positional)]
    filename: Option<String>,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubcommandEnum {
    One(SubCommandCliCapture),
    Two(SubCommandDevices),
    Three(SubCommandVersion),
    Four(SubCommandTest),
}

#[derive(FromArgs, PartialEq, Debug)]
/// List capture devices
#[argh(subcommand, name = "devices")]
struct SubCommandDevices {}

#[derive(FromArgs, PartialEq, Debug)]
/// Print version information
#[argh(subcommand, name = "version")]
struct SubCommandVersion {
    /// print dependency information with version
    #[argh(switch, long = "dependencies")]
    print_dependencies: bool,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Test an attached Cynthion
#[argh(subcommand, name = "test-cynthion")]
struct SubCommandTest {
    /// save the captures to disk
    #[argh(switch, long = "save-captures")]
    save_captures: bool,
}

fn main() {
    // On Windows, this env var will be set by the packetry-cli wrapper.
    #[cfg(windows)]
    if std::env::var("PACKETRY_ATTACH_CONSOLE").is_ok() {
        use winapi::um::wincon::{AttachConsole, ATTACH_PARENT_PROCESS};
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    let args: Args = argh::from_env();

    if let Some(subcmd) = args.sub_commands {
        match subcmd {
            SubcommandEnum::One(captureoptions) => {
                if let Err(e) = headless_capture(captureoptions) {
                    eprintln!("Error capturing: {}", e);
                }
            }

            SubcommandEnum::Two(_) => {
                if let Err(e) = list_devices() {
                    eprintln!("Error listing devices: {}", e);
                }
            }

            SubcommandEnum::Three(versionoptions) => {
                println!(
                    "Packetry version {}\n\n{}",
                    version(),
                    version_info(versionoptions.print_dependencies)
                );
            }

            SubcommandEnum::Four(testoptions) => {
                test_cynthion::run_test(testoptions.save_captures);
            }
        }
    } else {
        // Start the GUI application as no subcommand was provided.
        // Here, args[1] is the filename if one was provided. This should be added to the argh enum.

        let application = gtk::Application::new(
            Some("com.greatscottgadgets.packetry"),
            ApplicationFlags::NON_UNIQUE | ApplicationFlags::HANDLES_OPEN,
        );
        application.set_option_context_parameter_string(Some("[filename.pcap]"));
        application.connect_activate(|app| display_error(activate(app)));
        application.connect_open(|app, files, _hint| {
            app.activate();
            if let Some(file) = files.first() {
                display_error(open(file));
            }
        });
        application.run();
        display_error(stop_cynthion());
    }
}
