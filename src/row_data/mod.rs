//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use gtk::glib;
use gtk::subclass::prelude::*;
use crate::capture;
use std::rc::Rc;

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct RowData(ObjectSubclass<imp::RowData>);
}

// Constructor for new instances. This simply calls glib::Object::new() with
// initial values for our two properties and then returns the new instance
impl RowData {
    pub fn new(item: Option<capture::Item>, fields: Option<capture::ItemFields>) -> RowData {
        let mut row: RowData = glib::Object::new(&[]).expect("Failed to create row data");
        row.set_item(item);
        row.set_fields(fields);
        row
    }

    fn set_item(&mut self, item: Option<capture::Item>) {
        self.imp().item.replace(item);
    }

    pub fn get_item(&self) -> Option<capture::Item> {
        self.imp().item.borrow().clone()
    }

    pub fn set_fields(&mut self, fields: Option<capture::ItemFields>) {
        self.imp().fields.replace(
            match fields {
                Some(f) => Some(Rc::new(f)),
                None => None,
            }
        );
    }

    pub fn get_fields(&self) -> Option<Rc<capture::ItemFields>> {
        self.imp().fields.borrow().clone()
    }
}
