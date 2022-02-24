//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use crate::Capture;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use crate::row_data::RowData;

#[derive(Default)]
pub struct Model(pub(super) RefCell<Arc<Mutex<Capture>>>);

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
        self.0.borrow().lock().unwrap().packet_count() as u32
    }
    fn item(&self, _list_model: &Self::Type, position: u32) -> Option<glib::Object> {
        let arc = self.0.borrow();
        let mut cap = arc.lock().unwrap();
        let packet = cap.get_packet(position as u64);
        let data = cap.get_packet_data(packet.data_start..packet.data_end);
        Some(RowData::new(&format!("{}\t{:02X?}", packet, data)).upcast::<glib::Object>())
    }
}
