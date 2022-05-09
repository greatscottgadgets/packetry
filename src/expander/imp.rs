use gtk::{
    self,
    subclass::prelude::*,
    glib::{self, SignalHandlerId},
    Expander,
    Label,
};
use std::cell::RefCell;

unsafe impl IsSubclassable<ExpanderWrapper> for Expander {}

#[derive(Default)]
pub struct ExpanderWrapper {
    pub text_label: RefCell<Label>,
    pub conn_label: RefCell<Label>,
    pub expander: RefCell<Expander>,
    pub handler: RefCell<Option<SignalHandlerId>>,
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
