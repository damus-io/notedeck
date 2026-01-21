//! Publication tree node types
//!
//! Nodes represent either branch (index) or leaf (content) events in
//! a publication tree.

use crate::address::EventAddress;
use nostrdb::NoteKey;

/// Type of node in the publication tree
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// Branch node - a 30040 index with children
    Branch,
    /// Leaf node - content (30041, 30818, or 30023)
    Leaf,
}

/// Resolution status of a node
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Not yet fetched from relays
    Pending,
    /// Successfully fetched and resolved
    Resolved,
    /// Fetch failed or event not found
    Error,
}

/// A node in the publication tree
#[derive(Debug, Clone)]
pub struct PublicationTreeNode {
    /// Whether this is a branch (index) or leaf (content)
    pub node_type: NodeType,

    /// Current resolution status
    pub status: NodeStatus,

    /// Event address (kind:pubkey:dtag)
    pub address: EventAddress,

    /// Database key when resolved (for efficient lookups)
    pub note_key: Option<NoteKey>,

    /// Index of parent node in the tree's node vector (None for root)
    pub parent: Option<usize>,

    /// Indices of child nodes (empty for leaf nodes)
    pub children: Vec<usize>,

    /// Title extracted from event tags
    pub title: Option<String>,

    /// Order index within parent (for maintaining a-tag order)
    pub order: usize,
}

impl PublicationTreeNode {
    /// Create a new pending node
    pub fn new_pending(address: EventAddress, parent: Option<usize>, order: usize) -> Self {
        Self {
            node_type: NodeType::Leaf, // Default, will be determined on resolve
            status: NodeStatus::Pending,
            address,
            note_key: None,
            parent,
            children: Vec::new(),
            title: None,
            order,
        }
    }

    /// Create a resolved root node
    pub fn new_root(
        address: EventAddress,
        note_key: NoteKey,
        title: Option<String>,
        has_children: bool,
    ) -> Self {
        Self {
            node_type: if has_children {
                NodeType::Branch
            } else {
                NodeType::Leaf
            },
            status: NodeStatus::Resolved,
            address,
            note_key: Some(note_key),
            parent: None,
            children: Vec::new(),
            title,
            order: 0,
        }
    }

    /// Check if this node is the root
    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    /// Check if this node is a leaf (content)
    pub fn is_leaf(&self) -> bool {
        self.node_type == NodeType::Leaf
    }

    /// Check if this node is a branch (index)
    pub fn is_branch(&self) -> bool {
        self.node_type == NodeType::Branch
    }

    /// Check if this node has been resolved
    pub fn is_resolved(&self) -> bool {
        self.status == NodeStatus::Resolved
    }

    /// Check if this node had an error
    pub fn is_error(&self) -> bool {
        self.status == NodeStatus::Error
    }

    /// Check if this node is pending
    pub fn is_pending(&self) -> bool {
        self.status == NodeStatus::Pending
    }

    /// Mark this node as resolved
    pub fn resolve(&mut self, note_key: NoteKey, title: Option<String>, node_type: NodeType) {
        self.status = NodeStatus::Resolved;
        self.note_key = Some(note_key);
        self.title = title;
        self.node_type = node_type;
    }

    /// Mark this node as having an error
    pub fn mark_error(&mut self) {
        self.status = NodeStatus::Error;
    }

    /// Get display title (falls back to d-tag if no title)
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.address.dtag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address() -> EventAddress {
        EventAddress::new(30041, [0xaa; 32], "test-section".to_string())
    }

    #[test]
    fn test_new_pending() {
        let addr = test_address();
        let node = PublicationTreeNode::new_pending(addr.clone(), Some(0), 1);

        assert!(node.is_pending());
        assert!(node.is_leaf()); // Default type
        assert!(!node.is_root());
        assert_eq!(node.order, 1);
    }

    #[test]
    fn test_resolve() {
        let addr = test_address();
        let mut node = PublicationTreeNode::new_pending(addr, Some(0), 0);

        node.resolve(
            NoteKey::new(42),
            Some("My Section".to_string()),
            NodeType::Leaf,
        );

        assert!(node.is_resolved());
        assert!(node.is_leaf());
        assert_eq!(node.display_title(), "My Section");
    }

    #[test]
    fn test_display_title_fallback() {
        let addr = test_address();
        let node = PublicationTreeNode::new_pending(addr, None, 0);

        assert_eq!(node.display_title(), "test-section");
    }
}
