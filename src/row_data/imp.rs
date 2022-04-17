use glib::subclass::prelude::*;
use gtk::{
    glib::{self, ParamSpec, Value},
    prelude::*,
};
use std::cell::RefCell;
use crate::capture;

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
pub struct RowData<Item> {
    text: RefCell<Option<String>>,
    conn: RefCell<Option<String>>,
    pub(super) item: RefCell<Option<Item>>,
}

impl<Item> Default for RowData<Item> {
    fn default() -> Self {
        RowData::<Item> {
            text: RefCell::new(None),
            conn: RefCell::new(None),
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

// The ObjectImpl trait provides the setters/getters for GObject properties.
// Here we need to provide the values that are internally stored back to the
// caller, or store whatever new value the caller is providing.
//
// This maps between the GObject properties and our internal storage of the
// corresponding values of the properties.
impl<Item> ObjectImpl for RowData<Item> where RowData<Item>: ObjectSubclass {
    fn properties() -> &'static [ParamSpec] {
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

    fn set_property(&self, _obj: &Self::Type, _id: usize, value: &Value, pspec: &ParamSpec) {
        match pspec.name() {
            "text" => {
                let text = value.get().unwrap();
                self.text.replace(text);
            }
            "conn" => {
                let conn = value.get().unwrap();
                self.conn.replace(conn);
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _obj: &Self::Type, _id: usize, pspec: &ParamSpec) -> Value {
        match pspec.name() {
            "text" => self.text.borrow().to_value(),
            "conn" => self.conn.borrow().to_value(),
            _ => unimplemented!(),
        }
    }
}
