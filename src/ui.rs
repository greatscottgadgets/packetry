use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

#[cfg(feature="step-decoder")]
use std::{io::Read, net::TcpListener};

#[cfg(feature="record-ui-test")]
use std::sync::Mutex;

use anyhow::{Context as ErrorContext, Error, bail};

use gtk::gio::{ActionEntry, ListModel, Menu, MenuItem, SimpleActionGroup};
use gtk::glib::{Object, SignalHandlerId};
use gtk::{
    prelude::*,
    AboutDialog,
    Align,
    Application,
    ApplicationWindow,
    Button,
    DropDown,
    InfoBar,
    Label,
    License,
    ListItem,
    ColumnView,
    ColumnViewColumn,
    MenuButton,
    MessageType,
    ProgressBar,
    ResponseType,
    ScrolledWindow,
    Separator,
    SignalListItemFactory,
    SingleSelection,
    StringList,
    Orientation,
};

#[cfg(not(test))]
use gtk::{
    MessageDialog,
    DialogFlags,
    ButtonsType,
};

use crate::backend::cynthion::{
    CynthionDevice,
    CynthionHandle,
    CynthionStop,
    CynthionUsability::*,
    Speed};

use crate::capture::{
    create_capture,
    CaptureReader,
    CaptureWriter,
    ItemSource,
    TrafficItem,
    DeviceItem,
    PacketId,
};
use crate::decoder::Decoder;
use crate::expander::ExpanderWrapper;
use crate::pcap::{Loader, Writer};
use crate::model::{GenericModel, TrafficModel, DeviceModel};
use crate::row_data::{
    GenericRowData,
    ToGenericRowData,
    TrafficRowData,
    DeviceRowData};
use crate::util::{fmt_count, fmt_size};
use crate::version::{version, version_info};

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::rc::Rc,
    crate::record_ui::Recording,
};

static TOTAL: AtomicU64 = AtomicU64::new(0);
static CURRENT: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static UPDATE_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(feature="record-ui-test")]
static UPDATE_LOCK: Mutex<()> = Mutex::new(());

thread_local!(
    static WINDOW: RefCell<Option<ApplicationWindow>> =
        const { RefCell::new(None) };
    static UI: RefCell<Option<UserInterface>> =
        const { RefCell::new(None) };
);

#[derive(Copy, Clone, PartialEq)]
enum FileAction {
    Load,
    Save,
}

struct DeviceSelector {
    devices: Vec<CynthionDevice>,
    dev_strings: Vec<String>,
    dev_speeds: Vec<Vec<&'static str>>,
    dev_dropdown: DropDown,
    speed_dropdown: DropDown,
    change_handler: Option<SignalHandlerId>,
    container: gtk::Box,
}

impl DeviceSelector {
    fn new() -> Result<Self, Error> {
        let selector = DeviceSelector {
            devices: vec![],
            dev_strings: vec![],
            dev_speeds: vec![],
            dev_dropdown: DropDown::from_strings(&[]),
            speed_dropdown: DropDown::from_strings(&[]),
            change_handler: None,
            container: gtk::Box::builder()
                .orientation(Orientation::Horizontal)
                .build()
        };
        let device_label = Label::builder()
            .label("Device: ")
            .margin_start(2)
            .margin_end(2)
            .build();
        let speed_label = Label::builder()
            .label(" Speed: ")
            .margin_start(2)
            .margin_end(2)
            .build();
        selector.container.append(&device_label);
        selector.container.append(&selector.dev_dropdown);
        selector.container.append(&speed_label);
        selector.container.append(&selector.speed_dropdown);
        Ok(selector)
    }

    fn current_device(&self) -> Option<&CynthionDevice> {
        if self.devices.is_empty() {
            None
        } else {
            Some(&self.devices[self.dev_dropdown.selected() as usize])
        }
    }

