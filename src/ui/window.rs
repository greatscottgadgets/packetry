//! GObject subclass for the application window.

use std::collections::BTreeMap;

use anyhow::Error;

use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    glib,
    gio::{
        ActionEntry,
        ActionMap,
        Menu,
        MenuItem,
        SimpleActionGroup,
    },
    Align,
    Application,
    ApplicationWindow,
    MenuButton,
    Orientation,
    Paned,
    Stack,
    StackSwitcher,
    Widget,
    Window,
    WrapMode,
};

use crate::capture::create_capture;
use crate::ui::{
    TRAFFIC_MODES,
    DeviceSelector,
    DeviceWarning,
    FileAction,
    StopState,
    UserInterface,
    detect_hardware,
    display_error,
    choose_capture_file,
    show_about,
    show_metadata,
    start_capture,
    stop_operation,
    with_ui,
};

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::{rc::Rc, cell::RefCell},
    crate::ui::Recording,
};

glib::wrapper! {
    /// The outer type exposed to our Rust code.
    pub struct PacketryWindow(ObjectSubclass<imp::PacketryWindow>)
    @extends ApplicationWindow, Window, Widget,
    @implements ActionMap;
}

impl Default for PacketryWindow {
    fn default() -> Self {
        glib::Object::new::<PacketryWindow>()
    }
}

#[derive(Default)]
pub struct Panes {
    initialised: bool,
    vertical: Paned,
    horizontal: Paned,
}

macro_rules! button_action {
    ($name:literal, $button:ident, $body:expr) => {
        ActionEntry::builder($name)
            .activate(|_: &PacketryWindow, _, _| {
                let mut enabled = false;
                display_error(with_ui(|ui| {
                    enabled = ui.$button.get_sensitive(); Ok(())
                }));
                if enabled {
                    display_error($body);
                }
            })
            .build()
    }
}

impl PacketryWindow {
    pub fn setup(application: &Application)
        -> Result<(PacketryWindow, UserInterface), Error>
    {
        use FileAction::*;

        let window = Self::default();

        window.set_application(Some(application));
        window.set_title(Some("Packetry"));

        window.add_action_entries([
            button_action!("open", open_button, choose_capture_file(Load)),
            button_action!("save", save_button, choose_capture_file(Save)),
            button_action!("scan", scan_button, detect_hardware()),
            button_action!("capture", capture_button, start_capture()),
            button_action!("stop", stop_button, stop_operation()),
        ]);

        #[cfg(not(target_os="macos"))]
        {
            application.set_accels_for_action("win.open", &["<Ctrl>o"]);
            application.set_accels_for_action("win.save", &["<Ctrl>s"]);
            application.set_accels_for_action("win.scan", &["<Ctrl>r", "F5"]);
            application.set_accels_for_action("win.capture", &["<Ctrl>b"]);
            application.set_accels_for_action("win.stop", &["<Ctrl>e"]);
        }

        #[cfg(target_os="macos")]
        {
            application.set_accels_for_action("win.open", &["<Meta>o"]);
            application.set_accels_for_action("win.save", &["<Meta>s"]);
            application.set_accels_for_action("win.scan", &["<Meta>r", "F5"]);
            application.set_accels_for_action("win.capture", &["<Meta>b"]);
            application.set_accels_for_action("win.stop", &["<Meta>e"]);
        }

        let action_bar = gtk::ActionBar::new();

        let open_button = gtk::Button::builder()
            .icon_name("document-open")
            .tooltip_text("Open")
            .action_name("win.open")
            .build();
        let save_button = gtk::Button::builder()
            .icon_name("document-save")
            .tooltip_text("Save")
            .action_name("win.save")
            .build();
        let scan_button = gtk::Button::builder()
            .icon_name("view-refresh")
            .tooltip_text("Scan for devices")
            .action_name("win.scan")
            .build();
        let capture_button = gtk::Button::builder()
            .icon_name("media-record")
            .tooltip_text("Capture")
            .action_name("win.capture")
            .build();
        let stop_button = gtk::Button::builder()
            .icon_name("media-playback-stop")
            .tooltip_text("Stop")
            .action_name("win.stop")
            .build();

        open_button.set_sensitive(true);
        save_button.set_sensitive(false);
        scan_button.set_sensitive(true);

        let selector = DeviceSelector::new()?;
        capture_button.set_sensitive(selector.device_available());

        let menu = Menu::new();
        let meta_item = MenuItem::new(Some("Metadata..."), Some("actions.metadata"));
        let about_item = MenuItem::new(Some("About..."), Some("actions.about"));
        menu.append_item(&meta_item);
        menu.append_item(&about_item);
        let menu_button = MenuButton::builder()
            .menu_model(&menu)
            .build();
        let action_group = SimpleActionGroup::new();
        let action_metadata = ActionEntry::builder("metadata")
            .activate(|_, _, _| display_error(show_metadata()))
            .build();
        let action_about = ActionEntry::builder("about")
            .activate(|_, _, _| display_error(show_about()))
            .build();
        action_group.add_action_entries([action_metadata, action_about]);
        window.insert_action_group("actions", Some(&action_group));
        let metadata_action = action_group.lookup_action("metadata").unwrap();
        metadata_action.set_property("enabled", false);

        action_bar.pack_start(&open_button);
        action_bar.pack_start(&save_button);
        action_bar.pack_start(&gtk::Separator::new(Orientation::Vertical));
        action_bar.pack_start(&scan_button);
        action_bar.pack_start(&capture_button);
        action_bar.pack_start(&stop_button);
        action_bar.pack_start(&selector.container);
        action_bar.pack_end(&menu_button);

        let warning = DeviceWarning::new();
        warning.update(selector.device_unusable());

        let mut traffic_windows = BTreeMap::new();

        let traffic_stack = Stack::builder()
            .vexpand(true)
            .build();

        for mode in TRAFFIC_MODES {
            let window = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .build();
            traffic_windows
                .insert(mode, window.clone());
            traffic_stack
                .add_child(&window)
                .set_title(mode.display_name());
        }

        let traffic_stack_switcher = StackSwitcher::builder()
            .stack(&traffic_stack)
            .build();

        let traffic_box = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .vexpand(true)
            .build();

        traffic_box.append(&traffic_stack_switcher);
        traffic_box.append(&traffic_stack);

        let device_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let detail_text = gtk::TextBuffer::new(None);
        let detail_view = gtk::TextView::builder()
            .buffer(&detail_text)
            .editable(false)
            .wrap_mode(WrapMode::Word)
            .vexpand(true)
            .left_margin(5)
            .build();

        let detail_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&detail_view)
            .build();

