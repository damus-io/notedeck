use crate::agent_status::AgentStatus;
use crate::session::SessionId;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FocusPriority {
    Done = 0,
    Error = 1,
    NeedsInput = 2,
}

impl FocusPriority {
    pub fn from_status(status: AgentStatus) -> Option<Self> {
        match status {
            AgentStatus::NeedsInput => Some(Self::NeedsInput),
            AgentStatus::Error => Some(Self::Error),
            AgentStatus::Done => Some(Self::Done),
            AgentStatus::Idle | AgentStatus::Working => None,
        }
    }

    pub fn color(&self) -> egui::Color32 {
        match self {
            Self::NeedsInput => egui::Color32::from_rgb(255, 200, 0),
            Self::Error => egui::Color32::from_rgb(220, 60, 60),
            Self::Done => egui::Color32::from_rgb(70, 130, 220),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEntry {
    pub session_id: SessionId,
    pub priority: FocusPriority,
}

pub struct FocusQueue {
    entries: Vec<QueueEntry>, // kept sorted: NeedsInput -> Error -> Done
    cursor: Option<usize>,    // index into entries
    previous_statuses: HashMap<SessionId, AgentStatus>,
}

impl Default for FocusQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusQueue {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor: None,
            previous_statuses: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn sort_key(p: FocusPriority) -> i32 {
        // want NeedsInput first, then Error, then Done
        -(p as i32)
    }

    fn find(&self, session_id: SessionId) -> Option<usize> {
        self.entries.iter().position(|e| e.session_id == session_id)
    }

    fn normalize_cursor_after_remove(&mut self, removed_idx: usize) {
        match self.cursor {
            None => {}
            Some(_cur) if self.entries.is_empty() => self.cursor = None,
            Some(cur) if removed_idx < cur => self.cursor = Some(cur - 1),
            Some(cur) if removed_idx == cur => {
                // keep cursor pointing at a valid item (same index if possible, else last)
                let new_cur = cur.min(self.entries.len().saturating_sub(1));
                self.cursor = Some(new_cur);
            }
            Some(_) => {}
        }
    }

    /// Insert entry in priority order (stable within same priority).
    fn insert_sorted(&mut self, entry: QueueEntry) {
        let key = Self::sort_key(entry.priority);
        let pos = self
            .entries
            .iter()
            .position(|e| Self::sort_key(e.priority) > key)
            .unwrap_or(self.entries.len());
        self.entries.insert(pos, entry);

        // initialize cursor if this is the first item
        if self.cursor.is_none() && self.entries.len() == 1 {
            self.cursor = Some(0);
        } else if let Some(cur) = self.cursor {
            // if we inserted before the cursor, shift cursor right
            if pos <= cur {
                self.cursor = Some(cur + 1);
            }
        }
    }

    pub fn enqueue(&mut self, session_id: SessionId, priority: FocusPriority) {
        if let Some(i) = self.find(session_id) {
            if self.entries[i].priority == priority {
                return;
            }
            // remove old entry, then reinsert at correct spot
            self.entries.remove(i);
            self.normalize_cursor_after_remove(i);
        }
        self.insert_sorted(QueueEntry {
            session_id,
            priority,
        });
    }

    pub fn dequeue(&mut self, session_id: SessionId) {
        if let Some(i) = self.find(session_id) {
            self.entries.remove(i);
            self.normalize_cursor_after_remove(i);
        }
    }

    pub fn next(&mut self) -> Option<SessionId> {
        if self.entries.is_empty() {
            self.cursor = None;
            return None;
        }
        let cur = self.cursor.unwrap_or(0);
        let current_priority = self.entries[cur].priority;

        // Find all entries with the same priority
        let same_priority_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.priority == current_priority)
            .map(|(i, _)| i)
            .collect();

        // Find current position within same-priority items
        let pos_in_group = same_priority_indices
            .iter()
            .position(|&i| i == cur)
            .unwrap_or(0);

        // Cycle within same priority level (wrap around)
        let next_pos = (pos_in_group + 1) % same_priority_indices.len();
        let next = same_priority_indices[next_pos];
        self.cursor = Some(next);
        Some(self.entries[next].session_id)
    }

    pub fn prev(&mut self) -> Option<SessionId> {
        if self.entries.is_empty() {
            self.cursor = None;
            return None;
        }
        let cur = self.cursor.unwrap_or(0);
        let current_priority = self.entries[cur].priority;

        // Find all entries with the same priority
        let same_priority_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.priority == current_priority)
            .map(|(i, _)| i)
            .collect();

        // Find current position within same-priority items
        let pos_in_group = same_priority_indices
            .iter()
            .position(|&i| i == cur)
            .unwrap_or(0);

        // Cycle within same priority level (wrap around)
        let prev_pos = if pos_in_group == 0 {
            same_priority_indices.len() - 1
        } else {
            pos_in_group - 1
        };
        let prev = same_priority_indices[prev_pos];
        self.cursor = Some(prev);
        Some(self.entries[prev].session_id)
    }

    pub fn current(&self) -> Option<QueueEntry> {
        let i = self.cursor?;
        self.entries.get(i).copied()
    }

    pub fn current_position(&self) -> Option<usize> {
        Some(self.cursor? + 1) // 1-indexed
    }

    /// Get the raw cursor index (0-indexed)
    pub fn cursor_index(&self) -> Option<usize> {
        self.cursor
    }

    /// Set the cursor to a specific index, clamping to valid range
    pub fn set_cursor(&mut self, index: usize) {
        if self.entries.is_empty() {
            self.cursor = None;
        } else {
            self.cursor = Some(index.min(self.entries.len() - 1));
        }
    }

    /// Find the first entry with NeedsInput priority and return its index
    pub fn first_needs_input_index(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.priority == FocusPriority::NeedsInput)
    }

