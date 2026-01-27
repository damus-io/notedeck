use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use crate::agent_status::AgentStatus;
use crate::messages::PermissionResponse;
use crate::{DaveApiResponse, Message};
use claude_agent_sdk_rs::PermissionMode;
use tokio::sync::oneshot;
use uuid::Uuid;

pub type SessionId = u32;

/// A single chat session with Dave
pub struct ChatSession {
    pub id: SessionId,
    pub title: String,
    pub chat: Vec<Message>,
    pub input: String,
    pub incoming_tokens: Option<Receiver<DaveApiResponse>>,
    /// Pending permission requests waiting for user response
    pub pending_permissions: HashMap<Uuid, oneshot::Sender<PermissionResponse>>,
    /// Handle to the background task processing this session's AI requests.
    /// Aborted on drop to clean up the subprocess.
    pub task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Position in the RTS scene (in scene coordinates)
    pub scene_position: egui::Vec2,
    /// Cached status for the agent (derived from session state)
    cached_status: AgentStatus,
    /// Whether this session's input should be focused on the next frame
    pub focus_requested: bool,
    /// Permission mode for Claude (Default or Plan)
    pub permission_mode: PermissionMode,
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}

impl ChatSession {
    pub fn new(id: SessionId) -> Self {
        // Arrange sessions in a grid pattern
        let col = (id as i32 - 1) % 4;
        let row = (id as i32 - 1) / 4;
        let x = col as f32 * 150.0 - 225.0; // Center around origin
        let y = row as f32 * 150.0 - 75.0;

        ChatSession {
            id,
            title: "New Chat".to_string(),
            chat: vec![],
            input: String::new(),
            incoming_tokens: None,
            pending_permissions: HashMap::new(),
            task_handle: None,
            scene_position: egui::Vec2::new(x, y),
            cached_status: AgentStatus::Idle,
            focus_requested: false,
            permission_mode: PermissionMode::Default,
        }
    }

    /// Update the session title from the last message (user or assistant)
    pub fn update_title_from_last_message(&mut self) {
        for msg in self.chat.iter().rev() {
            let text = match msg {
                Message::User(text) | Message::Assistant(text) => text,
                _ => continue,
            };
            // Use first ~30 chars of last message as title
            let title: String = text.chars().take(30).collect();
            self.title = if text.len() > 30 {
                format!("{}...", title)
            } else {
                title
            };
            break;
        }
    }

    /// Get the current status of this session/agent
    pub fn status(&self) -> AgentStatus {
        self.cached_status
    }

    /// Update the cached status based on current session state
    pub fn update_status(&mut self) {
        self.cached_status = self.derive_status();
    }

    /// Derive status from the current session state
    fn derive_status(&self) -> AgentStatus {
        // Check for pending permission requests (needs input)
        if !self.pending_permissions.is_empty() {
            return AgentStatus::NeedsInput;
        }

        // Check for error in last message
        if let Some(Message::Error(_)) = self.chat.last() {
            return AgentStatus::Error;
        }

        // Check if actively working (has task handle and receiving tokens)
        if self.task_handle.is_some() && self.incoming_tokens.is_some() {
            return AgentStatus::Working;
        }

        // Check if done (has messages and no active task)
        if !self.chat.is_empty() && self.task_handle.is_none() {
            // Check if the last meaningful message was from assistant
            for msg in self.chat.iter().rev() {
                match msg {
                    Message::Assistant(_) => return AgentStatus::Done,
                    Message::User(_) => return AgentStatus::Idle, // Waiting for response
                    Message::Error(_) => return AgentStatus::Error,
                    _ => continue,
                }
            }
        }

        AgentStatus::Idle
    }
}

/// Manages multiple chat sessions
pub struct SessionManager {
    sessions: HashMap<SessionId, ChatSession>,
    order: Vec<SessionId>, // Sorted by recency (most recent first)
    active: Option<SessionId>,
    next_id: SessionId,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        let mut manager = SessionManager {
            sessions: HashMap::new(),
            order: Vec::new(),
            active: None,
            next_id: 1,
        };
        // Start with one session
        manager.new_session();
        manager
    }

    /// Create a new session and make it active
    pub fn new_session(&mut self) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session = ChatSession::new(id);
        self.sessions.insert(id, session);
        self.order.insert(0, id); // Most recent first
        self.active = Some(id);

        id
    }

    /// Get a reference to the active session
    pub fn get_active(&self) -> Option<&ChatSession> {
        self.active.and_then(|id| self.sessions.get(&id))
    }

    /// Get a mutable reference to the active session
    pub fn get_active_mut(&mut self) -> Option<&mut ChatSession> {
        self.active.and_then(|id| self.sessions.get_mut(&id))
    }

    /// Get the active session ID
    pub fn active_id(&self) -> Option<SessionId> {
        self.active
    }

    /// Switch to a different session
    pub fn switch_to(&mut self, id: SessionId) -> bool {
        if self.sessions.contains_key(&id) {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    /// Delete a session
    pub fn delete_session(&mut self, id: SessionId) -> bool {
        if self.sessions.remove(&id).is_some() {
            self.order.retain(|&x| x != id);

            // If we deleted the active session, switch to another
            if self.active == Some(id) {
                self.active = self.order.first().copied();

                // If no sessions left, create a new one
                if self.active.is_none() {
                    self.new_session();
                }
            }
            true
        } else {
            false
        }
    }

    /// Get sessions in order of recency (most recent first)
    pub fn sessions_ordered(&self) -> Vec<&ChatSession> {
        self.order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .collect()
    }

    /// Update the recency of a session (move to front of order)
    pub fn touch(&mut self, id: SessionId) {
        if self.sessions.contains_key(&id) {
            self.order.retain(|&x| x != id);
            self.order.insert(0, id);
        }
    }

    /// Get the number of sessions
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Check if there are no sessions
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Get a reference to a session by ID
    pub fn get(&self, id: SessionId) -> Option<&ChatSession> {
        self.sessions.get(&id)
    }

    /// Get a mutable reference to a session by ID
    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut ChatSession> {
        self.sessions.get_mut(&id)
    }

    /// Iterate over all sessions
    pub fn iter(&self) -> impl Iterator<Item = &ChatSession> {
        self.sessions.values()
    }

    /// Iterate over all sessions mutably
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut ChatSession> {
        self.sessions.values_mut()
    }

    /// Update status for all sessions
    pub fn update_all_statuses(&mut self) {
        for session in self.sessions.values_mut() {
            session.update_status();
        }
    }

    /// Get the first session that needs attention (NeedsInput status)
    pub fn find_needs_attention(&self) -> Option<SessionId> {
        for session in self.sessions.values() {
            if session.status() == AgentStatus::NeedsInput {
                return Some(session.id);
            }
        }
        None
    }

    /// Get all session IDs
    pub fn session_ids(&self) -> Vec<SessionId> {
        self.order.clone()
    }
}
