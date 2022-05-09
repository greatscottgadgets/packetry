//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use crate::capture::{self, Capture};

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use crate::row_data::{RowData, DeviceRowData};

#[derive(Default)]
pub struct Model {
    pub(super) capture: RefCell<Arc<Mutex<Capture>>>,
    pub(super) parent: RefCell<Option<capture::Item>>,
}

#[derive(Default)]
pub struct DeviceModel {
    pub(super) capture: RefCell<Arc<Mutex<Capture>>>,
    pub(super) parent: RefCell<Option<capture::DeviceItem>>,
}

/// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for Model {
    const NAME: &'static str = "Model";
    type Type = super::Model;
    type Interfaces = (gio::ListModel,);
}
#[glib::object_subclass]
impl ObjectSubclass for DeviceModel {
    const NAME: &'static str = "DeviceModel";
    type Type = super::DeviceModel;
    type Interfaces = (gio::ListModel,);

}

impl ObjectImpl for Model {}
impl ObjectImpl for DeviceModel {}

impl ListModelImpl for Model {
    fn item_type(&self, _list_model: &Self::Type) -> glib::Type {
        RowData::static_type()
    }
    fn n_items(&self, _list_model: &Self::Type) -> u32 {
        self.capture.borrow().lock().unwrap().item_count(&self.parent.borrow()) as u32
    }
    fn item(&self, _list_model: &Self::Type, position: u32) -> Option<glib::Object> {
        let arc = self.capture.borrow();
        let mut cap = arc.lock().unwrap();
        let item = cap.get_item(&self.parent.borrow(), position as u64);
        let summary = cap.get_summary(&item);
        let connectors = cap.get_connectors(&item);
        Some(RowData::new(Some(item), summary, connectors).upcast::<glib::Object>())
    }
}

impl ListModelImpl for DeviceModel {
    fn item_type(&self, _list_model: &Self::Type) -> glib::Type {
        DeviceRowData::static_type()
    }
    fn n_items(&self, _list_model: &Self::Type) -> u32 {
        self.capture.borrow().lock().unwrap().device_item_count(&self.parent.borrow()) as u32
    }
    fn item(&self, _list_model: &Self::Type, position: u32) -> Option<glib::Object> {
        let arc = self.capture.borrow();
        let mut cap = arc.lock().unwrap();
        let item = cap.get_device_item(&self.parent.borrow(), position as u64);
        let summary = cap.get_device_summary(&item);
        Some(DeviceRowData::new(Some(item), summary).upcast::<glib::Object>())
    }
}
