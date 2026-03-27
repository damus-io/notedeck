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
            AgentStatus::Idle | AgentStatus::Working | AgentStatus::Pending => None,
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

/// Auto-steal focus state machine.
///
/// - `Disabled`: auto-steal is off, user controls focus manually.
/// - `Idle`: auto-steal is on but no pending work.
/// - `Pending`: auto-steal is on and a focus-queue transition was
///   detected that hasn't been acted on yet (retries across frames
///   if temporarily suppressed, e.g. user is typing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStealState {
    Disabled,
    Idle,
    Pending,
}

impl AutoStealState {
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Idle | Self::Pending)
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
        // Move toward higher priority (index 0 = NeedsInput).
        // Entries are sorted highest-priority-first, so decrement.
        // Wraps from the beginning back to the end so every entry
        // is reachable — prevents getting stuck when collapsed
        // directories hide entire priority groups.
        let len = self.entries.len();
        let next = (cur + len - 1) % len;
        self.cursor = Some(next);
        Some(self.entries[next].session_id)
    }

    pub fn prev(&mut self) -> Option<SessionId> {
        if self.entries.is_empty() {
            self.cursor = None;
            return None;
        }
        let cur = self.cursor.unwrap_or(0);
        // Move toward lower priority (end = Done).
        // Entries are sorted highest-priority-first, so increment.
        // Wraps from the end back to the beginning. Symmetric with next().
        let prev = (cur + 1) % self.entries.len();
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

    /// Find the first entry with the given priority whose session is in `visible`.
    pub fn first_visible_index(
        &self,
        priority: FocusPriority,
        visible: &[SessionId],
    ) -> Option<usize> {
        visible.iter().find_map(|session_id| {
            let idx = self.find(*session_id)?;
            (self.entries[idx].priority == priority).then_some(idx)
        })
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

        // prev from NeedsInput moves toward Done (increment index)
        queue.prev();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Error);

        // prev again continues toward Done
        queue.prev();
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_next_advances_toward_higher_priority() {
        let mut queue = FocusQueue::new();

        // Sorted order: [NI(1), NI(2), Done(3)]
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at Done (index 2)
        queue.set_cursor(2);
        assert_eq!(queue.current().unwrap().session_id, session(3));

        // next moves toward higher priority (decrement index)
        let result = queue.next();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next continues toward higher priority
        let result = queue.next();
        assert_eq!(result, Some(session(1)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // next wraps to Done (lowest priority)
        let result = queue.next();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_prev_moves_toward_lower_priority() {
        let mut queue = FocusQueue::new();

        // Sorted order: [NI(1), NI(2), Done(3)]
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at first NeedsInput (index 0)
        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev moves toward lower priority (increment index)
        let result = queue.prev();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);

        // prev continues to Done
        let result = queue.prev();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);
    }

    #[test]
    fn test_prev_from_middle_continues_toward_lower() {
        let mut queue = FocusQueue::new();

        // Sorted order: [NI(1), NI(2), Done(3)]
        queue.enqueue(session(1), FocusPriority::NeedsInput);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Done);

        // Start at second NeedsInput (index 1)
        queue.set_cursor(1);
        assert_eq!(queue.current().unwrap().session_id, session(2));

        // prev moves toward lower priority → Done
        let result = queue.prev();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);

        // prev wraps back to first NeedsInput (highest priority)
        let result = queue.prev();
        assert_eq!(result, Some(session(1)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_next_from_done_moves_toward_higher_priority() {
        let mut queue = FocusQueue::new();

        // Sorted order: [NI(2), Err(3), Done(1)]
        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::NeedsInput);
        queue.enqueue(session(3), FocusPriority::Error);

        // Navigate to Done (index 2)
        queue.set_cursor(2);
        assert_eq!(queue.current().unwrap().session_id, session(1));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Done);

        // next from Done moves toward higher priority → Error
        let result = queue.next();
        assert_eq!(result, Some(session(3)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::Error);

        // next again → NeedsInput (highest priority)
        let result = queue.next();
        assert_eq!(result, Some(session(2)));
        assert_eq!(queue.current().unwrap().priority, FocusPriority::NeedsInput);
    }

    #[test]
    fn test_prev_wraps_around() {
        let mut queue = FocusQueue::new();

        // Only Done items: [Done(1), Done(2)]
        queue.enqueue(session(1), FocusPriority::Done);
        queue.enqueue(session(2), FocusPriority::Done);

        queue.set_cursor(0);
        assert_eq!(queue.current().unwrap().session_id, session(1));

        // prev increments → session 2
        let result = queue.prev();
        assert_eq!(result, Some(session(2)));

        // prev wraps back to session 1
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

    #[test]
    fn test_first_visible_index_honors_visible_order() {
        let mut queue = FocusQueue::new();
        queue.enqueue(session(10), FocusPriority::NeedsInput);
        queue.enqueue(session(20), FocusPriority::NeedsInput);
        queue.enqueue(session(30), FocusPriority::Done);

        // Queue order is [10, 20, 30], but UI-visible order can differ.
        let visible = vec![session(20), session(10), session(30)];

        let idx = queue
            .first_visible_index(FocusPriority::NeedsInput, &visible)
            .expect("expected visible NeedsInput entry");
        assert_eq!(queue.entries[idx].session_id, session(20));
    }
}
