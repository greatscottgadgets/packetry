pub mod capture;
pub mod decoder;
pub mod event;
pub mod file;
pub mod usb;
pub mod backend;
pub mod util;
pub mod version;

// Include build-time info.
mod built {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}
