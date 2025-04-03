//! The Packetry user interface.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[cfg(feature="step-decoder")]
use std::net::TcpListener;

#[cfg(feature="record-ui-test")]
use std::sync::Mutex;

use anyhow::{Context as ErrorContext, Error, bail};
use chrono::{DateTime, Local};
use bytemuck::bytes_of;

use gtk::gio::{
    self,
    Action,
    ActionEntry,
    Cancellable,
    FileCreateFlags,
    FileQueryInfoFlags,
    ListModel,
    Menu,
    MenuItem,
    SimpleActionGroup
};
use gtk::glib::{Object, SignalHandlerId, clone};
use gtk::{
    prelude::*,
    AboutDialog,
    Align,
    Application,
    ApplicationWindow,
    Button,
    Dialog,
    DialogFlags,
    DropDown,
    InfoBar,
    Label,
    License,
    ListItem,
    Grid,
    ColumnView,
    ColumnViewColumn,
    MenuButton,
    MessageType,
    PopoverMenu,
    ProgressBar,
    ResponseType,
    ScrolledWindow,
    Separator,
    SignalListItemFactory,
    SingleSelection,
    Stack,
    StackSwitcher,
    StringList,
    TextBuffer,
    TextView,
    Orientation,
    WrapMode,
};

#[cfg(not(test))]
use gtk::{
    MessageDialog,
    ButtonsType,
};

use crate::backend::{
    BackendHandle,
    BackendStop,
    ProbeResult,
    scan
};

use crate::capture::{
    create_capture,
    CaptureReader,
    CaptureWriter,
    CaptureMetadata,
    EndpointId,
    EndpointDataEvent,
    Group,
    GroupContent,
};
use crate::item::{
    ItemSource,
    TrafficItem,
    TrafficViewMode::{self,*},
    DeviceItem,
    DeviceItemContent,
    DeviceViewMode,
};
use crate::decoder::Decoder;
use crate::file::{
    GenericPacket,
    GenericLoader,
    GenericSaver,
    LoaderItem,
    PcapLoader,
    PcapSaver,
    PcapNgLoader,
    PcapNgSaver,
};
use crate::usb::{Descriptor, PacketFields, Speed, validate_packet};
use crate::util::{rcu::SingleWriterRcu, fmt_count, fmt_size};
use crate::version::{version, version_info};

pub mod item_widget;
pub mod model;
pub mod row_data;
pub mod tree_list_model;
#[cfg(any(test, feature="record-ui-test"))]
pub mod record_ui;
#[cfg(test)]
mod test_replay;

use item_widget::ItemWidget;
use model::{GenericModel, TrafficModel, DeviceModel};
use row_data::{
    GenericRowData,
    ToGenericRowData,
    TrafficRowData,
    DeviceRowData,
};

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::rc::Rc,
    record_ui::Recording,
};

const TRAFFIC_MODES: [TrafficViewMode; 3] =
    [Hierarchical, Transactions, Packets];

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

#[derive(Copy, Clone, PartialEq)]
enum FileFormat {
    Pcap,
    PcapNg,
}

enum StopState {
    Disabled,
    File(Cancellable),
    Backend(BackendStop),
}

struct DeviceSelector {
    devices: Vec<ProbeResult>,
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

    fn current_device(&self) -> Option<&ProbeResult> {
        if self.devices.is_empty() {
            None
        } else {
            Some(&self.devices[self.dev_dropdown.selected() as usize])
        }
    }

    fn device_available(&self) -> bool {
        match self.current_device() {
            None => false,
            Some(probe) => probe.result.is_ok()
        }
    }

