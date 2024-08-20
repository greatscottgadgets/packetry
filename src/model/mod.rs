//! Defines our custom model

mod imp;

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::cell::RefCell,
    std::rc::Rc,
};

use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use anyhow::Error;

use crate::capture::{CaptureReader, TrafficItem, DeviceItem};
use crate::tree_list_model::{TreeListModel, ItemNodeRc};

pub trait GenericModel<Item> where Self: Sized {
    const HAS_TIMES: bool;
    fn new(capture: CaptureReader,
           #[cfg(any(test, feature="record-ui-test"))]
           on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, Error>;
    fn set_expanded(&self,
                    node: &ItemNodeRc<Item>,
                    position: u32,
                    expanded: bool)
        -> Result<(), Error>;
    fn update(&self) -> Result<bool, Error>;
    fn description(&self, item: &Item, detail: bool) -> String;
    fn timestamp(&self, item: &Item) -> u64;
    fn connectors(&self, item: &Item) -> String;
}

macro_rules! model {
    ($model: ident, $item: ident, $has_times: literal) => {
        // Public part of the Model type.
        glib::wrapper! {
            pub struct $model(ObjectSubclass<imp::$model>)
                @implements gio::ListModel;
        }

        impl GenericModel<$item> for $model {
            const HAS_TIMES: bool = $has_times;

            fn new(capture: CaptureReader,
                   #[cfg(any(test, feature="record-ui-test"))]
                   on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
                -> Result<Self, Error>
            {
                let model: $model = glib::Object::new::<$model>();
                let tree = TreeListModel::new(
                    capture,
                    #[cfg(any(test, feature="record-ui-test"))]
                    on_item_update)?;
                model.imp().tree.replace(Some(tree));
                Ok(model)
            }

            fn set_expanded(&self,
                            node: &ItemNodeRc<$item>,
                            position: u32,
                            expanded: bool)
                -> Result<(), Error>
            {
                let tree_opt  = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.set_expanded(self, node, position as u64, expanded)
            }

            fn update(&self) -> Result<bool, Error> {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.update(self)
            }

            fn description(&self, item: &$item, detail: bool) -> String {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.description(item, detail)
            }

            fn timestamp(&self, item: &$item) -> u64 {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.timestamp(item)
            }

            fn connectors(&self, item: &$item) -> String {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.connectors(item)
            }
        }
    }
}

model!(TrafficModel, TrafficItem, true);
model!(DeviceModel, DeviceItem, false);
