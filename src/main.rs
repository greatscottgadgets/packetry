#[macro_use]
extern crate bitfield;
use thiserror::Error;

mod model;
mod row_data;
mod expander;
mod tree_list_model;

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

fn create_view<Item: 'static, Model, RowData>(capture: &Arc<Mutex<Capture>>)
        -> ListView
    where
        Item: Copy,
        Model: GenericModel<Item> + IsA<ListModel>,
        RowData: GenericRowData<Item> + IsA<Object>,
        Capture: ItemSource<Item>
{
    let model = Model::new(capture.clone())
                      .expect("Failed to create model");
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

        let node_ref = row.node();
        let node = node_ref.borrow();
        let summary = node.field(&cap_arc, Box::new(Capture::summary));
        expander_wrapper.set_text(summary);
        let connectors = node.field(&cap_arc, Box::new(Capture::connectors));
        expander_wrapper.set_connectors(connectors);
        let expander = expander_wrapper.expander();
        expander.set_visible(node.expandable());
        expander.set_expanded(node.expanded());
        let model = model.clone();
        let node_ref = node_ref.clone();
        let handler = expander.connect_expanded_notify(move |expander| {
            model.set_expanded(&node_ref, expander.is_expanded())
                 .expect("Failed to expand node")
        });
        expander_wrapper.set_handler(handler);
    });
    factory.connect_unbind(move |_, list_item| {
        let expander_wrapper = list_item
            .child()
            .expect("The child has to exist")
            .downcast::<ExpanderWrapper>()
            .expect("The child must be a ExpanderWrapper.");

        let expander = expander_wrapper.expander();
        match expander_wrapper.take_handler() {
            Some(handler) => expander.disconnect(handler),
            None => panic!("Handler was not set")
        };
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
    let mut pcap = pcap::Capture::from_file(&args[1])?;
    let mut cap = Capture::new()?;
    let mut decoder = Decoder::new(&mut cap)?;
    while let Ok(packet) = pcap.next() {
        decoder.handle_raw_packet(&packet)?;
    }
    cap.print_storage_summary();
    let capture = Arc::new(Mutex::new(cap));

    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::builder()
            .default_width(320)
            .default_height(480)
            .application(application)
            .title("Packetry")
            .build();

        let listview = create_view::
            <capture::TrafficItem, model::TrafficModel, row_data::TrafficRowData>(&capture);

        let scrolled_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic) // Disable horizontal scrolling
            .min_content_height(480)
            .min_content_width(640)
            .build();

        scrolled_window.set_child(Some(&listview));

        let device_tree = create_view::<capture::DeviceItem,
                                        model::DeviceModel,
                                        row_data::DeviceRowData>(&capture);
        let device_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(480)
            .min_content_width(240)
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
    application.run_with_args::<&str>(&[]);
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => {},
        Err(e) => println!("Error: {:?}", e)
    }
}
