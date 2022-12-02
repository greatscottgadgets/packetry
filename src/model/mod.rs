//! Defines our custom model

mod imp;

use std::sync::{Arc, Mutex};

use gtk::prelude::ListModelExt;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::capture::{Capture, TrafficItem, DeviceItem};
use crate::tree_list_model::{TreeListModel, ItemNodeRc, ModelError};

// Public part of the Model type.
glib::wrapper! {
    pub struct TrafficModel(ObjectSubclass<imp::TrafficModel>) @implements gio::ListModel;
}
glib::wrapper! {
    pub struct DeviceModel(ObjectSubclass<imp::DeviceModel>) @implements gio::ListModel;
}

pub trait GenericModel<Item> where Self: Sized {
    fn new(capture: Arc<Mutex<Capture>>) -> Result<Self, ModelError>;
    fn set_expanded(&self,
                    node: &ItemNodeRc<Item>,
                    expanded: bool)
        -> Result<(), ModelError>;
    fn update(&self) -> Result<(), ModelError>;
}

impl GenericModel<TrafficItem> for TrafficModel {
    fn new(capture: Arc<Mutex<Capture>>) -> Result<Self, ModelError> {
        let model: TrafficModel =
            glib::Object::new(&[]).expect("Failed to create TrafficModel");
        let tree = TreeListModel::new(capture)?;
        model.imp().tree.replace(Some(tree));
        Ok(model)
    }

    fn set_expanded(&self,
                    node: &ItemNodeRc<TrafficItem>,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let tree_opt  = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.set_expanded(self, node, expanded)
    }

    fn update(&self) -> Result<(), ModelError> {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        if let Some((position, _, added)) = tree.update()? {
            drop(tree_opt);
            self.items_changed(position, 0, added);
        }
        Ok(())
    }
}

impl GenericModel<DeviceItem> for DeviceModel {
    fn new(capture: Arc<Mutex<Capture>>) -> Result<Self, ModelError> {
        let model: DeviceModel =
            glib::Object::new(&[]).expect("Failed to create DeviceModel");
        let tree = TreeListModel::new(capture)?;
        model.imp().tree.replace(Some(tree));
        Ok(model)
    }

    fn set_expanded(&self,
                    node: &ItemNodeRc<DeviceItem>,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let tree_opt  = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        tree.set_expanded(self, node, expanded)
    }

    fn update(&self) -> Result<(), ModelError> {
        let tree_opt = self.imp().tree.borrow();
        let tree = tree_opt.as_ref().unwrap();
        if let Some((position, _, added)) = tree.update()? {
            drop(tree_opt);
            self.items_changed(position, 0, added);
        }
        Ok(())
    }
}
