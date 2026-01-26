use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use crate::messages::PermissionResponse;
use crate::{DaveApiResponse, Message};
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
}

impl ChatSession {
    pub fn new(id: SessionId) -> Self {
        ChatSession {
            id,
            title: "New Chat".to_string(),
            chat: vec![],
            input: String::new(),
            incoming_tokens: None,
            pending_permissions: HashMap::new(),
        }
    }

    /// Update the session title from the first user message
    pub fn update_title_from_first_message(&mut self) {
        for msg in &self.chat {
            if let Message::User(text) = msg {
                // Use first ~30 chars of first user message as title
                let title: String = text.chars().take(30).collect();
                self.title = if text.len() > 30 {
                    format!("{}...", title)
                } else {
                    title
                };
                break;
            }
        }
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
}
