use crate::agent_status::AgentStatus;
use crate::session::SessionId;
use std::collections::HashMap;

/// Priority levels for the focus queue (higher = more urgent)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FocusPriority {
    /// Low priority - agent completed work (may need follow-up)
    Done = 0,
    /// Medium priority - agent encountered an error
    Error = 1,
    /// High priority - agent needs user input (permission request)
    NeedsInput = 2,
}

impl FocusPriority {
    /// Convert from AgentStatus, returns None for non-queue-worthy statuses
    pub fn from_status(status: AgentStatus) -> Option<Self> {
        match status {
            AgentStatus::NeedsInput => Some(FocusPriority::NeedsInput),
            AgentStatus::Error => Some(FocusPriority::Error),
            AgentStatus::Done => Some(FocusPriority::Done),
            AgentStatus::Idle | AgentStatus::Working => None,
        }
    }

    /// Get the color associated with this priority
    pub fn color(&self) -> egui::Color32 {
        match self {
            FocusPriority::NeedsInput => egui::Color32::from_rgb(255, 200, 0), // Yellow/amber
            FocusPriority::Error => egui::Color32::from_rgb(220, 60, 60),      // Red
            FocusPriority::Done => egui::Color32::from_rgb(70, 130, 220),      // Blue
        }
    }
}

/// An entry in the focus queue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEntry {
    pub session_id: SessionId,
    pub priority: FocusPriority,
}

