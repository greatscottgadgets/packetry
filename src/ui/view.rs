//! Code for constructing the main views.

use std::marker::PhantomData;

#[cfg(any(test, feature="record-ui-test"))]
use std::{
    cell::RefCell,
    rc::Rc
};

use anyhow::{Context, bail};
use gtk::{
    prelude::*,
    glib::{Object, clone},
    gio::ListModel,
    ColumnView,
    ColumnViewColumn,
    Label,
    ListItem,
    SignalListItemFactory,
    SingleSelection,
};

use crate::capture::CaptureReader;
use crate::item::{
    ItemSource,
    TrafficItem, DeviceItem,
    TrafficViewMode, DeviceViewMode,
};
use crate::ui::{
    capture::Capture,
    item_widget::ItemWidget,
    model::{GenericModel, TrafficModel, DeviceModel},
    row_data::{
        GenericRowData, ToGenericRowData,
        TrafficRowData, DeviceRowData,
    },
};

#[cfg(any(test, feature="record-ui-test"))]
use crate::ui::record_ui::Recording;

use super::{ContextFn, display_error, with_ui};

pub struct View<Item, Model, RowData, ViewMode> {
    _marker: PhantomData<(Item, RowData, ViewMode)>,
    pub model: Model,
    pub selection: SingleSelection,
    pub widget: ColumnView,
}

pub type TrafficView =
    View<TrafficItem, TrafficModel, TrafficRowData, TrafficViewMode>;

pub type DeviceView =
    View<DeviceItem, DeviceModel, DeviceRowData, DeviceViewMode>;

