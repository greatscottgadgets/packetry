#[macro_use]
extern crate bitfield;

mod model;
pub mod row_data;

use std::sync::{Arc, Mutex};

use gtk::gio::ListModel;
use gtk::{
    prelude::*,
    ColumnView,
    ColumnViewColumn,
    Label,
    TreeExpander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
};
use row_data::RowData;

mod capture;
use capture::Capture;

mod file_vec;

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.luna-analyzer-rust"),
        Default::default(),
    );

    let args: Vec<_> = std::env::args().collect();
    let mut pcap = pcap::Capture::from_file(&args[1]).unwrap();
    let mut cap = Capture::new();
    while let Ok(packet) = pcap.next() {
        cap.handle_raw_packet(&packet);
    }
    cap.finish();
    let capture = Arc::new(Mutex::new(cap));

    application.connect_activate(move |application| {
        let window = gtk::ApplicationWindow::builder()
            .default_width(320)
            .default_height(480)
            .application(application)
            .title("luna-analyzer-rust")
            .build();

        // Create the top-level model
        let cap = capture.clone();
        let model = model::Model::new(cap, None);

        let cap = capture.clone();
        let treemodel = TreeListModel::new(&model, false, false, move |o| {
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
        let selection_model = SingleSelection::new(Some(&treemodel));

        let column_view = ColumnView::new(Some(&selection_model));

        // For each field in the Capture, add a column & factory for it.
        for i in 0..capture::ItemFields::field_names().len() {
            // Create factory for binding row data -> list item widgets
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, list_item| {
                let label = Label::new(None);
                if capture::ItemFields::expanders()[i] {
                    let expander = TreeExpander::new();
                    expander.set_child(Some(&label));
                    list_item.set_child(Some(&expander));
                } else {
                    list_item.set_child(Some(&label));
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

                let item_fields = row.get_fields().unwrap();
                let text = item_fields.fields()[i];
                if capture::ItemFields::expanders()[i] {
                    let expander = list_item
                        .child()
                        .expect("The child has to exist")
                        .downcast::<TreeExpander>()
                        .expect("The child must be a TreeExpander.");

                    let label = expander
                        .child()
                        .expect("The child has to exist")
                        .downcast::<Label>()
                        .expect("The child must be a Label.");
                    label.set_label(text);
                    expander.set_list_row(Some(&treelistrow));
                } else {
                    let label = list_item
                        .child()
                        .expect("The child has to exist")
                        .downcast::<Label>()
                        .expect("The child must be a Label.");
                    label.set_label(text);
                }

            });
            let summary_column = ColumnViewColumn::builder()
                .title(capture::ItemFields::field_names()[i])
                .factory(&factory)
                // NB: this sets whether the column should expand to fill the window width.
                // It is not related to the TreeExpander, but it's appropriate to make the "expander" field wide anyway.
                .expand(capture::ItemFields::expanders()[i])
                .build();
            column_view.append_column(&summary_column.clone());
        }

        let scrolled_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic) // Disable horizontal scrolling
            .min_content_height(480)
            .min_content_width(360)
            .build();

        scrolled_window.set_child(Some(&column_view));
        window.set_child(Some(&scrolled_window));
        window.show();
    });
    application.run_with_args::<&str>(&[]);
}
