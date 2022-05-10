#[macro_use]
extern crate bitfield;

mod model;
pub mod row_data;
mod expander;

use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    ListView,
    Label,
    TreeExpander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};
use row_data::GenericRowData;
use model::GenericModel;
use expander::ExpanderWrapper;

mod capture;
use capture::Capture;

mod decoder;
use decoder::Decoder;

mod file_vec;
mod hybrid_index;
mod usb;

fn create_view<Item, Model, RowData>(capture: &Arc<Mutex<Capture>>)
        -> ListView
    where
        Model: GenericModel<Item> + IsA<ListModel>,
        RowData: GenericRowData<Item> + IsA<Object>
{
    let cap = capture.clone();
    let model = Model::new(cap, None);
    let cap = capture.clone();
    let tree_model = TreeListModel::new(&model, false, false, move |o| {
        let row = o.downcast_ref::<RowData>().unwrap();
        match row.child_count(&mut cap.lock().unwrap()) {
            0 => None,
            _ => Some(
                Model::new(cap.clone(), row.get_item())
                    .upcast::<ListModel>()
            )
        }
    });
    let selection_model = SingleSelection::new(Some(&tree_model));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let text_label = Label::new(None);
        if RowData::CONNECTORS {
            let expander = ExpanderWrapper::new();
            list_item.set_child(Some(&expander));
        } else {
            let expander = TreeExpander::new();
            expander.set_child(Some(&text_label));
            list_item.set_child(Some(&expander));
        }
    });
    factory.connect_bind(move |_, list_item| {
        let treelistrow = list_item
            .item()
            .expect("The item has to exist.")
            .downcast::<TreeListRow>()
            .expect("The item has to be a TreeListRow.");

        let row = treelistrow
            .item()
            .expect("The item has to exist.")
            .downcast::<RowData>()
            .expect("The item has to be RowData.");

        let container = list_item
            .child()
            .expect("The child has to exist");

        let text_label = container
            .last_child()
            .expect("The child has to exist")
            .downcast::<Label>()
            .expect("The child must be a Label.");

        let summary = row.get_summary();
        text_label.set_text(&summary);

        if RowData::CONNECTORS {
            let expander_wrapper = container
                .downcast::<ExpanderWrapper>()
                .expect("The child must be a ExpanderWrapper.");

            expander_wrapper.set_connectors(row.get_connectors());
            let expander = expander_wrapper.expander();
            expander.set_visible(treelistrow.is_expandable());
            expander.set_expanded(treelistrow.is_expanded());
            let handler = expander.connect_expanded_notify(move |expander| {
                treelistrow.set_expanded(expander.is_expanded());
            });
            expander_wrapper.set_handler(handler);
        } else {
            let tree_expander = container
                .downcast::<TreeExpander>()
                .expect("The child must be a TreeExpander.");

            tree_expander.set_list_row(Some(&treelistrow));
        }
    });
    factory.connect_unbind(move |_, list_item| {
        let container = list_item
            .child()
            .expect("The child has to exist");

        if RowData::CONNECTORS {
            let expander_wrapper = container
                .downcast::<ExpanderWrapper>()
                .expect("The child must be a ExpanderWrapper.");

            let expander = expander_wrapper.expander();
            match expander_wrapper.take_handler() {
                Some(handler) => expander.disconnect(handler),
                None => panic!("Handler was not set")
            };
        } else {
            let tree_expander = container
                .downcast::<TreeExpander>()
                .expect("The child must be a TreeExpander.");

            tree_expander.set_list_row(None);
        }
    });
    ListView::new(Some(&selection_model), Some(&factory))
}

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.packetry"),
        Default::default(),
    );

    let args: Vec<_> = std::env::args().collect();
    let mut pcap = pcap::Capture::from_file(&args[1]).unwrap();
    let mut cap = Capture::new();
    let mut decoder = Decoder::new(&mut cap);
    while let Ok(packet) = pcap.next() {
        decoder.handle_raw_packet(&packet);
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
            <capture::Item, model::Model, row_data::RowData>(&capture);

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
}
