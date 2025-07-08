//! GObject subclass for the application window.

use std::collections::BTreeMap;

use anyhow::Error;

use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    glib::{self, Object},
    gio::{
        ActionEntry,
        ActionMap,
        Menu,
        MenuItem,
        SimpleActionGroup,
    },
    Application,
    ApplicationWindow,
    Buildable,
    Orientation,
    Widget,
    Window,
};

use crate::capture::create_capture;
use crate::item::TrafficViewMode;
use crate::ui::{
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
    @implements ActionMap, Buildable;
}

impl Default for PacketryWindow {
    fn default() -> Self {
        glib::Object::new::<PacketryWindow>()
    }
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
    pub fn setup(application: &Application) -> Result<UserInterface, Error>
    {
        use FileAction::*;
        use TrafficViewMode::*;

        let window: PacketryWindow = Object::builder()
            .property("application", application)
            .build();

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

        let open_button = window.imp().open_button.clone();
        let save_button = window.imp().save_button.clone();
        let scan_button = window.imp().scan_button.clone();
        let capture_button = window.imp().capture_button.clone();
        let stop_button = window.imp().stop_button.clone();

        open_button.set_sensitive(true);
        save_button.set_sensitive(false);
        scan_button.set_sensitive(true);

        let selector = DeviceSelector::new(
            window.imp().dev_dropdown.clone(),
            window.imp().speed_dropdown.clone(),
        )?;

        capture_button.set_sensitive(selector.device_available());

        let menu = Menu::new();
        let meta_item = MenuItem::new(Some("Metadata..."), Some("actions.metadata"));
        let about_item = MenuItem::new(Some("About..."), Some("actions.about"));
        menu.append_item(&meta_item);
        menu.append_item(&about_item);
        let menu_button = window.imp().menu_button.clone();
        menu_button.set_menu_model(Some(&menu));
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

        let warning = DeviceWarning::new(
            window.imp().info_bar.clone(),
            window.imp().info_label.clone()
        );
        warning.update(selector.device_unusable());

        let mut traffic_windows = BTreeMap::new();
        traffic_windows.insert(Hierarchical, window.imp().hierarchical.clone());
        traffic_windows.insert(Transactions, window.imp().transactions.clone());
        traffic_windows.insert(Packets, window.imp().packets.clone());

        let device_window = window.imp().device_window.clone();
        let detail_text = window.imp().detail_text.clone();
        let vertical_panes = window.imp().vertical_panes.clone();

        let separator = gtk::Separator::new(Orientation::Horizontal);

        let progress_bar = gtk::ProgressBar::builder()
            .show_text(true)
            .text("")
            .hexpand(true)
            .build();

        let status_label = window.imp().status_label.clone();
        let vbox = window.imp().vbox.clone();

        let (_, capture) = create_capture()?;

        let ui = UserInterface {
            window,
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

        Ok(ui)
    }
}

/// The internal implementation module.
mod imp {
    use std::cell::Cell;
    use gtk::{
        self,
        subclass::prelude::*,
        glib::{self, subclass::InitializingObject},
        ApplicationWindow,
        Button,
        CompositeTemplate,
        DropDown,
        InfoBar,
        Label,
        MenuButton,
        Paned,
        ScrolledWindow,
        TextBuffer,
    };

    /// The inner type to be used in the GObject type system.
    #[derive(Default, CompositeTemplate)]
    #[template(file="packetry.ui")]
    pub struct PacketryWindow {
        panes_initialised: Cell<bool>,
        #[template_child]
        pub open_button: TemplateChild<Button>,
        #[template_child]
        pub save_button: TemplateChild<Button>,
        #[template_child]
        pub scan_button: TemplateChild<Button>,
        #[template_child]
        pub capture_button: TemplateChild<Button>,
        #[template_child]
        pub stop_button: TemplateChild<Button>,
        #[template_child]
        pub dev_dropdown: TemplateChild<DropDown>,
        #[template_child]
        pub speed_dropdown: TemplateChild<DropDown>,
        #[template_child]
        pub menu_button: TemplateChild<MenuButton>,
        #[template_child]
        pub info_bar: TemplateChild<InfoBar>,
        #[template_child]
        pub info_label: TemplateChild<Label>,
        #[template_child]
        pub hierarchical: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub transactions: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub packets: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub device_window: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub detail_text: TemplateChild<TextBuffer>,
        #[template_child]
        pub vertical_panes: TemplateChild<Paned>,
        #[template_child]
        pub horizontal_panes: TemplateChild<Paned>,
        #[template_child]
        pub status_label: TemplateChild<Label>,
        #[template_child]
        pub vbox: TemplateChild<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PacketryWindow {
        const NAME: &'static str = "PacketryWindow";
        type Type = super::PacketryWindow;
        type ParentType = ApplicationWindow;

        fn class_init(cls: &mut Self::Class) {
            cls.bind_template();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PacketryWindow {}

    impl WidgetImpl for PacketryWindow {
        // Set the traffic window to 3/4 of initial height and width.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if !self.panes_initialised.get() {
                self.vertical_panes.set_position(3 * height / 4);
                self.horizontal_panes.set_position(3 * width / 4);
                self.panes_initialised.set(true);
            }
        }
    }

    impl WindowImpl for PacketryWindow {}

    impl ApplicationWindowImpl for PacketryWindow {}
}
