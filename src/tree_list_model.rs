use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::mem::drop;
use std::num::TryFromIntError;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::ops::DerefMut;

use gtk::prelude::{IsA, Cast};
use gtk::glib::Object;
use gtk::gio::prelude::ListModelExt;

use thiserror::Error;

use crate::capture::{Capture, CaptureError, ItemSource};
use crate::model::GenericModel;
use crate::row_data::GenericRowData;

#[derive(Error, Debug)]
pub enum ModelError {
    #[error(transparent)]
    CaptureError(#[from] CaptureError),
    #[error(transparent)]
    RangeError(#[from] TryFromIntError),
    #[error("Locking capture failed")]
    LockError,
    #[error("Parent not set (attempting to expand the root node?)")]
    ParentNotSet,
    #[error("Node references a dropped parent")]
    ParentDropped,
}

pub struct TreeNode<Item> {
    /// The item at this tree node.
    item: Option<Item>,

    /// Parent of this node in the tree.
    parent: Option<Weak<RefCell<TreeNode<Item>>>>,

    /// Index of this node below the parent Item.
    item_index: u32,

    /// Number of direct children below this node.
    direct_child_count: u32,

    /// Total number nodes below this node, recursively.
    total_child_count: u32,

    /// List of expanded child nodes directly below this node.
    children: BTreeMap<u32, Rc<RefCell<TreeNode<Item>>>>,
}

impl<Item> TreeNode<Item> where Item: Copy {
    pub fn expanded(&self) -> bool {
        match self.parent.as_ref() {
            Some(parent_weak) => match parent_weak.upgrade() {
                Some(parent_ref) => {
                    let parent = parent_ref.borrow();
                    parent.children.contains_key(&self.item_index)
                },
                // Parent is dropped, so node cannot be expanded.
                None => false
            },
            // This is the root, which is never expanded.
            None => false
        }
    }

    pub fn expandable(&self) -> bool {
        self.total_child_count != 0
    }

    /// Position of this node in a list, relative to its parent node.
    pub fn relative_position(&self) -> Result<u32, ModelError> {
        match self.parent.as_ref() {
            Some(parent_weak) => {
                let parent_ref = parent_weak.upgrade().ok_or(ModelError::ParentDropped)?;
                let parent = parent_ref.borrow();
                // Sum up the `child_count`s of any expanded nodes before this one, and add to `item_index`.
                Ok(parent.children.iter()
                    .take_while(|(&key, _)| key < self.item_index)
                    .map(|(_, node)| node.borrow().total_child_count)
                    .sum::<u32>() + self.item_index)
            },
            None => Ok(0),
        }

    }

    #[allow(clippy::type_complexity)]
    pub fn field(&self,
             capture: &Arc<Mutex<Capture>>,
             func: Box<dyn
                Fn(&mut Capture, &Item)
                    -> Result<String, CaptureError>>)
        -> String
    {
        match self.item {
            None => "Error: node has no item".to_string(),
            Some(item) => match capture.lock() {
                Err(_) => "Error: failed to lock capture".to_string(),
                Ok(mut guard) => {
                    let cap = guard.deref_mut();
                    match func(cap, &item) {
                        Err(e) => format!("Error: {:?}", e),
                        Ok(string) => string
                    }
                }
            }
        }
    }
}

pub struct TreeListModel<Item, Model, RowData> {
    _marker: PhantomData<(Model, RowData)>,
    capture: Arc<Mutex<Capture>>,
    root: Rc<RefCell<TreeNode<Item>>>,
}

impl<Item, Model, RowData> TreeListModel<Item, Model, RowData>
where Item: Copy,
      Model: GenericModel<Item> + ListModelExt,
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
            root: Rc::new(RefCell::new(TreeNode {
                item: None,
                parent: None,
                item_index: 0,
                direct_child_count: child_count,
                total_child_count: child_count,
                children: Default::default(),
            })),
        })
    }

    pub fn set_expanded(&self,
                        model: &Model,
                        node_ref: &Rc<RefCell<TreeNode<Item>>>,
                        expanded: bool)
        -> Result<(), ModelError>
    {
        let node = node_ref.borrow();
        if node.expanded() == expanded {
            return Ok(());
        }

        let node_parent_ref = node.parent
            .as_ref().ok_or(ModelError::ParentNotSet)?
            .upgrade().ok_or(ModelError::ParentDropped)?;
        let mut node_parent = node_parent_ref.borrow_mut();

        // Add this node to the parent's list of expanded child nodes.
        if expanded {
            node_parent.children.insert(node.item_index, node_ref.clone());
        } else {
            node_parent.children.remove(&node.item_index);
        }

        drop(node_parent);

        // Traverse back up the tree, modifying `total_child_count` for expanded/collapsed entries.
        let mut position = node.relative_position()?;
        let mut current_node = node_ref.clone();
        while let Some(parent_weak) = current_node.clone().borrow().parent.as_ref() {
            let parent = parent_weak.upgrade().ok_or(ModelError::ParentDropped)?;
            if expanded {
                parent.borrow_mut().total_child_count += node.total_child_count;
            } else {
                parent.borrow_mut().total_child_count -= node.total_child_count;
            }
            current_node = parent;
            position += current_node.borrow().relative_position()? + 1;
        }

        if expanded {
            model.items_changed(position, 0, node.total_child_count);
        } else {
            model.items_changed(position, node.total_child_count, 0);
        }

        Ok(())
    }

    pub fn update(&mut self) -> Result<Option<(u32, u32, u32)>, ModelError> {
        let mut cap = self.capture.lock().or(Err(ModelError::LockError))?;

        let mut node_borrow = self.root.borrow_mut();

        let new_child_count = cap.item_count(&None)? as u32;
        if node_borrow.direct_child_count == new_child_count {
            return Ok(None);
        }

        let position = node_borrow.total_child_count;
        let added = new_child_count - node_borrow.direct_child_count;
        node_borrow.direct_child_count = new_child_count;
        node_borrow.total_child_count += added;
        Ok(Some((position, 0, added)))
    }

    // The following methods correspond to the ListModel interface, and can be
    // called by a GObject wrapper class to implement that interface.

    pub fn n_items(&self) -> u32 {
        self.root.borrow().total_child_count
    }

    pub fn item(&self, position: u32) -> Option<Object> {
        // First check that the position is valid (must be within the root node's `total_child_count`).
        let mut parent_ref = self.root.clone();
        if position >= parent_ref.borrow().total_child_count {
            return None
        }

        let mut relative_position = position;
        'outer: loop {
            for (_, node_rc) in parent_ref.clone().borrow().children.iter() {
                let node = node_rc.borrow();
                // If the position is before this node, break out of the loop to look it up.
                if relative_position < node.item_index {
                    break;
                // If the position matches this node, return it.
                } else if relative_position == node.item_index {
                    return Some(RowData::new(node_rc.clone()).upcast::<Object>());
                // If the position is within this node's children, traverse down the tree and repeat.
                } else if relative_position <= node.item_index + node.total_child_count {
                    parent_ref = node_rc.clone();
                    relative_position -= node.item_index + 1;
                    continue 'outer;
                // Otherwise, if the position is after this node,
                // adjust the relative position for the node's children above.
                } else {
                    relative_position -= node.total_child_count;
                }
            }
            break;
        }

        // If we've broken out to this point, the node must be directly below `parent` - look it up.
        let mut cap = self.capture.lock().ok()?;
        let parent = parent_ref.borrow();
        let item = cap.item(&parent.item, relative_position as u64).ok()?;
        let child_count = cap.child_count(&item).ok()?;
        let node = TreeNode {
            item: Some(item),
            parent: Some(Rc::downgrade(&parent_ref)),
            item_index: relative_position,
            direct_child_count: child_count.try_into().ok()?,
            total_child_count: child_count.try_into().ok()?,
            children: Default::default(),
        };
        let rowdata = RowData::new(Rc::new(RefCell::new(node)));

        Some(rowdata.upcast::<Object>())
    }
}
