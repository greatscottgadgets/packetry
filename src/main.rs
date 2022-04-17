#[macro_use]
extern crate bitfield;

mod model;
pub mod row_data;

use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::{
    prelude::*,
    ListView,
    Label,
    Expander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};
use row_data::{RowData, GenericRowData};

mod capture;
use capture::Capture;

mod file_vec;
mod hybrid_index;

fn create_view(capture: &Arc<Mutex<Capture>>) -> ListView
{
    let cap = capture.clone();
    let model = model::Model::new(cap, None);
    let cap = capture.clone();
    let tree_model = TreeListModel::new(&model, false, false, move |o| {
        let row = o.downcast_ref::<RowData>().unwrap();
        let parent_item = row.get_item();
        match cap.lock().unwrap().item_count(&parent_item) {
            0 => None,
            _ => Some(
                model::Model::new(cap.clone(), parent_item)
                    .upcast::<ListModel>()
            )
        }
    });
    let selection_model = SingleSelection::new(Some(&tree_model));
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let container = gtk::Box::new(Orientation::Horizontal, 5);
        let conn_label = Label::new(None);
        let text_label = Label::new(None);
        let expander = Expander::new(None);
        container.append(&conn_label);
        container.append(&expander);
        container.append(&text_label);
        list_item.set_child(Some(&container));
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
            .expect("The child has to exist")
            .downcast::<gtk::Box>()
            .expect("The child must be a gtk::Box.");

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

        let text_label = container
            .last_child()
            .expect("The child has to exist")
            .downcast::<Label>()
            .expect("The child must be a Label.");

        let conn = format!("<tt>{}</tt>", row.property::<String>("conn"));
        let text = row.property::<String>("text");
        conn_label.set_markup(&conn);
        text_label.set_text(&text);
        expander.set_visible(treelistrow.is_expandable());
        expander.connect_expanded_notify(move |expander| {
            treelistrow.set_expanded(expander.is_expanded());
        });
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

        let listview = create_view(&capture);

        let scrolled_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic) // Disable horizontal scrolling
            .min_content_height(480)
            .min_content_width(640)
            .build();

        scrolled_window.set_child(Some(&listview));

        let device_tree = gtk::TreeView::new();
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
