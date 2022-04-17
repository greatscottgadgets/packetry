//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use crate::capture;

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct RowData(ObjectSubclass<imp::RowData<capture::Item>>);
}
glib::wrapper! {
    pub struct DeviceRowData(ObjectSubclass<imp::RowData<capture::DeviceItem>>);
}

pub trait GenericRowData<Item> {
    fn new(item: Option<Item>, properties: &[(&str, &dyn ToValue)]) -> Self;
    fn set_item(&mut self, item: Option<Item>);
    fn get_item(&self) -> Option<Item>;
    fn child_count(&self, capture: &mut capture::Capture) -> u64;
}


// Constructor for new instances. This simply calls glib::Object::new() with
// initial values for our two properties and then returns the new instance
impl GenericRowData<capture::Item> for RowData {
    fn new(item: Option<capture::Item>,
           properties: &[(&str, &dyn ToValue)]) -> RowData
    {
        let mut row: RowData =
            glib::Object::new(properties).expect("Failed to create row data");
        row.set_item(item);
        row
    }

    fn set_item(&mut self, item: Option<capture::Item>) {
        self.imp().item.replace(item);
    }

    fn get_item(&self) -> Option<capture::Item> {
        self.imp().item.borrow().clone()
    }

    fn child_count(&self, capture: &mut capture::Capture) -> u64 {
        capture.item_count(&self.imp().item.borrow())
    }
}

impl GenericRowData<capture::DeviceItem> for DeviceRowData {
    fn new(item: Option<capture::DeviceItem>,
           properties: &[(&str, &dyn ToValue)]) -> DeviceRowData
    {
        let mut row: DeviceRowData =
            glib::Object::new(properties).expect("Failed to create row data");
        row.set_item(item);
        row
    }

    fn set_item(&mut self, item: Option<capture::DeviceItem>) {
        self.imp().item.replace(item);
    }

    fn get_item(&self) -> Option<capture::DeviceItem> {
        self.imp().item.borrow().clone()
    }

    fn child_count(&self, capture: &mut capture::Capture) -> u64 {
        capture.device_item_count(&self.imp().item.borrow())
    }
}
