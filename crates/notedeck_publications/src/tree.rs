//! Publication Tree data structure
//!
//! A tree structure for navigating NKBIP-01 publications. Supports lazy loading
//! of nodes as they are fetched from relays.

use std::collections::HashMap;

use nostrdb::Note;
use tracing::warn;

use crate::address::EventAddress;
use crate::constants::is_index_kind;
use crate::node::{NodeStatus, NodeType, PublicationTreeNode};

/// A tree structure representing a NKBIP-01 publication
///
/// The tree is lazily populated - nodes start as `Pending` and are
/// resolved as events are fetched from relays.
#[derive(Debug)]
pub struct PublicationTree {
    /// All nodes in the tree (index-based for efficient access)
    nodes: Vec<PublicationTreeNode>,

    /// Map from address to node index for fast lookups
    address_to_index: HashMap<EventAddress, usize>,

    /// Index of the root node
    root_index: usize,

    /// Current cursor position for iteration
    #[allow(dead_code)]
    cursor: Option<usize>,

    /// Bookmark for resuming iteration
    bookmark: Option<usize>,
}

impl PublicationTree {
    /// Create a new publication tree from a root 30040 event
    pub fn from_root_note<'a>(note: Note<'a>) -> Option<Self> {
        let note_key = note.key()?;

        // Extract address from note
        let address = Self::extract_address(&note)?;

        // Check for a-tags to determine if this is a branch
        let has_children = Self::has_a_tags(&note);

        // Extract title
        let title = Self::extract_tag_value(&note, "title");

        // Create root node
        let root = PublicationTreeNode::new_root(address.clone(), note_key, title, has_children);

        let mut tree = Self {
            nodes: vec![root],
            address_to_index: HashMap::new(),
            root_index: 0,
            cursor: None,
            bookmark: None,
        };

        tree.address_to_index.insert(address, 0);

        // Populate children from a-tags
        tree.populate_children_from_note(0, &note);

        Some(tree)
    }

    /// Get the root node
    pub fn root(&self) -> &PublicationTreeNode {
        &self.nodes[self.root_index]
    }

    /// Get a node by index
    pub fn get_node(&self, index: usize) -> Option<&PublicationTreeNode> {
        self.nodes.get(index)
    }

    /// Get a node by address
    pub fn get_node_by_address(&self, address: &EventAddress) -> Option<&PublicationTreeNode> {
        self.address_to_index
            .get(address)
            .and_then(|&idx| self.nodes.get(idx))
    }

    /// Get the index for an address
    pub fn get_index(&self, address: &EventAddress) -> Option<usize> {
        self.address_to_index.get(address).copied()
    }

    /// Get all pending addresses that need to be fetched
    pub fn pending_addresses(&self) -> Vec<&EventAddress> {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Pending)
            .map(|n| &n.address)
            .collect()
    }

    /// Get count of pending nodes
    pub fn pending_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Pending)
            .count()
    }

    /// Get total node count
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get count of resolved nodes
    pub fn resolved_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Resolved)
            .count()
    }

    /// Resolve a pending node with a fetched note
    ///
    /// Returns the node index if successful
    pub fn resolve_node<'a>(&mut self, address: &EventAddress, note: Note<'a>) -> Option<usize> {
        let idx = *self.address_to_index.get(address)?;
        let note_key = note.key()?;

        // Determine node type from kind and presence of children
        let kind = note.kind();
        let has_children = Self::has_a_tags(&note);

        let node_type = if is_index_kind(kind) && has_children {
            NodeType::Branch
        } else {
            NodeType::Leaf
        };

        let title = Self::extract_tag_value(&note, "title");

        // Update the node
        self.nodes[idx].resolve(note_key, title, node_type);

        // If this is a branch, populate its children
        if node_type == NodeType::Branch {
            self.populate_children_from_note(idx, &note);
        }

        Some(idx)
    }

    /// Mark a node as having an error (e.g., fetch failed)
    pub fn mark_error(&mut self, address: &EventAddress) -> Option<usize> {
        let idx = *self.address_to_index.get(address)?;
        self.nodes[idx].mark_error();
        Some(idx)
    }

    /// Get the hierarchy (path from root to node)
    pub fn hierarchy(&self, index: usize) -> Vec<usize> {
        let mut path = Vec::new();
        let mut current = Some(index);

        while let Some(idx) = current {
            path.push(idx);
            current = self.nodes.get(idx).and_then(|n| n.parent);
        }

        path.reverse();
        path
    }

    /// Get depth of a node (root is 0)
    pub fn depth(&self, index: usize) -> usize {
        self.hierarchy(index).len().saturating_sub(1)
    }

    /// Iterator over all leaf nodes in reading order
    pub fn leaves(&self) -> impl Iterator<Item = (usize, &PublicationTreeNode)> {
        LeafIterator::new(self)
    }

    /// Iterator over resolved leaf nodes only
    pub fn resolved_leaves(&self) -> impl Iterator<Item = (usize, &PublicationTreeNode)> {
        self.leaves()
            .filter(|(_, node)| node.status == NodeStatus::Resolved)
    }

    /// Get children of a node in order
    pub fn children(&self, index: usize) -> Option<Vec<&PublicationTreeNode>> {
        let node = self.nodes.get(index)?;
        let mut children: Vec<_> = node
            .children
            .iter()
            .filter_map(|&idx| self.nodes.get(idx))
            .collect();
        children.sort_by_key(|n| n.order);
        Some(children)
    }

    /// Set bookmark for iteration
    pub fn set_bookmark(&mut self, address: &EventAddress) {
        if let Some(&idx) = self.address_to_index.get(address) {
            self.bookmark = Some(idx);
        }
    }

    /// Clear bookmark
    pub fn clear_bookmark(&mut self) {
        self.bookmark = None;
    }

    /// Get addresses of child nodes
    pub fn child_addresses(&self, index: usize) -> Vec<&EventAddress> {
        self.nodes
            .get(index)
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|&idx| self.nodes.get(idx).map(|n| &n.address))
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- Private helpers ---

    /// Populate child nodes from a-tags in the note
    fn populate_children_from_note<'a>(&mut self, parent_idx: usize, note: &Note<'a>) {
        let mut order = 0;

        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }

            let tag_name = match tag.get_unchecked(0).variant().str() {
                Some(s) => s,
                None => continue,
            };

            if tag_name != "a" {
                continue;
            }

            let addr_str = match tag.get_unchecked(1).variant().str() {
                Some(s) => s,
                None => continue,
            };

            match EventAddress::from_a_tag(addr_str) {
                Ok(address) => {
                    let child_idx = self.add_pending_node(address, Some(parent_idx), order);
                    self.nodes[parent_idx].children.push(child_idx);
                    order += 1;
                }
                Err(e) => {
                    warn!("Failed to parse a-tag '{}': {}", addr_str, e);
                }
            }
        }
    }

    /// Add a pending node to the tree
    fn add_pending_node(
        &mut self,
        address: EventAddress,
        parent: Option<usize>,
        order: usize,
    ) -> usize {
        // Return existing if already present
        if let Some(&existing) = self.address_to_index.get(&address) {
            return existing;
        }

        let idx = self.nodes.len();
        let node = PublicationTreeNode::new_pending(address.clone(), parent, order);
        self.nodes.push(node);
        self.address_to_index.insert(address, idx);
        idx
    }

    /// Extract event address from note
    fn extract_address<'a>(note: &Note<'a>) -> Option<EventAddress> {
        let kind = note.kind();
        let pubkey = note.pubkey();
        let dtag = Self::extract_tag_value(note, "d")?;

        Some(EventAddress::new(kind, *pubkey, dtag))
    }

    /// Extract a tag value by name
    fn extract_tag_value<'a>(note: &Note<'a>, tag_name: &str) -> Option<String> {
        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }

            if tag.get_unchecked(0).variant().str() == Some(tag_name) {
                return tag.get_unchecked(1).variant().str().map(String::from);
            }
        }
        None
    }

    /// Check if note has any a-tags
    fn has_a_tags<'a>(note: &Note<'a>) -> bool {
        for tag in note.tags() {
            if tag.count() >= 2 && tag.get_unchecked(0).variant().str() == Some("a") {
                return true;
            }
        }
        false
    }
}

