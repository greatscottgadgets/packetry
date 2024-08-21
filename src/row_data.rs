use gtk::glib;
use gtk::subclass::prelude::*;

#[cfg(any(test, feature="record-ui-test"))]
use gtk::prelude::Cast;

use crate::capture::{TrafficItem, DeviceItem};
use crate::tree_list_model::ItemNodeRc;

pub trait GenericRowData<Item> where Item: Copy {
    fn new(node: Result<ItemNodeRc<Item>, String>) -> Self where Self: Sized;
    fn node(&self) -> Result<ItemNodeRc<Item>, String>;
}

pub trait ToGenericRowData<Item> {
    #[cfg(any(test, feature="record-ui-test"))]
    fn to_generic_row_data(self) -> Box<dyn GenericRowData<Item>>;
}

macro_rules! row_data {
    ($row_data: ident, $item: ident) => {
        glib::wrapper! {
            pub struct $row_data(ObjectSubclass<imp::$row_data>);
        }

        impl GenericRowData<$item> for $row_data {
            fn new(node: Result<ItemNodeRc<$item>, String>) -> $row_data{
                let row: $row_data = glib::Object::new::<$row_data>();
                row.imp().node.replace(Some(node));
                row
            }

            fn node(&self) -> Result<ItemNodeRc<$item>, String> {
                self.imp().node.borrow().as_ref().unwrap().clone()
            }
        }

        impl ToGenericRowData<$item> for glib::Object {
            #[cfg(any(test, feature="record-ui-test"))]
            fn to_generic_row_data(self) -> Box<dyn GenericRowData<$item>> {
                Box::new(self.downcast::<$row_data>().unwrap())
            }
        }
    }
}

row_data!(TrafficRowData, TrafficItem);
row_data!(DeviceRowData, DeviceItem);

mod imp {
    use gtk::glib::{self, subclass::prelude::*};
    use std::cell::RefCell;

    use crate::capture::{TrafficItem, DeviceItem};
    use crate::tree_list_model::ItemNodeRc;

    macro_rules! row_data {
        ($row_data: ident, $item: ident) => {
            #[derive(Default)]
            pub struct $row_data {
                pub(super) node: RefCell<Option<
                    Result<ItemNodeRc<$item>, String>>>,
            }

            #[glib::object_subclass]
            impl ObjectSubclass for $row_data {
                const NAME: &'static str = stringify!($row_data);
                type Type = super::$row_data;
            }

            impl ObjectImpl for $row_data {}
        }
    }

    row_data!(TrafficRowData, TrafficItem);
    row_data!(DeviceRowData, DeviceItem);
}
