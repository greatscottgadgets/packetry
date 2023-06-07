use gtk::prelude::*;
use gtk::gio::ApplicationFlags;

use packetry::ui::{
    activate,
    display_error,
    stop_luna
};

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry"),
        ApplicationFlags::NON_UNIQUE
    );
    application.connect_activate(|app| display_error(activate(app)));
    application.run_with_args::<&str>(&[]);
    display_error(stop_luna());
}
