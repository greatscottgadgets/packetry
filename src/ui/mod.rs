//! The Packetry user interface.

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::ops::Range;
use std::panic::UnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, Receiver, channel};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

#[cfg(feature="step-decoder")]
use std::net::TcpListener;

use anyhow::{Context as ErrorContext, Error, bail};
use chrono::{DateTime, Local};
use bytemuck::bytes_of;
use scoped_panic_hook::{Panic, catch_panic};

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
use gtk::glib::{self, Object, clone};
use gtk::{
    prelude::*,
    AboutDialog,
    Align,
    Application,
    Button,
    Dialog,
    DialogFlags,
    Label,
    License,
    ListItem,
    Grid,
    ColumnView,
    ColumnViewColumn,
    Paned,
    PopoverMenu,
    ProgressBar,
    ResponseType,
    ScrolledWindow,
    Separator,
    SignalListItemFactory,
    SingleSelection,
    TextBuffer,
    TextView,
};

#[cfg(not(test))]
use gtk::{
    MessageDialog,
    MessageType,
    ButtonsType,
};

use crate::backend::{
    BackendStop,
    TimestampedEvent,
};

use crate::capture::prelude::*;
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

pub mod capture;
pub mod device;
pub mod item_widget;
pub mod model;
pub mod power;
pub mod row_data;
pub mod settings;
pub mod tree_list_model;
pub mod window;
#[cfg(any(test, feature="record-ui-test"))]
pub mod record_ui;
#[cfg(test)]
mod test_replay;

use capture::{Capture, CaptureState};
use device::{DeviceSelector, DeviceWarning};
use item_widget::ItemWidget;
use model::{GenericModel, TrafficModel, DeviceModel};
use power::PowerControl;
use row_data::{
    GenericRowData,
    ToGenericRowData,
    TrafficRowData,
    DeviceRowData,
};
use settings::Settings;
use window::PacketryWindow;

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
static SNAPSHOT_REQ: AtomicBool = AtomicBool::new(false);

thread_local!(
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
    File(Cancellable, JoinHandle<Result<(), Panic>>),
    Backend(BackendStop, JoinHandle<Result<(), Panic>>),
}

