use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::num::TryFromIntError;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::ops::DerefMut;

use gtk::prelude::{IsA, Cast};
use gtk::glib::Object;

use thiserror::Error;

use crate::capture::{Capture, CaptureError, ItemSource};
use crate::row_data::GenericRowData;

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
    #[error("Node already in requested expansion state")]
    AlreadyDone,
}

pub type ItemNodeRc<Item> = Rc<RefCell<ItemNode<Item>>>;
type AnyNodeRc<Item> = Rc<RefCell<dyn Node<Item>>>;

trait Node<Item> {
    /// Item at this node, or None if the root.
    fn item(&self) -> Option<Item>;

    /// Parent of this node, or None if the root.
    fn parent(&self) -> Result<Option<AnyNodeRc<Item>>, ModelError>;

    /// Access the expanded children of this node.
    fn children(&self) -> &Children<Item>;

    /// Mutably access the expanded children of this node.
    fn children_mut(&mut self) -> &mut Children<Item>;
}

struct Children<Item> {
    /// Number of direct children below this node.
    direct_count: u32,

    /// Total number nodes below this node, recursively.
    total_count: u32,

    /// Expanded children of this item.
    expanded: BTreeMap<u32, ItemNodeRc<Item>>,
}

impl<Item> Children<Item> {
    fn new(child_count: u32) -> Self {
        Children {
            direct_count: child_count,
            total_count: child_count,
            expanded: BTreeMap::new()
        }
    }
}

struct RootNode<Item> {
    /// Top level children.
    children: Children<Item>,
}

pub struct ItemNode<Item> {
    /// The item at this tree node.
    item: Item,

    /// Parent of this node in the tree.
    parent: Weak<RefCell<dyn Node<Item>>>,

    /// Index of this node below the parent Item.
    item_index: u32,

    /// Children of this item.
    children: Children<Item>,
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
}

impl<Item> Node<Item> for RootNode<Item> {
    fn item(&self) -> Option<Item> {
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
}

impl<Item> Node<Item> for ItemNode<Item> where Item: Copy {
    fn item(&self) -> Option<Item> {
        Some(self.item)
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
}

impl<Item> ItemNode<Item> where Item: Copy {
    pub fn expanded(&self) -> bool {
        match self.parent.upgrade() {
            Some(parent_ref) => parent_ref
                .borrow()
                .children()
                .expanded(self.item_index),
            // Parent is dropped, so node cannot be expanded.
            None => false
        }
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
                    Err(e) => format!("Error: {:?}", e),
                    Ok(string) => string
                }
            }
        }
    }
}

pub struct ModelUpdate {
    pub rows_added: u32,
    pub rows_removed: u32,
    pub rows_changed: u32,
}

pub struct TreeListModel<Item, RowData> {
    _marker: PhantomData<RowData>,
    capture: Arc<Mutex<Capture>>,
    root: Rc<RefCell<RootNode<Item>>>,
}

impl<Item, RowData> TreeListModel<Item, RowData>
where Item: 'static + Copy,
      RowData: GenericRowData<Item> + IsA<Object> + Cast,
      Capture: ItemSource<Item>
{
    pub fn new(capture: Arc<Mutex<Capture>>) -> Result<Self, ModelError> {
        let mut cap = capture.lock().or(Err(ModelError::LockError))?;
        let item_count = cap.item_count(&None)?;
        let child_count = u32::try_from(item_count)?;
        Ok(TreeListModel {
            _marker: PhantomData,
            capture: capture.clone(),
            root: Rc::new(RefCell::new(RootNode {
                children: Children::new(child_count),
            })),
        })
    }

    pub fn set_expanded(&self,
                        node_ref: &ItemNodeRc<Item>,
                        _position: u32,
                        expanded: bool)
        -> Result<ModelUpdate, ModelError>
    {
        let node = node_ref.borrow();
        if node.expanded() == expanded {
            return Err(ModelError::AlreadyDone);
        }

        node.parent
            .upgrade()
            .ok_or(ModelError::ParentDropped)?
            .borrow_mut()
            .children_mut()
            .set_expanded(node_ref, expanded);

        // Traverse back up the tree, modifying `children.total_count` for
        // expanded/collapsed entries.
        let mut current_node: AnyNodeRc<Item> = node_ref.clone();
        while let Some(parent_ref) = current_node.clone().borrow().parent()? {
            let mut parent = parent_ref.borrow_mut();
            let children = parent.children_mut();
            if expanded {
                children.total_count += node.children.total_count;
            } else {
                children.total_count -= node.children.total_count;
            }
            drop(parent);
            current_node = parent_ref;
        }

        Ok(ModelUpdate {
            rows_added: if expanded { node.children.total_count } else { 0 },
            rows_removed: if expanded { 0 } else { node.children.total_count },
            rows_changed: 0,
        })
    }

    pub fn update(&mut self) -> Result<Option<(u32, u32, u32)>, ModelError> {
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;

        let mut node_borrow = self.root.borrow_mut();

        let new_child_count = cap.item_count(&None)? as u32;
        if node_borrow.children.direct_count == new_child_count {
            return Ok(None);
        }

        let position = node_borrow.children.total_count;
        let added = new_child_count - node_borrow.children.direct_count;
        node_borrow.children.direct_count = new_child_count;
        node_borrow.children.total_count += added;
        Ok(Some((position, 0, added)))
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
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;
        let parent = parent_ref.borrow();
        let item = cap.item(&parent.item(), relative_position as u64)?;
        let child_count = cap.child_count(&item)?;
        let node = ItemNode {
            item,
            parent: Rc::downgrade(&parent_ref),
            item_index: relative_position,
            children: Children::new(child_count.try_into()?),
        };

        Ok(Rc::new(RefCell::new(node)))
    }

    // The following methods correspond to the ListModel interface, and can be
    // called by a GObject wrapper class to implement that interface.

    pub fn n_items(&self) -> u32 {
        self.root.borrow().children.total_count
    }

    pub fn item(&self, position: u32) -> Option<Object> {
        // First check that the position is valid (must be within the root
        // node's total child count).
        if position >= self.root.borrow().children.total_count {
            return None
        }
        let node_or_err_msg = self.fetch(position).map_err(|e| format!("{:?}", e));
        let row_data = RowData::new(node_or_err_msg);
        Some(row_data.upcast::<Object>())
    }
}
