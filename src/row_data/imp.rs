use gtk::glib::{self, subclass::prelude::*};
use std::cell::RefCell;

use crate::capture::{TrafficItem, DeviceItem};
use crate::tree_list_model::ItemNodeRc;

macro_rules! row_data {
    ($row_data: ident, $item: ident) => {
        // The actual data structure that stores our values. This is not accessible
        // directly from the outside.
        #[derive(Default)]
        pub struct $row_data {
            pub(super) node: RefCell<Option<
                Result<ItemNodeRc<$item>, String>>>,
        }

        // Basic declaration of our type for the GObject type system
        #[glib::object_subclass]
        impl ObjectSubclass for $row_data {
            const NAME: &'static str = stringify!($row_data);
            type Type = super::$row_data;
        }

        impl ObjectImpl for $row_data {}
    }
}

row_data!(TrafficRowData, TrafficItem);
row_data!(DeviceRowData, DeviceItem);
