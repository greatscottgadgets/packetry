//! Defines our custom model

mod imp;

#[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
use {
    std::cell::RefCell,
    std::rc::Rc,
};

use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::capture::{CaptureReader, TrafficItem, DeviceItem};
use crate::tree_list_model::{TreeListModel, ItemNodeRc, ModelError};

// Public part of the Model type.
glib::wrapper! {
    pub struct TrafficModel(ObjectSubclass<imp::TrafficModel>) @implements gio::ListModel;
}
glib::wrapper! {
    pub struct DeviceModel(ObjectSubclass<imp::DeviceModel>) @implements gio::ListModel;
}

pub trait GenericModel<Item> where Self: Sized {
    fn new(capture: CaptureReader,
           #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
           on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, ModelError>;
    fn set_expanded(&self,
                    node: &ItemNodeRc<Item>,
                    position: u32,
                    expanded: bool)
        -> Result<(), ModelError>;
    fn update(&self) -> Result<bool, ModelError>;
    fn summary(&self, item: &Item) -> String;
    fn connectors(&self, item: &Item) -> String;
}

impl GenericModel<TrafficItem> for TrafficModel {
    fn new(capture: CaptureReader,
           #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
           on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, ModelError>
    {
        let model: TrafficModel =
            glib::Object::new(&[]).expect("Failed to create TrafficModel");
        let tree = TreeListModel::new(
            capture,
            #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
            on_item_update)?;
        model.imp().tree.replace(Some(tree));
        Ok(model)
    }

    fn set_expanded(&self,
                    node: &ItemNodeRc<TrafficItem>,
                    position: u32,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let tree_opt  = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.set_expanded(self, node, position as u64, expanded)
    }

    fn update(&self) -> Result<bool, ModelError> {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.update(self)
    }

    fn summary(&self, item: &TrafficItem) -> String {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.summary(item)
    }

    fn connectors(&self, item: &TrafficItem) -> String {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.connectors(item)
    }
}

impl GenericModel<DeviceItem> for DeviceModel {
    fn new(capture: CaptureReader,
           #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
           on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, ModelError>
    {
        let model: DeviceModel =
            glib::Object::new(&[]).expect("Failed to create DeviceModel");
        let tree = TreeListModel::new(
            capture,
            #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
            on_item_update)?;
        model.imp().tree.replace(Some(tree));
        Ok(model)
    }

    fn set_expanded(&self,
                    node: &ItemNodeRc<DeviceItem>,
                    position: u32,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let tree_opt  = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.set_expanded(self, node, position as u64, expanded)
    }

    fn update(&self) -> Result<bool, ModelError> {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.update(self)
    }

    fn summary(&self, item: &DeviceItem) -> String {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.summary(item)
    }

    fn connectors(&self, item: &DeviceItem) -> String {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.connectors(item)
    }
}