    fn device_unusable(&self) -> Option<&str> {
        match self.current_device() {
            Some(ProbeResult {result: Err(msg), ..}) => Some(msg),
            _ => None
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
        self.devices = scan()?;
        let count = self.devices.len();
        self.dev_strings = Vec::with_capacity(count);
        self.dev_speeds = Vec::with_capacity(count);
        for probe in self.devices.iter() {
            self.dev_strings.push(
                if count <= 1 {
                    probe.name.to_string()
                } else {
                    let info = &probe.info;
                    if let Some(serial) = info.serial_number() {
                        format!("{} #{}", probe.name, serial)
                    } else {
                        format!("{} (bus {}, device {})",
                            probe.name,
                            info.bus_number(),
                            info.device_address())
                    }
                }
            );
            if let Ok(device) = &probe.result {
                self.dev_speeds.push(
                    device
                        .supported_speeds()
                        .iter()
                        .map(Speed::description)
                        .collect()
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

    fn open(&self) -> Result<(Box<dyn BackendHandle>, Speed), Error> {
        let device_id = self.dev_dropdown.selected();
        let probe = &self.devices[device_id as usize];
        match &probe.result {
            Ok(device) => {
                let speeds = device.supported_speeds();
                let speed_id = self.speed_dropdown.selected() as usize;
                let speed = speeds[speed_id];
                let handle = device.open_as_generic()?;
                Ok((handle, speed))
            },
            Err(reason) => {
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
    stop_state: StopState,
    traffic_windows: BTreeMap<TrafficViewMode, ScrolledWindow>,
    device_window: ScrolledWindow,
    pub traffic_models: BTreeMap<TrafficViewMode, TrafficModel>,
    pub device_model: Option<DeviceModel>,
    detail_text: TextBuffer,
    endpoint_count: u16,
    show_progress: Option<FileAction>,
    progress_bar: ProgressBar,
    separator: Separator,
    vbox: gtk::Box,
    vertical_panes: gtk::Paned,
    open_button: Button,
    save_button: Button,
    scan_button: Button,
    capture_button: Button,
    stop_button: Button,
    status_label: Label,
    warning: DeviceWarning,
    metadata_action: Action,
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

macro_rules! button_action {
    ($name:literal, $button:ident, $body:expr) => {
        ActionEntry::builder($name)
            .activate(|_: &ApplicationWindow, _, _| {
                let mut enabled = false;
                display_error(with_ui(|ui| { enabled = ui.$button.get_sensitive(); Ok(()) }));
                if enabled {
                    display_error($body);
                }
            })
            .build()
    }
}

pub fn activate(application: &Application) -> Result<(), Error> {
    use FileAction::*;

    let window = gtk::ApplicationWindow::builder()
        .default_width(320)
        .default_height(480)
        .application(application)
        .title("Packetry")
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

    #[cfg(not(test))]
    window.show();
    WINDOW.with(|win_opt| win_opt.replace(Some(window.clone())));

    let (_, capture) = create_capture()?;

    let mut traffic_windows = BTreeMap::new();

    let traffic_stack = Stack::builder()
        .vexpand(true)
        .build();

    for mode in TRAFFIC_MODES {
        let window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(480)
            .min_content_width(640)
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
        .min_content_height(480)
        .min_content_width(240)
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
        .min_content_width(640)
        .min_content_height(120)
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

    UI.with(|cell| {
        cell.borrow_mut().replace(
            UserInterface {
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
            }
        )
    });

    reset_capture()?;

    gtk::glib::idle_add_once(|| display_error(detect_hardware()));

    Ok(())
}

pub fn open(file: &gio::File) -> Result<(), Error> {
    start_file(FileAction::Load, file.clone())
}

type ContextFn<Item> =
    fn(&mut CaptureReader, &Item) -> Result<Option<PopoverMenu>, Error>;

fn create_view<Item, Model, RowData, ViewMode>(
        title: &str,
        capture: &CaptureReader,
        view_mode: ViewMode,
        context_menu_fn: ContextFn<Item>,
        #[cfg(any(test, feature="record-ui-test"))]
        recording_args: (&Rc<RefCell<Recording>>, &'static str))
    -> (Model, SingleSelection, ColumnView)
    where
        Item: Clone + 'static,
        ViewMode: Copy,
        Model: GenericModel<Item, ViewMode> + IsA<ListModel> + IsA<Object>,
        RowData: GenericRowData<Item> + IsA<Object>,
        CaptureReader: ItemSource<Item, ViewMode>,
        Object: ToGenericRowData<Item>
{
    #[cfg(any(test, feature="record-ui-test"))]
    let (name, expand_rec, update_rec, changed_rec) = {
        let (recording, name) = recording_args;
        (name, recording.clone(), recording.clone(), recording.clone())
    };
    let model = Model::new(
        capture.clone(),
        view_mode,
        #[cfg(any(test, feature="record-ui-test"))]
        Rc::new(
            RefCell::new(
                move |position, summary|
                    update_rec
                        .borrow_mut()
                        .log_item_updated(name, position, summary)
            )
        )).expect("Failed to create model");
    let selection_model = SingleSelection::new(Some(model.clone()));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let widget = ItemWidget::new();
        list_item.set_child(Some(&widget));
    });
    let bind = clone!(@strong model => move |list_item: &ListItem| {
        let row = list_item
            .item()
            .context("ListItem has no item")?
            .downcast::<RowData>()
            .or_else(|_| bail!("Item is not RowData"))?;

        let item_widget = list_item
            .child()
            .context("ListItem has no child widget")?
            .downcast::<ItemWidget>()
            .or_else(|_| bail!("Child widget is not an ItemWidget"))?;

        let expander = item_widget.expander();
        match row.node() {
            Ok(node_ref) => {
                let node = node_ref.borrow();
                let item = node.item.clone();
                let summary = model.description(&item, false);
                let connectors = model.connectors(&item);
                item_widget.set_text(summary);
                item_widget.set_connectors(connectors);
                expander.set_visible(node.expandable());
                expander.set_expanded(node.expanded());
                #[cfg(any(test,
                          feature="record-ui-test"))]
                let recording = expand_rec.clone();
                let handler = expander.connect_expanded_notify(
                    clone!(@strong model, @strong node_ref, @strong list_item =>
                        move |expander| {
                            let position = list_item.position();
                            let expanded = expander.is_expanded();
                            #[cfg(any(test,
                                      feature="record-ui-test"))]
                            recording.borrow_mut().log_item_expanded(
                                name, position, expanded);
                            display_error(
                                model.set_expanded(&node_ref, position, expanded))
                        }
                    )
                );
                item_widget.set_handler(handler);
                item_widget.set_context_menu_fn(move || {
                    let mut popover = None;
                    display_error(
                        with_ui(|ui| {
                            popover = context_menu_fn(&mut ui.capture, &item)?;
                            Ok(())
                        }).context("Failed to generate context menu")
                    );
                    popover
                });
                node.attach_widget(&item_widget);
            },
            Err(msg) => {
                item_widget.set_connectors("".to_string());
                item_widget.set_text(format!("Error: {msg}"));
                expander.set_visible(false);
            }
        };
        Ok(())
    });
    let unbind = move |list_item: &ListItem| {
        let row = list_item
            .item()
            .context("ListItem has no item")?
            .downcast::<RowData>()
            .or_else(|_| bail!("Item is not RowData"))?;

        let item_widget = list_item
            .child()
            .context("ListItem has no child widget")?
            .downcast::<ItemWidget>()
            .or_else(|_| bail!("Child widget is not an ItemWidget"))?;

        if let Ok(node_ref) = row.node() {
            node_ref.borrow().remove_widget(&item_widget);
        }

        let expander = item_widget.expander();
        if let Some(handler) = item_widget.take_handler() {
            expander.disconnect(handler);
        }

        Ok(())
    };
    factory.connect_bind(move |_, item| display_error(bind(item)));
    factory.connect_unbind(move |_, item| display_error(unbind(item)));

    let view = ColumnView::new(Some(selection_model.clone()));
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

    (model, selection_model, view)
}

pub fn reset_capture() -> Result<CaptureWriter, Error> {
    let (writer, reader) = create_capture()?;
    with_ui(|ui| {
        for mode in TRAFFIC_MODES {
            let (traffic_model, traffic_selection, traffic_view) =
                create_view::<
                    TrafficItem,
                    TrafficModel,
                    TrafficRowData,
                    TrafficViewMode
                >(
                    "Traffic",
                    &reader,
                    mode,
                    traffic_context_menu,
                    #[cfg(any(test, feature="record-ui-test"))]
                    (&ui.recording, mode.log_name())
                );
            ui.traffic_windows[&mode].set_child(Some(&traffic_view));
            ui.traffic_models.insert(mode, traffic_model.clone());
            traffic_selection.connect_selection_changed(
                move |selection_model, _position, _n_items| {
                    display_error(with_ui(|ui| {
                        let text = match selection_model.selected_item() {
                            Some(item) => {
                                let row = item
                                    .downcast::<TrafficRowData>()
                                    .or_else(|_|
                                        bail!("Item is not TrafficRowData"))?;
                                match row.node() {
                                    Ok(node_ref) => {
                                        let node = node_ref.borrow();
                                        traffic_model.description(&node.item, true)
                                    },
                                    Err(msg) => msg
                                }
                            },
                            None => String::from("No item selected"),
                        };
                        ui.detail_text.set_text(&text);
                        Ok(())
                    }))
                }
            );
        }
        let (device_model, _device_selection, device_view) =
            create_view::<
                DeviceItem,
                DeviceModel,
                DeviceRowData,
                DeviceViewMode
            >(
                "Devices",
                &reader,
                (),
                device_context_menu,
                #[cfg(any(test, feature="record-ui-test"))]
                (&ui.recording, "devices")
            );
        ui.capture = reader;
        ui.device_model = Some(device_model);
        ui.endpoint_count = 2;
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
                let devices = cap.devices.len().saturating_sub(1);
                let endpoints = cap.endpoints.len().saturating_sub(2);
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
            for model in ui.traffic_models.values() {
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
            let fraction = if total == 0 {
                None
            } else {
                Some((current as f64) / (total as f64))
            };
            let text = match (action, total) {
                (Load, 0) => format!("Loaded {} bytes",
                    fmt_size(current)),
                (Save, 0) => format!("Saved {} packets",
                    fmt_count(current)),
                (Load, total) => format!("Loaded {} / {}",
                    fmt_size(current), fmt_size(total)),
                (Save, total) => format!("Saved {} / {} packets",
                    fmt_count(current), fmt_count(total)),
            };
            ui.progress_bar.set_text(Some(&text));
            match fraction {
                Some(fraction) => ui.progress_bar.set_fraction(fraction),
                None => ui.progress_bar.pulse()
            };
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

fn choose_file<F>(
    action: FileAction,
    description: &str,
    handler: F
) -> Result<(), Error>
    where F: Fn(gio::File) -> Result<(), Error> + 'static
{
    use FileAction::*;
    let chooser = WINDOW.with(|cell| {
        let borrow = cell.borrow();
        let window = borrow.as_ref();
        match action {
            Load => gtk::FileChooserDialog::new(
                Some(&format!("Open {description}")),
                window,
                gtk::FileChooserAction::Open,
                &[("Open", gtk::ResponseType::Accept)]
            ),
            Save => gtk::FileChooserDialog::new(
                Some(&format!("Save {description}")),
                window,
                gtk::FileChooserAction::Save,
                &[("Save", gtk::ResponseType::Accept)]
            ),
        }
    });
    let all = gtk::FileFilter::new();
    let pcap = gtk::FileFilter::new();
    let pcapng = gtk::FileFilter::new();
    all.add_suffix("pcap");
    all.add_suffix("pcapng");
    pcap.add_suffix("pcap");
    pcapng.add_suffix("pcapng");
    all.set_name(Some("All captures (*.pcap, *.pcapng)"));
    pcap.set_name(Some("pcap (*.pcap)"));
    pcapng.set_name(Some("pcap-NG (*.pcapng)"));
    chooser.add_filter(&all);
    chooser.add_filter(&pcap);
    chooser.add_filter(&pcapng);
    chooser.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let Some(file) = dialog.file() {
                display_error(handler(file));
            }
            dialog.destroy();
        }
    });
    chooser.show();
    Ok(())
}

fn choose_capture_file(action: FileAction) -> Result<(), Error> {
    choose_file(action, "capture file", move |file| start_file(action, file))
}

fn start_file(action: FileAction, file: gio::File) -> Result<(), Error> {
    use FileAction::*;
    use FileFormat::*;
    let basename = match file.basename() {
        None => bail!("Could not determine format without file name"),
        Some(name) => name,
    };
    let format = match basename.extension().and_then(OsStr::to_str) {
        Some("pcap") => Pcap,
        Some("pcapng") => PcapNg,
        _ => bail!("Could not determine format from file name '{}'",
                   basename.display())
    };
    let writer = if action == Load {
        Some(reset_capture()?)
    } else {
        None
    };
    with_ui(|ui| {
        let cancel_handle = Cancellable::new();
        #[cfg(feature="record-ui-test")]
        ui.recording.borrow_mut().log_open_file(
            &file.path().context("Cannot record UI test for non-local path")?,
            &ui.capture);
        ui.open_button.set_sensitive(false);
        ui.save_button.set_sensitive(false);
        ui.scan_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        ui.stop_state = StopState::File(cancel_handle.clone());
        ui.vbox.insert_child_after(&ui.separator, Some(&ui.vertical_panes));
        ui.vbox.insert_child_after(&ui.progress_bar, Some(&ui.separator));
        ui.show_progress = Some(action);
        ui.file_name = Some(basename.to_string_lossy().to_string());
        let capture = ui.capture.clone();
        let packet_count = capture.packet_index.len();
        CURRENT.store(0, Ordering::Relaxed);
        TOTAL.store(match action {
            Load => 0,
            Save => packet_count,
        }, Ordering::Relaxed);
        std::thread::spawn(move || {
            let start_time = Instant::now();
            let result = match action {
                Load => load_file(file, format, writer.unwrap(), cancel_handle),
                Save => save_file(file, format, capture, cancel_handle),
            };
            let duration = Instant::now().duration_since(start_time);
            if result.is_ok() {
                eprintln!("{} in {} ms",
                    match action {
                        Load => "Loaded",
                        Save => "Saved",
                    },
                    duration.as_millis()
                );
            }
            display_error(result);
            gtk::glib::idle_add_once(|| display_error(rearm()));
        });
        gtk::glib::timeout_add_once(
            UPDATE_INTERVAL,
            || display_error(update_view()));
        Ok(())
    })
}

fn load<Source, Loader>(
    writer: CaptureWriter,
    source: Source
) -> Result<(), Error>
where
    Source: Read,
    Loader: GenericLoader<Source> + 'static
{
    #[cfg(feature="step-decoder")]
    let (mut client, _addr) =
        TcpListener::bind("127.0.0.1:46563")?.accept()?;
    let mut decoder = Decoder::new(writer)?;
    let mut loader = Loader::new(source)?;
    loop {
        use LoaderItem::*;
        match loader.next() {
            Packet(packet) => {
                #[cfg(feature="step-decoder")] {
                    let mut buf = [0; 1];
                    client.read(&mut buf).unwrap();
                };
                #[cfg(feature="record-ui-test")]
                let guard = UPDATE_LOCK.lock();
                decoder.handle_raw_packet(
                    packet.bytes(), packet.timestamp_ns())?;
                #[cfg(feature="record-ui-test")]
                drop(guard);
                CURRENT.store(packet.total_bytes_read(), Ordering::Relaxed);
            },
            Metadata(meta) => decoder.handle_metadata(meta),
            LoadError(e) => return Err(e),
            Ignore => continue,
            End => break,
        }
        if STOP.load(Ordering::Relaxed) {
            break;
        }
    }
    let writer = decoder.finish()?;
    writer.print_storage_summary();
    Ok(())
}

fn load_file(file: gio::File,
             format: FileFormat,
             writer: CaptureWriter,
             cancel_handle: Cancellable)
    -> Result<(), Error>
{
    use FileFormat::*;
    let info = file.query_info("standard::*",
                               FileQueryInfoFlags::NONE,
                               Some(&cancel_handle))?;
    if info.has_attribute(gio::FILE_ATTRIBUTE_STANDARD_SIZE) {
        let file_size = info.size() as u64;
        TOTAL.store(file_size, Ordering::Relaxed);
    }
    let source = file.read(Some(&cancel_handle))?.into_read();
    match format {
        Pcap => load::<_, PcapLoader<_>>(writer, source),
        PcapNg => load::<_, PcapNgLoader<_>>(writer, source),
    }
}

fn save<Dest, Saver>(
    mut capture: CaptureReader,
    dest: Dest)
-> Result<(), Error>
where
    Saver: GenericSaver<Dest>,
    Dest: Write
{
    let packet_count = capture.packet_index.len();
    let meta = capture.shared.metadata.load_full();
    let mut saver = Saver::new(dest, meta)?;
    if packet_count > 0 {
        for (result, i) in capture.timestamped_packets()?.zip(0..packet_count) {
            let (timestamp_ns, packet) = result?;
            saver.add_packet(&packet, timestamp_ns)?;
            CURRENT.store(i + 1, Ordering::Relaxed);
            if STOP.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    saver.close()
}

fn save_file(file: gio::File,
             format: FileFormat,
             capture: CaptureReader,
             cancel_handle: Cancellable)
    -> Result<(), Error>
{
    use FileFormat::*;
    let dest = file
        .replace(None, false, FileCreateFlags::NONE, Some(&cancel_handle))?
        .into_write();
    match format {
        Pcap => save::<_, PcapSaver<_>>(capture, dest),
        PcapNg => save::<_, PcapNgSaver<_>>(capture, dest),
    }
}

pub fn stop_operation() -> Result<(), Error> {
    with_ui(|ui| {
        match std::mem::replace(&mut ui.stop_state, StopState::Disabled) {
            StopState::Disabled => {},
            StopState::File(cancel_handle) => {
                STOP.store(true, Ordering::Relaxed);
                cancel_handle.cancel();
            },
            StopState::Backend(stop_handle) => {
                stop_handle.stop()?;
            }
        };
        Ok(())
    })
}

pub fn rearm() -> Result<(), Error> {
    with_ui(|ui| {
        STOP.store(false, Ordering::Relaxed);
        ui.stop_state = StopState::Disabled;
        ui.stop_button.set_sensitive(false);
        ui.scan_button.set_sensitive(true);
        ui.save_button.set_sensitive(true);
        ui.open_button.set_sensitive(true);
        ui.selector.set_sensitive(true);
        ui.capture_button.set_sensitive(ui.selector.device_available());
        if ui.show_progress.take().is_some() {
            ui.vbox.remove(&ui.separator);
            ui.vbox.remove(&ui.progress_bar);
        }
        ui.metadata_action.set_property("enabled", true);
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

pub fn start_capture() -> Result<(), Error> {
    let writer = reset_capture()?;

    with_ui(|ui| {
        let (device, speed) = ui.selector.open()?;
        let meta = CaptureMetadata {
            application: Some(format!("Packetry {}", version())),
            os: Some(std::env::consts::OS.to_string()),
            hardware: Some(std::env::consts::ARCH.to_string()),
            iface_speed: Some(speed),
            start_time: Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)?
            ),
            .. device.metadata().clone()
        };
        writer.shared.metadata.swap(Arc::new(meta));
        let (stream_handle, stop_handle) =
            device.start(speed, Box::new(display_error))?;
        ui.open_button.set_sensitive(false);
        ui.scan_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        ui.stop_state = StopState::Backend(stop_handle);
        let read_packets = move || {
            let mut decoder = Decoder::new(writer)?;
            for result in stream_handle {
                let packet = result
                    .context("Error processing raw capture data")?;
                decoder.handle_raw_packet(&packet.bytes, packet.timestamp_ns)?;
            }
            let writer = decoder.finish()?;
            writer.shared.metadata.update(|meta| {
                meta.end_time = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok();
                meta.dropped = Some(0);
            });
            Ok(())
        };
        std::thread::spawn(move || {
            display_error(read_packets());
            gtk::glib::idle_add_once(|| display_error(rearm()));
        });
        gtk::glib::timeout_add_once(
            UPDATE_INTERVAL,
            || display_error(update_view()));
        Ok(())
    })
}

fn context_popover<F>(
    name: &str,
    description: &str,
    action_fn: F
) -> PopoverMenu
    where F: Fn() -> Result<(), Error> + 'static
{
    let context_menu = Menu::new();
    let menu_item = MenuItem::new(
        Some(description),
        Some(&format!("context.{name}")));
    context_menu.append_item(&menu_item);
    let popover = PopoverMenu::from_model(Some(&context_menu));
    let context_actions = SimpleActionGroup::new();
    let action = ActionEntry::builder(name)
        .activate(move |_,_,_| display_error(action_fn()))
        .build();
    context_actions.add_action_entries([action]);
    popover.insert_action_group("context", Some(&context_actions));
    popover
}

fn traffic_context_menu(
    capture: &mut CaptureReader,
    item: &TrafficItem,
) -> Result<Option<PopoverMenu>, Error> {
    use TrafficItem::*;
    Ok(match item {
        TransactionGroup(group_id) => {
            let group = capture.group(*group_id)?;
            match group {
                Group {
                    endpoint_id,
                    content:
                        GroupContent::Data(data_range) |
                        GroupContent::Ambiguous(data_range, _),
                    is_start: true,
                    ..
                } => Some(
                    context_popover(
                        "save-data-transfer-payload",
                        "Save data transfer payload to file...",
                        move || choose_data_transfer_payload_file(
                            endpoint_id, data_range.clone())
                    )
                ),
                Group {
                    content: GroupContent::Request(transfer),
                    is_start: true,
                    ..
                } => Some(
                    context_popover(
                        "save-control-transfer-payload",
                        "Save control transfer payload to file...",
                        move || choose_data_file(
                            "control transfer payload",
                            transfer.data.clone()
                        )
                    )
                ),
                _ => None,
            }
        },
        Transaction(_, transaction_id) => {
            let transaction = capture.transaction(*transaction_id)?;
            if let Some(range) = transaction.payload_byte_range {
                let payload = capture.packet_data.get_range(&range)?;
                Some(context_popover(
                    "save-transaction-payload",
                    "Save transaction payload to file...",
                    move || choose_data_file(
                        "transaction payload",
                        payload.clone()
                    )
                ))
            } else {
                None
            }
        },
        Packet(.., packet_id) => {
            let packet = capture.packet(*packet_id)?;
            let len = packet.len();
            if validate_packet(&packet).is_ok() {
                match PacketFields::from_packet(&packet) {
                    PacketFields::Data(_) if len > 3 => {
                        let payload = packet[1 .. len - 2].to_vec();
                        Some(context_popover(
                            "save-packet-payload",
                            "Save packet payload to file...",
                            move || choose_data_file(
                                "packet payload",
                                payload.clone()
                            )
                        ))
                    },
                    _ => None
                }
            } else {
                None
            }
        },
    })
}

fn device_context_menu(
    _capture: &mut CaptureReader,
    item: &DeviceItem,
) -> Result<Option<PopoverMenu>, Error> {
    use DeviceItemContent::*;
    use Descriptor::*;
    let descriptor_bytes = match &item.content {
        DeviceDescriptor(Some(desc)) => bytes_of(desc),
        ConfigurationDescriptor(desc) => bytes_of(desc),
        FunctionDescriptor(desc) => bytes_of(desc),
        InterfaceDescriptor(desc) => bytes_of(desc),
        EndpointDescriptor(desc) => bytes_of(desc),
        OtherDescriptor(Other(_, bytes), _) => bytes,
        OtherDescriptor(Truncated(_, bytes), _) => bytes,
        _ => return Ok(None)
    }.to_vec();
    Ok(Some(context_popover(
        "save-descriptor",
        "Save descriptor to file...",
        move || choose_data_file("descriptor", descriptor_bytes.clone()))
    ))
}

fn choose_data_transfer_payload_file(
    endpoint_id: EndpointId,
    data_range: Range<EndpointDataEvent>
) -> Result<(), Error> {
    use FileAction::Save;
    choose_file(Save, "data transfer payload file", move |file|
        save_data_transfer_payload(file, endpoint_id, data_range.clone()))
}

fn save_data_transfer_payload(
    file: gio::File,
    endpoint_id: EndpointId,
    data_range: Range<EndpointDataEvent>
) -> Result<(), Error> {
    with_ui(|ui| {
        let cap = &mut ui.capture;
        let mut dest = file
            .replace(None, false, FileCreateFlags::NONE, Cancellable::NONE)?
            .into_write();
        let mut length = 0;
        for data_id in data_range {
            let ep_traf = cap.endpoint_traffic(endpoint_id)?;
            let ep_transaction_id = ep_traf.data_transactions.get(data_id)?;
            let transaction_id = ep_traf.transaction_ids.get(ep_transaction_id)?;
            let transaction = cap.transaction(transaction_id)?;
            let transaction_bytes = cap.transaction_bytes(&transaction)?;
            dest.write_all(&transaction_bytes)?;
            length += transaction_bytes.len();
        }
        println!(
            "Saved {} of data transfer payload to {}",
            fmt_size(length as u64),
            file.basename()
                .map_or(
                    "<unnamed>".to_string(),
                    |path| path.to_string_lossy().to_string())
        );
        Ok(())
    })
}

fn choose_data_file(
    description: &'static str,
    data: Vec<u8>,
) -> Result<(), Error> {
    choose_file(FileAction::Save, &format!("{description} file"),
        move |file| save_data(file, description, data.clone()))
}

fn save_data(
    file: gio::File,
    description: &'static str,
    data: Vec<u8>,
) -> Result<(), Error> {
    let mut dest = file
        .replace(None, false, FileCreateFlags::NONE, Cancellable::NONE)?
        .into_write();
    dest.write_all(&data)?;
    println!(
        "Saved {} of {description} to {}",
        fmt_size(data.len() as u64),
        file.basename()
            .map_or(
                "<unnamed>".to_string(),
                |path| path.to_string_lossy().to_string())
    );
    Ok(())
}

fn show_metadata() -> Result<(), Error> {
    let grid = Grid::new();
    let comment_buffer = TextBuffer::new(None);
    with_ui(|ui| {
        let meta = ui.capture.shared.metadata.load();
        const NONE: &str = "(not specified)";
        let mut current_row = 0;
        let row = &mut current_row;
        let make_label = |text: &'_ str, vertical_margin| {
            Label::builder()
                .halign(Align::Start)
                .margin_top(vertical_margin)
                .margin_bottom(vertical_margin)
                .margin_start(10)
                .margin_end(10)
                .use_markup(true)
                .label(text)
                .build()
        };
        let add_heading = |row: &mut i32, heading| {
            let label = make_label(&format!("<b>{heading}</b>"), 5);
            grid.attach(&label, 0, *row, 2, 1);
            *row += 1;
        };
        let add_field = |row: &mut i32, name, text: &'_ str| {
            grid.attach(&make_label(name, 0), 0, *row, 1, 1);
            grid.attach(&make_label(text, 0), 1, *row, 1, 1);
            *row += 1;
        };
        add_heading(row, "Writer:");
        for (name, field) in [
            ("Application:", &meta.application),
            ("OS:", &meta.os),
            ("Hardware:", &meta.hardware),
        ] {
            let text = field.as_deref().unwrap_or(NONE);
            add_field(row, name, text);
        }
        add_heading(row, "Interface:");
        for (name, field) in [
            ("Description:", &meta.iface_desc),
            ("Hardware:", &meta.iface_hardware),
            ("OS:", &meta.iface_os),
        ] {
            let text = field.as_deref().unwrap_or(NONE);
            add_field(row, name, text);
        }
        add_field(row, "Speed:",
            meta.iface_speed
                .as_ref()
                .map(Speed::description)
                .unwrap_or(NONE)
        );
        add_field(row, "Max packet size:",
            &meta.iface_snaplen
                .as_ref()
                .map(|len| format!("{} bytes", len.get()))
                .unwrap_or(NONE.to_string())
        );
        add_heading(row, "Capture:");
        for (name, field) in [
            ("Start time", &meta.start_time),
            ("End time", &meta.end_time),
        ] {
            let text = field
                .map(|duration| {
                    let time = SystemTime::UNIX_EPOCH + duration;
                    format!("{}", DateTime::<Local>::from(time).format("%c"))
                })
                .unwrap_or(NONE.to_string());
            add_field(row, name, &text);
        }
        add_field(row, "Packets dropped:",
            &meta.dropped
                .as_ref()
                .map(|p| format!("{p}"))
                .unwrap_or(NONE.to_string())
        );
        add_heading(row, "Comment:");
        if let Some(text) = &meta.comment {
            comment_buffer.set_text(text);
        }
        let comment_view = TextView::builder()
            .buffer(&comment_buffer)
            .margin_start(10)
            .margin_end(10)
            .margin_bottom(5)
            .build();
        grid.attach(&comment_view, 0, current_row, 2, 1);
        Ok(())
    })?;
    WINDOW.with(|win| {
        let dialog = Dialog::with_buttons(
            Some("Capture Metadata"),
            win.borrow().as_ref(),
            DialogFlags::DESTROY_WITH_PARENT,
            &[
                ("Close", ResponseType::Close),
                ("Apply", ResponseType::Apply),
            ]
        );
        dialog.content_area().append(&grid);
        let buf = comment_buffer.clone();
        dialog.connect_response(move |dialog, response| {
            if response == ResponseType::Apply {
                display_error(with_ui(|ui| {
                    ui.capture.shared.metadata.update(|meta| {
                        let start = buf.iter_at_offset(0);
                        let end = buf.iter_at_offset(-1);
                        let text = buf.text(&start, &end, false);
                        meta.comment = if text.is_empty() {
                            None
                        } else {
                            Some(text.to_string())
                        }
                    });
                    Ok(())
                }));
            }
            dialog.destroy();
        });
        dialog.present();
    });
    Ok(())
}

fn show_about() -> Result<(), Error> {
    const LICENSE: &str = include_str!("../../LICENSE");
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
        if let Some(g_error) = e.downcast_ref::<gtk::glib::Error>() {
            if g_error.matches(gio::IOErrorEnum::Cancelled) {
                // We cancelled a load/save operation. This isn't an error.
                return;
            }
        }
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

impl Speed {
    /// How this speed setting should be displayed in the UI.
    pub fn description(&self) -> &'static str {
        use Speed::*;
        match self {
            Auto => "Auto",
            High => "High (480Mbps)",
            Full => "Full (12Mbps)",
            Low => "Low (1.5Mbps)",
        }
    }
}
