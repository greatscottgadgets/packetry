#[macro_use]
extern crate bitfield;
use thiserror::Error;

mod backend;
mod model;
mod row_data;
mod expander;
mod tree_list_model;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    ListView,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};

use model::GenericModel;
use row_data::GenericRowData;
use expander::ExpanderWrapper;

mod capture;
use capture::{Capture, CaptureError, ItemSource};

mod decoder;
use decoder::Decoder;

mod id;
mod file_vec;
mod hybrid_index;
mod usb;
mod vec_map;

thread_local!(
    static MODELS: RefCell<Vec<Object>>  = RefCell::new(Vec::new());
    static LUNA: RefCell<Option<backend::luna::LunaCapture>> = RefCell::new(None);
);

fn create_view<Item: 'static, Model, RowData>(capture: &Arc<Mutex<Capture>>)
        -> ListView
    where
        Item: Copy,
        Model: GenericModel<Item> + IsA<ListModel> + IsA<Object>,
        RowData: GenericRowData<Item> + IsA<Object>,
        Capture: ItemSource<Item>
{
    let model = Model::new(capture.clone())
                      .expect("Failed to create model");

    MODELS.with(|models| models.borrow_mut().push(model.clone().upcast()));
    let cap_arc = capture.clone();
    let selection_model = SingleSelection::new(Some(&model));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let expander = ExpanderWrapper::new();
        list_item.set_child(Some(&expander));
    });
    factory.connect_bind(move |_, list_item| {
        let row = list_item
            .item()
            .expect("The item has to exist.")
            .downcast::<RowData>()
            .expect("The item has to be RowData.");

        let expander_wrapper = list_item
            .child()
            .expect("The child has to exist")
            .downcast::<ExpanderWrapper>()
            .expect("The child must be a ExpanderWrapper.");

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
                let model = model.clone();
                let node_ref = node_ref.clone();
                let handler = expander.connect_expanded_notify(move |expander| {
                    model.set_expanded(&node_ref, expander.is_expanded())
                         .expect("Failed to expand node")
                });
                expander_wrapper.set_handler(handler);
            },
            Err(msg) => {
                expander_wrapper.set_connectors("".to_string());
                expander_wrapper.set_text(format!("Error: {}", msg));
                expander.set_visible(false);
            }
        };
    });
    factory.connect_unbind(move |_, list_item| {
        let expander_wrapper = list_item
            .child()
            .expect("The child has to exist")
            .downcast::<ExpanderWrapper>()
            .expect("The child must be a ExpanderWrapper.");

        let expander = expander_wrapper.expander();

        if let Some(handler) = expander_wrapper.take_handler() {
            expander.disconnect(handler);
        }
    });
    ListView::new(Some(&selection_model), Some(&factory))
}

#[derive(Error, Debug)]
pub enum PacketryError {
    #[error(transparent)]
    CaptureError(#[from] CaptureError),
    #[error(transparent)]
    PcapError(#[from] pcap::Error),
}

fn run() -> Result<(), PacketryError> {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry"),
        Default::default(),
    );

    let args: Vec<_> = std::env::args().collect();
    let mut cap = Capture::new()?;
    let mut decoder = Decoder::new(&mut cap)?;
    cap.print_storage_summary();
    let capture = Arc::new(Mutex::new(cap));

    let app_capture = capture.clone();
    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::builder()
            .default_width(320)
            .default_height(480)
            .application(application)
            .title("Packetry")
            .build();

        let listview = create_view::
            <capture::TrafficItem, model::TrafficModel, row_data::TrafficRowData>(&app_capture);

        let scrolled_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic) // Disable horizontal scrolling
            .min_content_height(480)
            .min_content_width(240)
            .build();

        scrolled_window.set_child(Some(&listview));

        let device_tree = create_view::<capture::DeviceItem,
                                        model::DeviceModel,
                                        row_data::DeviceRowData>(&app_capture);
        let device_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(480)
            .min_content_width(100)
            .child(&device_tree)
            .build();

        let paned = gtk::Paned::builder()
            .orientation(Orientation::Horizontal)
            .wide_handle(true)
            .start_child(&scrolled_window)
            .end_child(&device_window)
            .build();

        window.set_child(Some(&paned));
        window.show();
    });

    let mut source_id: Option<gtk::glib::source::SourceId> = None;

    if args.len() > 1 {
        let mut pcap = pcap::Capture::from_file(&args[1])?;
        let mut cap = capture.lock().ok().unwrap();
        while let Ok(packet) = pcap.next() {
            decoder.handle_raw_packet(&mut cap, &packet).unwrap();
        }
    } else {
        LUNA.with(|cell| {
            cell.borrow_mut().replace(
                backend::luna::LunaDevice::open()
                    .unwrap()
                    .start()
                    .unwrap()
            );
        });
        let update_capture = capture.clone();
        source_id = Some(gtk::glib::timeout_add_local(std::time::Duration::from_millis(1), move || {
            let mut cap = update_capture.lock().ok().unwrap();

            LUNA.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let luna = borrow.as_mut().unwrap();
                while let Some(packet) = luna.next() {
                    decoder.handle_raw_packet(&mut cap, &packet).unwrap();
                }
            });

            drop(cap);

            MODELS.with(|models|
                for model in models.borrow().iter() {
                    let model = model.clone();
                    if let Ok(tree_model) = model.downcast::<crate::model::TrafficModel>() {
                        tree_model.update().unwrap();
                    };
                }
            );

            Continue(true)
        }));
    }

    application.run_with_args::<&str>(&[]);
    if let Some(source) = source_id {
        source.remove();
    }
    Ok(())
}

fn main() {
    let result = run();
    if let Err(e) = result {
        println!("Error: {:?}", e)
    }
    let stop_result = LUNA.with(|cell| {
        if let Some(luna) = cell.take() {
            luna.stop()
        } else {
            Ok(())
        }
    });
    if let Err(e) = stop_result {
        println!("Error stopping analyzer: {:?}", e)
    }
}
