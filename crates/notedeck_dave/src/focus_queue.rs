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
        let next = (cur + 1) % self.entries.len();
        self.cursor = Some(next);
        Some(self.entries[next].session_id)
    }

    pub fn prev(&mut self) -> Option<SessionId> {
        if self.entries.is_empty() {
            self.cursor = None;
            return None;
        }
        let cur = self.cursor.unwrap_or(0);
        let prev = (cur + self.entries.len() - 1) % self.entries.len();
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
