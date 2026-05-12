//! The main Packetry binary.

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
mod database;
mod decoder;
mod event;
mod file;
mod item;
mod testing;
mod ui;
mod usb;
mod util;
mod version;

// The GTK version we require at runtime.
const GTK_REQUIRED: (u32, u32, u32) = (4,12,0);

use std::ops::ControlFlow;

use gtk::{prelude::*, Application, ApplicationWindow, Label, Button, Orientation};
use gtk::gio::ApplicationFlags;
use gtk::glib::{self, clone, OptionArg, OptionFlags, ExitCode};

use testing::test_cynthion;
use ui::{
    activate,
    display_error,
    open,
    save_settings,
    stop_operation
};
use version::{version, version_info};

fn main() {
    // Set RUST_BACKTRACE to 1 so we always get backtraces. Do this before
    // spawning any further threads, since set_var is unsafe once there are
    // multiple threads in the program.
    unsafe {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    // On Windows, this env var will be set by the packetry-cli wrapper.
    #[cfg(windows)]
    if std::env::var("PACKETRY_ATTACH_CONSOLE").is_ok() {
        use winapi::um::wincon::{AttachConsole, ATTACH_PARENT_PROCESS};
        unsafe {AttachConsole(ATTACH_PARENT_PROCESS)};
    }

    // Try to inititalize GTK.
    if gtk::init().is_err() {
        eprintln!("Failed to initialize GTK");
        std::process::exit(1);
    }

    // Check whether a dark theme is preferred, and adjust GTK settings.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(
            matches!(
                dark_light::detect(),
                dark_light::Mode::Dark
            )
        );
    }

    // Create our GTK application.
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry"),
        ApplicationFlags::NON_UNIQUE |
        ApplicationFlags::HANDLES_OPEN
    );

    // Check that the GTK version present at runtime is sufficient.
    let (major_req, minor_req, micro_req) = GTK_REQUIRED;
    if gtk::check_version(major_req, minor_req, micro_req).is_some() {
        application.connect_activate(gtk_too_old);
        application.run();
        std::process::exit(1);
    }

    // Set up command line options.
    application.set_option_context_parameter_string(
        Some("[filename.pcap]"));
    application.add_main_option(
        "version", glib::Char::from(0),
        OptionFlags::NONE, OptionArg::None,
        "Print version information.", None);
    application.add_main_option(
        "dependencies", glib::Char::from(0),
        OptionFlags::NONE, OptionArg::None,
        "With --version, includes dependency versions.", None);
    application.add_main_option(
        "test-cynthion", glib::Char::from(0),
        OptionFlags::NONE, OptionArg::None,
        "Test an attached Cynthion USB analyzer.", None);
    application.add_main_option(
        "save-captures", glib::Char::from(0),
        OptionFlags::NONE, OptionArg::None,
        "With --test-cynthion, saves captures from test.", None);

    // Set up handling of command line options.
    application.connect_handle_local_options(|_app, options| {
        if options.contains("version") {
            // Print version information.
            let with_deps: bool = options.contains("dependencies");
            println!("Packetry version {}\n\n{}",
                     version(),
                     version_info(with_deps));
            ControlFlow::Break(ExitCode::SUCCESS)
        } else if options.contains("test-cynthion") {
            // Run Cynthion analyzer test.
            let save_captures: bool = options.contains("save-captures");
            test_cynthion(save_captures);
            ControlFlow::Break(ExitCode::SUCCESS)
        } else {
            // Continue with normal startup.
            ControlFlow::Continue(())
        }
    });

    // Connect the UI code that starts up the application.
    application.connect_activate(|app| display_error(activate(app)));

    // Connect the UI code for opening a file.
    application.connect_open(|app, files, _hint| {
        app.activate();
        if let Some(file) = files.first() {
            display_error(open(file));
        }
    });

    // Run the application.
    application.run();

    // The application has exited; stop any operation that was ongoing.
    display_error(stop_operation());

    // Save persistent settings.
    save_settings();
}

// Display a dialog box to indicate the GTK version present is too old.
fn gtk_too_old(app: &(impl IsA<Application> + ApplicationExt)) {
    let major_sys = gtk::major_version();
    let minor_sys = gtk::minor_version();
    let micro_sys = gtk::micro_version();
    let window = ApplicationWindow::builder()
        .title("Cannot launch Packetry")
        .application(app)
        .build();
    let (major_req, minor_req, micro_req) = GTK_REQUIRED;
    let required = format!("{major_req}.{minor_req}.{micro_req}");
    let available = format!("{major_sys}.{minor_sys}.{micro_sys}");
    let message = format!(
        "Cannot launch Packetry.\n\
         The GTK version in the current environment is {available}.\n\
         This version of Packetry requires GTK {required}.\n\
         Please ensure the environment is correct, or update GTK.");
    let label = Label::builder()
        .label(message)
        .hexpand(true)
        .vexpand(true)
        .build();
    let button = Button::builder()
        .label("OK")
        .build();
    button.connect_clicked(
        clone!(#[strong] app, move |_| app.quit()));
    let vbox = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .build();
    window.set_child(Some(&vbox));
    vbox.append(&label);
    vbox.append(&button);
    window.show();
}