/// Priority queue for agents needing attention.
/// Uses separate vectors for each priority level for simpler management.
/// Navigation goes through high priority first, then medium, then low.
pub struct FocusQueue {
    /// High priority - needs input (permission requests)
    needs_input: Vec<SessionId>,
    /// Medium priority - errors
    errors: Vec<SessionId>,
    /// Low priority - done
    done: Vec<SessionId>,
    /// Current navigation position as (priority_level, index within that level)
    /// priority_level: 0 = needs_input, 1 = errors, 2 = done
    cursor: Option<(usize, usize)>,
    /// Track previous status to detect transitions
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
            needs_input: Vec::new(),
            errors: Vec::new(),
            done: Vec::new(),
            cursor: None,
            previous_statuses: HashMap::new(),
        }
    }

    fn get_vec(&self, priority: FocusPriority) -> &Vec<SessionId> {
        match priority {
            FocusPriority::NeedsInput => &self.needs_input,
            FocusPriority::Error => &self.errors,
            FocusPriority::Done => &self.done,
        }
    }

    fn get_vec_mut(&mut self, priority: FocusPriority) -> &mut Vec<SessionId> {
        match priority {
            FocusPriority::NeedsInput => &mut self.needs_input,
            FocusPriority::Error => &mut self.errors,
            FocusPriority::Done => &mut self.done,
        }
    }

    fn get_vec_by_level(&self, level: usize) -> &Vec<SessionId> {
        match level {
            0 => &self.needs_input,
            1 => &self.errors,
            _ => &self.done,
        }
    }

    /// Find which vec contains a session and its index
    fn find_session(&self, session_id: SessionId) -> Option<(FocusPriority, usize)> {
        if let Some(idx) = self.needs_input.iter().position(|&id| id == session_id) {
            return Some((FocusPriority::NeedsInput, idx));
        }
        if let Some(idx) = self.errors.iter().position(|&id| id == session_id) {
            return Some((FocusPriority::Error, idx));
        }
        if let Some(idx) = self.done.iter().position(|&id| id == session_id) {
            return Some((FocusPriority::Done, idx));
        }
        None
    }

    /// Enqueue a session with the given priority.
    /// If already in queue at different priority, moves it.
    pub fn enqueue(&mut self, session_id: SessionId, priority: FocusPriority) {
        // Remove from any existing position first
        if let Some((old_priority, _)) = self.find_session(session_id) {
            if old_priority == priority {
                return; // Already in correct queue
            }
            self.get_vec_mut(old_priority)
                .retain(|&id| id != session_id);
        }

        // Add to appropriate queue
        self.get_vec_mut(priority).push(session_id);

        // Initialize cursor if this is the first item
        if self.cursor.is_none() && self.len() == 1 {
            let level = match priority {
                FocusPriority::NeedsInput => 0,
                FocusPriority::Error => 1,
                FocusPriority::Done => 2,
            };
            self.cursor = Some((level, 0));
        }
    }

    /// Remove a session from the queue
    pub fn dequeue(&mut self, session_id: SessionId) {
        if let Some((priority, idx)) = self.find_session(session_id) {
            let vec = self.get_vec_mut(priority);
            vec.remove(idx);

            // Adjust cursor if needed
            if let Some((level, cursor_idx)) = self.cursor {
                let priority_level = match priority {
                    FocusPriority::NeedsInput => 0,
                    FocusPriority::Error => 1,
                    FocusPriority::Done => 2,
                };

                if self.is_empty() {
                    self.cursor = None;
                } else if level == priority_level {
                    // Removed from current level
                    let vec_len = self.get_vec(priority).len();
                    if vec_len == 0 {
                        // Level is now empty, move to next non-empty level
                        self.cursor = self.find_next_valid_cursor(level, 0);
                    } else if idx <= cursor_idx {
                        // Adjust index within level
                        let new_idx = if idx < cursor_idx {
                            cursor_idx - 1
                        } else {
                            cursor_idx.min(vec_len - 1)
                        };
                        self.cursor = Some((level, new_idx));
                    }
                }
            }
        }
    }

    /// Find the next valid cursor position starting from given level
    fn find_next_valid_cursor(
        &self,
        start_level: usize,
        _start_idx: usize,
    ) -> Option<(usize, usize)> {
        // Try levels in order: needs_input (0), errors (1), done (2)
        for level in 0..3 {
            let check_level = (start_level + level) % 3;
            let vec = self.get_vec_by_level(check_level);
            if !vec.is_empty() {
                return Some((check_level, 0));
            }
        }
        None
    }

    /// Total number of items across all queues
    pub fn len(&self) -> usize {
        self.needs_input.len() + self.errors.len() + self.done.len()
    }

    /// Check if all queues are empty
    pub fn is_empty(&self) -> bool {
        self.needs_input.is_empty() && self.errors.is_empty() && self.done.is_empty()
    }

    /// Navigate to next item in queue (wraps around)
    /// Order: all needs_input -> all errors -> all done -> wrap to needs_input
    pub fn next(&mut self) -> Option<SessionId> {
        if self.is_empty() {
            return None;
        }

        let (level, idx) = self.cursor.unwrap_or((0, 0));

        // Try next in current level
        let vec_len = self.get_vec_by_level(level).len();
        if idx + 1 < vec_len {
            let session_id = self.get_vec_by_level(level)[idx + 1];
            self.cursor = Some((level, idx + 1));
            return Some(session_id);
        }

        // Move to next non-empty level
        for offset in 1..=3 {
            let next_level = (level + offset) % 3;
            let next_vec = self.get_vec_by_level(next_level);
            if !next_vec.is_empty() {
                let session_id = next_vec[0];
                self.cursor = Some((next_level, 0));
                return Some(session_id);
            }
        }

        // Shouldn't reach here if not empty
        None
    }

    /// Navigate to previous item in queue (wraps around)
    pub fn prev(&mut self) -> Option<SessionId> {
        if self.is_empty() {
            return None;
        }

        let (level, idx) = self.cursor.unwrap_or((0, 0));

        // Try previous in current level
        if idx > 0 {
            let session_id = self.get_vec_by_level(level)[idx - 1];
            self.cursor = Some((level, idx - 1));
            return Some(session_id);
        }

        // Move to previous non-empty level (going backwards)
        for offset in 1..=3 {
            let prev_level = (level + 3 - offset) % 3;
            let prev_vec = self.get_vec_by_level(prev_level);
            if !prev_vec.is_empty() {
                let last_idx = prev_vec.len() - 1;
                let session_id = prev_vec[last_idx];
                self.cursor = Some((prev_level, last_idx));
                return Some(session_id);
            }
        }

        None
    }

    /// Get the current queue entry without changing position
    pub fn current(&self) -> Option<QueueEntry> {
        let (level, idx) = self.cursor?;
        let vec = self.get_vec_by_level(level);
        let session_id = *vec.get(idx)?;
        let priority = match level {
            0 => FocusPriority::NeedsInput,
            1 => FocusPriority::Error,
            _ => FocusPriority::Done,
        };
        Some(QueueEntry {
            session_id,
            priority,
        })
    }

    /// Get current position (1-indexed for display) across all queues
    pub fn current_position(&self) -> Option<usize> {
        let (level, idx) = self.cursor?;
        let mut pos = idx + 1; // 1-indexed

        // Add counts from higher priority levels
        for l in 0..level {
            pos += self.get_vec_by_level(l).len();
        }

        Some(pos)
    }

    /// Get queue info for UI display: (position, total, priority)
    /// Returns None if queue is empty
    pub fn ui_info(&self) -> Option<(usize, usize, FocusPriority)> {
        let entry = self.current()?;
        let position = self.current_position()?;
        Some((position, self.len(), entry.priority))
    }

    /// Update queue based on status changes.
    /// Call this after updating all session statuses.
    pub fn update_from_statuses(
        &mut self,
        sessions: impl Iterator<Item = (SessionId, AgentStatus)>,
    ) {
        for (session_id, status) in sessions {
            let prev_status = self.previous_statuses.get(&session_id).copied();

            // Detect transition to a queue-worthy state
            if prev_status != Some(status) {
                if let Some(priority) = FocusPriority::from_status(status) {
                    // State transitioned to NeedsInput, Error, or Done
                    self.enqueue(session_id, priority);
                } else {
                    // State transitioned away from queue-worthy (e.g., back to Working)
                    self.dequeue(session_id);
                }
            }

            self.previous_statuses.insert(session_id, status);
        }
    }

    /// Remove tracking for a deleted session
    pub fn remove_session(&mut self, session_id: SessionId) {
        self.dequeue(session_id);
        self.previous_statuses.remove(&session_id);
    }

    /// Check if a session is in the queue and return its priority if so
    pub fn get_session_priority(&self, session_id: SessionId) -> Option<FocusPriority> {
        self.find_session(session_id).map(|(priority, _)| priority)
    }
}