pub struct UserInterface {
    window: PacketryWindow,
    settings: Settings,
    capture: Capture,
    snapshot_rx: Option<Receiver<CaptureSnapshot>>,
    selector: DeviceSelector,
    power: PowerControl,
    file_name: Option<String>,
    stop_state: StopState,
    traffic_windows: BTreeMap<TrafficViewMode, ScrolledWindow>,
    device_window: ScrolledWindow,
    pub traffic_models: BTreeMap<TrafficViewMode, TrafficModel>,
    pub device_model: Option<DeviceModel>,
    detail_text: TextBuffer,
    endpoint_count: u64,
    show_progress: Option<FileAction>,
    progress_bar: ProgressBar,
    separator: Separator,
    vbox: gtk::Box,
    vertical_panes: Paned,
    open_button: Button,
    save_button: Button,
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

pub fn activate(application: &Application) -> Result<(), Error> {

    let ui = PacketryWindow::setup(application)?;
    ui.power.connect_signals(|| {
        gtk::glib::idle_add_once(||
            display_error(power_changed())
        );
    });

    ui.selector.connect_signals(|| {
        glib::idle_add_once(||
            display_error(device_selection_changed())
        );
    });

    #[cfg(not(test))]
    ui.window.show();

    UI.with(|cell| {
        cell.borrow_mut().replace(ui);
    });

    reset_capture()?;

    Ok(())
}

pub fn open(file: &gio::File) -> Result<(), Error> {
    start_file(FileAction::Load, file.clone())
}

type ContextFn<Item> =
    fn(&mut CaptureReader, &Item) -> Result<Option<PopoverMenu>, Error>;

fn create_view<Item, Model, RowData, ViewMode>(
        title: &str,
        capture: &Capture,
        view_mode: ViewMode,
        context_menu_fn: ContextFn<Item>,
        #[cfg(any(test, feature="record-ui-test"))]
        recording_args: (&Rc<RefCell<Recording>>, &'static str))
    -> (Model, SingleSelection, ColumnView)
    where
        Item: Clone + 'static,
        ViewMode: Copy + 'static,
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
                let connectors = model.connectors(view_mode, &item);
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
                            display_error(with_ui(|ui| {
                                model.set_expanded(
                                    &mut ui.capture,
                                    &node_ref, position, expanded)
                            }))
                        }
                    )
                );
                item_widget.set_handler(handler);
                item_widget.set_context_menu_fn(move || {
                    let mut popover = None;
                    display_error(
                        with_ui(|ui| {
                            popover = context_menu_fn(
                                &mut ui.capture.reader, &item)?;
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
    let (mut writer, reader) = create_capture()?;
    let snapshot = writer.snapshot();
    with_ui(|ui| {
        let capture = Capture {
            reader,
            state: CaptureState::Ongoing(snapshot),
        };
        for mode in TRAFFIC_MODES {
            let (traffic_model, traffic_selection, traffic_view) =
                create_view::<
                    TrafficItem,
                    TrafficModel,
                    TrafficRowData,
                    TrafficViewMode
                >(
                    "Traffic",
                    &capture,
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
                &capture,
                (),
                device_context_menu,
                #[cfg(any(test, feature="record-ui-test"))]
                (&ui.recording, "devices")
            );
        ui.capture = capture;
        ui.device_model = Some(device_model);
        ui.endpoint_count = NUM_SPECIAL_ENDPOINTS;
        ui.device_window.set_child(Some(&device_view));
        ui.stop_button.set_sensitive(false);
        Ok(())
    })?;
    Ok(writer)
}

pub fn update_view() -> Result<(), Error> {
    use FileAction::*;
    with_ui(|ui| {
        SNAPSHOT_REQ.store(true, Ordering::Relaxed);
        if let Some(snapshot_rx) = &mut ui.snapshot_rx {
            if let Ok(snapshot) = snapshot_rx.try_recv() {
                ui.capture.set_snapshot(snapshot);
            }
        }
        let (devices, endpoints, transactions, packets) =
            match &mut ui.capture.state
        {
            CaptureState::Ongoing(snapshot) => {
                let cap = ui.capture.reader.at(snapshot);
                let devices = cap
                    .device_count()
                    .saturating_sub(NUM_SPECIAL_DEVICES);
                let endpoints = cap
                    .endpoint_count()
                    .saturating_sub(NUM_SPECIAL_ENDPOINTS);
                let transactions = cap.transaction_count();
                let packets = cap.packet_count();
                (devices, endpoints, transactions, packets)
            },
            CaptureState::Complete => {
                let cap = &mut ui.capture.reader;
                let devices = cap
                    .device_count()
                    .saturating_sub(NUM_SPECIAL_DEVICES);
                let endpoints = cap
                    .endpoint_count()
                    .saturating_sub(NUM_SPECIAL_ENDPOINTS);
                let transactions = cap.transaction_count();
                let packets = cap.packet_count();
                (devices, endpoints, transactions, packets)
            },
        };
        #[cfg(feature="record-ui-test")]
        ui.recording
            .borrow_mut()
            .log_update(packets);
        let mut more_updates = false;
        if ui.show_progress == Some(Save) {
            more_updates = true;
        } else {
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
                more_updates |= model.update(&mut ui.capture)?;
                let new_count = model.n_items();
                // If any endpoints were added, we need to redraw the rows above
                // to add the additional columns of the connecting lines.
                if new_count > old_count && endpoints > ui.endpoint_count {
                    model.items_changed(0, old_count, old_count);
                }
                ui.endpoint_count = endpoints;
            }
            if let Some(model) = &ui.device_model {
                more_updates |= model.update(&mut ui.capture)?;
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
        } else {
            ui.capture.set_completed();
            ui.snapshot_rx = None;
        }
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
    with_ui(|ui| {
        let chooser = match action {
            Load => gtk::FileChooserDialog::new(
                Some(&format!("Open {description}")),
                Some(&ui.window),
                gtk::FileChooserAction::Open,
                &[("Open", gtk::ResponseType::Accept)]
            ),
            Save => gtk::FileChooserDialog::new(
                Some(&format!("Save {description}")),
                Some(&ui.window),
                gtk::FileChooserAction::Save,
                &[("Save", gtk::ResponseType::Accept)]
            ),
        };
        let _ = chooser.set_current_folder(
            ui.settings.last_used_directory
                .as_ref()
                .map(gio::File::for_path)
                .as_ref()
        );
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
            let _ = with_ui(|ui| {
                ui.settings.last_used_directory = dialog
                    .current_folder()
                    .and_then(|file| file.path());
                Ok(())
            });
            if response == gtk::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(name) = file.basename() {
                        if action == Save && name.extension().is_none() {
                            // Automatically add the ".pcapng" extension.
                            let (file, _) = add_extension(file, name, "pcapng");
                            // Check whether the new filename already exists.
                            if file.query_exists(Cancellable::NONE) {
                                // The file already exists.
                                // Set the new filename in the dialog.
                                let _ = dialog.set_file(&file);
                                // Re-emit the response signal, so that the
                                // dialog will show its usual warning message
                                // about an existing file.
                                dialog.response(response);
                                // Return without closing the dialog.
                                return
                            } else {
                                // The file doesn't exist. Proceed normally, but
                                // with the amended destination file.
                                display_error(handler(file));
                                // Return after closing the dialog.
                                dialog.destroy();
                                return
                            }
                        }
                    }
                    display_error(handler(file));
                }
                dialog.destroy();
            }
        });
        chooser.show();
        Ok(())
    })
}

fn choose_capture_file(action: FileAction) -> Result<(), Error> {
    choose_file(action, "capture file", move |file| start_file(action, file))
}

fn start_file(action: FileAction, file: gio::File) -> Result<(), Error> {
    use FileAction::*;
    use FileFormat::*;
    let (format, file, name) = match file.basename() {
        None => bail!("Could not determine format without file name"),
        Some(name) => match name.extension().and_then(OsStr::to_str) {
            Some(ext) => match ext.to_lowercase().as_str() {
                "pcap" => (Pcap, file, name),
                "pcapng" => (PcapNg, file, name),
                ext => bail!(
                    "Could not determine format from extension '{ext}'")
            },
            None => bail!(
                "Could not determine format from file name '{}'",
                name.display()
            ),
        }
    };
    let writer = if action == Load {
        Some(reset_capture()?)
    } else {
        None
    };
    with_ui(|ui| {
        let cancel_handle = Cancellable::new();
        let (snapshot_tx, snapshot_rx) = channel();
        #[cfg(feature="record-ui-test")]
        ui.recording.borrow_mut().log_open_file(
            &file.path().context("Cannot record UI test for non-local path")?,
            &ui.capture.reader);
        ui.open_button.set_sensitive(false);
        ui.save_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        ui.vbox.insert_child_after(&ui.separator, Some(&ui.vertical_panes));
        ui.vbox.insert_child_after(&ui.progress_bar, Some(&ui.separator));
        ui.show_progress = Some(action);
        ui.file_name = Some(name.to_string_lossy().to_string());
        ui.snapshot_rx = Some(snapshot_rx);
        let capture = ui.capture.reader.clone();
        let packet_count = capture.packet_count();
        CURRENT.store(0, Ordering::Relaxed);
        TOTAL.store(match action {
            Load => 0,
            Save => packet_count,
        }, Ordering::Relaxed);
        let thread_description = match action {
            Load => "file loading",
            Save => "file saving",
        };
        let stop_handle = cancel_handle.clone();
        let thread = spawn_thread(thread_description, move || {
            let start_time = Instant::now();
            let result = match action {
                Load => load_file(
                    file, format, writer.unwrap(), cancel_handle, snapshot_tx),
                Save => save_file(
                    file, format, capture, cancel_handle),
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
        })?;
        ui.stop_state = StopState::File(stop_handle, thread);
        gtk::glib::timeout_add_once(
            UPDATE_INTERVAL,
            || display_error(update_view()));
        Ok(())
    })
}

fn load<Source, Loader>(
    writer: CaptureWriter,
    source: Source,
    snapshot_tx: Sender<CaptureSnapshot>,
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
                decoder.handle_raw_packet(
                    packet.bytes(), packet.timestamp_ns())?;
                CURRENT.store(packet.total_bytes_read(), Ordering::Relaxed);
                if SNAPSHOT_REQ.swap(false, Ordering::AcqRel) {
                    snapshot_tx.send(decoder.capture.snapshot())?;
                }
            },
            Event(event) => {
                decoder.handle_event(event.event_type, event.timestamp_ns)?;
                CURRENT.store(event.total_bytes_read, Ordering::Relaxed);
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
    let mut writer = decoder.finish()?;
    snapshot_tx.send(writer.snapshot())?;
    writer.print_storage_summary();
    Ok(())
}

fn load_file(file: gio::File,
             format: FileFormat,
             writer: CaptureWriter,
             cancel_handle: Cancellable,
             snapshot_tx: Sender<CaptureSnapshot>)
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
        Pcap => load::<_, PcapLoader<_>>(writer, source, snapshot_tx),
        PcapNg => load::<_, PcapNgLoader<_>>(writer, source, snapshot_tx),
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
    let packet_count = capture.packet_count();
    let meta = capture.shared.metadata.load_full();
    let mut saver = Saver::new(dest, meta)?;
    if packet_count > 0 {
        for (result, i) in capture
            .timestamped_packets_and_events()?
            .zip(0..packet_count)
        {
            use PacketOrEvent::*;
            match result? {
                (timestamp, Packet(packet)) =>
                    saver.add_packet(&packet, timestamp)?,
                (timestamp, Event(event_type)) =>
                    saver.add_event(event_type, timestamp)?,
            };
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

fn add_extension_if_missing(file: gio::File, extension: &str) -> gio::File {
    match file.basename() {
        Some(name) if name.extension().is_none() => {
            let (file, _name) = add_extension(file, name, extension);
            file
        },
        _ => file
    }
}

fn add_extension(
    file: gio::File,
    name: PathBuf,
    extension: &str
) -> (gio::File, PathBuf) {
    let name = name.with_extension(extension);
    let file = match file.parent() {
        Some(parent) => parent.child(&name),
        None => gio::File::for_path(&name),
    };
    (file, name)
}

pub fn stop_operation() -> Result<(), Error> {
    with_ui(|ui| {
        let thread = match std::mem::replace(
            &mut ui.stop_state, StopState::Disabled
        ) {
            StopState::Disabled => return Ok(()),
            StopState::File(cancel_handle, thread) => {
                STOP.store(true, Ordering::Relaxed);
                cancel_handle.cancel();
                thread
            },
            StopState::Backend(stop_handle, thread) => {
                stop_handle.stop()?;
                ui.power.stopped();
                thread
            }
        };
        display_problem(thread.join().unwrap());
        Ok(())
    })
}

pub fn rearm() -> Result<(), Error> {
    with_ui(|ui| {
        STOP.store(false, Ordering::Relaxed);
        ui.stop_state = StopState::Disabled;
        ui.stop_button.set_sensitive(false);
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

fn device_selection_changed() -> Result<(), Error> {
    with_ui(|ui| {
        ui.selector.open_device()?;
        ui.capture_button.set_sensitive(ui.selector.device_available());
        ui.warning.update(ui.selector.device_unusable());
        ui.power.update_controls(ui.selector.handle());
        Ok(())
    })
}

fn power_changed() -> Result<(), Error> {
    with_ui(|ui| {
        ui.power.update_device(ui.selector.handle())?;
        Ok(())
    })
}

pub fn start_capture() -> Result<(), Error> {
    let writer = reset_capture()?;

    with_ui(|ui| {
        let (device, speed) = ui.selector.handle_and_speed()?;
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
        let (snapshot_tx, snapshot_rx) = channel();
        let (stream_handle, stop_handle) =
            device.start(speed, Box::new(display_error))?;
        ui.snapshot_rx = Some(snapshot_rx);
        ui.power.started();
        ui.open_button.set_sensitive(false);
        ui.selector.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        let read_packets = move || {
            let mut decoder = Decoder::new(writer)?;
            for result in stream_handle {
                let event = result
                    .context("Error processing raw capture data")?;
                use TimestampedEvent::*;
                match event {
                    Packet { timestamp_ns, bytes } =>
                        decoder.handle_raw_packet(&bytes, timestamp_ns)
                            .context("Error decoding packet")?,
                    Event { timestamp_ns, event_type } =>
                        decoder.handle_event(event_type, timestamp_ns)
                            .context("Error handling event")?,
                }
                if SNAPSHOT_REQ.swap(false, Ordering::AcqRel) {
                    snapshot_tx.send(decoder.capture.snapshot())?;
                }
            }
            let mut writer = decoder.finish()?;
            snapshot_tx.send(writer.snapshot())?;
            writer.shared.metadata.update(|meta| {
                meta.end_time = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok();
                meta.dropped = Some(0);
            });
            Ok(())
        };
        let thread = spawn_thread("decoder", move || {
            display_error(read_packets());
            gtk::glib::idle_add_once(|| display_error(rearm()));
        })?;
        ui.stop_state = StopState::Backend(stop_handle, thread);
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
        TransactionGroup(_, endpoint_id, ep_group_id) |
        TransactionGroupEnd(_, endpoint_id, ep_group_id) => {
            let group = capture.group(*endpoint_id, *ep_group_id)?;
            match group {
                Group {
                    endpoint_id,
                    content:
                        GroupContent::Data(data_range) |
                        GroupContent::Ambiguous(data_range, _),
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
                let payload = capture.bytes(&range)?;
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
        _ => None,
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
        let cap = &mut ui.capture.reader;
        let file = add_extension_if_missing(file, "bin");
        let mut dest = file
            .replace(None, false, FileCreateFlags::NONE, Cancellable::NONE)?
            .into_write();
        let mut length = 0;
        for data_id in data_range {
            let ep_traf = cap.endpoint_traffic(endpoint_id)?;
            let ep_transaction_id = ep_traf.data_transaction(data_id)?;
            let transaction_id = ep_traf.transaction_id(ep_transaction_id)?;
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
    let file = add_extension_if_missing(file, "bin");
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
        let meta = ui.capture.reader.shared.metadata.load();
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
        let dialog = Dialog::with_buttons(
            Some("Capture Metadata"),
            Some(&ui.window),
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
                    ui.capture.reader.shared.metadata.update(|meta| {
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
        Ok(())
    })
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

pub fn save_settings() {
    let _ = with_ui(|ui| {
        ui.settings.save();
        Ok(())
    });
}

fn spawn_thread<F>(
    description: &str, f: F
) -> Result<JoinHandle<Result<(), Panic>>, Error>
    where F: FnOnce() + Send + UnwindSafe + 'static
{
    std::thread::Builder::new()
        .name(description.to_string())
        .spawn(|| {
            let result = catch_panic(f);
            if result.is_err() {
                gtk::glib::idle_add_once(|| {
                    display_error(stop_operation());
                    display_error(rearm());
                });
            }
            result
        })
        .context(format!("Failed to start {description} thread"))
}

trait Problem {
    fn message(&self) -> String;
    #[allow(dead_code)]
    fn backtrace(&self) -> &Backtrace;
    fn is_ignorable(&self) -> bool;
}

impl Problem for Error {
    fn message(&self) -> String {
        use std::fmt::Write;
        let mut message = format!("{self}");
        for cause in self.chain().skip(1) {
            write!(message, "\ncaused by: {cause} ({cause:?})").unwrap();
        }
        message
    }

    fn backtrace(&self) -> &Backtrace {
        self.backtrace()
    }

    fn is_ignorable(&self) -> bool {
        if let Some(g_error) = self.downcast_ref::<gtk::glib::Error>() {
            // We cancelled a load/save operation. This isn't an error.
            g_error.matches(gio::IOErrorEnum::Cancelled)
        } else {
            false
        }
    }
}

impl Problem for Panic {
    fn message(&self) -> String {
        format!("Panic in worker thread:\n{}\nat {}",
            self.message(),
            self.location()
                .map(|location| format!("{}", location))
                .unwrap_or("<unknown location>".to_string())
        )
    }

    fn backtrace(&self) -> &Backtrace {
        self.backtrace()
    }

    fn is_ignorable(&self) -> bool {
        false
    }
}

fn display_problem<P: Problem>(result: Result<(), P>) {
    #[cfg(not(test))]
    if let Err(problem) = result {
        if problem.is_ignorable() {
            return;
        }
        let message = problem.message();
        let backtrace_string = format!("{}", problem.backtrace());
        let backtrace = match backtrace_string.as_str() {
            "disabled backtrace" => None,
            _ => Some(backtrace_string),
        };
        gtk::glib::idle_add_once(move || {
            UI.with(|ui_opt| {
                match ui_opt.borrow().as_ref() {
                    None => match backtrace {
                        Some(backtrace) =>
                            println!("{message}\n\nBacktrace:\n{backtrace}"),
                        None =>
                            println!("{message}")
                    },
                    Some(ui) => {
                        let dialog = MessageDialog::new(
                            Some(&ui.window),
                            DialogFlags::MODAL,
                            MessageType::Error,
                            ButtonsType::Close,
                            &message
                        );
                        if let Some(backtrace) = backtrace {
                            let message_area = dialog
                                .message_area()
                                .downcast::<gtk::Box>()
                                .unwrap();
                            message_area.append(
                                &Label::builder()
                                    .use_markup(true)
                                    .label("<b>Backtrace:</b>")
                                    .halign(Align::Start)
                                    .build()
                            );
                            message_area.append(
                                &ScrolledWindow::builder()
                                    .vexpand(true)
                                    .child(
                                        &TextView::builder()
                                            .buffer(
                                                &TextBuffer::builder()
                                                    .text(backtrace)
                                                    .build()
                                            )
                                            .build()
                                    )
                                    .build()
                            );
                        }
                        dialog.set_width_request(600);
                        dialog.set_height_request(400);
                        dialog.set_transient_for(Some(&ui.window));
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
    if let Err(problem) = result {
        if problem.is_ignorable() {
            return;
        }
        panic!("{}", problem.message());
    }
}

pub fn display_error(result: Result<(), Error>) {
    display_problem(result)
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
