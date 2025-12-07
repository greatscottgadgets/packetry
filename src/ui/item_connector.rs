use gtk::{
    self,
    subclass::prelude::*,
    prelude::WidgetExt,
    glib::{self},
};

glib::wrapper! {
    /// The outer type exposed to our Rust code.
    pub struct ItemConnector(ObjectSubclass<imp::ItemConnector>)
    @extends gtk::Widget;
}

#[repr(u8)]
pub enum Shape {
     Unk,
     Space,
     Dot,  // ○
     Box,  // □
     HBar, // ─
     VBar, // │
     XBar, // ┼
     TBar, // ├
     LBar, // └
}

pub fn parse_connectors_to_shapes(s: String) -> Vec<Shape> {
    s.chars().map(|x| match x {
        ' ' => Shape::Space,
        '○' => Shape::Dot,
        '□' => Shape::Box,
        '─' => Shape::HBar,
        '│' => Shape::VBar,
        '┼' => Shape::XBar,
        '├' => Shape::TBar,
        '└' => Shape::LBar,
         _  => Shape::Unk
        }).collect::<Vec<Shape>>()
}

impl ItemConnector {

    /// Create a new widget.
    pub fn new(shapes: Option<Vec<Shape>>) -> ItemConnector {
        let mut wrapper: ItemConnector = glib::Object::new::<ItemConnector>();
        match shapes {
            None => {},
            Some(s) => wrapper.set_shapes(s),
        }
        wrapper
    }

    pub fn initialize(&self) {
        self.set_hexpand(true);
        self.set_vexpand(false);
    }

    pub fn set_shapes(&mut self, shapes: Vec<Shape>) {
        let old_len = self.imp().shape_list.borrow().len();
        let new_len = shapes.len();
        self.imp().shape_list.replace(shapes);
        if old_len != new_len {
            self.queue_resize();
        }
        self.queue_draw();
    }
}

impl Default for ItemConnector {
    fn default() -> Self {
        Self::new(None)
    }
}

/// The internal implementation module.
mod imp {
    use gtk::{
        self,
        prelude::*,
        subclass::prelude::*,
        gdk::RGBA,
        graphene::Rect,
        gsk::RoundedRect,
        Snapshot,
        Orientation,
        SizeRequestMode,
        SizeRequestMode::*,
        glib::{self},
    };
    use crate::ui::item_connector::Shape;
    use std::cell::RefCell;
    use std::ops::Deref;

    /// The inner type to be used in the GObject type system.
    #[derive(Default)]
    pub struct ItemConnector {
        pub shape_list: RefCell<Vec<Shape>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemConnector {
        const NAME: &'static str = "ItemConnector";
        type Type = super::ItemConnector;
        type ParentType = gtk::Widget;
    }

    impl WidgetImpl for ItemConnector {
        fn request_mode(&self) -> SizeRequestMode {
            WidthForHeight
        }
        fn measure(&self, orientation: Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                Orientation::Horizontal => {
                    let len = self.shape_list.borrow().deref().len() as i32;
                    // the horizontal size depend on the allocated height
                    let size :i32 = (for_size / 2) * len;
                    (size, size, -1, -1)
                },
                _ => (-1, -1, -1, -1)
            }
        }
        fn snapshot(&self, snapshot: &Snapshot) {
            let widget = self.obj();
            let h = widget.height() as f32;
            let h2 = (h / 2.0).trunc();
            let cw = (h / 2.0).trunc(); // char-cell width
            let c2 = (cw / 2.0).trunc();
            let sw = 1.0; // stroke width
            let col = widget.style_context().color(); // NOTE: deprecated in 4.10, to be replace with widget.color()
            let mut x: f32 = 0.0;
            for s in self.shape_list.borrow().deref() {
                match s {
                    Shape::Space => {},
                    Shape::Dot => {
                        snapshot.append_border(
                            &RoundedRect::from_rect(Rect::new(x, c2+sw, cw, cw), c2),
                            &[sw, sw, sw, sw], &[col, col, col, col]);
                    },
                    Shape::Box => {
                        snapshot.append_border(
                            &RoundedRect::from_rect(Rect::new(x, c2+sw, cw, cw), 0.0),
                            &[sw, sw, sw, sw], &[col, col, col, col]);
                    },
                    Shape::HBar => {
                        snapshot.append_color(&col, &Rect::new(x, h2, cw, sw));
                    },
                    Shape::VBar => {
                        snapshot.append_color(&col, &Rect::new(x + c2, 0.0, sw, h));
                    },
                    Shape::XBar => {
                        snapshot.append_color(&col, &Rect::new(x, h2, cw, sw));
                        snapshot.append_color(&col, &Rect::new(x + c2, 0.0, sw, h));
                    },
                    Shape::LBar => {
                        snapshot.append_color(&col, &Rect::new(x + c2, h2, cw - c2, sw));
                        snapshot.append_color(&col, &Rect::new(x + c2, 0.0, sw, h2));
                    },
                    Shape::TBar => {
                        snapshot.append_color(&col, &Rect::new(x + c2, h2, cw - c2, sw));
                        snapshot.append_color(&col, &Rect::new(x + c2, 0.0, sw, h));
                    },
                    _ => {snapshot.append_color(&RGBA::RED, &Rect::new(x, 0.0, cw, h));}
                }
                x += cw;
            }
        }
    }

    impl ObjectImpl for ItemConnector {}
}