    fn device_available(&self) -> bool {
        match self.current_device() {
            None => false,
            Some(device) => match device.usability {
                Usable(..) => true,
                Unusable(..) => false,
            }
        }
    }

    fn device_unusable(&self) -> Option<&str> {
        match self.current_device() {
            None => None,
            Some(device) => match &device.usability {
                Usable(..) => None,
                Unusable(string) => Some(string),
            }
        }
    }

    fn set_sensitive(&mut self, sensitive: bool) {
        if sensitive {
            self.dev_dropdown.set_sensitive(!self.devices.is_empty());
            self.speed_dropdown.set_sensitive(self.device_available());
        } else {
            self.dev_dropdown.set_sensitive(false);
            self.speed_dropdown.set_sensitive(false);
        }
    }

    fn scan(&mut self) -> Result<(), Error> {
        if let Some(handler) = self.change_handler.take() {
            self.dev_dropdown.disconnect(handler);
        }
        self.devices = CynthionDevice::scan()?;
        let count = self.devices.len();
        self.dev_strings = Vec::with_capacity(count);
        self.dev_speeds = Vec::with_capacity(count);
        for device in self.devices.iter() {
            self.dev_strings.push(
                if count <= 1 {
                    String::from("Cynthion")
                } else {
                    let info = &device.device_info;
                    if let Some(serial) = info.serial_number() {
                        format!("Cynthion #{}", serial)
                    } else {
                        format!("Cynthion (bus {}, device {})",
                            info.bus_number(),
                            info.device_address())
                    }
                }
            );
            if let Usable(_, speeds) = &device.usability {
                self.dev_speeds.push(
                    speeds.iter().map(Speed::description).collect()
                )
            } else {
                self.dev_speeds.push(vec![]);
            }
        }
        let no_speeds = vec![];
        let speed_strings = self.dev_speeds.first().unwrap_or(&no_speeds);
        self.replace_dropdown(&self.dev_dropdown, &self.dev_strings);
        self.replace_dropdown(&self.speed_dropdown, speed_strings);
        self.dev_dropdown.set_sensitive(!self.devices.is_empty());
        self.speed_dropdown.set_sensitive(!speed_strings.is_empty());
        self.change_handler = Some(
            self.dev_dropdown.connect_selected_notify(
                |_| display_error(device_selection_changed())));
        Ok(())
    }

    fn update_speeds(&self) {
        let index = self.dev_dropdown.selected() as usize;
        let speed_strings = &self.dev_speeds[index];
        self.replace_dropdown(&self.speed_dropdown, speed_strings);
        self.speed_dropdown.set_sensitive(!speed_strings.is_empty());
    }

    fn open(&self) -> Result<(CynthionHandle, Speed), Error> {
        let device_id = self.dev_dropdown.selected();
        let device = &self.devices[device_id as usize];
        match &device.usability {
            Usable(_, speeds) => {
                let speed_id = self.speed_dropdown.selected() as usize;
                let speed = speeds[speed_id];
                let cynthion = device.open()?;
                Ok((cynthion, speed))
            },
            Unusable(reason) => {
                bail!("Device not usable: {}", reason)
            }
        }
    }

    fn replace_dropdown<T: AsRef<str>>(
        &self, dropdown: &DropDown, strings: &[T])
    {
        let strings = strings
            .iter()
            .map(T::as_ref)
            .collect::<Vec<_>>();
        if let Some(model) = dropdown.model() {
            let num_items = model.n_items();
            if let Ok(list) = model.downcast::<StringList>() {
                list.splice(0, num_items, strings.as_slice());
            }
        }
    }
}

struct DeviceWarning {
    info_bar: InfoBar,
    label: Label,
}

impl DeviceWarning {
    fn new() -> DeviceWarning {
        let info_bar = InfoBar::new();
        info_bar.set_show_close_button(true);
        info_bar.connect_response(|info_bar, response| {
            if response == ResponseType::Close {
                info_bar.set_revealed(false);
            }
        });
        let label = Label::new(None);
        label.set_wrap(true);
        info_bar.add_child(&label);
        DeviceWarning {
            info_bar,
            label,
        }
    }

