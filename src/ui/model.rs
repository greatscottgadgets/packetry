//! GObject subclasses implementing ListModel for each UI view.

#[cfg(any(test, feature="record-ui-test"))]
use {
    std::cell::RefCell,
    std::rc::Rc,
};

use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use anyhow::Error;

use crate::item::{
    ItemSource, TrafficItem, TrafficViewMode, DeviceItem, DeviceViewMode};

use crate::ui::capture::Capture;
use crate::ui::tree_list_model::{TreeListModel, ItemNodeRc};

/// Trait implemented by each of our ListModel implementations.
pub trait GenericModel<Item, ViewMode> where Self: Sized {
    /// Whether this model has timestamps.
    const HAS_TIMES: bool;

    /// Create a new model instance for the given capture.
    fn new(
        cap: Capture,
        view_mode: ViewMode,
        #[cfg(any(test, feature="record-ui-test"))]
        on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>
    ) -> Result<Self, Error>;

    /// Set whether a tree node is expanded.
    fn set_expanded(
        &self,
        cap: &mut Capture,
        node: &ItemNodeRc<Item>,
        position: u32,
        expanded: bool
    ) -> Result<(), Error>;

    /// Update the model with new data from the capture.
    ///
    /// Returns true if there will be further updates in future.
    fn update(
        &self,
        cap: &mut Capture,
    ) -> Result<bool, Error>;

    /// Fetch the description for a given item.
    fn description(&self, item: &Item, detail: bool) -> String;

    /// Fetch the timestamp for a given item.
    fn timestamp(&self, item: &Item) -> u64;

    /// Fetch the connecting lines for a given item.
    fn connectors(&self, view_mode: ViewMode, item: &Item) -> String;
}

macro_rules! dispatch {
    ($capture: ident => $expr: expr) => {{
        match (
            &mut $capture.snapshot,
            &mut $capture.filter,
            &mut $capture.filter_snapshot,
        ) {
            (Some(cap_snapshot), Some(filter), Some(filter_snapshot)) => {
                let mut capture_snap = $capture.reader.at(cap_snapshot);
                let mut filter_snap = filter.at(filter_snapshot);
                let mut filtered = filter_snap.apply(&mut capture_snap);
                let $capture = &mut filtered;
                $expr
            },
            (None, Some(filter), Some(filter_snapshot)) => {
                let mut filter_snap = filter.at(filter_snapshot);
                let mut filtered = filter_snap.apply(&mut $capture.reader);
                let $capture = &mut filtered;
                $expr
            },
            (None, Some(filter), None)  => {
                let mut filtered = filter.apply(&mut $capture.reader);
                let $capture = &mut filtered;
                $expr
            },
            (Some(snapshot), None, None) => {
                let mut snapshot_reader = $capture.reader.at(snapshot);
                let $capture = &mut snapshot_reader;
                $expr
            },
            (..) => {
                let $capture = &mut $capture.reader;
                $expr
            },
        }
    }}
}

/// Define the outer type exposed to our Rust code.
macro_rules! model {
    ($model: ident, $item: ident, $view_mode: ident, $has_times: literal) => {

        glib::wrapper! {
            pub struct $model(ObjectSubclass<imp::$model>)
                @implements gio::ListModel;
        }

        impl GenericModel<$item, $view_mode> for $model {
            const HAS_TIMES: bool = $has_times;

            fn new(
                mut capture: Capture,
                view_mode: $view_mode,
                #[cfg(any(test, feature="record-ui-test"))]
                on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>
            ) -> Result<Self, Error> {
                let model: $model = glib::Object::new::<$model>();
                let tree = dispatch!(
                    capture => TreeListModel::new(
                        capture,
                        view_mode,
                        #[cfg(any(test, feature="record-ui-test"))]
                        on_item_update
                    )
                )?;
                model.imp().tree.replace(Some(tree));
                model.imp().capture.replace(Some(capture));
                Ok(model)
            }

            fn set_expanded(
                &self,
                capture: &mut Capture,
                node: &ItemNodeRc<$item>,
                position: u32,
                expanded: bool
            ) -> Result<(), Error> {
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                dispatch!(capture => tree.set_expanded(capture,
                    self, node, position as u64, expanded))
            }

            fn update(&self, ext_capture: &mut Capture) -> Result<bool, Error> {
                {
                    let mut cap_opt = self.imp().capture.borrow_mut();
                    let int_capture = cap_opt.as_mut().unwrap();
                    int_capture.update_from(&ext_capture);
                }
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                dispatch!(ext_capture => tree.update(ext_capture, self))
            }

            fn description(&self, item: &$item, detail: bool) -> String {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = dispatch!(
                    capture => capture.description(item, detail)
                );
                match result {
                    Ok(string) => string,
                    Err(e) => format!("Error: {e:?}")
                }
            }

            fn timestamp(&self, item: &$item) -> u64 {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = dispatch!(capture => capture.timestamp(item));
                result.unwrap_or(0)
            }

            fn connectors(&self, view_mode: $view_mode, item: &$item) -> String {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = dispatch!(
                    capture => capture.connectors(view_mode, item)
                );
                match result {
                    Ok(string) => string,
                    Err(e) => format!("Error: {e:?}")
                }
            }
        }
    }
}

// Repeat the above boilerplate for each model.
model!(TrafficModel, TrafficItem, TrafficViewMode, true);
model!(DeviceModel, DeviceItem, DeviceViewMode, false);

/// The internal implementation module.
mod imp {
    use gio::subclass::prelude::*;
    use gtk::{gio, glib, prelude::*};

    use std::cell::RefCell;
    use crate::item::{TrafficItem, TrafficViewMode, DeviceItem, DeviceViewMode};
    use crate::ui::capture::Capture;
    use crate::ui::row_data::{TrafficRowData, DeviceRowData};
    use crate::ui::tree_list_model::TreeListModel;

    /// Define the inner type to be used in the GObject type system.
    macro_rules! model {
        ($model:ident, $item:ident, $row_data:ident, $view_mode: ident) => {

            #[derive(Default)]
            pub struct $model {
                pub(super) tree: RefCell<Option<TreeListModel<
                    $item, super::$model, $row_data, $view_mode>>>,
                pub(super) capture: RefCell<Option<Capture>>,
            }

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
                    let tree_opt = self.tree.borrow();
                    let mut cap_opt = self.capture.borrow_mut();
                    match (tree_opt.as_ref(), cap_opt.as_mut()) {
                        (Some(tree), Some(capture)) =>
                            dispatch!(capture => tree.item(capture, position)),
                        _ => None
                    }
                }
            }
        }
    }

    // Repeat the above boilerplate for each model.
    model!(TrafficModel, TrafficItem, TrafficRowData, TrafficViewMode);
    model!(DeviceModel, DeviceItem, DeviceRowData, DeviceViewMode);
}
