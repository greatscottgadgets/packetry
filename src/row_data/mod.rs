//! Our GObject subclass for carrying a name and count for the ListBox model
//!
//! Both name and count are stored in a RefCell to allow for interior mutability
//! and are exposed via normal GObject properties. This allows us to use property
//! bindings below to bind the values with what widgets display in the UI

mod imp;

use std::rc::Rc;
use std::cell::RefCell;

use gtk::glib;
use gtk::subclass::prelude::*;

use crate::capture::{TrafficItem, DeviceItem};
use crate::tree_list_model::TreeNode;

// Public part of the RowData type. This behaves like a normal gtk-rs-style GObject
// binding
glib::wrapper! {
    pub struct TrafficRowData(ObjectSubclass<imp::TrafficRowData>);
}
glib::wrapper! {
    pub struct DeviceRowData(ObjectSubclass<imp::DeviceRowData>);
}

pub trait GenericRowData<Item> where Item: Copy {
    fn new(node: Rc<RefCell<TreeNode<Item>>>) -> Self;
    fn node(&self) -> Rc<RefCell<TreeNode<Item>>>;
}

impl GenericRowData<TrafficItem> for TrafficRowData {
    fn new(node: Rc<RefCell<TreeNode<TrafficItem>>>) -> TrafficRowData {
        let row: TrafficRowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.imp().node.replace(Some(node));
        row
    }

    fn node(&self) -> Rc<RefCell<TreeNode<TrafficItem>>> {
        self.imp().node.borrow().as_ref().unwrap().clone()
    }
}

impl GenericRowData<DeviceItem> for DeviceRowData {
    fn new(node: Rc<RefCell<TreeNode<DeviceItem>>>) -> DeviceRowData {
        let row: DeviceRowData =
            glib::Object::new(&[]).expect("Failed to create row data");
        row.imp().node.replace(Some(node));
        row
    }

    fn node(&self) -> Rc<RefCell<TreeNode<DeviceItem>>> {
        self.imp().node.borrow().as_ref().unwrap().clone()
    }
}
