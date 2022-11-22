#[macro_use]
extern crate bitfield;
use thiserror::Error;

mod backend;
mod model;
mod row_data;
mod expander;
mod tree_list_model;

use std::cell::RefCell;
use std::fs::File;
use std::io::BufReader;
use std::mem::size_of;
use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    Application,
    ApplicationWindow,
    Button,
    MessageDialog,
    DialogFlags,
    MessageType,
    ButtonsType,
    ListItem,
    ListView,
    ProgressBar,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};

use pcap_file::{PcapError, pcap::{PcapReader, PcapHeader, PacketHeader}};

use backend::luna::{LunaDevice, LunaStop};
use model::{GenericModel, TrafficModel, DeviceModel};
use row_data::{GenericRowData, TrafficRowData, DeviceRowData};
use expander::ExpanderWrapper;
use tree_list_model::ModelError;

mod capture;
use capture::{
    Capture,
    CaptureError,
    ItemSource,
    TrafficItem,
    DeviceItem,
    fmt_size,
};

mod decoder;
use decoder::Decoder;

mod id;
mod file_vec;
mod hybrid_index;
mod usb;
mod vec_map;

static PCAP_SIZE: AtomicU64 = AtomicU64::new(0);
static PCAP_READ: AtomicU64 = AtomicU64::new(0);

struct UserInterface {
    capture: Arc<Mutex<Capture>>,
    stop_handle: Option<LunaStop>,
    traffic_model: TrafficModel,
    device_model: DeviceModel,
    show_progress: bool,
    progress_bar: ProgressBar,
    vbox: gtk::Box,
    capture_button: Button,
    stop_button: Button,
}

thread_local!(
    static WINDOW: RefCell<Option<ApplicationWindow>> = RefCell::new(None);
    static UI: RefCell<Option<UserInterface>> = RefCell::new(None);
);

fn create_view<Item: 'static, Model, RowData>(capture: &Arc<Mutex<Capture>>)
    -> (Model, ListView)
    where
        Item: Copy,
        Model: GenericModel<Item> + IsA<ListModel> + IsA<Object>,
        RowData: GenericRowData<Item> + IsA<Object>,
        Capture: ItemSource<Item>
{
    let model = Model::new(capture.clone())
                      .expect("Failed to create model");
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
                let handler = expander.connect_expanded_notify(move |expander| {
                    display_error(
                        model.set_expanded(&node_ref, expander.is_expanded())
                            .map_err(PacketryError::Model));
                });
                expander_wrapper.set_handler(handler);
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
        let expander_wrapper = list_item
            .child()
            .or_bug("ListItem has no child widget")?
            .downcast::<ExpanderWrapper>()
            .or_bug("Child widget is not an ExpanderWrapper")?;
        let expander = expander_wrapper.expander();
        let handler = expander_wrapper
            .take_handler()
            .or_bug("ExpanderWrapper handler was not set")?;
        expander.disconnect(handler);
        Ok(())
    };
    factory.connect_bind(move |_, item| display_error(bind(item)));
    factory.connect_unbind(move |_, item| display_error(unbind(item)));

    let view = ListView::new(Some(&selection_model), Some(&factory));

    (model, view)
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

fn activate(application: &Application) -> Result<(), PacketryError> {
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

    open_button.set_sensitive(false);
    save_button.set_sensitive(false);
    capture_button.set_sensitive(true);
    stop_button.set_sensitive(false);

    header_bar.pack_start(&open_button);
    header_bar.pack_start(&save_button);
    header_bar.pack_start(&capture_button);
    header_bar.pack_start(&stop_button);

    window.set_titlebar(Some(&header_bar));
    window.show();
    WINDOW.with(|win_opt| win_opt.replace(Some(window.clone())));

    let args: Vec<_> = std::env::args().collect();
    let capture = Arc::new(Mutex::new(Capture::new()?));

    let (traffic_model, traffic_view) =
        create_view::<TrafficItem, TrafficModel, TrafficRowData>(&capture);

    let traffic_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(480)
        .min_content_width(640)
        .build();

    traffic_window.set_child(Some(&traffic_view));

    let (device_model, device_view) =
        create_view::<DeviceItem, DeviceModel, DeviceRowData>(&capture);

    let device_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(480)
        .min_content_width(240)
        .child(&device_view)
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
    stop_button.connect_clicked(|_| display_error(stop_luna()));

    UI.with(|cell|
        cell.borrow_mut().replace(
            UserInterface {
                capture,
                stop_handle: None,
                traffic_model,
                device_model,
                show_progress: false,
                progress_bar,
                vbox,
                capture_button,
                stop_button,
            }
        )
    );

    if args.len() > 1 {
        let filename = args[1].clone();
        start_pcap(filename)?;
    }

    gtk::glib::timeout_add_local(
        std::time::Duration::from_millis(10),
        move || {
            let result = update_view();
            if result.is_ok() {
                Continue(true)
            } else {
                display_error(result);
                Continue(false)
            }
        }
    );

    Ok(())
}

