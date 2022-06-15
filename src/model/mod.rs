//! Defines our custom model

mod imp;

use std::sync::{Arc, Mutex};

use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::capture::{self, Capture};

// Public part of the Model type.
glib::wrapper! {
    pub struct TrafficModel(ObjectSubclass<imp::TrafficModel>) @implements gio::ListModel;
}
glib::wrapper! {
    pub struct DeviceModel(ObjectSubclass<imp::DeviceModel>) @implements gio::ListModel;
}

pub trait GenericModel<Item> {
    fn new(capture: Arc<Mutex<Capture>>, parent: Option<Item>) -> Self;
    fn set_capture(&mut self, capture: Arc<Mutex<Capture>>);
    fn set_parent(&mut self, parent: Option<Item>);
}

// Constructor for new instances. This simply calls glib::Object::new()
impl GenericModel<capture::TrafficItem> for TrafficModel {
    fn new(capture: Arc<Mutex<Capture>>, parent: Option<capture::TrafficItem>) -> Self {
        let mut model: TrafficModel =
            glib::Object::new(&[]).expect("Failed to create TrafficModel");
        model.set_capture(capture);
        model.set_parent(parent);
        model
    }

    fn set_capture(&mut self, capture: Arc<Mutex<Capture>>) {
        self.imp().capture.replace(Some(capture));
    }

    fn set_parent(&mut self, parent: Option<capture::TrafficItem>) {
        self.imp().parent.replace(parent);
    }
}

impl GenericModel<capture::DeviceItem> for DeviceModel {
    fn new(capture: Arc<Mutex<Capture>>, parent: Option<capture::DeviceItem>)
        -> DeviceModel
    {
        let mut model: DeviceModel =
            glib::Object::new(&[]).expect("Failed to create DeviceModel");
        model.set_capture(capture);
        model.set_parent(parent);
        model
    }

    fn set_capture(&mut self, capture: Arc<Mutex<Capture>>) {
        self.imp().capture.replace(Some(capture));
    }

    fn set_parent(&mut self, parent: Option<capture::DeviceItem>) {
        self.imp().parent.replace(parent);
    }
}
