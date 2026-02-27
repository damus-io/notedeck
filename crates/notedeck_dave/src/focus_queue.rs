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

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NeedsInput => "needs_input",
            Self::Error => "error",
            Self::Done => "done",
        }
    }

    pub fn from_indicator_str(s: &str) -> Option<Self> {
        match s {
            "needs_input" => Some(Self::NeedsInput),
            "error" => Some(Self::Error),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

pub struct FocusQueueUpdate {
    pub new_needs_input: bool,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEntry {
    pub session_id: SessionId,
    pub priority: FocusPriority,
}

pub struct FocusQueue {
    entries: Vec<QueueEntry>, // kept sorted: NeedsInput -> Error -> Done
    cursor: Option<usize>,    // index into entries
    previous_indicators: HashMap<SessionId, Option<FocusPriority>>,
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
            previous_indicators: HashMap::new(),
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

    /// Remove a session from the queue only if its priority is Done.
    pub fn dequeue_done(&mut self, session_id: SessionId) {
        if let Some(i) = self.find(session_id) {
            if self.entries[i].priority == FocusPriority::Done {
                self.entries.remove(i);
                self.normalize_cursor_after_remove(i);
            }
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

        // If at last item in group, try to go up to highest priority level
        if pos_in_group == same_priority_indices.len() - 1 {
            // Check if there's a higher priority group (entries are sorted highest first)
            let first_same_priority = same_priority_indices[0];
            if first_same_priority > 0 {
                // Go to the very first item (highest priority)
                self.cursor = Some(0);
                return Some(self.entries[0].session_id);
            }
            // No higher priority group, wrap to first item in current group
            let next = same_priority_indices[0];
            self.cursor = Some(next);
            Some(self.entries[next].session_id)
        } else {
            // Move forward within the same priority group
            let next = same_priority_indices[pos_in_group + 1];
            self.cursor = Some(next);
            Some(self.entries[next].session_id)
        }
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

        // If at first item in group, try to drop to next lower priority level
        if pos_in_group == 0 {
            // Find first item with lower priority (higher index since sorted by priority desc)
            let last_same_priority = *same_priority_indices.last().unwrap();
            if last_same_priority + 1 < self.entries.len() {
                // There's a lower priority group, go to first item of it
                let next_idx = last_same_priority + 1;
                self.cursor = Some(next_idx);
                return Some(self.entries[next_idx].session_id);
            }
            // No lower priority group, wrap to last item in current group
            let prev = *same_priority_indices.last().unwrap();
            self.cursor = Some(prev);
            Some(self.entries[prev].session_id)
        } else {
            // Move backward within the same priority group
            let prev = same_priority_indices[pos_in_group - 1];
            self.cursor = Some(prev);
            Some(self.entries[prev].session_id)
        }
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

    /// Find the first entry with Done priority and return its index
    pub fn first_done_index(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.priority == FocusPriority::Done)
    }

    /// Check if there are any Done items in the queue
    pub fn has_done(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.priority == FocusPriority::Done)
    }

    pub fn ui_info(&self) -> Option<(usize, usize, FocusPriority)> {
        let entry = self.current()?;
        Some((self.current_position()?, self.len(), entry.priority))
    }

    /// Update focus queue based on session indicator fields.
    pub fn update_from_indicators(
        &mut self,
        sessions: impl Iterator<Item = (SessionId, Option<FocusPriority>)>,
    ) -> FocusQueueUpdate {
        let mut new_needs_input = false;
        let mut changed = false;
        for (session_id, indicator) in sessions {
            let prev = self.previous_indicators.get(&session_id).copied();
            if prev != Some(indicator) {
                changed = true;
                if let Some(priority) = indicator {
                    self.enqueue(session_id, priority);
                    if priority == FocusPriority::NeedsInput {
                        new_needs_input = true;
                    }
                } else {
                    self.dequeue(session_id);
                }
            }
            self.previous_indicators.insert(session_id, indicator);
        }
        FocusQueueUpdate {
            new_needs_input,
            changed,
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
        self.previous_indicators.remove(&session_id);
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

        // Navigate to front (highest priority)
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(2));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // prev from NeedsInput should drop down to Error
        queue.prev();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Error);

        // prev from Error should drop down to Done
        queue.prev();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_next_cycles_within_priority_then_wraps() {
        let mut queue = FocusQueue::new();

        // Add two NeedsInput items and one Done
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Cursor starts at session 1 (first NeedsInput)
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next should cycle to session 2 (also NeedsInput)
        let result = queue.next();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next again should wrap back to session 1 (stays in NeedsInput, doesn't drop to Done)
        let result = queue.next();
        assert_eq!(result, Some(session(1)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_prev_drops_to_lower_priority() {
        let mut queue = FocusQueue::new();

        // Add two NeedsInput items and one Done
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at first NeedsInput
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev from first in group should drop to Done (lower priority)
        let result = queue.prev();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_prev_cycles_within_priority_before_dropping() {
        let mut queue = FocusQueue::new();

        // Add two NeedsInput items and one Done
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at second NeedsInput
        queue.set_cursor(1);
        assert_eq!(queue.current().unwrap().session_id, session(2));

        // prev should go to session 1 (earlier in same priority group)
        let result = queue.prev();
        assert_eq!(result, Some(session(1)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // prev again should now drop to Done
        let result = queue.prev();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_next_from_done_goes_to_needs_input() {
        let mut queue = FocusQueue::new();

        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Error);

        // Navigate to Done (only one item with this priority)
        queue.set_cursor(2); // Done is at index 2
        assert_eq!(queue.current().unwrap().session_id, session(1));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);

        // next from Done should go up to NeedsInput (highest priority)
        let result = queue.next();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_prev_from_lowest_wraps_within_group() {
        let mut queue = FocusQueue::new();

        // Only Done items, no lower priority to drop to
        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::Done);

        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev from first Done should wrap to last Done (no lower priority exists)
        let result = queue.prev();
        assert_eq!(result, Some(session(2)));
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

        // next should go up to the new higher priority item
        queue.next();
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

        // Move cursor to session 2 (Error) using set_cursor
        queue.set_cursor(1);
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
    fn test_update_from_indicators() {
        let mut queue = FocusQueue::new();

        // Initial indicators - order matters for cursor position
        let indicators = vec![
            (session(1), Some(FocusPriority::Done)),
            (session(2), Some(FocusPriority::NeedsInput)),
            (session(3), None), // No indicator = no dot
        ];
        let update = queue.update_from_indicators(indicators.into_iter());

        assert!(update.changed);
        assert!(update.new_needs_input);
        assert_eq!(queue.len(), 2);
        // Verify NeedsInput is first in priority order
        assert_eq!(queue.entries[0].session_id, session(2));
        assert_eq!(queue.entries[0].priority, FocusPriority::NeedsInput);

        // No-op: same indicators again → no change
        let indicators = vec![
            (session(1), Some(FocusPriority::Done)),
            (session(2), Some(FocusPriority::NeedsInput)),
        ];
        let update = queue.update_from_indicators(indicators.into_iter());
        assert!(!update.changed);
        assert!(!update.new_needs_input);

        // Update: session 2 indicator cleared (should be removed from queue)
        let indicators = vec![(session(2), None)];
        let update = queue.update_from_indicators(indicators.into_iter());

        assert!(update.changed);
        assert!(!update.new_needs_input);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.current().unwrap().session_id, session(1));
    }
}