fn with_ui<F>(f: F) -> Result<(), PacketryError>
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

fn update_view() -> Result<(), PacketryError> {
    with_ui(|ui| {
        ui.traffic_model.update()?;
        ui.device_model.update()?;
        if ui.show_progress {
            let bytes_total = PCAP_SIZE.load(Ordering::Relaxed);
            let bytes_read = PCAP_READ.load(Ordering::Relaxed);
            if bytes_read >= bytes_total {
                ui.vbox.remove(&ui.progress_bar);
                ui.show_progress = false;
            } else {
                let text = format!(
                    "Loaded {} / {}",
                    fmt_size(bytes_read),
                    fmt_size(bytes_total));
                let fraction = (bytes_read as f64) / (bytes_total as f64);
                ui.progress_bar.set_text(Some(&text));
                ui.progress_bar.set_fraction(fraction);
            }
        }
        Ok(())
    })
}

fn start_pcap(filename: String) -> Result<(), PacketryError> {
    with_ui(|ui| {
        ui.capture_button.set_sensitive(false);
        ui.vbox.append(&ui.progress_bar);
        ui.show_progress = true;
        let capture = ui.capture.clone();
        let read_pcap = move || {
            let file = File::open(filename)?;
            let file_size = file.metadata()?.len();
            PCAP_SIZE.store(file_size, Ordering::Relaxed);
            let reader = BufReader::new(file);
            let pcap = PcapReader::new(reader)?;
            let mut bytes_read = size_of::<PcapHeader>() as u64;
            let mut decoder = Decoder::default();
            use PacketryError::Lock;
            for result in pcap {
                let packet = result?;
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.handle_raw_packet(&mut cap, &packet.data)?;
                let size = size_of::<PacketHeader>() + packet.data.len();
                bytes_read += size as u64;
                PCAP_READ.store(bytes_read, Ordering::Relaxed);
            }
            let cap = capture.lock().or(Err(Lock))?;
            cap.print_storage_summary();
            Ok(())
        };
        std::thread::spawn(move || display_error(read_pcap()));
        Ok(())
    })
}

fn start_luna() -> Result<(), PacketryError> { 
    with_ui(|ui| {
        let (mut stream_handle, stop_handle) = LunaDevice::open()?.start()?;
        ui.stop_handle.replace(stop_handle);
        ui.capture_button.set_sensitive(false);
        ui.stop_button.set_sensitive(true);
        let capture = ui.capture.clone();
        let mut read_luna = move || {
            use PacketryError::Lock;
            let mut decoder = Decoder::default();
            while let Some(packet) = stream_handle.next() {
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.handle_raw_packet(&mut cap, &packet?)?;
            }
            Ok(())
        };
        std::thread::spawn(move || {
            display_error(read_luna());
            gtk::glib::idle_add_once(|| {
                display_error(with_ui(|ui| {
                    ui.stop_button.set_sensitive(false);
                    Ok(())
                }));
            });
        });
        Ok(())
    })
}

fn stop_luna() -> Result<(), PacketryError> {
    with_ui(|ui| {
        if let Some(stop_handle) = ui.stop_handle.take() {
            stop_handle.stop()?;
        }
        Ok(())
    })
}

fn display_error(result: Result<(), PacketryError>) {
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

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry"),
        Default::default(),
    );
    application.connect_activate(|app| display_error(activate(app)));
    application.run_with_args::<&str>(&[]);
    display_error(stop_luna());
}
