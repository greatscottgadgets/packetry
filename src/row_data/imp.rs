use gtk::glib::{self, subclass::prelude::*};
use std::cell::RefCell;
use crate::capture;

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
#[derive(Default)]
pub struct RowData {
    pub summary: RefCell<String>,
    pub connectors: RefCell<String>,
    pub(super) item: RefCell<Option<capture::Item>>,
}

#[derive(Default)]
pub struct DeviceRowData {
    pub summary: RefCell<String>,
    pub(super) item: RefCell<Option<capture::DeviceItem>>,
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for RowData {
    const NAME: &'static str = "RowData";
    type Type = super::RowData;
}

#[glib::object_subclass]
impl ObjectSubclass for DeviceRowData {
    const NAME: &'static str = "DeviceRowData";
    type Type = super::DeviceRowData;
}

impl ObjectImpl for RowData {}
impl ObjectImpl for DeviceRowData {}
