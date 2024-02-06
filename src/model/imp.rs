//! Defines the implementation of our model

use gio::subclass::prelude::*;
use gtk::{gio, glib, prelude::*};

use std::cell::RefCell;
use crate::capture::{TrafficItem, DeviceItem};
use crate::row_data::{TrafficRowData, DeviceRowData};
use crate::tree_list_model::TreeListModel;

#[derive(Default)]
pub struct TrafficModel {
    pub(super) tree: RefCell<Option<TreeListModel<TrafficItem, super::TrafficModel, TrafficRowData>>>,
}

#[derive(Default)]
pub struct DeviceModel {
    pub(super) tree: RefCell<Option<TreeListModel<DeviceItem, super::DeviceModel, DeviceRowData>>>,
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
    fn item_type(&self) -> glib::Type {
        TrafficRowData::static_type()
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

impl ListModelImpl for DeviceModel {
    fn item_type(&self) -> glib::Type {
        DeviceRowData::static_type()
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
