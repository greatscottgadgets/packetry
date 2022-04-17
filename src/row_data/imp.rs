use glib::subclass::prelude::*;
use gtk::{
    glib::{self, ParamSpec, Value},
};
use std::cell::RefCell;
use std::collections::HashMap;
use crate::capture;

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
pub struct RowData<Item> {
    values: RefCell<HashMap<&'static str, Value>>,
    pub(super) item: RefCell<Option<Item>>,
}

impl<Item> Default for RowData<Item> {
    fn default() -> Self {
        RowData::<Item> {
            values: RefCell::new(HashMap::new()),
            item: RefCell::new(None),
        }
    }
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for RowData<capture::Item> {
    const NAME: &'static str = "RowData";
    type Type = super::RowData;
}
#[glib::object_subclass]
impl ObjectSubclass for RowData<capture::DeviceItem> {
    const NAME: &'static str = "DeviceRowData";
    type Type = super::DeviceRowData;
}

pub trait Properties {
    fn get_properties() -> &'static [ParamSpec];
}

impl Properties for RowData<capture::Item> {
    fn get_properties() -> &'static [ParamSpec] {
        use once_cell::sync::Lazy;
        static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::new(
                    "text",
                    "Text",
                    "Text",
                    None, // Default value
                    glib::ParamFlags::READWRITE,
                ),
                glib::ParamSpecString::new(
                    "conn",
                    "Connectors",
                    "Connectors",
                    None, // Default value
                    glib::ParamFlags::READWRITE,
                ),
            ]
        });
        PROPERTIES.as_ref()
    }
}

impl Properties for RowData<capture::DeviceItem> {
    fn get_properties() -> &'static [ParamSpec] {
        use once_cell::sync::Lazy;
        static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::new(
                    "text",
                    "Text",
                    "Text",
                    None, // Default value
                    glib::ParamFlags::READWRITE,
                ),
            ]
        });
        PROPERTIES.as_ref()
    }
}

// The ObjectImpl trait provides the setters/getters for GObject properties.
// Here we need to provide the values that are internally stored back to the
// caller, or store whatever new value the caller is providing.
//
// This maps between the GObject properties and our internal storage of the
// corresponding values of the properties.
impl<Item> ObjectImpl for RowData<Item>
    where RowData<Item>: ObjectSubclass + Properties
{
    fn properties() -> &'static [ParamSpec] {
        RowData::<Item>::get_properties()
    }

    fn set_property(&self, _obj: &Self::Type, _id: usize, value: &Value, pspec: &ParamSpec) {
        self.values.borrow_mut().insert(pspec.name(), value.clone());
    }

    fn property(&self, _obj: &Self::Type, _id: usize, pspec: &ParamSpec) -> Value {
        match self.values.borrow().get(pspec.name()) {
            Some(value) => value.clone(),
            None => panic!("Property was not set")
        }
    }
}