impl<Item, Model, RowData, ViewMode>
View<Item, Model, RowData, ViewMode>
where
    Item: Clone + 'static,
    ViewMode: Copy + 'static,
    Model: GenericModel<Item, ViewMode> + IsA<ListModel> + IsA<Object>,
    RowData: GenericRowData<Item> + IsA<Object>,
    CaptureReader: ItemSource<Item, ViewMode>,
    Object: ToGenericRowData<Item>
{
    pub fn create(
        title: &str,
        capture: &Capture,
        view_mode: ViewMode,
        context_menu_fn: ContextFn<Item>,
        #[cfg(any(test, feature="record-ui-test"))]
        recording_args: (&Rc<RefCell<Recording>>, &'static str)
    ) -> Self {
        #[cfg(any(test, feature="record-ui-test"))]
        let (name, expand_rec, update_rec, changed_rec) = {
            let (recording, name) = recording_args;
            (name, recording.clone(), recording.clone(), recording.clone())
        };
        let model = Model::new(
            capture.clone(),
            view_mode,
            #[cfg(any(test, feature="record-ui-test"))]
            Rc::new(
                RefCell::new(
                    move |position, summary|
                        update_rec
                            .borrow_mut()
                            .log_item_updated(name, position, summary)
                )
            )).expect("Failed to create model");
        let selection = SingleSelection::new(Some(model.clone()));
        let factory = SignalListItemFactory::new();
        factory.connect_setup(move |_, list_item| {
            let widget = ItemWidget::new();
            list_item.set_child(Some(&widget));
        });
        let bind = clone!(@strong model => move |list_item: &ListItem| {
            let row = list_item
                .item()
                .context("ListItem has no item")?
                .downcast::<RowData>()
                .or_else(|_| bail!("Item is not RowData"))?;

            let item_widget = list_item
                .child()
                .context("ListItem has no child widget")?
                .downcast::<ItemWidget>()
                .or_else(|_| bail!("Child widget is not an ItemWidget"))?;

            let expander = item_widget.expander();
            match row.node() {
                Ok(node_ref) => {
                    let node = node_ref.borrow();
                    let item = node.item.clone();
                    let summary = model.description(&item, false);
                    let connectors = model.connectors(view_mode, &item);
                    item_widget.set_text(summary);
                    item_widget.set_connectors(connectors);
                    expander.set_visible(node.expandable());
                    expander.set_expanded(node.expanded());
                    #[cfg(any(test,
                              feature="record-ui-test"))]
                    let recording = expand_rec.clone();
                    let handler = expander.connect_expanded_notify(
                        clone!(@strong model, @strong node_ref, @strong list_item =>
                            move |expander| {
                                let position = list_item.position();
                                let expanded = expander.is_expanded();
                                #[cfg(any(test,
                                          feature="record-ui-test"))]
                                recording.borrow_mut().log_item_expanded(
                                    name, position, expanded);
                                display_error(with_ui(|ui| {
                                    model.set_expanded(
                                        &mut ui.capture,
                                        &node_ref, position, expanded)
                                }))
                            }
                        )
                    );
                    item_widget.set_handler(handler);
                    item_widget.set_context_menu_fn(move || {
                        let mut popover = None;
                        display_error(
                            with_ui(|ui| {
                                popover = context_menu_fn(
                                    &mut ui.capture.reader, &item)?;
                                Ok(())
                            }).context("Failed to generate context menu")
                        );
                        popover
                    });
                    node.attach_widget(&item_widget);
                },
                Err(msg) => {
                    item_widget.set_connectors("".to_string());
                    item_widget.set_text(format!("Error: {msg}"));
                    expander.set_visible(false);
                }
            };
            Ok(())
        });
        let unbind = move |list_item: &ListItem| {
            let row = list_item
                .item()
                .context("ListItem has no item")?
                .downcast::<RowData>()
                .or_else(|_| bail!("Item is not RowData"))?;

            let item_widget = list_item
                .child()
                .context("ListItem has no child widget")?
                .downcast::<ItemWidget>()
                .or_else(|_| bail!("Child widget is not an ItemWidget"))?;

            if let Ok(node_ref) = row.node() {
                node_ref.borrow().remove_widget(&item_widget);
            }

            let expander = item_widget.expander();
            if let Some(handler) = item_widget.take_handler() {
                expander.disconnect(handler);
            }

            Ok(())
        };
        factory.connect_bind(move |_, item| display_error(bind(item)));
        factory.connect_unbind(move |_, item| display_error(unbind(item)));

        let column_view = ColumnView::new(Some(selection.clone()));
        let column = ColumnViewColumn::new(Some(title), Some(factory));
        column_view.append_column(&column);
        column_view.add_css_class("data-table");

        if Model::HAS_TIMES {
            let model = model.clone();
            let factory = SignalListItemFactory::new();
            factory.connect_setup(move |_, list_item| {
                let label = Label::new(None);
                list_item.set_child(Some(&label));
            });
            let bind = move |list_item: &ListItem| {
                let row = list_item
                    .item()
                    .context("ListItem has no item")?
                    .downcast::<RowData>()
                    .or_else(|_| bail!("Item is not RowData"))?;
                let label = list_item
                    .child()
                    .context("ListItem has no child widget")?
                    .downcast::<Label>()
                    .or_else(|_| bail!("Child widget is not a Label"))?;
                match row.node() {
                    Ok(node_ref) => {
                        let node = node_ref.borrow();
                        let timestamp = model.timestamp(&node.item);
                        label.set_markup(&format!("<tt><small>{}.{:09}</small></tt>",
                                               timestamp / 1_000_000_000,
                                               timestamp % 1_000_000_000));
                    },
                    Err(msg) => {
                        label.set_text(&format!("Error: {msg}"));
                    }
                }
                Ok(())
            };

            factory.connect_bind(move |_, item| display_error(bind(item)));

            let timestamp_column =
                ColumnViewColumn::new(Some("Time"), Some(factory));
            column_view.insert_column(0, &timestamp_column);
        }

        #[cfg(any(test, feature="record-ui-test"))]
        model.connect_items_changed(move |model, position, removed, added|
            changed_rec.borrow_mut().log_items_changed(
                name, model, position, removed, added));

        View {
            _marker: PhantomData,
            model,
            selection,
            widget: column_view
        }
    }
}
