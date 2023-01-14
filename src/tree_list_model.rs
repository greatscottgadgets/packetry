use std::cell::RefCell;
use std::cmp::min;
use std::collections::{BTreeMap, HashSet};
use std::collections::btree_map::Entry;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::num::TryFromIntError;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::ops::{DerefMut, Range};

use gtk::prelude::{IsA, Cast, WidgetExt};
use gtk::glib::Object;
use gtk::gio::prelude::ListModelExt;

use derive_more::AddAssign;
use itertools::Itertools;
use thiserror::Error;

use crate::capture::{
    Capture,
    CaptureError,
    ItemSource,
    CompletionStatus,
    SearchResult,
};
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

    /// Update this node's completion status.
    fn set_completion(&mut self, completion: CompletionStatus);
}

struct Children<Item> {
    /// Number of direct children below this node.
    direct_count: u64,

    /// Total number of displayed rows below this node, recursively.
    total_count: u64,

    /// Expanded children of this item.
    expanded: BTreeMap<u64, ItemNodeRc<Item>>,

    /// Incomplete children of this item.
    incomplete: BTreeMap<u64, ItemNodeWeak<Item>>,
}

impl<Item> Children<Item> {
    fn new(child_count: u64) -> Self {
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
    item_index: u64,

    /// Completion status of this node.
    completion: CompletionStatus,

    /// Children of this item.
    children: Children<Item>,

    /// Widgets to update when this item changes.
    widgets: RefCell<HashSet<ExpanderWrapper>>,
}

impl<Item> Children<Item> {
    /// Whether this child is expanded.
    fn expanded(&self, index: u64) -> bool {
        self.expanded.contains_key(&index)
    }