    fn update(&self, warning: Option<&str>) {
        if let Some(reason) = warning {
            self.info_bar.set_message_type(MessageType::Warning);
            self.label.set_text(&format!(
                "This device is not usable because: {reason}"));
            self.info_bar.set_revealed(true);
        } else {
            self.info_bar.set_revealed(false);
        }
    }
}

pub struct UserInterface {
    pub capture: CaptureReader,
    selector: DeviceSelector,
    file_name: Option<String>,
    stop_handle: Option<CynthionStop>,
    traffic_window: ScrolledWindow,
    device_window: ScrolledWindow,
    pub traffic_model: Option<TrafficModel>,
    pub device_model: Option<DeviceModel>,
    endpoint_count: u16,
    show_progress: Option<FileAction>,
    progress_bar: ProgressBar,
    separator: Separator,
    vbox: gtk::Box,
    paned: gtk::Paned,
    open_button: Button,
    save_button: Button,
    scan_button: Button,
    capture_button: Button,
    stop_button: Button,
    status_label: Label,
    warning: DeviceWarning,
    #[cfg(any(test, feature="record-ui-test"))]
    pub recording: Rc<RefCell<Recording>>,
}

pub fn with_ui<F>(f: F) -> Result<(), Error>
    where F: FnOnce(&mut UserInterface) -> Result<(), Error>
{
    UI.with(|cell| {
        if let Some(ui) = cell.borrow_mut().as_mut() {
            f(ui)
        } else {
            bail!("UI not set up")
        }
    })
}

pub fn activate(application: &Application) -> Result<(), Error> {
    use FileAction::*;

    let window = gtk::ApplicationWindow::builder()
        .default_width(320)
        .default_height(480)
        .application(application)
        .title("Packetry")
        .build();

    let action_bar = gtk::ActionBar::new();

    let open_button = gtk::Button::builder()
        .icon_name("document-open")
        .tooltip_text("Open")
        .build();
    let save_button = gtk::Button::builder()
        .icon_name("document-save")
        .tooltip_text("Save")
        .build();
    let scan_button = gtk::Button::builder()
        .icon_name("view-refresh")
        .tooltip_text("Scan for devices")
        .build();
    let capture_button = gtk::Button::builder()
        .icon_name("media-record")
        .tooltip_text("Capture")
        .build();
    let stop_button = gtk::Button::builder()
        .icon_name("media-playback-stop")
        .tooltip_text("Stop")
        .build();

    open_button.set_sensitive(true);
    save_button.set_sensitive(false);
    scan_button.set_sensitive(true);

    let selector = DeviceSelector::new()?;
    capture_button.set_sensitive(selector.device_available());

    let menu = Menu::new();
    let about_item = MenuItem::new(Some("About..."), Some("actions.about"));
    menu.append_item(&about_item);
    let menu_button = MenuButton::builder()
        .menu_model(&menu)
        .build();
    let action_group = SimpleActionGroup::new();
    let action_about = ActionEntry::builder("about")
        .activate(|_, _, _| display_error(show_about()))
        .build();
    action_group.add_action_entries([action_about]);
    window.insert_action_group("actions", Some(&action_group));

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

    #[cfg(not(test))]
    window.show();
    WINDOW.with(|win_opt| win_opt.replace(Some(window.clone())));

    let args: Vec<_> = std::env::args().collect();
    let (_, capture) = create_capture()?;

    let traffic_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(480)
        .min_content_width(640)
        .build();

    let device_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(480)
        .min_content_width(240)
        .build();

    let paned = gtk::Paned::builder()
        .orientation(Orientation::Horizontal)
        .wide_handle(true)
        .start_child(&traffic_window)
        .end_child(&device_window)
        .vexpand(true)
        .build();

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
    vbox.append(&paned);
    vbox.append(&gtk::Separator::new(Orientation::Horizontal));
    vbox.append(&status_label);
    vbox.append(&gtk::Separator::new(Orientation::Horizontal));

    window.set_child(Some(&vbox));

    scan_button.connect_clicked(|_| display_error(detect_hardware()));
    capture_button.connect_clicked(|_| display_error(start_cynthion()));
    open_button.connect_clicked(|_| display_error(choose_file(Load)));
    save_button.connect_clicked(|_| display_error(choose_file(Save)));

    UI.with(|cell| {
        cell.borrow_mut().replace(
            UserInterface {
                #[cfg(any(test, feature="record-ui-test"))]
                recording: Rc::new(RefCell::new(
                    Recording::new(capture.clone()))),
                capture,
                selector,
                file_name: None,
                stop_handle: None,
                traffic_window,
                device_window,
                traffic_model: None,
                device_model: None,
                endpoint_count: 2,
                show_progress: None,
                progress_bar,
                separator,
                vbox,
                paned,
                scan_button,
                open_button,
                save_button,
                capture_button,
                stop_button,
                status_label,
                warning,
            }
        )
    });

    reset_capture()?;

    if args.len() > 1 {
        let filename = args[1].clone();
        let path = PathBuf::from(filename);
        start_pcap(Load, path)?;
    }

    gtk::glib::idle_add_once(|| display_error(detect_hardware()));

    Ok(())
}

