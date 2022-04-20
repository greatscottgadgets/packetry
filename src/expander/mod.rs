mod imp;

use std::cell::RefMut;
use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    glib::{self, SignalHandlerId},
    Expander};

glib::wrapper! {
    pub struct ExpanderWrapper(ObjectSubclass<imp::ExpanderWrapper>)
    @extends gtk::Box, gtk::Widget;
}

impl ExpanderWrapper {
    pub fn new() -> ExpanderWrapper {
        let wrapper: ExpanderWrapper =
            glib::Object::new(&[])
                         .expect("Failed to create new expander wrapper");
        let expander = Expander::new(None);
        expander.set_parent(&wrapper);
        wrapper.imp().expander.replace(expander);
        wrapper
    }

    pub fn expander(&self) -> RefMut<Expander> {
        self.imp().expander.borrow_mut()
    }

    pub fn set_handler(&self, handler: SignalHandlerId) {
        self.imp().handler.replace(Some(handler));
    }

    pub fn take_handler(&self) -> Option<SignalHandlerId> {
        self.imp().handler.take().take()
    }
}
