// On Windows, produce a GUI app rather than a console one.
#![windows_subsystem = "windows"]

// We need the bitfield macro.
#[macro_use]
extern crate bitfield;

// We need the ctor macro for the replay test on macOS.
#[cfg(all(test, target_os="macos"))]
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
mod compact_index;
mod data_stream;
mod decoder;
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
#[cfg(any(test, feature="record-ui-test"))]
mod record_ui;
#[cfg(test)]
mod test_replay;

use gtk::prelude::*;
use gtk::gio::ApplicationFlags;

use ui::{
    activate,
    display_error,
    stop_cynthion
};
use version::{version, version_info};

fn have_argument(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
}

fn main() {
    if have_argument("--version") {
        println!("Packetry version {}\n\n{}",
                 version(),
                 version_info(have_argument("--dependencies")));
    } else if have_argument("--test-cynthion") {
        let save_captures = have_argument("--save-captures");
        test_cynthion::run_test(save_captures);
    } else {
        let application = gtk::Application::new(
            Some("com.greatscottgadgets.packetry"),
            ApplicationFlags::NON_UNIQUE
        );
        application.connect_activate(|app| display_error(activate(app)));
        application.run_with_args::<&str>(&[]);
        display_error(stop_cynthion());
    }
}
