use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::Debug;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::Duration;

#[cfg(feature="step-decoder")]
use std::{io::Read, net::TcpListener};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    Application,
    ApplicationWindow,
    Button,
    DropDown,
    ListItem,
    ListView,
    ProgressBar,
    ScrolledWindow,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};

#[cfg(not(feature="test-ui-replay"))]
use gtk::{
    MessageDialog,
    DialogFlags,
    MessageType,
    ButtonsType,
};

use pcap_file::{
    PcapError,
    DataLink,
    pcap::{PcapReader, PcapWriter, PcapHeader, RawPcapPacket},
};

use thiserror::Error;

use crate::backend::luna::{LunaDevice, LunaStop, Speed};
use crate::capture::{
    Capture,
    CaptureError,
    ItemSource,
    TrafficItem,
    TrafficCursor,
    DeviceItem,
    PacketId,
    fmt_size,
    fmt_count,
};
use crate::decoder::Decoder;
use crate::expander::ExpanderWrapper;
use crate::model::{GenericModel, TrafficModel, DeviceModel};
use crate::row_data::{
    GenericRowData,
    ToGenericRowData,
    TrafficRowData,
    DeviceRowData};
use crate::tree_list_model::ModelError;

#[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
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
    static WINDOW: RefCell<Option<ApplicationWindow>> = RefCell::new(None);
    static UI: RefCell<Option<UserInterface>> = RefCell::new(None);
);

#[derive(Copy, Clone, PartialEq)]
enum FileAction {
    Load,
    Save,
}

