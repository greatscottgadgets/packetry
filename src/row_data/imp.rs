use glib::subclass::prelude::*;
use gtk::{
    glib::{self, ParamSpec, Value},
};
use std::cell::RefCell;
use std::rc::Rc;
use crate::capture;

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
#[derive(Default)]
pub struct RowData {
    pub(super) item: RefCell<Option<capture::Item>>,
    pub(super) fields: RefCell<Option<Rc<capture::ItemFields>>>,
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for RowData {
    const NAME: &'static str = "RowData";
    type Type = super::RowData;
}

// The ObjectImpl trait provides the setters/getters for GObject properties.
// Here we need to provide the values that are internally stored back to the
// caller, or store whatever new value the caller is providing.
//
// This maps between the GObject properties and our internal storage of the
// corresponding values of the properties.
impl ObjectImpl for RowData {
    fn properties() -> &'static [ParamSpec] {
        use once_cell::sync::Lazy;
        static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| vec![]);
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _obj: &Self::Type, _id: usize, _value: &Value, _pspec: &ParamSpec) {
        unimplemented!()
    }

    fn property(&self, _obj: &Self::Type, _id: usize, _pspec: &ParamSpec) -> Value {
        unimplemented!()
    }
}
