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
    pub fn new() -> Model {
        glib::Object::new(&[]).expect("Failed to create Model")
    }

    pub fn set_capture(&mut self, capture: Arc<Mutex<Capture>>) {
        let removed = self.imp().0.borrow().lock().unwrap().packet_count();
        let added = capture.lock().unwrap().packet_count();
        self.imp().0.replace(capture);
        self.items_changed(0, removed as u32, added as u32);
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}
