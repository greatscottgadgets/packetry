#[macro_use]
extern crate bitfield;

mod model;
pub mod row_data;

use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::collections::HashMap;

use glib::signal::SignalHandlerId;
use gtk::gio::ListModel;
use gtk::{
    prelude::*,
    Label,
    Expander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
    Orientation,
};
use row_data::RowData;

mod capture;
use capture::Capture;

mod file_vec;
mod hybrid_index;

struct ExpanderData {
    expander: Expander,
    handler: Option<SignalHandlerId>,
}

struct Expanders {
    data: RefCell<Vec<Box<ExpanderData>>>,
    lookup: RefCell<HashMap<*const Expander, usize>>,
}

thread_local!(
    static EXPANDERS: Expanders = Expanders {
        data: RefCell::new(Vec::new()),
        lookup: RefCell::new(HashMap::new()),
    };
);

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

        // Create factory for binding row data -> list item widgets
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, list_item| {
            let container = gtk::Box::new(Orientation::Horizontal, 5);
            let conn_label = Label::new(None);
            let text_label = Label::new(None);
            container.append(&conn_label);
            EXPANDERS.with(|expanders| {
                let index = {
                    let mut data = expanders.data.borrow_mut();
                    data.push(Box::new(ExpanderData {
                        expander: Expander::new(None),
                        handler: None
                    }));
                    data.len() - 1
                };
                expanders.lookup.borrow_mut().insert(
                    &expanders.data.borrow()[index].expander as *const Expander,
                    index
                );
                container.append(&expanders.data.borrow()[index].expander);
            });
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
            expander.set_expanded(treelistrow.is_expanded());
            let handler = expander.connect_expanded_notify(move |expander| {
                treelistrow.set_expanded(expander.is_expanded());
            });
            EXPANDERS.with(|expanders| {
                let lookup = expanders.lookup.borrow();
                let mut data = expanders.data.borrow_mut();
                match lookup.get(&(&expander as *const Expander)) {
                    Some(&index) => { data[index].handler = Some(handler) },
                    None => {}
                }
            });
        });
        factory.connect_unbind(move |_, list_item| {
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

            EXPANDERS.with(|expanders| {
                let lookup = expanders.lookup.borrow();
                let mut data = expanders.data.borrow_mut();
                match lookup.get(&(&expander as *const Expander)) {
                    Some(&index) => match data[index].handler.take() {
                        Some(handler) => {
                            expander.disconnect(handler);
                        }
                        None => {}
                    },
                    None => {}
                }
            });
        });

        // Finally, create a view around the model/factory
        let listview = gtk::ListView::new(Some(&selection_model), Some(&factory));

        let scrolled_window = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic) // Disable horizontal scrolling
            .min_content_height(480)
            .min_content_width(360)
            .build();

        scrolled_window.set_child(Some(&listview));
        window.set_child(Some(&scrolled_window));
        window.show();
    });
    application.run_with_args::<&str>(&[]);
}
