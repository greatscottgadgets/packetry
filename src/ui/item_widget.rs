//! GObject subclass for the widget we use to display an item.
//!
//! Wraps a GTK box which contains further widgets.

use std::cell::RefMut;
use gtk::{
    self,
    prelude::*,
    subclass::prelude::*,
    gdk::Rectangle,
    glib::{self, SignalHandlerId, clone},
    pango::EllipsizeMode,
    EventSequenceState,
    Expander,
    GestureClick,
    Label,
    Orientation,
    PopoverMenu,
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
        let right_click = GestureClick::new();
        right_click.set_button(3);
        right_click.connect_released(clone!(@strong wrapper =>
            move |gesture, _n, x, y| {
                if let Some(context_menu_fn) = wrapper
                    .imp()
                    .context_menu_fn
                    .borrow_mut()
                    .as_mut()
                {
                    if let Some(new_popover) = context_menu_fn() {
                        gesture.set_state(EventSequenceState::Claimed);
                        let mut current_popover =
                            wrapper.imp().popover.borrow_mut();
                        if let Some(old_popover) = current_popover.take() {
                            old_popover.unparent();
                        }
                        new_popover.set_parent(&wrapper.clone());
                        new_popover.set_pointing_to(
                            Some(&Rectangle::new(x as i32, y as i32, 1, 1))
                        );
                        new_popover.popup();
                        current_popover.replace(new_popover);
                    }
                }
            }
        ));
        wrapper.add_controller(right_click);
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
        self.imp().handler.take()
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

    /// Set the function to build the context menu for this widget.
    pub fn set_context_menu_fn<F>(&self, context_menu_fn: F)
        where F: FnMut() -> Option<PopoverMenu> + 'static
    {
        self.imp().context_menu_fn.replace(Some(Box::new(context_menu_fn)));
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
        prelude::*,
        subclass::prelude::*,
        glib::{self, SignalHandlerId},
        Expander,
        Label,
        PopoverMenu,
    };
    use std::cell::RefCell;

    type PopoverFn = dyn FnMut() -> Option<PopoverMenu>;

    /// The inner type to be used in the GObject type system.
    #[derive(Default)]
    pub struct ItemWidget {
        pub text_label: RefCell<Label>,
        pub conn_label: RefCell<Label>,
        pub expander: RefCell<Expander>,
        pub handler: RefCell<Option<SignalHandlerId>>,
        pub context_menu_fn: RefCell<Option<Box<PopoverFn>>>,
        pub popover: RefCell<Option<PopoverMenu>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemWidget {
        const NAME: &'static str = "ItemWidget";
        type Type = super::ItemWidget;
        type ParentType = gtk::Box;
    }

    impl BoxImpl for ItemWidget {}
    impl WidgetImpl for ItemWidget {}

    impl ObjectImpl for ItemWidget {
        fn dispose(&self) {
            if let Some(popover) = self.popover.borrow().as_ref() {
                popover.unparent();
            }
        }
    }
}
