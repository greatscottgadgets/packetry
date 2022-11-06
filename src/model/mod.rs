//! Defines our custom model

mod imp;

use std::sync::{Arc, Mutex};

use gtk::prelude::{IsA, ListModelExt};
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use crate::capture::{Capture, TrafficItem, DeviceItem};
use crate::tree_list_model::{TreeListModel, ItemNodeRc, ModelError, ModelUpdate};

// Public part of the Model type.
glib::wrapper! {
    pub struct TrafficModel(ObjectSubclass<imp::TrafficModel>) @implements gio::ListModel;
}
glib::wrapper! {
    pub struct DeviceModel(ObjectSubclass<imp::DeviceModel>) @implements gio::ListModel;
}

trait ApplyUpdate {
    fn apply_update(&self, position: u32, update: ModelUpdate);
}

impl<T> ApplyUpdate for T where T: Sized + IsA<gio::ListModel> {
    fn apply_update(&self, position: u32, update: ModelUpdate) {
        let rows_removed = update.rows_removed + update.rows_changed;
        let rows_added = update.rows_added + update.rows_changed;
        self.items_changed(position, rows_removed, rows_added);
    }
}

pub trait GenericModel<Item> where Self: Sized {
    fn new(capture: Arc<Mutex<Capture>>) -> Result<Self, ModelError>;
    fn set_expanded(&self,
                    node: &ItemNodeRc<Item>,
                    position: u32,
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
                    position: u32,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let update = self.imp().tree
            .borrow_mut()
            .as_mut()
            .unwrap()
            .set_expanded(node, position, expanded)?;
        self.apply_update(position + 1, update);
        Ok(())
    }

    fn update(&self) -> Result<(), ModelError> {
        let update_opt = self.imp().tree
            .borrow_mut()
            .as_mut()
            .unwrap()
            .update()?;
        if let Some((position, update)) = update_opt {
            self.apply_update(position, update);
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
                    position: u32,
                    expanded: bool)
        -> Result<(), ModelError>
    {
        let update = self.imp().tree
            .borrow_mut()
            .as_mut()
            .unwrap()
            .set_expanded(node, position, expanded)?;
        self.apply_update(position + 1, update);
        Ok(())
    }

    fn update(&self) -> Result<(), ModelError> {
        let update_opt = self.imp().tree
            .borrow_mut()
            .as_mut()
            .unwrap()
            .update()?;
        if let Some((position, update)) = update_opt {
            self.apply_update(position, update);
        }
        Ok(())
    }
}
