//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use gtk::glib;
use gtk::subclass::prelude::*;
use crate::capture;

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct RowData(ObjectSubclass<imp::RowData>);
}

// Constructor for new instances. This simply calls glib::Object::new() with
// initial values for our two properties and then returns the new instance
impl RowData {
    pub fn new(item: Option<capture::Item>, text: &str, conn: &str) -> RowData {
        let mut row: RowData = glib::Object::new(
            &[
                ("text", &text),
                ("conn", &conn),
            ]).expect("Failed to create row data");
        row.set_item(item);
        row
    }

    fn set_item(&mut self, item: Option<capture::Item>) {
        self.imp().item.replace(item);
    }

    pub fn get_item(&self) -> Option<capture::Item> {
        self.imp().item.borrow().clone()
    }
}
