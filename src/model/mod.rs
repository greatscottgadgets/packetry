//! Defines our custom model

mod imp;

use std::sync::{Arc, Mutex};

use gtk::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use crate::Capture;

// Public part of the Model type.
glib::wrapper! {
    pub struct Model(ObjectSubclass<imp::Model>) @implements gio::ListModel;
}

// Constructor for new instances. This simply calls glib::Object::new()
impl Model {
    pub fn new(capture: Arc<Mutex<Capture>>) -> Model {
        let mut model: Model = glib::Object::new(&[]).expect("Failed to create Model");
        model.set_capture(capture);
        model
    }

    fn set_capture(&mut self, capture: Arc<Mutex<Capture>>) {
        self.imp().0.replace(capture);
    }
}
