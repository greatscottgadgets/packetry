//! Defines our custom model

mod imp;

use std::sync::{Arc, Mutex};

use gtk::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use crate::capture::{self, Capture};
use crate::row_data::RowData;

// Public part of the Model type.
glib::wrapper! {
    pub struct Model(ObjectSubclass<imp::Model>) @implements gio::ListModel;
}

// Constructor for new instances. This simply calls glib::Object::new()
impl Model {
    pub fn new(capture: Arc<Mutex<Capture>>, parent: Option<capture::Item>) -> Model {
        let mut model: Model = glib::Object::new(&[]).expect("Failed to create Model");
        model.set_capture(capture);
        model.set_parent(parent);
        model
    }

    fn set_capture(&mut self, capture: Arc<Mutex<Capture>>) {
        self.imp().capture.replace(capture);
    }

    fn set_parent(&mut self, parent: Option<capture::Item>) {
        self.imp().parent.replace(parent);
    }
}