    /// Check if there are any NeedsInput items in the queue
    pub fn has_needs_input(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.priority == FocusPriority::NeedsInput)
    }

    pub fn ui_info(&self) -> Option<(usize, usize, FocusPriority)> {
        let entry = self.current()?;
        Some((self.current_position()?, self.len(), entry.priority))
    }

    pub fn update_from_statuses(
        &mut self,
        sessions: impl Iterator<Item = (SessionId, AgentStatus)>,
    ) {
        for (session_id, status) in sessions {
            let prev = self.previous_statuses.get(&session_id).copied();
            if prev != Some(status) {
                if let Some(priority) = FocusPriority::from_status(status) {
                    self.enqueue(session_id, priority);
                } else {
                    self.dequeue(session_id);
                }
            }
            self.previous_statuses.insert(session_id, status);
        }
    }

    pub fn get_session_priority(&self, session_id: SessionId) -> Option<FocusPriority> {
        self.entries
            .iter()
            .find(|e| e.session_id == session_id)
            .map(|e| e.priority)
    }

    pub fn remove_session(&mut self, session_id: SessionId) {
        self.dequeue(session_id);
        self.previous_statuses.remove(&session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: u32) -> SessionId {
        id
    }

    #[test]
    fn test_empty_queue() {
        let mut queue = FocusQueue::new();
        assert!(queue.is_empty());
        assert_eq!(queue.next(), None);
        assert_eq!(queue.prev(), None);
        assert_eq!(queue.current(), None);
    }

    #[test]
    fn test_priority_ordering() {
        // Items should be sorted: NeedsInput -> Error -> Done
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Error);

        // Verify internal ordering: NeedsInput(2), Error(3), Done(1)
        assert_eq!(queue.entries[0].session_id, session(2));
        assert_eq!(queue.entries[0].priority, FocusPriority::NeedsInput);
        assert_eq!(queue.entries[1].session_id, session(3));
        assert_eq!(queue.entries[1].priority, FocusPriority::Error);
        assert_eq!(queue.entries[2].session_id, session(1));
        assert_eq!(queue.entries[2].priority, FocusPriority::Done);

        // Cursor tracks what you were viewing - it shifted as items were inserted
        // before Done (now at index 2), so navigate to front (highest priority)
        queue.prev();
        queue.prev();
        assert_eq!(queue.current().unwrap().session_id, session(2));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // Navigate through: NeedsInput -> Error -> Done
        queue.next();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Error);
        queue.next();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_cycling_within_same_priority() {
        let mut queue = FocusQueue::new();

        // Add two NeedsInput items and one Done
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Cursor starts at session 1 (first NeedsInput)
        // After insertions, cursor should still be pointing at first entry
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next should cycle to session 2 (also NeedsInput), not jump to Done
        let result = queue.next();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next again should wrap back to session 1 (still NeedsInput)
        let result = queue.next();
        assert_eq!(result, Some(session(1)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_prev_cycles_within_same_priority() {
        let mut queue = FocusQueue::new();

        // Add two NeedsInput items
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at first NeedsInput
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev should wrap to session 2 (last in same priority group)
        let result = queue.prev();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // prev again should wrap back to session 1
        let result = queue.prev();
        assert_eq!(result, Some(session(1)));
    }

    #[test]
    fn test_single_item_in_priority_stays_put() {
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Error);

        // Navigate to Done (only one item with this priority)
        queue.set_cursor(2); // Done is at index 2
        assert_eq!(queue.current().unwrap().session_id, session(1));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);

        // next/prev should stay on the same item since it's the only Done
        let result = queue.next();
        assert_eq!(result, Some(session(1)));
        let result = queue.prev();
        assert_eq!(result, Some(session(1)));
    }

    #[test]
    fn test_cursor_adjustment_on_higher_priority_insert() {
        let mut queue = FocusQueue::new();

        // Start with a Done item
        queue.enqueue(session(1), FocusPriority::Done);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // Insert a higher priority item - cursor should shift to keep pointing at same item
        queue.enqueue(session(2), FocusPriority::NeedsInput);

        // Cursor should still point to session 1 (now at index 1)
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev should now go to the new higher priority item
        queue.prev();
        assert_eq!(queue.current().unwrap().session_id, session(2));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_priority_upgrade() {
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::Done);

        // Session 2 should be after session 1 (same priority, insertion order)
        assert_eq!(queue.entries[0].session_id, session(1));
        assert_eq!(queue.entries[1].session_id, session(2));

        // Upgrade session 2 to NeedsInput
        queue.enqueue(session(2), FocusPriority::NeedsInput);

        // Session 2 should now be first
        assert_eq!(queue.entries[0].session_id, session(2));
        assert_eq!(queue.entries[0].priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_dequeue_adjusts_cursor() {
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::Error);
        queue.enqueue(session(3), FocusPriority::Done);

        // Move cursor to session 2 (Error)
        queue.next();
        assert_eq!(queue.current().unwrap().session_id, session(2));

        // Remove session 1 (before cursor)
        queue.dequeue(session(1));

        // Cursor should adjust and still point to session 2
        assert_eq!(queue.current().unwrap().session_id, session(2));
    }

    #[test]
    fn test_single_item_navigation() {
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::NeedsInput);

        // With single item, next and prev should both return that item
        assert_eq!(queue.next(), Some(session(1)));
        assert_eq!(queue.prev(), Some(session(1)));
        assert_eq!(queue.current().unwrap().session_id, session(1));
    }

    #[test]
    fn test_update_from_statuses() {
        let mut queue = FocusQueue::new();

        // Initial statuses - order matters for cursor position
        // First item added gets cursor, subsequent inserts shift it
        let statuses = vec![
            (session(1), AgentStatus::Done),
            (session(2), AgentStatus::NeedsInput),
            (session(3), AgentStatus::Working), // Should not be added (Idle/Working excluded)
        ];
        queue.update_from_statuses(statuses.into_iter());

        assert_eq!(queue.len(), 2);
        // Verify NeedsInput is first in priority order
        assert_eq!(queue.entries[0].session_id, session(2));
        assert_eq!(queue.entries[0].priority, FocusPriority::NeedsInput);

        // Update: session 2 becomes Idle (should be removed from queue)
        let statuses = vec![(session(2), AgentStatus::Idle)];
        queue.update_from_statuses(statuses.into_iter());

        assert_eq!(queue.len(), 1);
        assert_eq!(queue.current().unwrap().session_id, session(1));
    }
}
