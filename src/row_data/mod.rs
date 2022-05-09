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
glib::wrapper! {
    pub struct DeviceRowData(ObjectSubclass<imp::DeviceRowData>);
}

impl RowData {
    pub fn new(item: Option<capture::Item>, summary: String, connectors: String)
        -> RowData
    {
        let mut row: RowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.set_item(item);
        row.set_summary(summary);
        row.set_connectors(connectors);
        row
    }

    fn set_item(&mut self, item: Option<capture::Item>) {
        self.imp().item.replace(item);
    }

    fn set_summary(&mut self, summary: String) {
        self.imp().summary.replace(summary);
    }

    fn set_connectors(&mut self, connectors: String) {
        self.imp().connectors.replace(connectors);
    }
}

impl DeviceRowData {
    pub fn new(item: Option<capture::DeviceItem>, summary: String) -> DeviceRowData {
        let mut row: DeviceRowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.set_item(item);
        row.set_summary(summary);
        row
    }

    fn set_item(&mut self, item: Option<capture::DeviceItem>) {
        self.imp().item.replace(item);
    }

    fn set_summary(&mut self, summary: String) {
        self.imp().summary.replace(summary);
    }
}

pub trait GenericRowData<Item> {
    const CONNECTORS: bool;
    fn get_item(&self) -> Option<Item>;
    fn child_count(&self, capture: &mut capture::Capture) -> u64;
    fn get_summary(&self) -> String;
    fn get_connectors(&self) -> Option<String>;
}

impl GenericRowData<capture::Item> for RowData {
    const CONNECTORS: bool = true;

    fn get_item(&self) -> Option<capture::Item> {
        self.imp().item.borrow().clone()
    }

    fn child_count(&self, capture: &mut capture::Capture) -> u64 {
        capture.item_count(&self.imp().item.borrow())
    }

    fn get_summary(&self) -> String {
        self.imp().summary.borrow().clone()
    }

    fn get_connectors(&self) -> Option<String> {
        Some(self.imp().connectors.borrow().clone())
    }
}

impl GenericRowData<capture::DeviceItem> for DeviceRowData {
    const CONNECTORS: bool = false;

    fn get_item(&self) -> Option<capture::DeviceItem> {
        self.imp().item.borrow().clone()
    }

    fn child_count(&self, capture: &mut capture::Capture) -> u64 {
        capture.device_item_count(&self.imp().item.borrow())
    }

    fn get_summary(&self) -> String {
        self.imp().summary.borrow().clone()
    }

    fn get_connectors(&self) -> Option<String> {
        None
    }
}
