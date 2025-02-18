//! GObject subclasses implementing ListModel for each UI view.

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::cell::RefCell,
    std::rc::Rc,
};

use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use anyhow::Error;

use crate::capture::{CaptureReader, CaptureSnapshot};
use crate::database::Snapshot;
use crate::item::{TrafficItem, TrafficViewMode, DeviceItem, DeviceViewMode};
use crate::ui::tree_list_model::{TreeListModel, ItemNodeRc};

/// Trait implemented by each of our ListModel implementations.
pub trait GenericModel<Item, ViewMode> where Self: Sized {
    /// Whether this model has timestamps.
    const HAS_TIMES: bool;

    /// Create a new model instance for the given capture.
    fn new(
        capture: CaptureReader,
        snapshot: Snapshot,
        view_mode: ViewMode,
        #[cfg(any(test, feature="record-ui-test"))]
        on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>
    ) -> Result<Self, Error>;

    /// Set whether a tree node is expanded.
    fn set_expanded(&self,
                    cap: &mut CaptureSnapshot,
                    node: &ItemNodeRc<Item>,
                    position: u32,
                    expanded: bool)
        -> Result<(), Error>;

    /// Update the model with new data from the capture.
    ///
    /// Returns true if there will be further updates in future.
    fn update(&self, snapshot: &Snapshot) -> Result<bool, Error>;

    /// Fetch the description for a given item.
    fn description(&self, item: &Item, detail: bool) -> String;

    /// Fetch the timestamp for a given item.
    fn timestamp(&self, item: &Item) -> u64;

    /// Fetch the connecting lines for a given item.
    fn connectors(&self, item: &Item) -> String;

    /// Fetch the currently selected item, if any.
    fn selected_item(&self) -> Option<Item>;
}

/// Define the outer type exposed to our Rust code.
macro_rules! model {
    ($model: ident, $item: ident, $view_mode: ident, $has_times: literal) => {

        glib::wrapper! {
            pub struct $model(ObjectSubclass<imp::$model>)
                @implements gio::ListModel, gtk::SelectionModel;
        }

        impl GenericModel<$item, $view_mode> for $model {
            const HAS_TIMES: bool = $has_times;

            fn new(
                capture: CaptureReader,
                snapshot: Snapshot,
                view_mode: $view_mode,
                #[cfg(any(test, feature="record-ui-test"))]
                on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>
            ) -> Result<Self, Error> {
                let model: $model = glib::Object::new::<$model>();
                let tree = TreeListModel::new(
                    capture,
                    snapshot,
                    view_mode,
                    #[cfg(any(test, feature="record-ui-test"))]
                    on_item_update)?;
                model.imp().tree.replace(Some(tree));
                Ok(model)
            }

            fn set_expanded(
                &self,
                cap: &mut CaptureSnapshot,
                node: &ItemNodeRc<$item>,
                position: u32,
                expanded: bool
            ) -> Result<(), Error> {
                let tree_opt  = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.set_expanded(self, cap, node, position as u64, expanded)
            }

            fn update(&self, snapshot: &Snapshot) -> Result<bool, Error> {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.update(self, snapshot)
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

            fn selected_item(&self) -> Option<$item> {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                tree.selected.clone()
            }
        }
    }
}

// Repeat the above boilerplate for each model.
model!(TrafficModel, TrafficItem, TrafficViewMode, true);
model!(DeviceModel, DeviceItem, DeviceViewMode, false);

/// The internal implementation module.
mod imp {
    use gtk::{gio, glib, prelude::*, subclass::prelude::*, Bitset};

    use std::cell::RefCell;
    use crate::item::{TrafficItem, TrafficViewMode, DeviceItem, DeviceViewMode};
    use crate::ui::row_data::{TrafficRowData, DeviceRowData};
    use crate::ui::tree_list_model::TreeListModel;

    /// Define the inner type to be used in the GObject type system.
    macro_rules! model {
        ($model:ident, $item:ident, $row_data:ident, $view_mode: ident) => {
            #[derive(Default)]
            pub struct $model {
                pub(super) tree: RefCell<Option<
                    TreeListModel<$item, super::$model, $row_data, $view_mode>>>,
            }

            #[glib::object_subclass]
            impl ObjectSubclass for $model {
                const NAME: &'static str = stringify!($model);
                type Type = super::$model;
                type Interfaces = (gio::ListModel, gtk::SelectionModel);
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

            impl SelectionModelImpl for $model {
                fn selection_in_range(&self, _position: u32, _n_items: u32)
                    -> Bitset
                {
                    unimplemented!()
                }

                fn is_selected(&self, position: u32) -> bool {
                    match self.tree.borrow().as_ref() {
                        Some(tree) => tree.is_selected(position),
                        None => false,
                    }
                }

                fn select_all(&self) -> bool {
                    false
                }

                fn select_item(&self, position: u32, unselect_rest: bool)
                    -> bool
                {
                    let result = match self.tree.borrow_mut().as_mut() {
                        Some(tree) => tree.select_item(position, unselect_rest),
                        None => false,
                    };
                    if result {
                        self.obj().selection_changed(0, self.n_items());
                    }
                    result
                }

                fn select_range(&self,
                                _position: u32,
                                _n_items: u32,
                                _unselect_rest: bool)
                    -> bool
                {
                    false
                }

                fn set_selection(&self, _selected: &Bitset, _mask: &Bitset)
                    -> bool
                {
                    unimplemented!()
                }

                fn unselect_all(&self) -> bool {
                    unimplemented!()
                }

                fn unselect_item(&self, _position: u32) -> bool {
                    unimplemented!()
                }

                fn unselect_range(&self, _position: u32, _n_items: u32) -> bool {
                    unimplemented!()
                }
            }
        }
    }

    // Repeat the above boilerplate for each model.
    model!(TrafficModel, TrafficItem, TrafficRowData, TrafficViewMode);
    model!(DeviceModel, DeviceItem, DeviceRowData, DeviceViewMode);
}