fn create_view<Item, Model, RowData>(
        title: &str,
        capture: &CaptureReader,
        #[cfg(any(test, feature="record-ui-test"))]
        recording_args: (&Rc<RefCell<Recording>>, &'static str))
    -> (Model, ColumnView)
    where
        Item: Copy + 'static,
        Model: GenericModel<Item> + IsA<ListModel> + IsA<Object>,
        RowData: GenericRowData<Item> + IsA<Object>,
        CaptureReader: ItemSource<Item>,
        Object: ToGenericRowData<Item>
{
    #[cfg(any(test, feature="record-ui-test"))]
    let (name, expand_rec, update_rec, changed_rec) = {
        let (recording, name) = recording_args;
        (name, recording.clone(), recording.clone(), recording.clone())
    };
    let model = Model::new(
        capture.clone(),
        #[cfg(any(test, feature="record-ui-test"))]
        Rc::new(
            RefCell::new(
                move |position, summary|
                    update_rec
                        .borrow_mut()
                        .log_item_updated(name, position, summary)
            )
        )).expect("Failed to create model");
    let bind_model = model.clone();
    let selection_model = SingleSelection::new(Some(model.clone()));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let expander = ExpanderWrapper::new();
        list_item.set_child(Some(&expander));
    });
    let bind = move |list_item: &ListItem| -> Result<(), Error> {
        let row = list_item
            .item()
            .context("ListItem has no item")?
            .downcast::<RowData>()
            .or_else(|_| bail!("Item is not RowData"))?;

        let expander_wrapper = list_item
            .child()
            .context("ListItem has no child widget")?
            .downcast::<ExpanderWrapper>()
            .or_else(|_| bail!("Child widget is not an ExpanderWrapper"))?;

        let expander = expander_wrapper.expander();
        match row.node() {
            Ok(node_ref) => {
                let node = node_ref.borrow();
                let summary = bind_model.summary(&node.item);
                let connectors = bind_model.connectors(&node.item);
                expander_wrapper.set_text(summary);
                expander_wrapper.set_connectors(connectors);
                expander.set_visible(node.expandable());
                expander.set_expanded(node.expanded());
                let model = bind_model.clone();
                let node_ref = node_ref.clone();
                let list_item = list_item.clone();
                #[cfg(any(test,
                          feature="record-ui-test"))]
                let recording = expand_rec.clone();
                let handler = expander.connect_expanded_notify(move |expander| {
                    let position = list_item.position();
                    let expanded = expander.is_expanded();
                    #[cfg(any(test,
                              feature="record-ui-test"))]
                    recording.borrow_mut().log_item_expanded(
                        name, position, expanded);
                    display_error(
                        model.set_expanded(&node_ref, position, expanded))
                });
                expander_wrapper.set_handler(handler);
                node.attach_widget(&expander_wrapper);
            },
            Err(msg) => {
                expander_wrapper.set_connectors("".to_string());
                expander_wrapper.set_text(format!("Error: {msg}"));
                expander.set_visible(false);
            }
        };
        Ok(())
    };
    let unbind = move |list_item: &ListItem| {
        let row = list_item
            .item()
            .context("ListItem has no item")?
            .downcast::<RowData>()
            .or_else(|_| bail!("Item is not RowData"))?;

        let expander_wrapper = list_item
            .child()
            .context("ListItem has no child widget")?
            .downcast::<ExpanderWrapper>()
            .or_else(|_| bail!("Child widget is not an ExpanderWrapper"))?;

        if let Ok(node_ref) = row.node() {
            node_ref.borrow().remove_widget(&expander_wrapper);
        }

        let expander = expander_wrapper.expander();
        if let Some(handler) = expander_wrapper.take_handler() {
            expander.disconnect(handler);
        }

        Ok(())
    };
    factory.connect_bind(move |_, item| display_error(bind(item)));
    factory.connect_unbind(move |_, item| display_error(unbind(item)));

    let view = ColumnView::new(Some(selection_model));
    let column = ColumnViewColumn::new(Some(title), Some(factory));
    view.append_column(&column);
    view.add_css_class("data-table");

    if Model::HAS_TIMES {
        let model = model.clone();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, list_item| {
            let label = Label::new(None);
            list_item.set_child(Some(&label));
        });
        let bind = move |list_item: &ListItem| {
            let row = list_item
                .item()
                .context("ListItem has no item")?
                .downcast::<RowData>()
                .or_else(|_| bail!("Item is not RowData"))?;
            let label = list_item
                .child()
                .context("ListItem has no child widget")?
                .downcast::<Label>()
                .or_else(|_| bail!("Child widget is not a Label"))?;
            match row.node() {
                Ok(node_ref) => {
                    let node = node_ref.borrow();
                    let timestamp = model.timestamp(&node.item);
                    label.set_markup(&format!("<tt><small>{}.{:09}</small></tt>",
                                           timestamp / 1_000_000_000,
                                           timestamp % 1_000_000_000));
                },
                Err(msg) => {
                    label.set_text(&format!("Error: {msg}"));
                }
            }
            Ok(())
        };

        factory.connect_bind(move |_, item| display_error(bind(item)));

        let timestamp_column =
            ColumnViewColumn::new(Some("Time"), Some(factory));
        view.insert_column(0, &timestamp_column);
    }

    #[cfg(any(test, feature="record-ui-test"))]
    model.connect_items_changed(move |model, position, removed, added|
        changed_rec.borrow_mut().log_items_changed(
            name, model, position, removed, added));

    (model, view)
}

