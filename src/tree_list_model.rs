use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::collections::btree_map::Entry;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::num::TryFromIntError;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::ops::DerefMut;

use gtk::prelude::{IsA, Cast, WidgetExt};
use gtk::glib::Object;
use gtk::gio::prelude::ListModelExt;

use itertools::Itertools;
use thiserror::Error;

use crate::capture::{Capture, CaptureError, ItemSource, CompletionStatus};
use crate::model::GenericModel;
use crate::row_data::GenericRowData;
use crate::expander::ExpanderWrapper;

#[derive(Error, Debug)]
pub enum ModelError {
    #[error(transparent)]
    CaptureError(#[from] CaptureError),
    #[error(transparent)]
    RangeError(#[from] TryFromIntError),
    #[error("Locking capture failed")]
    LockError,
    #[error("Node references a dropped parent")]
    ParentDropped,
    #[error("Internal error: {0}")]
    InternalError(String),
}

use ModelError::InternalError;

type RootNodeRc<Item> = Rc<RefCell<RootNode<Item>>>;
pub type ItemNodeRc<Item> = Rc<RefCell<ItemNode<Item>>>;
pub type ItemNodeWeak<Item> = Weak<RefCell<ItemNode<Item>>>;
type AnyNodeRc<Item> = Rc<RefCell<dyn Node<Item>>>;

trait Node<Item> {
    /// Item at this node, or None if the root.
    fn item(&self) -> Option<&Item>;

    /// Parent of this node, or None if the root.
    fn parent(&self) -> Result<Option<AnyNodeRc<Item>>, ModelError>;

    /// Access the children of this node.
    fn children(&self) -> &Children<Item>;

    /// Mutably access the children of this node.
    fn children_mut(&mut self) -> &mut Children<Item>;

    /// Whether the children of this node are displayed.
    fn expanded(&self) -> bool;

    /// Mark this node as completed.
    fn set_completed(&mut self);

    /// Access this node as an item node, if it is one.
    fn item_node(&mut self) -> Option<&mut ItemNode<Item>>;
}

struct Children<Item> {
    /// Number of direct children below this node.
    direct_count: u32,

    /// Total number of displayed rows below this node, recursively.
    total_count: u32,

    /// Expanded children of this item.
    expanded: BTreeMap<u32, ItemNodeRc<Item>>,

    /// Incomplete children of this item.
    incomplete: BTreeMap<u32, ItemNodeWeak<Item>>,
}

impl<Item> Children<Item> {
    fn new(child_count: u32) -> Self {
        Children {
            direct_count: child_count,
            total_count: child_count,
            expanded: BTreeMap::new(),
            incomplete: BTreeMap::new(),
        }
    }
}

struct RootNode<Item> {
    /// Top level children.
    children: Children<Item>,

    /// Whether the capture is complete.
    complete: bool,
}

pub struct ItemNode<Item> {
    /// The item at this tree node.
    pub item: Item,

    /// Parent of this node in the tree.
    parent: Weak<RefCell<dyn Node<Item>>>,

    /// Index of this node below the parent Item.
    item_index: u32,

    /// Children of this item.
    children: Children<Item>,

    /// Widgets to update when this item changes.
    widgets: RefCell<HashSet<ExpanderWrapper>>,
}

impl<Item> Children<Item> {
    /// Whether this child is expanded.
    fn expanded(&self, index: u32) -> bool {
        self.expanded.contains_key(&index)
    }

    /// Iterate over the expanded children.
    fn iter_expanded(&self) -> impl Iterator<Item=(&u32, &ItemNodeRc<Item>)> + '_ {
        self.expanded.iter()
    }

    /// Set whether this child of the owning node is expanded.
    fn set_expanded(&mut self, child_rc: &ItemNodeRc<Item>, expanded: bool) {
        let child = child_rc.borrow();
        if expanded {
            self.expanded.insert(child.item_index, child_rc.clone());
        } else {
            self.expanded.remove(&child.item_index);
        }
    }

    /// Add an incomplete child.
    fn add_incomplete(&mut self, index: u32, child_rc: &ItemNodeRc<Item>) {
        self.incomplete.insert(index, Rc::downgrade(child_rc));
    }

    /// Fetch an incomplete child.
    fn fetch_incomplete(&self, index: u32) -> Option<ItemNodeRc<Item>> {
        self.incomplete.get(&index).and_then(Weak::upgrade)
    }

    /// Get the number of rows between two children.
    fn rows_between(&self, start: u32, end: u32) -> u32 {
        (end - start) +
            self.expanded
                .range(start..end)
                .map(|(_, node_rc)| node_rc.borrow().children.total_count)
                .sum::<u32>()
    }
}

impl<Item> Node<Item> for RootNode<Item> {
    fn item(&self) -> Option<&Item> {
        None
    }

