//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use crate::capture::{self, Capture};

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use crate::row_data::{RowData, GenericRowData};

#[derive(Default)]
pub struct Model {
    pub(super) capture: RefCell<Arc<Mutex<Capture>>>,
    pub(super) parent: RefCell<Option<capture::Item>>,
}

/// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for Model {
    const NAME: &'static str = "Model";
    type Type = super::Model;
    type Interfaces = (gio::ListModel,);

}

impl ObjectImpl for Model {}

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
        Some(RowData::new(Some(item), &summary, &connectors).upcast::<glib::Object>())
    }
}