#[derive(Error, Debug)]
pub enum PacketryError {
    #[error("capture data error: {0}")]
    Capture(#[from] CaptureError),
    #[error("tree model error: {0}")]
    Model(#[from] ModelError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pcap error: {0}")]
    Pcap(#[from] PcapError),
    #[error("LUNA error: {0}")]
    Luna(#[from] crate::backend::luna::Error),
    #[error("locking failed")]
    Lock,
    #[error("internal bug: {0}")]
    Bug(&'static str)
}

pub struct UserInterface {
    pub capture: Arc<Mutex<Capture>>,
    stop_handle: Option<LunaStop>,
    traffic_window: ScrolledWindow,
    device_window: ScrolledWindow,
    pub traffic_model: Option<TrafficModel>,
    pub device_model: Option<DeviceModel>,
    endpoint_count: u16,
    show_progress: Option<FileAction>,
    progress_bar: ProgressBar,
    vbox: gtk::Box,
    open_button: Button,
    save_button: Button,
    capture_button: Button,
    stop_button: Button,
    speed_dropdown: DropDown,
    #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
    pub recording: Rc<RefCell<Recording>>,
}

pub fn with_ui<F>(f: F) -> Result<(), PacketryError>
    where F: FnOnce(&mut UserInterface) -> Result<(), PacketryError>
{
    UI.with(|cell| {
        if let Some(ui) = cell.borrow_mut().as_mut() {
            f(ui)
        } else {
            Err(PacketryError::Bug("UI not set up"))
        }
    })
}

pub fn activate(application: &Application) -> Result<(), PacketryError> {
    use FileAction::*;

    let window = gtk::ApplicationWindow::builder()
        .default_width(320)
        .default_height(480)
        .application(application)
        .title("Packetry")
        .build();

    let header_bar = gtk::HeaderBar::new();

    let open_button = gtk::Button::from_icon_name("document-open");
    let save_button = gtk::Button::from_icon_name("document-save");
    let capture_button = gtk::Button::from_icon_name("media-record");
    let stop_button = gtk::Button::from_icon_name("media-playback-stop");
    let speed_dropdown = gtk::DropDown::from_strings(&[
        "High (480Mbps)",
        "Full (12Mbps)",
        "Low (1.5Mbps)"
    ]);

    open_button.set_sensitive(true);
    save_button.set_sensitive(false);
    capture_button.set_sensitive(true);

    header_bar.pack_start(&open_button);
    header_bar.pack_start(&save_button);
    header_bar.pack_start(&capture_button);
    header_bar.pack_start(&stop_button);
    header_bar.pack_start(&speed_dropdown);

    window.set_titlebar(Some(&header_bar));
    #[cfg(not(feature="test-ui-replay"))]
    window.show();
    WINDOW.with(|win_opt| win_opt.replace(Some(window.clone())));

    let args: Vec<_> = std::env::args().collect();
    let capture = Arc::new(Mutex::new(Capture::new()?));

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

    let vbox = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .build();

    vbox.append(&paned);
    vbox.append(&separator);

    window.set_child(Some(&vbox));

    capture_button.connect_clicked(|_| display_error(start_luna()));
    open_button.connect_clicked(|_| display_error(choose_file(Load)));
    save_button.connect_clicked(|_| display_error(choose_file(Save)));

    UI.with(|cell| {
        cell.borrow_mut().replace(
            UserInterface {
                #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
                recording: Rc::new(RefCell::new(
                    Recording::new(capture.clone()))),
                capture,
                stop_handle: None,
                traffic_window,
                device_window,
                traffic_model: None,
                device_model: None,
                endpoint_count: 2,
                show_progress: None,
                progress_bar,
                vbox,
                open_button,
                save_button,
                capture_button,
                stop_button,
                speed_dropdown,
            }
        )
    });

    reset_capture()?;

    if args.len() > 1 {
        let filename = args[1].clone();
        let path = PathBuf::from(filename);
        start_pcap(Load, path)?;
    }

    Ok(())
}

fn create_view<Item, Model, RowData, Cursor>(
        capture: &Arc<Mutex<Capture>>,
        #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
        recording_args: (&Rc<RefCell<Recording>>, &'static str))
    -> (Model, ListView)
    where
        Item: Copy + PartialOrd + Debug + 'static,
        Model: GenericModel<Item> + IsA<ListModel> + IsA<Object>,
        RowData: GenericRowData<Item> + IsA<Object>,
        Cursor: 'static,
        Capture: ItemSource<Item, Cursor>,
        Object: ToGenericRowData<Item>
{
    #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
    let (name, expand_rec, update_rec, changed_rec) = {
        let (recording, name) = recording_args;
        (name, recording.clone(), recording.clone(), recording.clone())
    };
    let model = Model::new(
        capture.clone(),
        #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
        Rc::new(
            RefCell::new(
                move |position, summary|
                    update_rec
                        .borrow_mut()
                        .log_item_updated(name, position, summary)
            )
        )).expect("Failed to create model");
    let bind_model = model.clone();
    let cap_arc = capture.clone();
    let selection_model = SingleSelection::new(Some(&model));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let expander = ExpanderWrapper::new();
        list_item.set_child(Some(&expander));
    });
    let bind = move |list_item: &ListItem| {
        let row = list_item
            .item()
            .or_bug("ListItem has no item")?
            .downcast::<RowData>()
            .or_bug("Item is not RowData")?;

        let expander_wrapper = list_item
            .child()
            .or_bug("ListItem has no child widget")?
            .downcast::<ExpanderWrapper>()
            .or_bug("Child widget is not an ExpanderWrapper")?;

        let expander = expander_wrapper.expander();
        match row.node() {
            Ok(node_ref) => {
                let node = node_ref.borrow();
                let summary = node.field(
                    &cap_arc, Box::new(Capture::summary));
                expander_wrapper.set_text(summary);
                let connectors = node.field(
                    &cap_arc, Box::new(Capture::connectors));
                expander_wrapper.set_connectors(connectors);
                expander.set_visible(node.expandable());
                expander.set_expanded(node.expanded());
                let model = bind_model.clone();
                let node_ref = node_ref.clone();
                let list_item = list_item.clone();
                #[cfg(any(feature="test-ui-replay",
                          feature="record-ui-test"))]
                let recording = expand_rec.clone();
                let handler = expander.connect_expanded_notify(move |expander| {
                    let position = list_item.position();
                    let expanded = expander.is_expanded();
                    #[cfg(any(feature="test-ui-replay",
                              feature="record-ui-test"))]
                    recording.borrow_mut().log_item_expanded(
                        name, position, expanded);
                    display_error(
                        model.set_expanded(&node_ref, position, expanded)
                            .map_err(PacketryError::Model))
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
            .or_bug("ListItem has no item")?
            .downcast::<RowData>()
            .or_bug("Item is not RowData")?;

        let expander_wrapper = list_item
            .child()
            .or_bug("ListItem has no child widget")?
            .downcast::<ExpanderWrapper>()
            .or_bug("Child widget is not an ExpanderWrapper")?;

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

    let view = ListView::new(Some(&selection_model), Some(&factory));

    #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
    model.connect_items_changed(move |model, position, removed, added|
        changed_rec.borrow_mut().log_items_changed(
            name, model, position, removed, added));

    (model, view)
}

pub fn reset_capture() -> Result<(), PacketryError> {
    let capture = Arc::new(Mutex::new(Capture::new()?));
    with_ui(|ui| {
        let (traffic_model, traffic_view) = create_view::<
            TrafficItem, TrafficModel, TrafficRowData, TrafficCursor>(
                &capture,
                #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
                (&ui.recording, "traffic")
            );
        let (device_model, device_view) = create_view::<
            DeviceItem, DeviceModel, DeviceRowData, ()>(
                &capture,
                #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
                (&ui.recording, "devices")
            );
        ui.capture = capture;
        ui.traffic_model = Some(traffic_model);
        ui.device_model = Some(device_model);
        ui.endpoint_count = 2;
        ui.traffic_window.set_child(Some(&traffic_view));
        ui.device_window.set_child(Some(&device_view));
        ui.stop_button.set_sensitive(false);
        Ok(())
    })
}

pub fn update_view() -> Result<(), PacketryError> {
    with_ui(|ui| {
        use FileAction::*;
        use PacketryError::Lock;
        #[cfg(feature="record-ui-test")]
        let guard = {
            let guard = UPDATE_LOCK.lock();
            let packet_count = ui.capture
                .lock()
                .or(Err(Lock))?
                .packet_index.len();
            ui.recording
                .borrow_mut()
                .log_update(packet_count);
            guard
        };
        let mut more_updates = false;
        if ui.show_progress == Some(Save) {
            more_updates = true;
        } else {
            if let Some(model) = &ui.traffic_model {
                let old_count = model.n_items();
                more_updates |= model.update()?;
                let new_count = model.n_items();
                // If any endpoints were added, we need to redraw the rows above
                // to add the additional columns of the connecting lines.
                if new_count > old_count {
                    let new_ep_count = ui.capture
                        .lock()
                        .or(Err(Lock))?
                        .endpoints.len() as u16;
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

fn choose_file(action: FileAction) -> Result<(), PacketryError> {
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

fn start_pcap(action: FileAction, path: PathBuf) -> Result<(), PacketryError> {
    use FileAction::*;
    if action == Load {
        reset_capture()?;
    }
    with_ui(|ui| {
        #[cfg(feature="record-ui-test")]
        ui.recording.borrow_mut().log_open_file(&path, &ui.capture);
        ui.open_button.set_sensitive(false);
        ui.save_button.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.speed_dropdown.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        let signal_id = ui.stop_button.connect_clicked(|_|
            display_error(stop_pcap()));
        ui.vbox.append(&ui.progress_bar);
        ui.show_progress = Some(action);
        let capture = ui.capture.clone();
        use PacketryError::Lock;
        let worker = move || match action {
            Load => {
                let file = File::open(path)?;
                let file_size = file.metadata()?.len();
                TOTAL.store(file_size, Ordering::Relaxed);
                let reader = BufReader::new(file);
                let mut pcap = PcapReader::new(reader)?;
                let mut bytes_read = size_of::<PcapHeader>() as u64;
                let mut decoder = Decoder::default();
                #[cfg(feature="step-decoder")]
                let (mut client, _addr) =
                    TcpListener::bind("127.0.0.1:46563")?.accept()?;
                while let Some(result) = pcap.next_raw_packet() {
                    #[cfg(feature="step-decoder")] {
                        let mut buf = [0; 1];
                        client.read(&mut buf).unwrap();
                    };
                    let packet = result?;
                    #[cfg(feature="record-ui-test")]
                    let guard = UPDATE_LOCK.lock();
                    let mut cap = capture.lock().or(Err(Lock))?;
                    decoder.handle_raw_packet(&mut cap, &packet.data)?;
                    #[cfg(feature="record-ui-test")]
                    drop(guard);
                    let size = 16 + packet.data.len();
                    bytes_read += size as u64;
                    CURRENT.store(bytes_read, Ordering::Relaxed);
                    if STOP.load(Ordering::Relaxed) {
                        break;
                    }
                }
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.finish(&mut cap)?;
                cap.print_storage_summary();
                Ok(())
            },
            Save => {
                let packet_count = capture
                    .lock()
                    .or(Err(Lock))?
                    .packet_index.len();
                TOTAL.store(packet_count, Ordering::Relaxed);
                CURRENT.store(0, Ordering::Relaxed);
                let file = File::create(path)?;
                let writer = BufWriter::new(file);
                let header = PcapHeader {
                    datalink: DataLink::USB_2_0,
                    .. PcapHeader::default()
                };
                let mut pcap = PcapWriter::with_header(writer, header)?;
                for i in 0..packet_count {
                    let mut cap = capture.lock().or(Err(Lock))?;
                    let packet_id = PacketId::from(i);
                    let bytes = cap.packet(packet_id)?;
                    let length: u32 = bytes
                        .len()
                        .try_into()
                        .or_bug("Packet too large for pcap file")?;
                    let packet = RawPcapPacket {
                        ts_sec: 0,
                        ts_frac: 0,
                        incl_len: length,
                        orig_len: length,
                        data: Cow::from(bytes)
                    };
                    pcap.write_raw_packet(&packet)?;
                    CURRENT.store(i + 1, Ordering::Relaxed);
                    if STOP.load(Ordering::Relaxed) {
                        break;
                    }
                }
                pcap.into_writer().flush()?;
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
                        ui.vbox.remove(&ui.progress_bar);
                        ui.stop_button.disconnect(signal_id);
                        ui.stop_button.set_sensitive(false);
                        ui.open_button.set_sensitive(true);
                        ui.save_button.set_sensitive(true);
                        ui.capture_button.set_sensitive(true);
                        ui.speed_dropdown.set_sensitive(true);
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

pub fn stop_pcap() -> Result<(), PacketryError> {
    STOP.store(true, Ordering::Relaxed);
    with_ui(|ui| {
        ui.stop_button.set_sensitive(false);
        Ok(())
    })
}

pub fn start_luna() -> Result<(), PacketryError> { 
    reset_capture()?;
    with_ui(|ui| {
        let speed_id: u8 = ui.speed_dropdown.selected().try_into().unwrap();
        let speed = Speed::from(speed_id);
        let luna = LunaDevice::open()?;
        let (mut stream_handle, stop_handle) = luna.start(speed)?;
        ui.stop_handle.replace(stop_handle);
        ui.open_button.set_sensitive(false);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        ui.speed_dropdown.set_sensitive(false);
        let signal_id = ui.stop_button.connect_clicked(|_|
            display_error(stop_luna()));
        let capture = ui.capture.clone();
        let mut read_luna = move || {
            use PacketryError::Lock;
            let mut decoder = Decoder::default();
            while let Some(packet) = stream_handle.next() {
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.handle_raw_packet(&mut cap, &packet?)?;
            }
            let mut cap = capture.lock().or(Err(Lock))?;
            decoder.finish(&mut cap)?;
            Ok(())
        };
        std::thread::spawn(move || {
            display_error(read_luna());
            gtk::glib::idle_add_once(|| {
                display_error(
                    with_ui(|ui| {
                        ui.stop_button.disconnect(signal_id);
                        ui.stop_button.set_sensitive(false);
                        ui.open_button.set_sensitive(true);
                        ui.capture_button.set_sensitive(true);
                        ui.speed_dropdown.set_sensitive(true);
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

pub fn stop_luna() -> Result<(), PacketryError> {
    with_ui(|ui| {
        if let Some(stop_handle) = ui.stop_handle.take() {
            stop_handle.stop()?;
        }
        ui.save_button.set_sensitive(true);
        Ok(())
    })
}

pub fn display_error(result: Result<(), PacketryError>) {
    #[cfg(not(feature="test-ui-replay"))]
    if let Err(e) = result {
        let message = format!("{e}");
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
    #[cfg(feature="test-ui-replay")]
    result.unwrap();
}

trait OrBug<T> {
    fn or_bug(self, msg: &'static str) -> Result<T, PacketryError>;
}

impl<T> OrBug<T> for Option<T> {
    fn or_bug(self, msg: &'static str) -> Result<T, PacketryError> {
        self.ok_or(PacketryError::Bug(msg))
    }
}

impl<T, E> OrBug<T> for Result<T, E> {
    fn or_bug(self, msg: &'static str) -> Result<T, PacketryError> {
        self.or(Err(PacketryError::Bug(msg)))
    }
}