    fn parent(&self) -> Result<Option<AnyNodeRc<Item>>, ModelError> {
        Ok(None)
    }

    fn children(&self) -> &Children<Item> {
        &self.children
    }

    fn children_mut(&mut self) -> &mut Children<Item> {
        &mut self.children
    }

    fn expanded(&self) -> bool {
        true
    }

    fn set_completed(&mut self) {
        self.complete = true;
    }

    fn item_node(&mut self) -> Option<&mut ItemNode<Item>> {
        None
    }
}

impl<Item> Node<Item> for ItemNode<Item> where Item: Copy {
    fn item(&self) -> Option<&Item> {
        Some(&self.item)
    }

    fn parent(&self) -> Result<Option<AnyNodeRc<Item>>, ModelError> {
        Ok(Some(self.parent.upgrade().ok_or(ModelError::ParentDropped)?))
    }

    fn children(&self) -> &Children<Item> {
        &self.children
    }

    fn children_mut(&mut self) -> &mut Children<Item> {
        &mut self.children
    }

    fn expanded(&self) -> bool {
        match self.parent.upgrade() {
            Some(parent_ref) => parent_ref
                .borrow()
                .children()
                .expanded(self.item_index),
            // Parent is dropped, so node cannot be expanded.
            None => false
        }
    }

    fn set_completed(&mut self) {
        if let Some(parent_rc) = self.parent.upgrade() {
            parent_rc
                .borrow_mut()
                .children_mut()
                .incomplete
                .remove(&self.item_index);
        }
    }

    fn item_node(&mut self) -> Option<&mut ItemNode<Item>> {
        Some(self)
    }
}

trait UpdateTotal<Item> {
    fn update_total(&self, expanded: bool, rows_affected: u32)
        -> Result<(), ModelError>;
}

impl<T, Item> UpdateTotal<Item> for Rc<RefCell<T>>
where T: Node<Item> + 'static, Item: Copy + 'static
{
    fn update_total(&self, expanded: bool, rows_affected: u32)
        -> Result<(), ModelError>
    {
        let mut node_rc: AnyNodeRc<Item> = self.clone();
        while let Some(parent_rc) = node_rc.clone().borrow().parent()? {
            let mut parent = parent_rc.borrow_mut();
            let children = parent.children_mut();
            if expanded {
                children.total_count += rows_affected;
            } else {
                children.total_count -= rows_affected
            }
            drop(parent);
            node_rc = parent_rc;
        }
        Ok(())
    }
}

trait NodeRcOps<Item>: UpdateTotal<Item> {
    fn source(&self) -> Source<Item>;
}

impl<Item> NodeRcOps<Item> for RootNodeRc<Item>
where Item: Copy + 'static
{
    fn source(&self) -> Source<Item> {
        TopLevelItems()
    }
}

impl<Item> NodeRcOps<Item> for ItemNodeRc<Item>
where Item: Copy + 'static
{
    fn source(&self) -> Source<Item> {
        ChildrenOf(self.clone())
    }
}

impl<Item> ItemNode<Item> where Item: Copy {

