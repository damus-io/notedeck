//! Navigation types for the publications reader

use enostr::NoteId;
use std::hash::{Hash, Hasher};

/// Selection for viewing a publication (kind 30040 index with 30041 content)
#[derive(Debug, Clone)]
pub struct PublicationSelection {
    /// The NoteId of the current publication index event (kind 30040)
    pub index_id: NoteId,

    /// Navigation history for nested publications (parent publications)
    history: Vec<NoteId>,
}

impl PublicationSelection {
    /// Create a new publication selection
    pub fn new(index_id: NoteId) -> Self {
        Self {
            index_id,
            history: Vec::new(),
        }
    }

    /// Create from a note ID
    pub fn from_note_id(note_id: NoteId) -> Self {
        Self::new(note_id)
    }

    /// Navigate into a nested publication, pushing current to history
    pub fn navigate_into(&mut self, new_index_id: NoteId) {
        self.history.push(self.index_id);
        self.index_id = new_index_id;
    }

    /// Navigate back to the previous publication
    /// Returns the new current index_id if navigation succeeded
    pub fn navigate_back(&mut self) -> Option<NoteId> {
        if let Some(prev_id) = self.history.pop() {
            self.index_id = prev_id;
            Some(prev_id)
        } else {
            None
        }
    }

    /// Check if we can navigate back
    pub fn can_go_back(&self) -> bool {
        !self.history.is_empty()
    }

    /// Get the breadcrumb trail (parent publication IDs, oldest first)
    pub fn breadcrumbs(&self) -> &[NoteId] {
        &self.history
    }

    /// Get the navigation depth (0 = root publication)
    pub fn depth(&self) -> usize {
        self.history.len()
    }
}

/// Hash only by current index_id for timeline cache lookups
impl Hash for PublicationSelection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index_id.hash(state)
    }
}

/// Equality only checks current index_id for hash lookups
impl PartialEq for PublicationSelection {
    fn eq(&self, other: &Self) -> bool {
        self.index_id == other.index_id
    }
}

impl Eq for PublicationSelection {}
