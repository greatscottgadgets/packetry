mod imp;

use std::cell::RefMut;
use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    glib::{self, SignalHandlerId},
    pango::EllipsizeMode,
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
        let wrapper: ExpanderWrapper = glib::Object::new::<ExpanderWrapper>();
        wrapper.imp().text_label.replace(
            Label::builder()
                .ellipsize(EllipsizeMode::End)
                .build());
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

    pub fn set_text(&self, text: String) {
        self.imp().text_label.borrow_mut().set_text(&text);
    }

    pub fn set_connectors(&self, connectors: String) {
        self.imp().conn_label.borrow_mut().set_markup(
                format!("<tt>{connectors}</tt>").as_str());
    }
}

impl Default for ExpanderWrapper {
    fn default() -> Self {
        Self::new()
    }
}
