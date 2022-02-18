mod model;
pub mod row_data;

use gtk::{
    prelude::*,
    gio::ListModel,
    Label,
    TreeExpander,
    TreeListModel,
    TreeListRow,
    SignalListItemFactory,
    SingleSelection,
};
use row_data::RowData;

fn main() {
    let application = gtk::Application::new(
        Some("com.greatscottgadgets.luna-analyzer-rust"),
        Default::default(),
    );

    application.connect_activate(build_ui);
    application.run();
}

fn build_ui(application: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .default_width(320)
        .default_height(480)
        .application(application)
        .title("Custom Model")
        .build();

    // Create the model and fill with a row or two
    let model = model::Model::new();
    let mut data = Vec::new();
    for i in 0..10_000_000 {
        data.push(format!("test {}", i));
    }
    model.append(&mut data);

    let treemodel = TreeListModel::new(&model, false, false, |o| {
        if !o.property::<String>("name").starts_with("test") {
            return None
        }
        let model = model::Model::new();
        let mut data = Vec::new();
        for i in 0..10 {
            data.push(format!("child {}", i));
        }
        model.append(&mut data);
        Some(model.upcast::<ListModel>())
    });
    let selection_model = SingleSelection::new(Some(&treemodel));

    // Create factory for binding row data -> list item widgets
    let factory = SignalListItemFactory::new();
    factory.connect_setup(move |_, list_item| {
        let label = Label::new(None);
        let expander = TreeExpander::new();
        expander.set_child(Some(&label));
        list_item.set_child(Some(&expander));
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

        let text = row.property::<String>("name");
        label.set_label(&text);
        expander.set_list_row(Some(&treelistrow));
    });

    // Finally, create a view around the model/factory
    let listview = gtk::ListView::new(Some(&selection_model), Some(&factory));

    let scrolled_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never) // Disable horizontal scrolling
        .min_content_height(480)
        .min_content_width(360)
        .build();

    scrolled_window.set_child(Some(&listview));
    window.set_child(Some(&scrolled_window));
    window.show();
}