    pub fn expanded(&self) -> bool {
        Node::<Item>::expanded(self)
    }

    pub fn expandable(&self) -> bool {
        self.children.total_count != 0
    }

    #[allow(clippy::type_complexity)]
    pub fn field(&self,
             capture: &Arc<Mutex<Capture>>,
             func: Box<dyn
                Fn(&mut Capture, &Item)
                    -> Result<String, CaptureError>>)
        -> String
    {
        match capture.lock() {
            Err(_) => "Error: failed to lock capture".to_string(),
            Ok(mut guard) => {
                let cap = guard.deref_mut();
                match func(cap, &self.item) {
                    Err(e) => format!("Error: {e:?}"),
                    Ok(string) => string
                }
            }
        }
    }

    pub fn attach_widget(&self, widget: &ExpanderWrapper) {
        self.widgets.borrow_mut().insert(widget.clone());
    }

    pub fn remove_widget(&self, widget: &ExpanderWrapper) {
        self.widgets.borrow_mut().remove(widget);
    }
}

#[derive(Clone)]
enum Source<Item> {
    TopLevelItems(),
    ChildrenOf(ItemNodeRc<Item>),
}

use Source::*;

#[derive(Clone)]
struct Region<Item> {
    source: Source<Item>,
    offset: u32,
    length: u32,
}

impl<Item> Debug for Region<Item>
where Item: Clone + Debug
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)
        -> Result<(), std::fmt::Error>
    {
        use Source::*;
        match &self.source {
            TopLevelItems() =>
                write!(f, "Top level items"),
            ChildrenOf(rc) =>
                write!(f, "Children of {:?}", rc.borrow().item),
        }?;
        write!(f, ", offset {}, length {}", self.offset, self.length)
    }
}

impl<Item> Region<Item> where Item: Clone {
    fn merge(
        region_a: &Region<Item>,
        region_b: &Region<Item>
    ) -> Option<Region<Item>> {
        match (&region_a.source, &region_b.source) {
            (ChildrenOf(a_ref), ChildrenOf(b_ref))
                if Rc::ptr_eq(a_ref, b_ref) => Some(
                    Region {
                        source: region_a.source.clone(),
                        offset: region_a.offset,
                        length: region_a.length + region_b.length,
                    }
                ),
            (TopLevelItems(), TopLevelItems()) => Some(
                Region {
                    source: TopLevelItems(),
                    offset: region_a.offset,
                    length: region_a.length + region_b.length,
                }
            ),
            (..) => None,
        }
    }
}

pub struct TreeListModel<Item, Model, RowData> {
    _marker: PhantomData<(Model, RowData)>,
    capture: Arc<Mutex<Capture>>,
    root: RootNodeRc<Item>,
    regions: RefCell<BTreeMap<u32, Region<Item>>>,
    #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
    on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>,
}

