//! CLI wrapper program.
//!
//! Necessary on Windows, where the GUI binary cannot have console output.

use std::process::{Command, ExitCode};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

#[cfg(windows)]
const MAIN_BINARY: &str = "packetry.exe";
#[cfg(not(windows))]
const MAIN_BINARY: &str = "packetry";

fn main() -> ExitCode {
    // Find the main Packetry executable.
    let packetry_binary_path = std::env::current_exe()
        .expect("Failed to find path to current executable")
        .parent()
        .expect("Failed to find parent directory of current executable")
        .join(MAIN_BINARY);

    // Prepare to call it, passing through all arguments we were passed.
    let mut command = Command::new(packetry_binary_path);
    command.args(std::env::args().skip(1));
    
    // If on Windows, tell the child that it needs to attach to our console.
    #[cfg(windows)]
    command.env("PACKETRY_ATTACH_CONSOLE", "1");

    // Spawn the main binary as a child process, and wait for its exit status.
    let exit_status = command 
        .status()
        .expect("Failed to start main packetry binary");
    
    // Try to exit with the same code the child did.
    match exit_status.code() {
        Some(code) => ExitCode::from(code as u8),
        None => {
            #[cfg(unix)]
            if let Some(signal) = exit_status.signal() {
                panic!("Packetry was terminated by signal {signal}");
            }
            panic!("Packetry was terminated without an exit code");
        }
    }
}