/// Iterator over leaf nodes in reading order (depth-first)
struct LeafIterator<'a> {
    tree: &'a PublicationTree,
    stack: Vec<usize>,
}

impl<'a> LeafIterator<'a> {
    fn new(tree: &'a PublicationTree) -> Self {
        let mut stack = Vec::new();
        // Push children of root in reverse order (so first child is processed first)
        if let Some(root) = tree.nodes.get(tree.root_index) {
            for &child_idx in root.children.iter().rev() {
                stack.push(child_idx);
            }
        }
        Self { tree, stack }
    }
}

impl<'a> Iterator for LeafIterator<'a> {
    type Item = (usize, &'a PublicationTreeNode);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(idx) = self.stack.pop() {
            let node = self.tree.nodes.get(idx)?;

            if node.is_leaf() || node.children.is_empty() {
                return Some((idx, node));
            }

            // Branch node: push children in reverse order
            for &child_idx in node.children.iter().rev() {
                self.stack.push(child_idx);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::NoteKey;

    // Note: Full tests would require mocking nostrdb Note
    // These are placeholder tests for the helper functions

    #[test]
    fn test_hierarchy() {
        // Create a simple tree manually for testing
        let root_addr = EventAddress::new(30040, [0xaa; 32], "root".to_string());
        let child_addr = EventAddress::new(30041, [0xaa; 32], "child".to_string());

        let tree = PublicationTree {
            nodes: vec![
                PublicationTreeNode {
                    node_type: NodeType::Branch,
                    status: NodeStatus::Resolved,
                    address: root_addr.clone(),
                    note_key: Some(NoteKey::new(1)),
                    parent: None,
                    children: vec![1],
                    title: Some("Root".to_string()),
                    order: 0,
                },
                PublicationTreeNode {
                    node_type: NodeType::Leaf,
                    status: NodeStatus::Resolved,
                    address: child_addr.clone(),
                    note_key: Some(NoteKey::new(2)),
                    parent: Some(0),
                    children: vec![],
                    title: Some("Child".to_string()),
                    order: 0,
                },
            ],
            address_to_index: HashMap::from([
                (root_addr, 0),
                (child_addr, 1),
            ]),
            root_index: 0,
            cursor: None,
            bookmark: None,
        };

        assert_eq!(tree.hierarchy(0), vec![0]);
        assert_eq!(tree.hierarchy(1), vec![0, 1]);
        assert_eq!(tree.depth(0), 0);
        assert_eq!(tree.depth(1), 1);
    }

    #[test]
    fn test_pending_count() {
        let addr1 = EventAddress::new(30040, [0xaa; 32], "root".to_string());
        let addr2 = EventAddress::new(30041, [0xaa; 32], "pending".to_string());

        let tree = PublicationTree {
            nodes: vec![
                PublicationTreeNode {
                    node_type: NodeType::Branch,
                    status: NodeStatus::Resolved,
                    address: addr1.clone(),
                    note_key: Some(NoteKey::new(1)),
                    parent: None,
                    children: vec![1],
                    title: None,
                    order: 0,
                },
                PublicationTreeNode {
                    node_type: NodeType::Leaf,
                    status: NodeStatus::Pending,
                    address: addr2.clone(),
                    note_key: None,
                    parent: Some(0),
                    children: vec![],
                    title: None,
                    order: 0,
                },
            ],
            address_to_index: HashMap::from([(addr1, 0), (addr2, 1)]),
            root_index: 0,
            cursor: None,
            bookmark: None,
        };

        assert_eq!(tree.pending_count(), 1);
        assert_eq!(tree.resolved_count(), 1);
        assert_eq!(tree.node_count(), 2);
    }
}
