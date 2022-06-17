//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use std::sync::{Arc, Mutex};
use std::ops::DerefMut;

use gtk::glib;
use gtk::subclass::prelude::*;
use crate::capture::{Capture, CaptureError, TrafficItem, DeviceItem};

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct TrafficRowData(ObjectSubclass<imp::TrafficRowData>);
}
glib::wrapper! {
    pub struct DeviceRowData(ObjectSubclass<imp::DeviceRowData>);
}

impl TrafficRowData {
    pub fn new(item: Option<TrafficItem>) -> TrafficRowData {
        let row: TrafficRowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.imp().item.replace(item);
        row
    }
}

impl DeviceRowData {
    pub fn new(item: Option<DeviceItem>) -> DeviceRowData {
        let row: DeviceRowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.imp().item.replace(item);
        row
    }
}

pub trait GenericRowData<Item> {
    const CONNECTORS: bool;
    fn get_item(&self) -> Option<Item>;
    fn field(&self,
             capture: &Arc<Mutex<Capture>>,
             func: Box<dyn
                Fn(&mut Capture, &Item)
                    -> Result<String, CaptureError>>)
        -> String;
}

impl GenericRowData<TrafficItem> for TrafficRowData {
    const CONNECTORS: bool = true;

    fn get_item(&self) -> Option<TrafficItem> {
        self.imp().item.borrow().clone()
    }

    fn field(&self,
             capture: &Arc<Mutex<Capture>>,
             func: Box<dyn
                Fn(&mut Capture, &TrafficItem)
                    -> Result<String, CaptureError>>)
        -> String
    {
        match self.get_item() {
            None => "Error: row has no item".to_string(),
            Some(item) => {
                match capture.lock() {
                    Err(_) => "Error: failed to lock capture".to_string(),
                    Ok(mut guard) => {
                        let cap = guard.deref_mut();
                        match func(cap, &item) {
                            Err(e) => format!("Error: {:?}", e),
                            Ok(string) => string
                        }
                    }
                }
            }
        }
    }
}

impl GenericRowData<DeviceItem> for DeviceRowData {
    const CONNECTORS: bool = false;

    fn get_item(&self) -> Option<DeviceItem> {
        self.imp().item.borrow().clone()
    }

    fn field(&self,
             capture: &Arc<Mutex<Capture>>,
             func: Box<dyn
                Fn(&mut Capture, &DeviceItem)
                    -> Result<String, CaptureError>>)
        -> String
    {
        match self.get_item() {
            None => "Error: row has no item".to_string(),
            Some(item) => {
                match capture.lock() {
                    Err(_) => "Error: failed to lock capture".to_string(),
                    Ok(mut guard) => {
                        let cap = guard.deref_mut();
                        match func(cap, &item) {
                            Err(e) => format!("Error: {:?}", e),
                            Ok(string) => string
                        }
                    }
                }
            }
        }
    }
}
