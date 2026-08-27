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

use crate::ui::capture::{Capture, CaptureState};
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
                let tree = match &capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        TreeListModel::new(
                            &mut capture.reader.at(&snapshot),
                            view_mode,
                            #[cfg(any(test, feature="record-ui-test"))]
                            on_item_update
                        )?,
                    CaptureState::Complete =>
                        TreeListModel::new(
                            &mut capture.reader,
                            view_mode,
                            #[cfg(any(test, feature="record-ui-test"))]
                            on_item_update
                        )?,
                };
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
                match &capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        tree.set_expanded(&mut capture.reader.at(&snapshot),
                            self, node, position as u64, expanded),
                    CaptureState::Complete =>
                        tree.set_expanded(&mut capture.reader,
                            self, node, position as u64, expanded),
                }
            }

            fn update(&self, ext_capture: &mut Capture) -> Result<bool, Error> {
                {
                    let mut cap_opt = self.imp().capture.borrow_mut();
                    let int_capture = cap_opt.as_mut().unwrap();
                    if let CaptureState::Ongoing(snapshot) = &ext_capture.state {
                        int_capture.set_snapshot(snapshot.clone());
                    } else {
                        int_capture.set_completed();
                    }
                }
                let tree_opt = self.imp().tree.borrow();
                let tree = tree_opt.as_ref().unwrap();
                let result = match &ext_capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        tree.update(&mut ext_capture.reader.at(&snapshot), self),
                    CaptureState::Complete =>
                        tree.update(&mut ext_capture.reader, self)
                };
                result
            }

            fn description(&self, item: &$item, detail: bool) -> String {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = match &capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        capture.reader.at(&snapshot).description(item, detail),
                    CaptureState::Complete =>
                        capture.reader.description(item, detail),
                };
                match result {
                    Ok(string) => string,
                    Err(e) => if detail {
                        format!("Error: {e:?}")
                    } else {
                        format!("Error: {e}")
                    }
                }
            }

            fn timestamp(&self, item: &$item) -> u64 {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = match &capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        capture.reader.at(&snapshot).timestamp(item),
                    CaptureState::Complete =>
                        capture.reader.timestamp(item),
                };
                result.unwrap_or(0)
            }

            fn connectors(&self, view_mode: $view_mode, item: &$item) -> String {
                let mut cap_opt = self.imp().capture.borrow_mut();
                let capture = cap_opt.as_mut().unwrap();
                let result = match &capture.state {
                    CaptureState::Ongoing(snapshot) =>
                        capture.reader.at(&snapshot).connectors(view_mode, item),
                    CaptureState::Complete =>
                        capture.reader.connectors(view_mode, item),
                };
                match result {
                    Ok(string) => string,
                    Err(e) => format!("Error: {e}")
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
    use crate::ui::capture::{Capture, CaptureState};
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
                        (Some(tree), Some(capture)) => match &capture.state {
                            CaptureState::Ongoing(snapshot) => tree.item(
                                &mut capture.reader.at(&snapshot), position),
                            CaptureState::Complete => tree.item(
                                &mut capture.reader, position),
                        },
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