pub fn reset_capture() -> Result<CaptureWriter, Error> {
    let (writer, reader) = create_capture()?;
    with_ui(|ui| {
        let (traffic_model, traffic_view) =
            create_view::<TrafficItem, TrafficModel, TrafficRowData>(
                "Traffic",
                &reader,
                #[cfg(any(test, feature="record-ui-test"))]
                (&ui.recording, "traffic")
            );
        let (device_model, device_view) =
            create_view::<DeviceItem, DeviceModel, DeviceRowData>(
                "Devices",
                &reader,
                #[cfg(any(test, feature="record-ui-test"))]
                (&ui.recording, "devices")
            );
        ui.capture = reader;
        ui.traffic_model = Some(traffic_model);
        ui.device_model = Some(device_model);
        ui.endpoint_count = 2;
        ui.traffic_window.set_child(Some(&traffic_view));
        ui.device_window.set_child(Some(&device_view));
        ui.stop_button.set_sensitive(false);
        Ok(())
    })?;
    Ok(writer)
}

pub fn update_view() -> Result<(), Error> {
    with_ui(|ui| {
        use FileAction::*;
        #[cfg(feature="record-ui-test")]
        let guard = {
            let guard = UPDATE_LOCK.lock();
            let packet_count = ui.capture.packet_index.len();
            ui.recording
                .borrow_mut()
                .log_update(packet_count);
            guard
        };
        let mut more_updates = false;
        if ui.show_progress == Some(Save) {
            more_updates = true;
        } else {
            let (devices, endpoints, transactions, packets) = {
                let cap = &ui.capture;
                let devices = cap.devices.len() - 1;
                let endpoints = cap.endpoints.len() - 2;
                let transactions = cap.transaction_index.len();
                let packets = cap.packet_index.len();
                (devices, endpoints, transactions, packets)
            };
            ui.status_label.set_text(&format!(
                "{}: {} devices, {} endpoints, {} transactions, {} packets",
                ui.file_name.as_deref().unwrap_or("Unsaved capture"),
                fmt_count(devices),
                fmt_count(endpoints),
                fmt_count(transactions),
                fmt_count(packets)
            ));
            if let Some(model) = &ui.traffic_model {
                let old_count = model.n_items();
                more_updates |= model.update()?;
                let new_count = model.n_items();
                // If any endpoints were added, we need to redraw the rows above
                // to add the additional columns of the connecting lines.
                if new_count > old_count {
                    let new_ep_count = ui.capture.endpoints.len() as u16;
                    if new_ep_count > ui.endpoint_count {
                        model.items_changed(0, old_count, old_count);
                        ui.endpoint_count = new_ep_count;
                    }
                }
            }
            if let Some(model) = &ui.device_model {
                more_updates |= model.update()?;
            }
        }
        if let Some(action) = ui.show_progress {
            let total = TOTAL.load(Ordering::Relaxed);
            let current = CURRENT.load(Ordering::Relaxed);
            let fraction = (current as f64) / (total as f64);
            let text = match action {
                Load => format!("Loaded {} / {}",
                                fmt_size(current), fmt_size(total)),
                Save => format!("Saved {} / {} packets",
                                fmt_count(current), fmt_count(total)),
            };
            ui.progress_bar.set_text(Some(&text));
            ui.progress_bar.set_fraction(fraction);
        }
        if more_updates {
            gtk::glib::timeout_add_once(
                UPDATE_INTERVAL,
                || display_error(update_view())
            );
        }
        #[cfg(feature="record-ui-test")]
        drop(guard);
        Ok(())
    })
}

