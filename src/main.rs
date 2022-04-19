#[macro_use]
extern crate bitfield;

mod model;
pub mod row_data;

use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::glib::Object;
use gtk::{
    prelude::*,
    ListView,
    Label,
    Expander,
    TreeExpander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};
use row_data::GenericRowData;
use model::GenericModel;

mod capture;
use capture::Capture;

mod file_vec;
mod hybrid_index;

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
            let container = gtk::Box::new(Orientation::Horizontal, 5);
            let conn_label = Label::new(None);
            let expander = Expander::new(None);
            container.append(&conn_label);
            container.append(&expander);
            container.append(&text_label);
            list_item.set_child(Some(&container));
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
            let conn_label = container
                .first_child()
                .expect("The child has to exist")
                .downcast::<Label>()
                .expect("The child must be a Label.");

            let expander = conn_label
                .next_sibling()
                .expect("The child has to exist")
                .downcast::<Expander>()
                .expect("The child must be a Expander.");

            expander.set_visible(treelistrow.is_expandable());

            match row.get_connectors() {
                Some(connectors) =>
                    conn_label.set_markup(
                        format!("<tt>{}</tt>", connectors).as_str()),
                None => {}
            };

            expander.connect_expanded_notify(move |expander| {
                treelistrow.set_expanded(expander.is_expanded());
            });
        } else {
            let tree_expander = container
                .downcast::<TreeExpander>()
                .expect("The child must be a TreeExpander.");

            tree_expander.set_list_row(Some(&treelistrow));
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
    while let Ok(packet) = pcap.next() {
        cap.handle_raw_packet(&packet);
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
