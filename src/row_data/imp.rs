use gtk::glib::{self, subclass::prelude::*};
use std::rc::Rc;
use std::cell::RefCell;

use crate::capture::{TrafficItem, DeviceItem};
use crate::tree_list_model::{TreeNode};

// The actual data structure that stores our values. This is not accessible
// directly from the outside.
#[derive(Default)]
pub struct TrafficRowData {
    pub(super) node: RefCell<Option<Rc<RefCell<TreeNode<TrafficItem>>>>>,
}

#[derive(Default)]
pub struct DeviceRowData {
    pub(super) node: RefCell<Option<Rc<RefCell<TreeNode<DeviceItem>>>>>,
}

// Basic declaration of our type for the GObject type system
#[glib::object_subclass]
impl ObjectSubclass for TrafficRowData {
    const NAME: &'static str = "TrafficRowData";
    type Type = super::TrafficRowData;
}

#[glib::object_subclass]
impl ObjectSubclass for DeviceRowData {
    const NAME: &'static str = "DeviceRowData";
    type Type = super::DeviceRowData;
}

impl ObjectImpl for TrafficRowData {}
impl ObjectImpl for DeviceRowData {}