fn choose_file(action: FileAction) -> Result<(), Error> {
    use FileAction::*;
    let chooser = WINDOW.with(|cell| {
        let borrow = cell.borrow();
        let window = borrow.as_ref();
        match action {
            Load => gtk::FileChooserDialog::new(
                Some("Open pcap file"),
                window,
                gtk::FileChooserAction::Open,
                &[("Open", gtk::ResponseType::Accept)]
            ),
            Save => gtk::FileChooserDialog::new(
                Some("Save pcap file"),
                window,
                gtk::FileChooserAction::Save,
                &[("Save", gtk::ResponseType::Accept)]
            ),
        }
    });
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let Some(file) = dialog.file() {
                if let Some(path) = file.path() {
                    display_error(start_pcap(action, path));
                }
            }
            dialog.destroy();
        }
    });
    chooser.show();
    Ok(())
}

fn start_pcap(action: FileAction, path: PathBuf) -> Result<(), Error> {
    use FileAction::*;
    let writer = if action == Load {
        Some(reset_capture()?)
    } else {
        None
    };
    with_ui(|ui| {
        #[cfg(feature="record-ui-test")]
        ui.recording.borrow_mut().log_open_file(&path, &ui.capture);
        ui.file_name = path
            .file_name()
            .map(|path| path.to_string_lossy().to_string());
        ui.open_button.set_sensitive(false);
        ui.save_button.set_sensitive(false);
        ui.scan_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        let signal_id = ui.stop_button.connect_clicked(|_|
            display_error(stop_pcap()));
        ui.vbox.insert_child_after(&ui.separator, Some(&ui.paned));
        ui.vbox.insert_child_after(&ui.progress_bar, Some(&ui.separator));
        ui.show_progress = Some(action);
        let mut capture = ui.capture.clone();
        let worker = move || match action {
            Load => {
                let mut loader = Loader::open(path)?;
                TOTAL.store(loader.file_size, Ordering::Relaxed);
                let mut decoder = Decoder::new(writer.unwrap())?;
                #[cfg(feature="step-decoder")]
                let (mut client, _addr) =
                    TcpListener::bind("127.0.0.1:46563")?.accept()?;
                while let Some(result) = loader.next() {
                    let (packet, timestamp_ns) = result?;
                    #[cfg(feature="step-decoder")] {
                        let mut buf = [0; 1];
                        client.read(&mut buf).unwrap();
                    };
                    #[cfg(feature="record-ui-test")]
                    let guard = UPDATE_LOCK.lock();
                    decoder.handle_raw_packet(&packet.data, timestamp_ns)?;
                    #[cfg(feature="record-ui-test")]
                    drop(guard);
                    CURRENT.store(loader.bytes_read, Ordering::Relaxed);
                    if STOP.load(Ordering::Relaxed) {
                        break;
                    }
                }
                let writer = decoder.finish()?;
                writer.print_storage_summary();
                Ok(())
            },
            Save => {
                let packet_count = capture.packet_index.len();
                TOTAL.store(packet_count, Ordering::Relaxed);
                CURRENT.store(0, Ordering::Relaxed);
                let mut writer = Writer::open(path)?;
                for i in 0..packet_count {
                    let packet_id = PacketId::from(i);
                    let packet = capture.packet(packet_id)?;
                    let timestamp_ns = capture.packet_time(packet_id)?;
                    writer.add_packet(&packet, timestamp_ns)?;
                    CURRENT.store(i + 1, Ordering::Relaxed);
                    if STOP.load(Ordering::Relaxed) {
                        break;
                    }
                }
                writer.close()?;
                Ok(())
            },
        };
        std::thread::spawn(move || {
            display_error(worker());
            gtk::glib::idle_add_once(|| {
                STOP.store(false, Ordering::Relaxed);
                display_error(
                    with_ui(|ui| {
                        ui.show_progress = None;
                        ui.vbox.remove(&ui.separator);
                        ui.vbox.remove(&ui.progress_bar);
                        ui.stop_button.disconnect(signal_id);
                        ui.stop_button.set_sensitive(false);
                        ui.open_button.set_sensitive(true);
                        ui.save_button.set_sensitive(true);
                        ui.scan_button.set_sensitive(true);
                        ui.selector.set_sensitive(true);
                        ui.capture_button.set_sensitive(ui.selector.device_available());
                        Ok(())
                    })
                );
            });
        });
        gtk::glib::timeout_add_once(
            UPDATE_INTERVAL,
            || display_error(update_view()));
        Ok(())
    })
}

