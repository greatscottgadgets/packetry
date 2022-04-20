use gtk::{
    self,
    subclass::prelude::*,
    glib,
    Expander,
};
use std::cell::RefCell;

unsafe impl IsSubclassable<ExpanderWrapper> for Expander {}

#[derive(Default)]
pub struct ExpanderWrapper {
    pub expander: RefCell<Expander>,
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for ExpanderWrapper {
    const NAME: &'static str = "ExpanderWrapper";
    type Type = super::ExpanderWrapper;
    type ParentType = gtk::Box;
}

impl BoxImpl for ExpanderWrapper {}
impl WidgetImpl for ExpanderWrapper {}
impl ObjectImpl for ExpanderWrapper {}
