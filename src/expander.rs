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

mod imp {
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
}
