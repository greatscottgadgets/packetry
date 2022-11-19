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
use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    Application,
    ApplicationWindow,
    MessageDialog,
    DialogFlags,
    MessageType,
    ButtonsType,
    ListItem,
    ListView,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};

use pcap_file::{PcapError, pcap::PcapReader};

use backend::luna::{LunaDevice, LunaStop};
use model::{GenericModel, TrafficModel, DeviceModel};
use row_data::{GenericRowData, TrafficRowData, DeviceRowData};
use expander::ExpanderWrapper;
use tree_list_model::ModelError;

mod capture;
use capture::{Capture, CaptureError, ItemSource, TrafficItem, DeviceItem};

mod decoder;
use decoder::Decoder;

mod id;
mod file_vec;
mod hybrid_index;
mod usb;
mod vec_map;

struct UserInterface {
    stop_handle: Option<LunaStop>,
    traffic_model: TrafficModel,
    device_model: DeviceModel,
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

    window.show();
    WINDOW.with(|win_opt| win_opt.replace(Some(window.clone())));

    let args: Vec<_> = std::env::args().collect();
    let mut cap = Capture::new()?;
    let mut decoder = Decoder::new(&mut cap)?;
    let capture = Arc::new(Mutex::new(cap));
    let app_capture = capture.clone();

    let (traffic_model, traffic_view) =
        create_view::<TrafficItem, TrafficModel, TrafficRowData>(&app_capture);

    let traffic_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(480)
        .min_content_width(640)
        .build();

    traffic_window.set_child(Some(&traffic_view));

    let (device_model, device_view) =
        create_view::<DeviceItem, DeviceModel, DeviceRowData>(&app_capture);

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
        .build();

    window.set_child(Some(&paned));

    UI.with(|cell|
        cell.borrow_mut().replace(
            UserInterface {
                stop_handle: None,
                traffic_model,
                device_model,
            }
        )
    );

    use PacketryError::Lock;
    if args.len() > 1 {
        let mut read_pcap = move || {
            let file = File::open(&args[1])?;
            let reader = BufReader::new(file);
            let pcap_reader = PcapReader::new(reader)?;
            for result in pcap_reader {
                let packet = result?;
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.handle_raw_packet(&mut cap, &packet.data)?;
            }
            let cap = capture.lock().or(Err(Lock))?;
            cap.print_storage_summary();
            Ok(())
        };
        std::thread::spawn(move || display_error(read_pcap()));
    } else {
        let (mut stream_handle, stop_handle) = LunaDevice::open()?.start()?;
        with_ui(|ui| { ui.stop_handle.replace(stop_handle); Ok(())})?;
        let mut read_luna = move || {
            while let Some(packet) = stream_handle.next() {
                let mut cap = capture.lock().or(Err(Lock))?;
                decoder.handle_raw_packet(&mut cap, &packet?)?;
            }
            Ok(())
        };
        std::thread::spawn(move || display_error(read_luna()));
    };

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