        let horizontal_panes = gtk::Paned::builder()
            .orientation(Orientation::Horizontal)
            .wide_handle(true)
            .start_child(&traffic_box)
            .end_child(&device_window)
            .vexpand(true)
            .build();

        let vertical_panes = gtk::Paned::builder()
            .orientation(Orientation::Vertical)
            .wide_handle(true)
            .start_child(&horizontal_panes)
            .end_child(&detail_window)
            .hexpand(true)
            .build();

        window.imp().panes.replace(
            Panes {
                initialised: false,
                vertical: vertical_panes.clone(),
                horizontal: horizontal_panes.clone(),
            }
        );

        let separator = gtk::Separator::new(Orientation::Horizontal);

        let progress_bar = gtk::ProgressBar::builder()
            .show_text(true)
            .text("")
            .hexpand(true)
            .build();

        let status_label = gtk::Label::builder()
            .label("Ready")
            .single_line_mode(true)
            .halign(Align::Start)
            .hexpand(true)
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(3)
            .margin_end(3)
            .build();

        let vbox = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .build();

        vbox.append(&action_bar);
        vbox.append(&gtk::Separator::new(Orientation::Horizontal));
        vbox.append(&warning.info_bar);
        vbox.append(&gtk::Separator::new(Orientation::Horizontal));
        vbox.append(&vertical_panes);
        vbox.append(&gtk::Separator::new(Orientation::Horizontal));
        vbox.append(&status_label);
        vbox.append(&gtk::Separator::new(Orientation::Horizontal));

        window.set_child(Some(&vbox));

        let (_, capture) = create_capture()?;

        let ui = UserInterface {
            #[cfg(any(test, feature="record-ui-test"))]
            recording: Rc::new(RefCell::new(
                Recording::new(capture.clone()))),
            capture,
            selector,
            file_name: None,
            stop_state: StopState::Disabled,
            traffic_windows,
            device_window,
            traffic_models: BTreeMap::new(),
            device_model: None,
            detail_text,
            endpoint_count: 2,
            show_progress: None,
            progress_bar,
            separator,
            vbox,
            vertical_panes,
            scan_button,
            open_button,
            save_button,
            capture_button,
            stop_button,
            status_label,
            warning,
            metadata_action,
        };

        Ok((window, ui))
    }
}

/// The internal implementation module.
mod imp {
    use std::cell::RefCell;
    use gtk::{
        self,
        subclass::prelude::*,
        glib,
        ApplicationWindow,
    };
    use super::Panes;

    /// The inner type to be used in the GObject type system.
    #[derive(Default)]
    pub struct PacketryWindow {
        pub panes: RefCell<Panes>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PacketryWindow {
        const NAME: &'static str = "PacketryWindow";
        type Type = super::PacketryWindow;
        type ParentType = ApplicationWindow;
    }

    impl ObjectImpl for PacketryWindow {}

    impl WidgetImpl for PacketryWindow {
        // Set the traffic window to 3/4 of initial height and width.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let mut panes = self.panes.borrow_mut();
            if !panes.initialised {
                panes.vertical.set_position(3 * height / 4);
                panes.horizontal.set_position(3 * width / 4);
                panes.initialised = true;
            }
        }
    }

    impl WindowImpl for PacketryWindow {}

    impl ApplicationWindowImpl for PacketryWindow {}
}