pub fn stop_pcap() -> Result<(), Error> {
    STOP.store(true, Ordering::Relaxed);
    with_ui(|ui| {
        ui.scan_button.set_sensitive(true);
        ui.stop_button.set_sensitive(false);
        Ok(())
    })
}

fn detect_hardware() -> Result<(), Error> {
    with_ui(|ui| {
        ui.selector.scan()?;
        ui.capture_button.set_sensitive(ui.selector.device_available());
        ui.warning.update(ui.selector.device_unusable());
        Ok(())
    })
}

fn device_selection_changed() -> Result<(), Error> {
    with_ui(|ui| {
        ui.capture_button.set_sensitive(ui.selector.device_available());
        ui.warning.update(ui.selector.device_unusable());
        ui.selector.update_speeds();
        Ok(())
    })
}

pub fn start_cynthion() -> Result<(), Error> {
    let writer = reset_capture()?;
    with_ui(|ui| {
        let (cynthion, speed) = ui.selector.open()?;
        let (stream_handle, stop_handle) =
            cynthion.start(speed, display_error)?;
        ui.stop_handle.replace(stop_handle);
        ui.open_button.set_sensitive(false);
        ui.scan_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        let signal_id = ui.stop_button.connect_clicked(|_|
            display_error(stop_cynthion()));
        let read_cynthion = move || {
            let mut decoder = Decoder::new(writer)?;
            for packet in stream_handle {
                decoder.handle_raw_packet(&packet.bytes, packet.timestamp_ns)?;
            }
            decoder.finish()?;
            Ok(())
        };
        std::thread::spawn(move || {
            display_error(read_cynthion());
            gtk::glib::idle_add_once(|| {
                display_error(
                    with_ui(|ui| {
                        ui.stop_button.disconnect(signal_id);
                        ui.stop_button.set_sensitive(false);
                        ui.open_button.set_sensitive(true);
                        ui.selector.set_sensitive(true);
                        ui.capture_button.set_sensitive(ui.selector.device_available());
                        Ok(())
                    })
                );
            });
        });
        gtk::glib::timeout_add_once(
            UPDATE_INTERVAL,
            || display_error(update_view()));
        Ok(())
    })
}