impl<Item, Model, RowData> TreeListModel<Item, Model, RowData>
where Item: 'static + Copy + Debug,
      Model: GenericModel<Item> + ListModelExt,
      RowData: GenericRowData<Item> + IsA<Object> + Cast,
      Capture: ItemSource<Item>
{
    pub fn new(capture: Arc<Mutex<Capture>>,
               #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
               on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, ModelError>
    {
        let mut cap = capture.lock().or(Err(ModelError::LockError))?;
        let (completion, item_count) = cap.item_children(None)?;
        let child_count = u32::try_from(item_count)?;
        Ok(TreeListModel {
            _marker: PhantomData,
            capture: capture.clone(),
            root: Rc::new(RefCell::new(RootNode {
                children: Children::new(child_count),
                complete: matches!(completion, CompletionStatus::Complete),
            })),
            regions: RefCell::new(BTreeMap::new()),
            #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
            on_item_update,
        })
    }

    fn row_count(&self) -> u32 {
        self.root.borrow().children().total_count
    }

    fn check(&self) -> Result<(), ModelError> {
        // Check that we have the expected number of rows in the region map.
        let expected_count = self.row_count();
        let actual_count = self.regions
            .borrow()
            .iter()
            .next_back()
            .map(|(start, region)| start + region.length)
            .unwrap_or(0);
        if expected_count != actual_count {
            Err(InternalError(format!(
                "Region map total row count is {}, expected {}",
                actual_count, expected_count)))
        } else {
            Ok(())
        }
    }

    pub fn set_expanded(&self,
                        model: &Model,
                        node_ref: &ItemNodeRc<Item>,
                        position: u32,
                        expanded: bool)
        -> Result<(), ModelError>
    {
        let mut node = node_ref.borrow_mut();

        if node.expanded() == expanded {
            return Ok(());
        }

        let parent_rc = node.parent
            .upgrade()
            .ok_or(ModelError::ParentDropped)?;

        let rows_affected = node.children.direct_count;
        let expanded_children = node.children.expanded.clone();

        // There cannot be any visible incomplete children at this point.
        node.children.incomplete.clear();

        drop(node);

        // The children of this node appear after its own row.
        let children_position = position + 1;

        if expanded {
            #[cfg(feature="debug-region-map")]
            println!("\nExpanding node at {}", position);
            // Update the region map for the added children.
            self.expand(children_position, node_ref)?;
        } else {
            #[cfg(feature="debug-region-map")]
            println!("\nCollapsing node at {}", position);
            // If collapsing, first recursively collapse children of this node.
            for (index, child_ref) in expanded_children {
                let child_position = children_position + index;
                #[cfg(feature="debug-region-map")]
                println!("\nRecursively collapsing child at {}",
                         child_position);
                self.set_expanded(model, &child_ref, child_position, false)?;
            }
            // Update the region map for the removed children.
            self.collapse(children_position, node_ref)?;
        }

        // Merge adjacent regions with the same source.
        self.merge_regions();

        // Add or remove this node from the parent's expanded children.
        parent_rc
            .borrow_mut()
            .children_mut()
            .set_expanded(node_ref, expanded);

        // Traverse back up the tree, modifying `children.total_count` for
        // expanded/collapsed entries.
        node_ref.update_total(expanded, rows_affected)?;

        if expanded {
            model.items_changed(children_position, 0, rows_affected);
        } else {
            model.items_changed(children_position, rows_affected, 0);
        }

        #[cfg(feature="debug-region-map")] {
            println!();
            println!("Region map after {}:",
                     if expanded {"expansion"} else {"collapse"});
            for (start, region) in self.regions.borrow().iter() {
                println!("{}: {:?}", start, region);
            }
        }

        self.check()?;

        Ok(())
    }

    fn expand(&self, position: u32, node_ref: &ItemNodeRc<Item>)
        -> Result<(), ModelError>
    {
        // Find the start of the parent region.
        let (&parent_start, _) = self.regions
            .borrow()
            .range(..position)
            .next_back()
            .ok_or_else(||
                InternalError(format!(
                    "No region before position {position}")))?;

        // Find position of the new region relative to its parent.
        let relative_position = position - parent_start;

        // Remove the parent region.
        let parent = self.regions
            .borrow_mut()
            .remove(&parent_start)
            .ok_or_else(||
                InternalError(format!(
                    "Parent not found at position {parent_start}")))?;

        // Remove all following regions, to iterate over later.
        let following_regions = self.regions
            .borrow_mut()
            .split_off(&parent_start)
            .into_iter();

        // Split the parent region and construct a new region between.
        let rows_added = self.split_parent(parent_start, &parent, node_ref,
            vec![Region {
                source: parent.source.clone(),
                offset: parent.offset,
                length: relative_position,
            }],
            Region {
                source: ChildrenOf(node_ref.clone()),
                offset: 0,
                length: node_ref.borrow().children.direct_count,
            },
            vec![Region {
                source: parent.source.clone(),
                offset: parent.offset + relative_position,
                length: parent.length - relative_position,
            }]
        )?;

        // Shift all remaining regions down by the added rows.
        for (start, region) in following_regions {
            self.insert_region(start + rows_added, region)?;
        }

        Ok(())
    }

    fn collapse(&self, position: u32, node_ref: &ItemNodeRc<Item>)
        -> Result<(), ModelError>
    {
        // Clone the region starting at this position.
        let region = self.regions
            .borrow()
            .get(&position)
            .ok_or_else(||
                InternalError(format!(
                    "No region to delete at position {position}")))?
            .clone();

        // Remove it with following regions, to iterate over and replace them.
        let mut following_regions = self.regions
            .borrow_mut()
            .split_off(&position)
            .into_iter();

        // Process the effects of removing this region.
        let rows_removed = match &region.source {
            // Root regions cannot be collapsed.
            TopLevelItems() => return Err(
                InternalError(String::from(
                    "Unable to collapse root region"))),
            // Non-interleaved region is just removed.
            ChildrenOf(_) => {
                let (_, _region) = following_regions.next().unwrap();
                #[cfg(feature="debug-region-map")] {
                    println!();
                    println!("Removing: {:?}", _region);
                }
                node_ref.borrow().children.direct_count
            }
        };

        // Shift all following regions up by the removed rows.
        for (start, region) in following_regions {
            self.insert_region(start - rows_removed, region)?;
        }

        Ok(())
    }

    fn insert_region(&self, position: u32, region: Region<Item>)
        -> Result<(), ModelError>
    {
        match self.regions.borrow_mut().entry(position) {
            Entry::Occupied(mut entry) => {
                let old_region = entry.get();
                if old_region.length == 0 {
                    entry.insert(region);
                    Ok(())
                } else {
                    Err(InternalError(format!(
                        "At position {position}, overwriting region")))
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(region);
                Ok(())
            }
        }
    }

    fn split_parent(&self,
                    parent_start: u32,
                    parent: &Region<Item>,
                    _node_ref: &ItemNodeRc<Item>,
                    parts_before: Vec<Region<Item>>,
                    new_region: Region<Item>,
                    parts_after: Vec<Region<Item>>)
        -> Result<u32, ModelError>
    {
        let length_before: u32 = parts_before
            .iter()
            .map(|region| region.length)
            .sum();

        let length_after: u32 = parts_after
            .iter()
            .map(|region| region.length)
            .sum();

        let total_length = length_before + new_region.length + length_after;

        let rows_added = total_length - parent.length;
        let _rows_changed = parent.length - length_before - length_after;

        #[cfg(feature="debug-region-map")] {
            println!();
            println!("Splitting: {:?}", parent);
            for (i, region) in parts_before.iter().enumerate() {
                if i == 0 {
                    println!("   before: {:?}", region);
                } else {
                    println!("           {:?}", region);
                }
            }
            println!("      new: {:?}", new_region);
            for (i, region) in parts_after
                .iter()
                .filter(|region| region.length > 0)
                .enumerate()
            {
                if i == 0 {
                    println!("    after: {:?}", region);
                } else {
                    println!("           {:?}", region);
                }
            }
            println!("           {} rows added", rows_added);
        }

        let new_position = parent_start + length_before;
        let position_after = new_position + new_region.length;

        let mut position = parent_start;
        for region in parts_before {
            let length = region.length;
            self.insert_region(position, region)?;
            position += length;
        }

        self.insert_region(new_position, new_region)?;

        position = position_after;
        for region in parts_after
            .into_iter()
            .filter(|region| region.length > 0)
        {
            let length = region.length;
            self.insert_region(position, region)?;
            position += length;
        }

        Ok(rows_added)
    }

    fn merge_regions(&self) {
        #[cfg(feature="debug-region-map")] {
            println!();
            println!("Before merge:");
            for (start, region) in self.regions.borrow().iter() {
                println!("{}: {:?}", start, region);
            }
        }

        let new_regions = self.regions
            .borrow_mut()
            .split_off(&0)
            .into_iter()
            .coalesce(|(start_a, region_a), (start_b, region_b)|
                match Region::merge(&region_a, &region_b) {
                    Some(region_c) => {
                        #[cfg(feature="debug-region-map")] {
                            println!();
                            println!("Merging: {:?}", region_a);
                            println!("    and: {:?}", region_b);
                            println!("   into: {:?}", region_c);
                        }
                        Ok((start_a, region_c))
                    },
                    None => Err(((start_a, region_a), (start_b, region_b)))
                }
            )
            .collect();
        self.regions.replace(new_regions);
    }

    pub fn update(&self, model: &Model) -> Result<bool, ModelError> {
        #[cfg(feature="debug-region-map")]
        let rows_before = self.row_count();
        self.update_node(&self.root, 0, model)?;
        #[cfg(feature="debug-region-map")] {
            let rows_after = self.row_count();
            let rows_added = rows_after - rows_before;
            if rows_added > 0 {
                println!();
                println!("Region map after update adding {} rows:", rows_added);
                for (start, region) in self.regions.borrow().iter() {
                    println!("{}: {:?}", start, region);
                }
            }
        }

        self.check()?;

        Ok(!self.root.borrow().complete)
    }

    fn update_node<T>(&self,
                   node_rc: &Rc<RefCell<T>>,
                   mut position: u32,
                   model: &Model)
        -> Result<u32, ModelError>
        where T: Node<Item> + 'static,
              Rc<RefCell<T>>: NodeRcOps<Item>,
    {
        use CompletionStatus::*;

        // Extract details about the current node.
        let mut node = node_rc.borrow_mut();
        let expanded = node.expanded();
        let children = node.children();
        let old_direct_count = children.direct_count;
        let incomplete_children = children.incomplete
            .range(0..)
            .map(|(i, weak)| (*i, weak.clone()))
            .collect::<Vec<(u32, ItemNodeWeak<Item>)>>();

        // Check if this node had children added and/or was completed.
        let (completion, new_direct_count) = self.capture
            .lock()
            .or(Err(ModelError::LockError))?
            .item_children(node.item())?;
        let new_direct_count = new_direct_count as u32;
        let completed = matches!(completion, Complete);
        let children_added = new_direct_count - old_direct_count;

        // Deal with this node's own row, if it has one.
        if let Some(item_node) = node.item_node() {
            let mut cap = self.capture
                .lock()
                .or(Err(ModelError::LockError))?;

            // Check whether this item itself should be updated.
            let item_updated = if children_added > 0 {
                // Update due to added children.
                true
            } else if let Some(new_item) = cap.item_update(&item_node.item)? {
                // Update due to new version of item.
                item_node.item = new_item;
                true
            } else {
                // No update.
                false
            };

            if item_updated {
                // The node's description may change.
                let summary = cap.summary(&item_node.item)?;
                #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
                (self.on_item_update.borrow_mut())(position, summary.clone());
                for widget in item_node.widgets.borrow().iter() {
                    widget.set_text(summary.clone());
                    // If there were no previous children, the row was not
                    // previously expandable.
                    if children_added > 0 && old_direct_count == 0 {
                        widget.expander().set_visible(true);
                    }
                }
            }

            // Advance past this node's own row.
            position += 1;
        }

        // If completed, remove from incomplete node list.
        if completed {
            node.set_completed();
        }

        // Release our borrow on the node, as it may be needed by other calls.
        drop(node);

        if expanded {
            // Deal with incomplete children of this node.
            let mut last_index = 0;
            for (index, child_weak) in incomplete_children {
                if let Some(child_rc) = child_weak.upgrade() {
                    // Advance position up to this child.
                    position += node_rc
                        .borrow()
                        .children()
                        .rows_between(last_index, index);
                    // Recursively update this child.
                    position = self.update_node::<ItemNode<Item>>(
                        &child_rc, position, model)?;
                    last_index = index + 1;
                } else {
                    // Child no longer referenced, remove it.
                    node_rc
                        .borrow_mut()
                        .children_mut()
                        .incomplete
                        .remove(&index);
                }
            }

            // Advance to the end of this node's existing children.
            position += node_rc
                .borrow_mut()
                .children_mut()
                .rows_between(last_index, old_direct_count);
        }

        // Now deal with any new children of this node.
        if children_added > 0 {
            // Update this node's child counts.
            let mut node = node_rc.borrow_mut();
            let mut children = node.children_mut();
            children.direct_count += children_added;
            children.total_count += children_added;
            drop(node);

            if expanded {
                #[cfg(feature="debug-region-map")]
                println!("\nAdding {} new children at {}",
                         children_added, position);

                // Move the following regions down to make space.
                let following_regions = self.regions
                    .borrow_mut()
                    .split_off(&position);
                for (start, region) in following_regions {
                    self.regions
                        .borrow_mut()
                        .insert(start + children_added, region);
                }

                // Insert a new region with the new children.
                self.insert_region(position, Region {
                    source: node_rc.source(),
                    offset: old_direct_count,
                    length: children_added
                })?;

                self.merge_regions();

                // Update total counts for parent nodes.
                node_rc.update_total(true, children_added)?;

                // Add rows for the new children.
                model.items_changed(position, 0, children_added);

                // Update the position to continue from.
                position += children_added;
            }
        }

        // Return the position after all of this node's rows.
        Ok(position)
    }

    fn fetch(&self, position: u32) -> Result<ItemNodeRc<Item>, ModelError> {
        let mut parent_ref: Rc<RefCell<dyn Node<Item>>> = self.root.clone();
        let mut relative_position = position;
        'outer: loop {
            for (_, node_rc) in parent_ref
                .clone()
                .borrow()
                .children()
                .iter_expanded()
            {
                let node = node_rc.borrow();
                // If the position is before this node, break out of the loop to look it up.
                if relative_position < node.item_index {
                    break;
                // If the position matches this node, return it.
                } else if relative_position == node.item_index {
                    return Ok(node_rc.clone());
                // If the position is within this node's children, traverse down the tree and repeat.
                } else if relative_position <= node.item_index + node.children.total_count {
                    parent_ref = node_rc.clone();
                    relative_position -= node.item_index + 1;
                    continue 'outer;
                // Otherwise, if the position is after this node,
                // adjust the relative position for the node's children above.
                } else {
                    relative_position -= node.children.total_count;
                }
            }
            break;
        }

        // If we've broken out to this point, the node must be directly below `parent` - look it up.

        // First, check if we already have an incomplete node for this item.
        if let Some(node_rc) = parent_ref
            .borrow()
            .children()
            .fetch_incomplete(relative_position)
        {
            return Ok(node_rc)
        }

        // Otherwise, fetch it from the database.
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        let mut parent = parent_ref.borrow_mut();
        let item = cap.item(parent.item(), relative_position as u64)?;
        let (completion, child_count) = cap.item_children(Some(&item))?;
        let node = ItemNode {
            item,
            parent: Rc::downgrade(&parent_ref),
            item_index: relative_position,
            children: Children::new(child_count.try_into()?),
            widgets: RefCell::new(HashSet::new()),
        };
        let node_rc = Rc::new(RefCell::new(node));
        if matches!(completion, CompletionStatus::Ongoing) {
            parent
                .children_mut()
                .add_incomplete(relative_position, &node_rc);
        }
        Ok(node_rc)
    }

    // The following methods correspond to the ListModel interface, and can be
    // called by a GObject wrapper class to implement that interface.

    pub fn n_items(&self) -> u32 {
        self.row_count()
    }

    pub fn item(&self, position: u32) -> Option<Object> {
        // First check that the position is valid (must be within the root
        // node's total child count).
        if position >= self.row_count() {
            return None
        }
        let node_or_err_msg = self.fetch(position).map_err(|e| format!("{e:?}"));
        let row_data = RowData::new(node_or_err_msg);
        Some(row_data.upcast::<Object>())
    }
}
