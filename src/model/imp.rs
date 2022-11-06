//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use std::cell::RefCell;
use crate::capture::{TrafficItem, DeviceItem};
use crate::row_data::{GenericRowData, TrafficRowData, DeviceRowData};
use crate::tree_list_model::TreeListModel;

use super::{clamp, MAX_ROWS};

#[derive(Default)]
pub struct TrafficModel {
    pub(super) tree: RefCell<Option<TreeListModel<TrafficItem>>>,
}

#[derive(Default)]
pub struct DeviceModel {
    pub(super) tree: RefCell<Option<TreeListModel<DeviceItem>>>,
}

/// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for TrafficModel {
    const NAME: &'static str = "TrafficModel";
    type Type = super::TrafficModel;
    type Interfaces = (gio::ListModel,);
}
#[glib::object_subclass]
impl ObjectSubclass for DeviceModel {
    const NAME: &'static str = "DeviceModel";
    type Type = super::DeviceModel;
    type Interfaces = (gio::ListModel,);
}

impl ObjectImpl for TrafficModel {}
impl ObjectImpl for DeviceModel {}

impl ListModelImpl for TrafficModel {
    fn item_type(&self, _list_model: &Self::Type) -> glib::Type {
        TrafficRowData::static_type()
    }

    fn n_items(&self, _list_model: &Self::Type) -> u32 {
        match self.tree.borrow().as_ref() {
            Some(tree) => clamp(tree.row_count(), MAX_ROWS),
            None => 0
        }
    }

    fn item(&self, _list_model: &Self::Type, position: u32)
        -> Option<glib::Object>
    {
        match self.tree.borrow().as_ref() {
            Some(tree) => {
                if position >= clamp(tree.row_count(), MAX_ROWS) {
                    None
                } else {
                    let result = tree.fetch(position as u64)
                        .map_err(|e| format!("{:?}", e));
                    Some(TrafficRowData::new(result).upcast::<glib::Object>())
                }
            }
            None => None
        }
    }
}

impl ListModelImpl for DeviceModel {
    fn item_type(&self, _list_model: &Self::Type) -> glib::Type {
        DeviceRowData::static_type()
    }

    fn n_items(&self, _list_model: &Self::Type) -> u32 {
        match self.tree.borrow().as_ref() {
            Some(tree) => clamp(tree.row_count(), MAX_ROWS),
            None => 0
        }
    }

    fn item(&self, _list_model: &Self::Type, position: u32)
        -> Option<glib::Object>
    {
        match self.tree.borrow().as_ref() {
            Some(tree) => {
                if position >= clamp(tree.row_count(), MAX_ROWS) {
                    None
                } else {
                    let result = tree.fetch(position as u64)
                        .map_err(|e| format!("{:?}", e));
                    Some(DeviceRowData::new(result).upcast::<glib::Object>())
                }
            },
            None => None
        }
    }
}