pub fn stop_cynthion() -> Result<(), Error> {
    with_ui(|ui| {
        if let Some(stop_handle) = ui.stop_handle.take() {
            stop_handle.stop()?;
        }
        ui.scan_button.set_sensitive(true);
        ui.save_button.set_sensitive(true);
        Ok(())
    })
}

fn show_about() -> Result<(), Error> {
    const LICENSE: &str = include_str!("../LICENSE");
    let about = AboutDialog::builder()
        .program_name("Packetry")
        .version(format!("Version: {}", version()))
        .comments("A fast, intuitive USB 2.0 protocol analysis application")
        .copyright("© 2022-2024 Great Scott Gadgets. All rights reserved.")
        .license_type(License::Bsd3)
        .license(LICENSE)
        .website("https://github.com/greatscottgadgets/packetry/")
        .website_label("https://github.com/greatscottgadgets/packetry/")
        .system_information(version_info(true))
        .build();
    about.present();
    Ok(())
}

pub fn display_error(result: Result<(), Error>) {
    #[cfg(not(test))]
    if let Err(e) = result {
        use std::fmt::Write;
        let mut message = format!("{e}");
        for cause in e.chain().skip(1) {
            write!(message, "\ncaused by: {cause} ({cause:?})").unwrap();
        }
        let backtrace = format!("{}", e.backtrace());
        if backtrace != "disabled backtrace" {
            write!(message, "\n\nBacktrace:\n{backtrace}").unwrap();
        }
        gtk::glib::idle_add_once(move || {
            WINDOW.with(|win_opt| {
                match win_opt.borrow().as_ref() {
                    None => println!("{message}"),
                    Some(window) => {
                        let dialog = MessageDialog::new(
                            Some(window),
                            DialogFlags::MODAL,
                            MessageType::Error,
                            ButtonsType::Close,
                            &message
                        );
                        dialog.set_transient_for(Some(window));
                        dialog.set_modal(true);
                        dialog.connect_response(
                            move |dialog, _| dialog.destroy());
                        dialog.show();
                    }
                }
            });
        });
    }
    #[cfg(test)]
    result.unwrap();
}
