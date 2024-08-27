//! GObject subclass for our custom widget.

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
    /// The outer type exposed to our Rust code.
    pub struct ItemWidget(ObjectSubclass<imp::ItemWidget>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Orientable;
}

impl ItemWidget {
    /// Create a new widget.
    pub fn new() -> ItemWidget {
        let wrapper: ItemWidget = glib::Object::new::<ItemWidget>();
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

    /// Fetch the Expander from the widget.
    pub fn expander(&self) -> RefMut<Expander> {
        self.imp().expander.borrow_mut()
    }

    /// Store a signal handler to be retained by this widget.
    pub fn set_handler(&self, handler: SignalHandlerId) {
        self.imp().handler.replace(Some(handler));
    }

    /// Take the signal handler retained by this widget.
    pub fn take_handler(&self) -> Option<SignalHandlerId> {
        self.imp().handler.take().take()
    }

    /// Set the summary text on this widget.
    pub fn set_text(&self, text: String) {
        self.imp().text_label.borrow_mut().set_text(&text);
    }

    /// Set the connecting lines on this widget.
    pub fn set_connectors(&self, connectors: String) {
        self.imp().conn_label.borrow_mut().set_markup(
                format!("<tt>{connectors}</tt>").as_str());
    }
}

impl Default for ItemWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// The internal implementation module.
mod imp {
    use gtk::{
        self,
        subclass::prelude::*,
        glib::{self, SignalHandlerId},
        Expander,
        Label,
    };
    use std::cell::RefCell;

    /// The inner type to be used in the GObject type system.
    #[derive(Default)]
    pub struct ItemWidget {
        pub text_label: RefCell<Label>,
        pub conn_label: RefCell<Label>,
        pub expander: RefCell<Expander>,
        pub handler: RefCell<Option<SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemWidget {
        const NAME: &'static str = "ItemWidget";
        type Type = super::ItemWidget;
        type ParentType = gtk::Box;
    }

    impl BoxImpl for ItemWidget {}
    impl WidgetImpl for ItemWidget {}
    impl ObjectImpl for ItemWidget {}
}
