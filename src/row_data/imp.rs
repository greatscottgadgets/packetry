use gtk::glib::{self, subclass::prelude::*};
use std::cell::RefCell;
use crate::capture;

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
#[derive(Default)]
pub struct TrafficRowData {
    pub summary: RefCell<String>,
    pub connectors: RefCell<String>,
    pub(super) item: RefCell<Option<capture::TrafficItem>>,
}

#[derive(Default)]
pub struct DeviceRowData {
    pub summary: RefCell<String>,
    pub(super) item: RefCell<Option<capture::DeviceItem>>,
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for TrafficRowData {
    const NAME: &'static str = "TrafficRowData";
    type Type = super::TrafficRowData;
}

#[glib::object_subclass]
impl ObjectSubclass for DeviceRowData {
    const NAME: &'static str = "DeviceRowData";
    type Type = super::DeviceRowData;
}

impl ObjectImpl for TrafficRowData {}
impl ObjectImpl for DeviceRowData {}
