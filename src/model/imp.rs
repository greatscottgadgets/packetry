//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use std::cell::RefCell;
use crate::capture::{TrafficItem, DeviceItem};
use crate::row_data::{TrafficRowData, DeviceRowData};
use crate::tree_list_model::TreeListModel;

macro_rules! model {
    ($model:ident, $item:ident, $row_data:ident) => {
        #[derive(Default)]
        pub struct $model {
            pub(super) tree: RefCell<Option<
                TreeListModel<$item, super::$model, $row_data>>>,
        }

        /// Basic declaration of our type for the GObject type system
        #[glib::object_subclass]
        impl ObjectSubclass for $model {
            const NAME: &'static str = stringify!($model);
            type Type = super::$model;
            type Interfaces = (gio::ListModel,);
        }

        impl ObjectImpl for $model {}

        impl ListModelImpl for $model {
            fn item_type(&self) -> glib::Type {
                $row_data::static_type()
            }

            fn n_items(&self) -> u32 {
                match self.tree.borrow().as_ref() {
                    Some(tree) => tree.n_items(),
                    None => 0
                }
            }

            fn item(&self, position: u32)
                -> Option<glib::Object>
            {
                match self.tree.borrow().as_ref() {
                    Some(tree) => tree.item(position),
                    None => None
                }
            }
        }
    }
}

model!(TrafficModel, TrafficItem, TrafficRowData);
model!(DeviceModel, DeviceItem, DeviceRowData);
