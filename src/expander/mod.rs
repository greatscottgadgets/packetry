mod imp;

use std::cell::RefMut;
use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    glib::{self, SignalHandlerId},
    Expander,
    Label,
    Orientation,
};

glib::wrapper! {
    pub struct ExpanderWrapper(ObjectSubclass<imp::ExpanderWrapper>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Orientable;
}

impl ExpanderWrapper {
    pub fn new() -> ExpanderWrapper {
        let wrapper: ExpanderWrapper =
            glib::Object::new(&[])
                         .expect("Failed to create new expander wrapper");
        wrapper.imp().text_label.replace(Label::new(None));
        wrapper.imp().conn_label.replace(Label::new(None));
        wrapper.imp().expander.replace(Expander::new(None));
        wrapper.append(&wrapper.imp().conn_label.borrow().clone());
        wrapper.append(&wrapper.imp().expander.borrow().clone());
        wrapper.append(&wrapper.imp().text_label.borrow().clone());
        wrapper.set_orientation(Orientation::Horizontal);
        wrapper.set_spacing(5);
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

    pub fn set_connectors(&self, connectors: Option<String>) {
        match connectors {
            Some(text) =>
                self.imp().conn_label.borrow_mut().set_markup(
                    format!("<tt>{}</tt>", text).as_str()),
            None => {}
        };
    }
}