    /// Get the expanded child with the given index.
    fn get_expanded(&self, index: u64) -> Option<ItemNodeRc<Item>> {
        self.expanded.get(&index).map(Rc::clone)
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
    fn add_incomplete(&mut self, index: u64, child_rc: &ItemNodeRc<Item>) {
        self.incomplete.insert(index, Rc::downgrade(child_rc));
    }

    /// Fetch an incomplete child.
    fn fetch_incomplete(&self, index: u64) -> Option<ItemNodeRc<Item>> {
        self.incomplete.get(&index).and_then(Weak::upgrade)
    }

    /// Get the number of rows between two children.
    fn rows_between(&self, start: u64, end: u64) -> u64 {
        (end - start) +
            self.expanded
                .range(start..end)
                .map(|(_, node_rc)| node_rc.borrow().children.total_count)
                .sum::<u64>()
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

    fn set_completion(&mut self, completion: CompletionStatus) {
        self.complete = completion.is_complete();
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

    fn set_completion(&mut self, completion: CompletionStatus) {
        if completion.is_complete() {
            if let Some(parent_rc) = self.parent.upgrade() {
                parent_rc
                    .borrow_mut()
                    .children_mut()
                    .incomplete
                    .remove(&self.item_index);
            }
        }
        self.completion = completion;
    }
}

trait UpdateTotal<Item> {
    fn update_total(&self, expanded: bool, rows_affected: u64)
        -> Result<(), ModelError>;
}

impl<T, Item> UpdateTotal<Item> for Rc<RefCell<T>>
where T: Node<Item> + 'static, Item: Copy + 'static
{
    fn update_total(&self, expanded: bool, rows_affected: u64)
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
    fn item_node_rc(&self) -> Option<ItemNodeRc<Item>>;
}

impl<Item> NodeRcOps<Item> for RootNodeRc<Item>
where Item: Copy + 'static
{
    fn source(&self) -> Source<Item> {
        TopLevelItems()
    }

    fn item_node_rc(&self) -> Option<ItemNodeRc<Item>> {
        None
    }
}

impl<Item> NodeRcOps<Item> for ItemNodeRc<Item>
where Item: Copy + 'static
{
    fn source(&self) -> Source<Item> {
        ChildrenOf(self.clone())
    }

    fn item_node_rc(&self) -> Option<ItemNodeRc<Item>> {
        Some(self.clone())
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
struct ItemNodeSet<Item> {
    items: BTreeMap<u64, ItemNodeRc<Item>>,
}

impl<Item> ItemNodeSet<Item> where Item: Copy {
    fn new(node_rc: &ItemNodeRc<Item>) -> Self {
        let mut new = Self {
            items: BTreeMap::new()
        };
        new.add(node_rc);
        new
    }

    fn add(&mut self, node_rc: &ItemNodeRc<Item>) {
        let index = node_rc.borrow().item_index;
        self.items.insert(index, node_rc.clone());
    }

    fn remove(&mut self, node_rc: &ItemNodeRc<Item>) {
        let index = node_rc.borrow().item_index;
        self.items.remove(&index);
    }

    fn with(&self, node_rc: &ItemNodeRc<Item>) -> Self {
        let mut new = self.clone();
        new.add(node_rc);
        new
    }

    fn without(&self, node_rc: &ItemNodeRc<Item>) -> Self {
        let mut new = self.clone();
        new.remove(node_rc);
        new
    }

    fn any_incomplete(&self) -> bool {
        self.items
            .values()
            .any(|node_rc| !node_rc.borrow().completion.is_complete())
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn iter_items(&self) -> impl Iterator<Item=(u64, Item)> + '_ {
        self.items
            .iter()
            .map(|(index, node_rc)| (*index, node_rc.borrow().item))
    }
}

impl<Item> PartialEq for ItemNodeSet<Item> {
    fn eq(&self, other: &Self) -> bool {
        self.items.len() == other.items.len() &&
            self.items.values()
                .zip(other.items.values())
                .all(|(a, b)| Rc::ptr_eq(a, b))
    }
}

impl<Item> Debug for ItemNodeSet<Item>
where Item: Clone + Debug
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)
        -> Result<(), std::fmt::Error>
    {
        write!(f, "{:?}",
            self.items
                .values()
                .map(|rc| rc.borrow().item.clone())
                .collect::<Vec<Item>>())
    }
}

#[derive(Clone)]
enum Source<Item> {
    TopLevelItems(),
    ChildrenOf(ItemNodeRc<Item>),
    InterleavedSearch(ItemNodeSet<Item>, Range<u64>),
}

use Source::*;

#[derive(Clone)]
struct Region<Item> {
    source: Source<Item>,
    offset: u64,
    length: u64,
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
            InterleavedSearch(expanded, range) =>
                write!(f, "Interleaved search in {range:?} from {expanded:?}"),
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
            (InterleavedSearch(exp_a, range_a),
             InterleavedSearch(exp_b, range_b)) if exp_a == exp_b => Some(
                    Region {
                        source: InterleavedSearch(
                            exp_a.clone(),
                            range_a.start..range_b.end),
                        offset: region_a.offset,
                        length: region_a.length + region_b.length,
                    }
                ),
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

#[derive(Default, AddAssign)]
struct ModelUpdate {
    rows_added: u64,
    rows_removed: u64,
    rows_changed: u64,
}

impl std::fmt::Display for ModelUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} added, {} removed, {} changed",
               self.rows_added, self.rows_removed, self.rows_changed)
    }
}

enum InterleavedUpdate<Item> {
    AddedOngoing(ItemNodeRc<Item>, u64),
    AddedComplete(ItemNodeRc<Item>, u64, u64),
}

enum UpdateState<Item> {
    Root(Vec<InterleavedUpdate<Item>>),
    Interleaved(Option<InterleavedUpdate<Item>>),
    Contiguous(),
}

pub struct TreeListModel<Item, Model, RowData, Cursor> {
    _marker: PhantomData<(Model, RowData, Cursor)>,
    capture: Arc<Mutex<Capture>>,
    root: RootNodeRc<Item>,
    regions: RefCell<BTreeMap<u64, Region<Item>>>,
    #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
    on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>,
}

impl<Item, Model, RowData, Cursor> TreeListModel<Item, Model, RowData, Cursor>
where Item: 'static + Copy + Debug,
      Model: GenericModel<Item> + ListModelExt,
      RowData: GenericRowData<Item> + IsA<Object> + Cast,
      Capture: ItemSource<Item, Cursor>
{
    pub fn new(capture: Arc<Mutex<Capture>>,
               #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
               on_item_update: Rc<RefCell<dyn FnMut(u32, String)>>)
        -> Result<Self, ModelError>
    {
        let mut cap = capture.lock().or(Err(ModelError::LockError))?;
        let (completion, item_count) = cap.item_children(None)?;
        Ok(TreeListModel {
            _marker: PhantomData,
            capture: capture.clone(),
            root: Rc::new(RefCell::new(RootNode {
                children: Children::new(item_count),
                complete: completion.is_complete(),
            })),
            regions: RefCell::new(BTreeMap::new()),
            #[cfg(any(feature="test-ui-replay", feature="record-ui-test"))]
            on_item_update,
        })
    }

    fn row_count(&self) -> u64 {
        self.root.borrow().children().total_count
    }

    fn item_count(&self) -> u64 {
        self.root.borrow().children().direct_count
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
                "Region map total row count is {actual_count}, \
                 expected {expected_count}")))
        } else {
            Ok(())
        }
    }

    pub fn set_expanded(&self,
                        model: &Model,
                        node_ref: &ItemNodeRc<Item>,
                        position: u64,
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
        let interleaved = node.completion.is_interleaved();

        // There cannot be any visible incomplete children at this point.
        node.children.incomplete.clear();

        drop(node);

        // The children of this node appear after its own row.
        let children_position = position + 1;

        let update = if expanded {
            #[cfg(feature="debug-region-map")]
            println!("\nExpanding node at {}", position);
            // Update the region map for the added children.
            self.expand(children_position, node_ref)?
        } else {
            #[cfg(feature="debug-region-map")]
            println!("\nCollapsing node at {}", position);
            if interleaved {
                // Collapse expanded children of this node, from last to first.
                let mut end_position = self.row_count();
                for child_ref in expanded_children.into_values().rev() {
                    // Since this node's children are interleaved, we find each
                    // expanded child's position by searching the region map
                    // in reverse order, for the first region containing the
                    // expanded child's own children. The search window extends
                    // from the parent node to either the end of the region map,
                    // or the previous collapsed child, shrinking each time.
                    let child_position =
                        self.find_expanded(position..end_position, &child_ref)?;
                    // Update end position for next search.
                    end_position = child_position;
                    // Collapse this child.
                    #[cfg(feature="debug-region-map")]
                    println!("\nRecursively collapsing child at {}",
                             child_position);
                    self.set_expanded(
                        model, &child_ref, child_position, false)?;
                }
            } else {
                // If this node's children are not interleaved, each child's
                // position is at a simple offset, provided we collapse them
                // from first to last.
                for (index, child_ref) in expanded_children {
                    let child_position = children_position + index;
                    #[cfg(feature="debug-region-map")]
                    println!("\nRecursively collapsing child at {}",
                             child_position);
                    self.set_expanded(
                        model, &child_ref, child_position, false)?;
                }
            }
            // Update the region map for the removed children.
            self.collapse(children_position, node_ref)?
        };

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

        #[cfg(feature="debug-region-map")] {
            println!();
            println!("Region map after {}:",
                     if expanded {"expansion"} else {"collapse"});
            for (start, region) in self.regions.borrow().iter() {
                println!("{}: {:?}", start, region);
            }
        }

        self.check()?;

        // Update model.
        self.apply_update(model, children_position, update);

        Ok(())
    }

    fn find_expanded(&self, range: Range<u64>, node_rc: &ItemNodeRc<Item>)
        -> Result<u64, ModelError>
    {
        self.regions
            .borrow()
            .range(range)
            .rev()
            .find_map(|(start, region)| {
                match (region.offset, &region.source) {
                    (0, ChildrenOf(rc))
                        if Rc::ptr_eq(rc, node_rc)
                            // The node appears on the row before
                            // its first children.
                            => Some(*start - 1),
                    _ => None
                }
            })
            .ok_or_else(||
                InternalError(String::from(
                    "No region found for expanded child node")))
    }

    fn find_top_level_item(&self, node_rc: &ItemNodeRc<Item>)
        -> Result<u64, ModelError>
    {
        let index = node_rc.borrow().item_index;
        for (start, region) in self.regions.borrow().iter() {
            match &region.source {
                TopLevelItems() if
                    region.offset <= index &&
                    region.length > index - region.offset  =>
                {
                    return Ok(start + index - region.offset);
                },
                InterleavedSearch(expanded, range) if
                    range.start < index &&
                    range.end > index  =>
                {
                    let count_range = range.start..index;
                    let position =
                        self.count_items(&count_range)? +
                        self.count_within(expanded, &count_range)?;
                    if position >= region.offset &&
                       position < region.offset + region.length
                    {
                        return Ok(start + position - region.offset)
                    }
                }
                _ => {}
            }
        }
        Err(InternalError(String::from("Item not found")))
    }

    fn expand(&self, position: u64, node_ref: &ItemNodeRc<Item>)
        -> Result<ModelUpdate, ModelError>
    {
        // Extract some details of the node being expanded.
        let node = node_ref.borrow();
        let node_start = node.item_index;
        let next_item = node_start + 1;
        let interleaved = node.completion.is_interleaved();

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
        let mut following_regions = self.regions
            .borrow_mut()
            .split_off(&parent_start);

        let more_after = !following_regions.is_empty();

        // Split the parent region and construct a new region between.
        //
        // Where the new region is an interleaved one, its overlap with the
        // remainder of the parent is handled in the split_parent call.
        let mut update = match (interleaved, &parent.source) {
            // Self-contained region expanded.
            (false, _) => {
                self.split_parent(parent_start, &parent, node_ref, more_after,
                    vec![Region {
                        source: parent.source.clone(),
                        offset: parent.offset,
                        length: relative_position,
                    }],
                    Region {
                        source: ChildrenOf(node_ref.clone()),
                        offset: 0,
                        length: node.children.direct_count,
                    },
                    vec![Region {
                        source: parent.source.clone(),
                        offset: parent.offset + relative_position,
                        length: parent.length - relative_position,
                    }]
                )?
            },
            // Interleaved region expanded from within a root region.
            (true, TopLevelItems()) => {
                let expanded = ItemNodeSet::new(node_ref);
                let range = node_start..next_item;
                let added = self.count_within(&expanded, &range)?;
                if parent.length == 1 &&
                    parent_start + parent.length != self.row_count()
                {
                    // There's nothing to split, and there must already be an
                    // interleaved region after this, so keep the parent as is.
                    let mut update = ModelUpdate::default();
                    self.preserve_region(
                        &mut update, parent_start, &parent, false)?;
                    update
                } else {
                    self.split_parent(
                        parent_start, &parent, node_ref, more_after,
                        vec![Region {
                            source: TopLevelItems(),
                            offset: parent.offset,
                            length: relative_position,
                        }],
                        Region {
                            source: InterleavedSearch(expanded, range),
                            offset: 0,
                            length: added,
                        },
                        vec![Region {
                            source: TopLevelItems(),
                            offset: parent.offset + relative_position,
                            length: parent.length - relative_position,
                        }]
                    )?
                }
            },
            // New interleaved region expanded from within an existing one.
            (true, InterleavedSearch(parent_expanded, parent_range)) => {
                let range_1 = parent_range.start..node_start;
                let range_2 = node_start..next_item;
                let range_3 = next_item..parent_range.end;
                let expanded_1 = parent_expanded.clone();
                let expanded_2 = parent_expanded.with(node_ref);
                let expanded_3 = parent_expanded.clone();
                let total_changed = self.count_within(parent_expanded, &range_2)?;
                let rows_present = parent.length - relative_position;
                let (changed, added) = if rows_present >= total_changed {
                    let changed = total_changed;
                    let added = self.count_in_range(&range_2, node_ref)?;
                    (changed, added)
                } else {
                    let changed = rows_present;
                    let added = self.count_to_item(
                        parent_expanded, &range_2, rows_present - 1, node_ref)?;
                    (changed, added)
                };
                if parent.offset != 0 {
                    // Update the range end of previous parts of this parent.
                    for region in self.regions.borrow_mut().values_mut().rev() {
                        match &mut region.source {
                            TopLevelItems() => break,
                            ChildrenOf(..) => continue,
                            InterleavedSearch(_, range)
                                if range.end > node_start =>
                            {
                                range.end = node_start;
                            },
                            InterleavedSearch(..) => break
                        }
                    }
                }
                if total_changed > rows_present {
                    // Update the range start of following parts of this parent.
                    for region in following_regions.values_mut() {
                        match &mut region.source {
                            TopLevelItems() => break,
                            ChildrenOf(..) => continue,
                            InterleavedSearch(_, range)
                                if range.start < node_start =>
                            {
                                range.start = node_start;
                                region.offset -= relative_position;
                            },
                            InterleavedSearch(..) => break
                        }
                    }
                }
                self.split_parent(parent_start, &parent, node_ref, more_after,
                    vec![
                        Region {
                            source: InterleavedSearch(expanded_1, range_1),
                            offset: parent.offset,
                            length: relative_position - 1,
                        },
                        Region {
                            source: TopLevelItems(),
                            offset: node_start,
                            length: 1,
                        }
                    ],
                    Region {
                        source: InterleavedSearch(expanded_2, range_2),
                        offset: 0,
                        length: changed + added,
                    },
                    if rows_present > changed {
                        vec![
                            Region {
                                source: TopLevelItems(),
                                offset: next_item,
                                length: 1,
                            },
                            Region {
                                source: InterleavedSearch(expanded_3, range_3),
                                offset: 0,
                                length: rows_present - changed - 1,
                            }
                        ]
                    } else {
                        vec![]
                    }
                )?
            },
            // Other combinations are not supported.
            (..) => return
                Err(InternalError(format!(
                    "Unable to expand from {parent:?}")))
        };

        drop(node);

        // For an interleaved source, update all regions that it overlaps.
        let mut following_regions = following_regions.into_iter();
        if interleaved {
            while let Some((start, region)) = following_regions.next() {
                let more_after = following_regions.len() > 0;
                // Do whatever is necessary to overlap this region.
                if !self.overlap_region(
                    &mut update, start, &region, node_ref, more_after)?
                {
                    // No further overlapping regions.
                    break;
                }
            }
        }

        // Shift all remaining regions down by the added rows.
        for (start, region) in following_regions {
            self.insert_region(start + update.rows_added, region)?;
        }

        Ok(update)
    }

    fn collapse(&self, position: u64, node_ref: &ItemNodeRc<Item>)
        -> Result<ModelUpdate, ModelError>
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
        let update = match &region.source {
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
                ModelUpdate {
                    rows_added: 0,
                    rows_removed: node_ref.borrow().children.direct_count,
                    rows_changed: 0,
                }
            },
            // For an interleaved source, update all overlapped regions.
            InterleavedSearch(..) => {
                let mut update = ModelUpdate::default();
                for (start, region) in following_regions.by_ref() {
                    // Do whatever is necessary to unoverlap this region.
                    if !self.unoverlap_region(
                        &mut update, start, &region, node_ref)?
                    {
                        // No further overlapping regions.
                        break;
                    }
                }
                update
            }
        };

        // Shift all following regions up by the removed rows.
        for (start, region) in following_regions {
            self.insert_region(start - update.rows_removed, region)?;
        }

        Ok(update)
    }

    fn overlap_region(&self,
                      update: &mut ModelUpdate,
                      start: u64,
                      region: &Region<Item>,
                      node_ref: &ItemNodeRc<Item>,
                      more_after: bool)
        -> Result<bool, ModelError>
    {
        use Source::*;

        let node_range = self.node_range(node_ref);

        Ok(match &region.source {
            TopLevelItems() if region.offset >= node_range.end => {
                // This region is not overlapped.
                self.preserve_region(update, start, region, false)?
            },
            InterleavedSearch(_, range) if range.start >= node_range.end => {
                // This region is not overlapped.
                self.preserve_region(update, start, region, false)?
            },
            ChildrenOf(_) => {
                // This region is overlapped but self-contained.
                self.preserve_region(update, start, region, true)?
            },
            TopLevelItems() if region.length == 1 => {
                // This region includes only a single root item. Check
                // whether it is overlapped by the new node.
                let overlapped = region.offset < node_range.end;
                // Either way, the region itself is unchanged.
                self.preserve_region(update, start, region, overlapped)?;
                // We may need to add a new interleaved region after this item
                // if it is overlapped, is the last item, and there is not
                // already an interleaved region to follow it.
                let item_count = self.item_count();
                let last_item = region.offset + 1 == item_count;
                if overlapped && last_item && !more_after {
                    let range = region.offset..item_count;
                    let added = self.count_in_range(&range, node_ref)?;
                    let expanded = ItemNodeSet::new(node_ref);
                    let trailing_region = Region {
                        source: InterleavedSearch(expanded, range),
                        offset: 0,
                        length: added,
                    };
                    #[cfg(feature="debug-region-map")] {
                        println!();
                        println!("Inserting: {:?}", trailing_region);
                    }
                    self.insert_region(
                        start + region.length + update.rows_added,
                        trailing_region
                    )?;
                    update.rows_added += added;
                }
                overlapped
            },
            TopLevelItems()
                if region.offset + region.length <= node_range.end =>
            {
                // This region is fully overlapped by the new node.
                // Replace with a new interleaved region.
                let end = region.offset + region.length;
                let contains_last_item = end == self.item_count();
                let expanded = ItemNodeSet::new(node_ref);
                if contains_last_item && !more_after {
                    // This region contains the last item, and there is not
                    // currently a following interleaved region. Create one
                    // that runs to the end of the model.
                    let range = region.offset..end;
                    let added = self.count_in_range(&range, node_ref)?;
                    self.replace_region(update, start, region,
                        vec![
                            Region {
                                source: TopLevelItems(),
                                offset: region.offset,
                                length: 1,
                            },
                            Region {
                                source: InterleavedSearch(expanded, range),
                                offset: 0,
                                length: region.length - 1 + added,
                            }
                        ],
                    )?;
                    false
                } else {
                    // There are further regions after this one, so just
                    // overlap up to the end of these top level items.
                    let range = region.offset..(end - 1);
                    let added = self.count_in_range(&range, node_ref)?;
                    self.replace_region(update, start, region,
                        vec![
                            Region {
                                source: TopLevelItems(),
                                offset: region.offset,
                                length: 1,
                            },
                            Region {
                                source: InterleavedSearch(expanded, range),
                                offset: 0,
                                length: region.length - 2 + added,
                            },
                            Region {
                                source: TopLevelItems(),
                                offset: region.offset + region.length - 1,
                                length: 1
                            }
                        ]
                    )?;
                    true
                }
            },
            TopLevelItems() => {
                // This region is partially overlapped by the new node.
                // Split it into overlapped and unoverlapped parts.
                let expanded = ItemNodeSet::new(node_ref);
                let range = region.offset..node_range.end;
                let changed = self.count_items(&range)?;
                let added = self.count_in_range(&range, node_ref)?;
                self.partial_overlap(update, start, region,
                    vec![
                        Region {
                            source: TopLevelItems(),
                            offset: region.offset,
                            length: 1,
                        },
                        Region {
                            source: InterleavedSearch(expanded, range),
                            offset: 0,
                            length: changed + added
                        }
                    ],
                    vec![
                        Region {
                            source: TopLevelItems(),
                            offset: region.offset + changed + 1,
                            length: region.length - changed - 1,
                        }
                    ]
                )?;
                // No longer overlapping.
                false
            },
            InterleavedSearch(expanded, range)
                if range.end <= node_range.end =>
            {
                // This region is fully overlapped by the new node.
                // Replace with a new interleaved region.
                let (added_before_offset, added_after_offset) =
                    self.count_around_offset(
                        expanded, range, node_ref,
                        region.offset,
                        region.offset + region.length)?;
                let range = range.clone();
                let more_expanded = expanded.with(node_ref);
                self.replace_region(update, start, region,
                    vec![
                        Region {
                            source: InterleavedSearch(more_expanded, range),
                            offset: region.offset + added_before_offset,
                            length: region.length + added_after_offset,
                        }
                    ]
                )?;
                true
            },
            InterleavedSearch(expanded, range) => {
                // This region may be partially overlapped by the new node,
                // depending on its starting offset and length.
                let range_1 = range.start..node_range.end;
                let range_2 = node_range.end..range.end;
                // Work out the offset at which this source would be split.
                let split_offset = self.count_items(&range_1)? +
                    self.count_within(expanded, &range_1)?;
                if region.offset >= split_offset {
                    // This region begins after the split, so isn't overlapped.
                    self.preserve_region(update, start, region, false)?
                } else if region.offset + region.length <= split_offset {
                    // This region ends before the split, so is fully overlapped.
                    let (added_before_offset, added_after_offset) =
                        self.count_around_offset(
                            expanded, range, node_ref,
                            region.offset,
                            region.offset + region.length)?;
                    let range = range.clone();
                    let more_expanded = expanded.with(node_ref);
                    self.replace_region(update, start, region,
                        vec![
                            Region {
                                source: InterleavedSearch(more_expanded, range),
                                offset: region.offset + added_before_offset,
                                length: region.length + added_after_offset,
                            }
                        ]
                    )?;
                    true
                } else {
                    // This region straddles the split. Break it into overlapped
                    // and unoverlapped parts.
                    let changed = split_offset - region.offset;
                    let (added_before_offset, added_after_offset) =
                        self.count_around_offset(
                            expanded, range, node_ref,
                            region.offset, split_offset)?;
                    let expanded_1 = expanded.with(node_ref);
                    let expanded_2 = expanded.clone();
                    self.partial_overlap(update, start, region,
                        vec![
                            Region {
                                source: InterleavedSearch(expanded_1, range_1),
                                offset: region.offset + added_before_offset,
                                length: changed + added_after_offset,
                            }
                        ],
                        vec![
                            Region {
                                source: TopLevelItems(),
                                offset: node_range.end,
                                length: 1,
                            },
                            Region {
                                source: InterleavedSearch(expanded_2, range_2),
                                offset: 0,
                                length: region.length - changed - 1,
                            }
                        ]
                    )?;
                    // No longer overlapping.
                    false
                }
            }
        })
    }

    fn unoverlap_region(&self,
                        update: &mut ModelUpdate,
                        start: u64,
                        region: &Region<Item>,
                        node_ref: &ItemNodeRc<Item>)
        -> Result<bool, ModelError>
    {
        use Source::*;

        let node_range = self.node_range(node_ref);

        Ok(match &region.source {
            TopLevelItems() if region.offset >= node_range.end => {
                // This region is not overlapped.
                self.preserve_region(update, start, region, false)?
            },
            InterleavedSearch(_, range) if range.start >= node_range.end => {
                // This region is not overlapped.
                self.preserve_region(update, start, region, false)?
            },
            ChildrenOf(_) | TopLevelItems() => {
                // This region is overlapped but self-contained.
                self.preserve_region(update, start, region, true)?
            },
            InterleavedSearch(expanded, range) => {
                // This region is overlapped. Replace with a new one.
                let less_expanded = expanded.without(node_ref);
                let new_region = if less_expanded.is_empty() {
                    // This node was the last expanded one in this region.
                    Region {
                        source: TopLevelItems(),
                        offset: range.start + 1,
                        length: self.count_items(range)?,
                    }
                } else {
                    // There are other nodes expanded in this region.
                    let (removed_before_offset, removed_after_offset) =
                        self.count_around_offset(
                            expanded, range, node_ref,
                            region.offset,
                            region.offset + region.length)?;
                    let range = range.clone();
                    Region {
                        source: InterleavedSearch(less_expanded, range),
                        offset: region.offset - removed_before_offset,
                        length: region.length - removed_after_offset,
                    }
                };
                self.replace_region(update, start, region, vec![new_region])?;
                true
            }
        })
    }

    fn insert_region(&self, position: u64, region: Region<Item>)
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

    fn preserve_region(&self,
                       update: &mut ModelUpdate,
                       start: u64,
                       region: &Region<Item>,
                       include_as_changed: bool)
        -> Result<bool, ModelError>
    {
        let new_position = start
            + update.rows_added
            - update.rows_removed;

        self.insert_region(new_position, region.clone())?;

        if include_as_changed {
            update.rows_changed += region.length;
        }

        Ok(include_as_changed)
    }

    fn replace_region(&self,
                      update: &mut ModelUpdate,
                      start: u64,
                      region: &Region<Item>,
                      new_regions: Vec<Region<Item>>)
        -> Result<(), ModelError>
    {
        use std::cmp::Ordering::*;

        let new_length: u64 = new_regions
            .iter()
            .map(|region| region.length)
            .sum();

        let effect = match new_length.cmp(&region.length) {
            Greater => ModelUpdate {
                rows_added: new_length - region.length,
                rows_removed: 0,
                rows_changed: region.length
            },
            Less => ModelUpdate {
                rows_added: 0,
                rows_removed: region.length - new_length,
                rows_changed: new_length
            },
            Equal => ModelUpdate {
                rows_added: 0,
                rows_removed: 0,
                rows_changed: region.length
            },
        };

        #[cfg(feature="debug-region-map")]
        {
            println!();
            println!("Replacing: {:?}", region);
            for (i, new_region) in new_regions.iter().enumerate() {
                if i == 0 {
                    println!("     with: {:?}", new_region);
                } else {
                    println!("           {:?}", new_region);
                }
            }
            println!("           {}", effect);
        }

        let mut position = start
            + update.rows_added
            - update.rows_removed;

        for region in new_regions {
            let length = region.length;
            self.insert_region(position, region)?;
            position += length;
        }

        *update += effect;

        Ok(())
    }

    fn partial_overlap(&self,
                       update: &mut ModelUpdate,
                       start: u64,
                       region: &Region<Item>,
                       changed_regions: Vec<Region<Item>>,
                       unchanged_regions: Vec<Region<Item>>)
        -> Result<(), ModelError>
    {
        let changed_length: u64 = changed_regions
            .iter()
            .map(|region| region.length)
            .sum();

        let unchanged_length: u64 = unchanged_regions
            .iter()
            .map(|region| region.length)
            .sum();

        let total_length = changed_length + unchanged_length;

        let effect = ModelUpdate {
            rows_added: total_length - region.length,
            rows_removed: 0,
            rows_changed: region.length - unchanged_length,
        };

        #[cfg(feature="debug-region-map")]
        {
            println!();
            println!("Splitting: {:?}", region);
            for (i, changed_region) in changed_regions.iter().enumerate() {
                if i == 0 {
                    println!("  changed: {:?}", changed_region);
                } else {
                    println!("         : {:?}", changed_region);
                }
            }
            for (i, unchanged_region) in unchanged_regions.iter().enumerate() {
                if i == 0 {
                    println!("unchanged: {:?}", unchanged_region);
                } else {
                    println!("         : {:?}", unchanged_region);
                }
            }
            println!("           {}", effect);
        }

        let mut position = start + update.rows_added - update.rows_removed;
        for region in changed_regions
            .into_iter()
            .chain(unchanged_regions)
        {
            let length = region.length;
            self.insert_region(position, region)?;
            position += length;
        }

        *update += effect;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn split_parent(&self,
                    parent_start: u64,
                    parent: &Region<Item>,
                    node_ref: &ItemNodeRc<Item>,
                    more_after: bool,
                    parts_before: Vec<Region<Item>>,
                    new_region: Region<Item>,
                    mut parts_after: Vec<Region<Item>>)
        -> Result<ModelUpdate, ModelError>
    {
        let length_before: u64 = parts_before
            .iter()
            .map(|region| region.length)
            .sum();

        let length_after: u64 = parts_after
            .iter()
            .map(|region| region.length)
            .sum();

        let total_length = length_before + new_region.length + length_after;

        let rows_added = total_length - parent.length;
        let rows_changed = parent.length - length_before - length_after;

        let mut update = ModelUpdate {
            rows_added,
            rows_removed: 0,
            rows_changed,
        };

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
            println!("           {}", &update);
        }

        let interleaved = matches!(&new_region.source, InterleavedSearch(..));
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

        parts_after.retain(|region| region.length > 0);
        let mut add_after = parts_after.into_iter();
        let mut overlap = interleaved;
        while let Some(region) = add_after.next() {
            let length = region.length;
            if overlap {
                let more_after = more_after || add_after.len() > 0;
                overlap = self.overlap_region(&mut update,
                    position - rows_added,
                    &region,
                    node_ref,
                    more_after)?;
            } else {
                self.insert_region(position, region)?;
            }
            position += length;
        }

        Ok(update)
    }

    fn pre_merge(&self, printed: &mut bool) {
        if *printed {
            return
        }
        #[cfg(feature="debug-region-map")] {
            println!();
            println!("Before merge:");
            for (start, region) in self.regions.borrow().iter() {
                println!("{}: {:?}", start, region);
            }
        }
        *printed = true;
    }

    fn merge_pairs(&self, printed: &mut bool) {
        let new_regions = self.regions
            .borrow()
            .clone()
            .into_iter()
            .coalesce(|(start_a, region_a), (start_b, region_b)|
                match Region::merge(&region_a, &region_b) {
                    Some(region_c) => {
                        self.pre_merge(printed);
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

    fn merge_regions(&self) {
        let mut printed = false;

        // Merge adjacent regions with the same source.
        self.merge_pairs(&mut printed);

        // Find starts and lengths of superfluous root regions.
        let superfluous_regions: Vec<(u64, u64)> = self.regions
            .borrow()
            .iter()
            .tuple_windows()
            .filter_map(|((_, a), (b_start, b), (_, c))|
                 match (&a.source, &b.source, &c.source) {
                    (InterleavedSearch(exp_a, _),
                     TopLevelItems(),
                     InterleavedSearch(exp_c, _)) if exp_a == exp_c => {
                            self.pre_merge(&mut printed);
                            #[cfg(feature="debug-region-map")]
                            {
                                println!();
                                println!("Dropping: {:?}", b);
                            }
                            Some((*b_start, b.length))
                        },
                    _ => None
                 })
            .collect();

        // Remove superfluous regions.
        for (start, length) in superfluous_regions {
            let mut regions = self.regions.borrow_mut();
            regions.remove(&start);
            let (_, prev_region) = regions
                .range_mut(..start)
                .next_back()
                .unwrap();
            prev_region.length += length;
        }

        // Once again merge adjacent regions with the same source.
        self.merge_pairs(&mut printed);
    }

    fn node_range(&self, node_ref: &ItemNodeRc<Item>) -> Range<u64> {
        use CompletionStatus::*;
        let node = node_ref.borrow();
        let start = node.item_index;
        let end = match node.completion {
            InterleavedComplete(index) => index,
            InterleavedOngoing => self.item_count(),
            _ => start,
        };
        start..end
    }

    fn count_items(&self, range: &Range<u64>) -> Result<u64, ModelError> {
        let length = range.end - range.start;
        if length == 0 {
            Err(InternalError(String::from("Range is empty")))
        } else {
            Ok(length - 1)
        }
    }

    fn count_to_item(&self,
                     expanded: &ItemNodeSet<Item>,
                     range: &Range<u64>,
                     to_index: u64,
                     node_ref: &ItemNodeRc<Item>)
        -> Result<u64, ModelError>
    {
        use SearchResult::*;
        let node = node_ref.borrow();
        let item_index = node.item_index;
        let item = &node.item;
        let mut expanded = expanded.iter_items();
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        let search_result = cap.find_child(&mut expanded, range, to_index)?;
        Ok(match search_result {
            (TopLevelItem(found_index, _), _cursor) => {
                let range_to_item = range.start..found_index;
                cap.count_within(item_index, item, &range_to_item)?
            },
            (NextLevelItem(span_index, .., child), _cursor) => {
                let range_to_span = range.start..span_index;
                cap.count_within(item_index, item, &range_to_span)? +
                    cap.count_before(item_index, item, span_index, &child)?
            },
        })
    }

    fn count_in_range(&self,
                      range: &Range<u64>,
                      node_ref: &ItemNodeRc<Item>)
        -> Result<u64, ModelError>
    {
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        let node = node_ref.borrow();
        Ok(cap.count_within(node.item_index, &node.item, range)?)
    }

    fn count_around_offset(&self,
                           expanded: &ItemNodeSet<Item>,
                           range: &Range<u64>,
                           node_ref: &ItemNodeRc<Item>,
                           offset: u64,
                           end: u64)
        -> Result<(u64, u64), ModelError>
    {
        let length =
            self.count_items(range)? + self.count_within(expanded, range)?;
        let rows_before_offset = if offset == 0 {
            0
        } else {
            self.count_to_item(expanded, range, offset - 1, node_ref)?
        };
        let rows_before_end = if end == length {
            self.count_in_range(range, node_ref)?
        } else {
            self.count_to_item(expanded, range, end, node_ref)?
        };
        let rows_after_offset = rows_before_end - rows_before_offset;
        Ok((rows_before_offset, rows_after_offset))
    }

    fn count_within(&self,
                    expanded: &ItemNodeSet<Item>,
                    range: &Range<u64>)
        -> Result<u64, ModelError>
    {
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        Ok(expanded
            .iter_items()
            .map(|(index, item)| cap.count_within(index, &item, range))
            .collect::<Result<Vec<u64>, CaptureError>>()?
            .iter()
            .sum())
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
                   mut position: u64,
                   model: &Model)
        -> Result<(u64, Option<InterleavedUpdate<Item>>), ModelError>
        where T: Node<Item> + 'static,
              Rc<RefCell<T>>: NodeRcOps<Item>,
    {
        use InterleavedUpdate::*;
        use UpdateState::*;

        // Extract details about the current node.
        let node = node_rc.borrow();
        let expanded = node.expanded();
        let children = node.children();
        let old_direct_count = children.direct_count;
        let incomplete_children = children.incomplete
            .range(0..)
            .map(|(i, weak)| (*i, weak.clone()))
            .collect::<Vec<(u64, ItemNodeWeak<Item>)>>();

        // Check if this node had children added and/or was completed.
        let (completion, new_direct_count) = self.capture
            .lock()
            .or(Err(ModelError::LockError))?
            .item_children(node.item())?;
        let new_direct_count = new_direct_count;
        let children_added = new_direct_count - old_direct_count;
        drop(node);

        let mut state = if let Some(item_node_rc) = node_rc.item_node_rc() {
            // This is an item node.
            let mut item_node = item_node_rc.borrow_mut();

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
                if let Ok(position) = u32::try_from(position) {
                    let mut on_item_update = self.on_item_update.borrow_mut();
                    on_item_update(position, summary.clone());
                }
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

            // Construct node state for remaining steps.
            use CompletionStatus::*;
            let children_to_add = if expanded && children_added > 0 {
                Some(children_added)
            } else {
                None
            };
            match (item_node.completion, children_to_add) {
                (InterleavedComplete(end), Some(count)) => {
                    drop(item_node);
                    Interleaved(Some(AddedComplete(item_node_rc, count, end)))
                },
                (InterleavedOngoing, Some(count)) => {
                    drop(item_node);
                    Interleaved(Some(AddedOngoing(item_node_rc, count)))
                },
                (InterleavedComplete(_) | InterleavedOngoing, None) =>
                    Interleaved(None),
                (..) =>
                    Contiguous()
            }
        } else {
            Root(Vec::new())
        };

        // Update node's completion status.
        node_rc.borrow_mut().set_completion(completion);

        if expanded {
            // Deal with incomplete children of this node.
            let mut last_index = 0;
            for (index, child_weak) in incomplete_children {
                if let Some(child_rc) = child_weak.upgrade() {
                    // Advance position to this child.
                    position = match state {
                        Root(..) =>
                            // Find position of top level item from region map.
                            self.find_top_level_item(&child_rc)?,
                        Interleaved(..) => {
                            // There can be only one incomplete child of an
                            // item whose children are interleaved. If it is
                            // expanded, we can find it from the region map.
                            // Otherwise, it must always be at the last row.
                            let end = self.row_count();
                            if child_rc.borrow().expanded() {
                                self.find_expanded(position..end, &child_rc)?
                            } else {
                                end - 1
                            }
                        },
                        Contiguous(..) => {
                            // If the children of this node are contiguous, we
                            // can advance to another child by summing the
                            // total rows of the intermediate children.
                            let delta = node_rc
                                .borrow()
                                .children()
                                .rows_between(last_index, index);
                            last_index = index + 1;
                            position + delta
                        },
                    };
                    // Recursively update the child.
                    let (new_position, update) =
                        self.update_node::<ItemNode<Item>>(
                            &child_rc, position, model)?;
                    // If the child has a pending interleaved update, store it.
                    if let (Root(todo), Some(update)) = (&mut state, update) {
                        todo.push(update);
                    }
                    // Advance position to after this child and its children.
                    position = new_position;
                } else {
                    // Child no longer referenced, remove it.
                    node_rc
                        .borrow_mut()
                        .children_mut()
                        .incomplete
                        .remove(&index);
                }
            }

            if matches!(state, Contiguous()) {
                // Advance to the end of this node's existing children.
                position += node_rc
                    .borrow_mut()
                    .children_mut()
                    .rows_between(last_index, old_direct_count);
            }
        }

        // Now deal with any new children of this node.
        if let Root(interleaved_updates) = &state {
            // Updating the root node. New rows are added after all others.
            let initial_position = self.row_count();
            position = initial_position;

            // Running totals of rows pending and added.
            let mut top_level_added = 0;
            let mut top_level_pending = children_added;
            let mut second_level_added = 0;
            let mut second_level_pending = 0;

            // Collect completed items, and count pending second level items.
            let mut completed = BTreeMap::new();
            for update in interleaved_updates {
                match update {
                    AddedComplete(child_rc, children_added, end) => {
                        completed.insert(*end, child_rc);
                        second_level_pending += children_added;
                    },
                    AddedOngoing(_child_rc, children_added) => {
                        second_level_pending += children_added;
                    }
                }
            }

            // Handle completed items.
            let mut completed = completed.into_iter();
            if let Some((child_end, child_rc)) = completed.next() {

                // Find the last interleaved region.
                let (mut expanded, range, old_length) = self.regions
                    .borrow_mut()
                    .values_mut()
                    .rev()
                    .find_map(|region|
                        if let InterleavedSearch(expanded, range) =
                            &mut region.source
                        {
                            // Copy all the fields.
                            let fields = (
                                expanded.clone(),
                                range.clone(),
                                region.offset + region.length,
                            );
                            // Extend the search range to the new end.
                            range.end = child_end;
                            // Return the copied fields.
                            Some(fields)
                        } else {
                            None
                        }
                    )
                    .ok_or_else(||
                        InternalError(String::from(
                            "No interleaved region found")))?;

                // Add a new region with the additional rows.
                let full_range = range.start..child_end;
                let old_top = self.count_items(&range)?;
                let full_top = self.count_items(&full_range)?;
                let full_second = self.count_within(&expanded, &full_range)?;
                let full_length = full_top + full_second;
                let new_length = full_length - old_length;
                let new_top = full_top - old_top;
                let new_second = new_length - new_top;
                let new_region = Region {
                    source: InterleavedSearch(expanded.clone(), full_range),
                    offset: old_length,
                    length: new_length,
                };
                self.insert_region(position, new_region)?;
                position += new_length;
                top_level_added += new_top;
                top_level_pending -= new_top;
                second_level_added += new_second;
                second_level_pending -= new_second;

                // Update for next child completion.
                let mut last_end = child_end;
                expanded = expanded.without(child_rc);

                // Now handle any further completed children.
                for (child_end, child_rc) in completed {
                    // Add a region for the in-between top level item.
                    self.insert_region(position, Region {
                        source: TopLevelItems(),
                        offset: last_end,
                        length: 1,
                    })?;
                    position += 1;
                    top_level_added += 1;
                    top_level_pending -= 1;

                    // Add a new interleaved region.
                    let range = last_end..child_end;
                    let new_top = self.count_items(&range)?;
                    let new_second = self.count_within(&expanded, &range)?;
                    let new_length = new_top + new_second;
                    self.insert_region(position, Region {
                        source: InterleavedSearch(
                            expanded.clone(), range.clone()),
                        offset: 0,
                        length: new_length,
                    })?;
                    position += new_length;
                    top_level_added += new_top;
                    top_level_pending -= new_top;
                    second_level_added += new_second;
                    second_level_pending -= new_second;

                    // Update for next completion.
                    last_end = child_end;
                    expanded = expanded.without(child_rc);
                }
            }

            // Add any further pending items at the end.
            if top_level_pending > 0 || second_level_pending > 0 {
                // Find the source and offset needed to extend the last region
                // that contains top level items, or default to a new top level
                // source if the map is empty.
                let (source, offset, old_end, old_length) = self.regions
                    .borrow_mut()
                    .values_mut()
                    .rev()
                    .find_map(|region| {
                        let old_length = region.offset + region.length;
                        Some(match &mut region.source {
                            TopLevelItems() if second_level_pending == 0 => {
                                let source = region.source.clone();
                                let old_end = region.offset + region.length;
                                let offset = old_end;
                                (source, offset, old_end, old_length)
                            },
                            InterleavedSearch(expanded, range) => {
                                let old_end = range.end;
                                if expanded.any_incomplete() {
                                    // Still ongoing. Extend its range
                                    // and continue it with a new region.
                                    range.end += top_level_pending;
                                    let source = InterleavedSearch(
                                        expanded.clone(), range.clone());
                                    let offset = region.offset + region.length;
                                    (source, offset, old_end, old_length)
                                } else {
                                    // Region has ended, start a new top
                                    // level region after it.
                                    let source = TopLevelItems();
                                    let offset = old_end;
                                    (source, offset, old_end, old_length)
                                }
                            },
                            _ => return None
                        })
                    })
                    .unwrap_or((TopLevelItems(), 0, 0, 0));

                // Add a region with the new rows.
                let (new_length, new_top, new_second) = match &source {
                    InterleavedSearch(expanded, full_range) => {
                        let old_range = full_range.start..old_end;
                        let old_top =
                            self.count_items(&old_range).unwrap_or(0);
                        let full_top =
                            self.count_items(full_range)?;
                        let full_second =
                            self.count_within(expanded, full_range)?;
                        let full_length = full_top + full_second;
                        let new_length = full_length - old_length;
                        let new_top = full_top - old_top;
                        let new_second = new_length - new_top;
                        (new_length, new_top, new_second)
                    },
                    TopLevelItems() =>
                        (top_level_pending, top_level_pending, 0),
                    _ =>
                        unreachable!()
                };
                let region = Region {source, offset, length: new_length};
                self.insert_region(position, region)?;
                position += new_length;
                top_level_added += new_top;
                top_level_pending -= new_top;
                second_level_added += new_second;
                second_level_pending -= new_second;

                #[cfg(feature="debug-region-map")] {
                    println!();
                    println!("Region map after root node update:");
                    for (start, region) in self.regions.borrow().iter() {
                        println!("{}: {:?}", start, region);
                    }
                }

                // We should now have added all pending rows.
                assert!(top_level_pending == 0);
                assert!(second_level_pending == 0);

                self.merge_regions();

                // Update child counts.
                let mut root = self.root.borrow_mut();
                root.children.direct_count += top_level_added;
                root.children.total_count +=
                    top_level_added + second_level_added;
                drop(root);

                // Apply a single update to cover all the regions added.
                self.apply_update(model, initial_position, ModelUpdate {
                    rows_added: top_level_added + second_level_added,
                    rows_removed: 0,
                    rows_changed: 0
                });
            }
        } else if children_added > 0 {
            // This is an item node. Update child counts.
            let mut node = node_rc.borrow_mut();
            let mut children = node.children_mut();
            children.direct_count += children_added;
            children.total_count += children_added;
            drop(node);

            let interleaved = matches!(state, Interleaved(..));

            if expanded && !interleaved {
                #[cfg(feature="debug-region-map")] {
                    println!();
                    println!("Adding {} new children at {}",
                             children_added, position);
                }

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
                self.apply_update(model, position, ModelUpdate {
                    rows_added: children_added,
                    rows_removed: 0,
                    rows_changed: 0
                });

                // Update the position to continue from.
                position += children_added;
            }
        }

        // Return the position after all of this node's rows, and any pending
        // interleaved update to do for this node.
        Ok(match state {
            Interleaved(update) => (position, update),
            _ => (position, None)
        })
    }

    fn fetch(&self, position: u64) -> Result<ItemNodeRc<Item>, ModelError> {
        // Fetch the region this row is in.
        let (start, region) = self.regions
            .borrow()
            .range(..=position)
            .next_back()
            .map(|(start, region)| (*start, region.clone()))
            .ok_or_else(||
                InternalError(format!(
                    "No region before position {position}")))?;

        // Get the index of this row relative to the start of the region source.
        let row_index = region.offset + (position - start);

        // Get the parent for this row, according to the type of region.
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        let (parent_ref, item_index, item):
            (AnyNodeRc<Item>, u64, Item) =
            match region.source
        {
            TopLevelItems() => (
                self.root.clone(),
                row_index,
                cap.item(None, row_index)?),
            ChildrenOf(node_ref) => (
                node_ref.clone(),
                row_index,
                cap.item(Some(&node_ref.borrow().item), row_index)?),
            InterleavedSearch(expanded, range) => {
                // Run the interleaved search.
                let mut expanded_items = expanded.iter_items();
                let (search_result, _cursor) =
                    cap.find_child(&mut expanded_items, &range, row_index)?;
                // Return a node corresponding to the search result.
                use SearchResult::*;
                match search_result {
                    // Search found a top level item.
                    TopLevelItem(index, item) => {
                        (self.root.clone(), index, item)
                    },
                    // Search found a child of an expanded top level item.
                    NextLevelItem(_, parent_index, child_index, item) => {
                        // There must already be a node for its parent.
                        let parent_ref = self.root
                            .borrow()
                            .children()
                            .get_expanded(parent_index)
                            .ok_or(ModelError::ParentDropped)?;
                        (parent_ref, child_index, item)
                    }
                }
            }
        };

        // Check if we already have a node for this item in the parent's
        // expanded children.
        if let Some(node_rc) = parent_ref
            .borrow()
            .children()
            .expanded
            .get(&item_index)
        {
            return Ok(node_rc.clone())
        }

        // Also check if we already have an incomplete node for this item.
        if let Some(node_rc) = parent_ref
            .borrow()
            .children()
            .fetch_incomplete(item_index)
        {
            return Ok(node_rc)
        }

        // Otherwise, create a new node.
        let (completion, child_count) = cap.item_children(Some(&item))?;
        let node = ItemNode {
            item,
            parent: Rc::downgrade(&parent_ref),
            item_index,
            completion,
            children: Children::new(child_count),
            widgets: RefCell::new(HashSet::new()),
        };
        let node_rc = Rc::new(RefCell::new(node));
        if !completion.is_complete() {
            parent_ref
                .borrow_mut()
                .children_mut()
                .add_incomplete(item_index, &node_rc);
        }
        Ok(node_rc)
    }

    fn apply_update(&self, model: &Model, position: u64, update: ModelUpdate)
    {
        if let Ok(position) = u32::try_from(position) {
            let rows_addressable = u32::MAX - position;
            let rows_removed = clamp(
                update.rows_removed + update.rows_changed,
                rows_addressable);
            let rows_added = clamp(
                update.rows_added + update.rows_changed,
                rows_addressable);
            model.items_changed(position, rows_removed, rows_added);
        }
    }

    // The following methods correspond to the ListModel interface, and can be
    // called by a GObject wrapper class to implement that interface.

    pub fn n_items(&self) -> u32 {
        clamp(self.row_count(), u32::MAX)
    }

    pub fn item(&self, position: u32) -> Option<Object> {
        // First check that the position is valid (must be within the root
        // node's total child count).
        let position = position as u64;
        if position >= self.row_count() {
            return None
        }
        let node_or_err_msg = self.fetch(position).map_err(|e| format!("{e:?}"));
        let row_data = RowData::new(node_or_err_msg);
        Some(row_data.upcast::<Object>())
    }
}

fn clamp(value: u64, max: u32) -> u32 {
    min(value, max as u64) as u32
}
