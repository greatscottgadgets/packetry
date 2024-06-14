//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use gtk::glib;
use gtk::subclass::prelude::*;

#[cfg(any(test, feature="record-ui-test"))]
use gtk::prelude::Cast;

use crate::capture::{TrafficItem, DeviceItem};
use crate::tree_list_model::ItemNodeRc;

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct TrafficRowData(ObjectSubclass<imp::TrafficRowData>);
}
glib::wrapper! {
    pub struct DeviceRowData(ObjectSubclass<imp::DeviceRowData>);
}

pub trait GenericRowData<Item> where Item: Copy {
    fn new(node: Result<ItemNodeRc<Item>, String>) -> Self where Self: Sized;
    fn node(&self) -> Result<ItemNodeRc<Item>, String>;
}

impl GenericRowData<TrafficItem> for TrafficRowData {
    fn new(node: Result<ItemNodeRc<TrafficItem>, String>) -> TrafficRowData {
        let row: TrafficRowData = glib::Object::new::<TrafficRowData>();
        row.imp().node.replace(Some(node));
        row
    }

    fn node(&self) -> Result<ItemNodeRc<TrafficItem>, String> {
        self.imp().node.borrow().as_ref().unwrap().clone()
    }
}

impl GenericRowData<DeviceItem> for DeviceRowData {
    fn new(node: Result<ItemNodeRc<DeviceItem>, String>) -> DeviceRowData {
        let row: DeviceRowData = glib::Object::new::<DeviceRowData>();
        row.imp().node.replace(Some(node));
        row
    }

    fn node(&self) -> Result<ItemNodeRc<DeviceItem>, String> {
        self.imp().node.borrow().as_ref().unwrap().clone()
    }
}

pub trait ToGenericRowData<Item> {
    #[cfg(any(test, feature="record-ui-test"))]
    fn to_generic_row_data(self) -> Box<dyn GenericRowData<Item>>;
}

impl ToGenericRowData<TrafficItem> for glib::Object {
    #[cfg(any(test, feature="record-ui-test"))]
    fn to_generic_row_data(self) -> Box<dyn GenericRowData<TrafficItem>> {
        Box::new(self.downcast::<TrafficRowData>().unwrap())
    }
}

impl ToGenericRowData<DeviceItem> for glib::Object {
    #[cfg(any(test, feature="record-ui-test"))]
    fn to_generic_row_data(self) -> Box<dyn GenericRowData<DeviceItem>> {
        Box::new(self.downcast::<DeviceRowData>().unwrap())
    }
}
