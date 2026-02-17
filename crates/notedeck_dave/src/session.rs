use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::agent_status::AgentStatus;
use crate::config::AiMode;
use crate::git_status::GitStatusCache;
use crate::messages::{
    CompactionInfo, PermissionResponse, QuestionAnswer, SessionInfo, SubagentStatus,
};
use crate::session_events::ThreadingState;
use crate::{DaveApiResponse, Message};
use claude_agent_sdk_rs::PermissionMode;
use tokio::sync::oneshot;
use uuid::Uuid;

pub type SessionId = u32;

/// State for permission response with message
#[derive(Default, Clone, Copy, PartialEq)]
pub enum PermissionMessageState {
    #[default]
    None,
    /// User pressed Shift+1, waiting for message then will Allow
    TentativeAccept,
    /// User pressed Shift+2, waiting for message then will Deny
    TentativeDeny,
}

/// Agentic-mode specific session data (Claude backend only)
pub struct AgenticSessionData {
    /// Pending permission requests waiting for user response
    pub pending_permissions: HashMap<Uuid, oneshot::Sender<PermissionResponse>>,
    /// Position in the RTS scene (in scene coordinates)
    pub scene_position: egui::Vec2,
    /// Permission mode for Claude (Default or Plan)
    pub permission_mode: PermissionMode,
    /// State for permission response message (tentative accept/deny)
    pub permission_message_state: PermissionMessageState,
    /// State for pending AskUserQuestion responses (keyed by request UUID)
    pub question_answers: HashMap<Uuid, Vec<QuestionAnswer>>,
    /// Current question index for multi-question AskUserQuestion (keyed by request UUID)
    pub question_index: HashMap<Uuid, usize>,
    /// Working directory for claude-code subprocess
    pub cwd: PathBuf,
    /// Session info from Claude Code CLI (tools, model, agents, etc.)
    pub session_info: Option<SessionInfo>,
    /// Indices of subagent messages in chat (keyed by task_id)
    pub subagent_indices: HashMap<String, usize>,
    /// Whether conversation compaction is in progress
    pub is_compacting: bool,
    /// Info from the last completed compaction (for display)
    pub last_compaction: Option<CompactionInfo>,
    /// Claude session ID to resume (UUID from Claude CLI's session storage)
    /// When set, the backend will use --resume to continue this session
    pub resume_session_id: Option<String>,
    /// Git status cache for this session's working directory
    pub git_status: GitStatusCache,
    /// Threading state for live kind-1988 event generation.
    pub live_threading: ThreadingState,
    /// Maps permission request UUID → note ID of the published request event.
    /// Used to link permission response events back to their requests.
    pub perm_request_note_ids: HashMap<Uuid, [u8; 32]>,
    /// Subscription for remote permission response events (kind-1988, t=ai-permission).
    /// Set up once when the session's claude_session_id becomes known.
    pub perm_response_sub: Option<nostrdb::Subscription>,
}

impl AgenticSessionData {
    pub fn new(id: SessionId, cwd: PathBuf) -> Self {
        // Arrange sessions in a grid pattern
        let col = (id as i32 - 1) % 4;
        let row = (id as i32 - 1) / 4;
        let x = col as f32 * 150.0 - 225.0; // Center around origin
        let y = row as f32 * 150.0 - 75.0;

        let git_status = GitStatusCache::new(cwd.clone());

        AgenticSessionData {
            pending_permissions: HashMap::new(),
            scene_position: egui::Vec2::new(x, y),
            permission_mode: PermissionMode::Default,
            permission_message_state: PermissionMessageState::None,
            question_answers: HashMap::new(),
            question_index: HashMap::new(),
            cwd,
            session_info: None,
            subagent_indices: HashMap::new(),
            is_compacting: false,
            last_compaction: None,
            resume_session_id: None,
            git_status,
            live_threading: ThreadingState::new(),
            perm_request_note_ids: HashMap::new(),
            perm_response_sub: None,
        }
    }

    /// Get the session ID to use for live kind-1988 events.
    ///
    /// Prefers claude_session_id from SessionInfo, falls back to resume_session_id.
    pub fn event_session_id(&self) -> Option<&str> {
        self.session_info
            .as_ref()
            .and_then(|i| i.claude_session_id.as_deref())
            .or(self.resume_session_id.as_deref())
    }

    /// Update a subagent's output (appending new content, keeping only the tail)
    pub fn update_subagent_output(
        &mut self,
        chat: &mut [Message],
        task_id: &str,
        new_output: &str,
    ) {
        if let Some(&idx) = self.subagent_indices.get(task_id) {
            if let Some(Message::Subagent(subagent)) = chat.get_mut(idx) {
                subagent.output.push_str(new_output);
                // Keep only the most recent content up to max_output_size
                if subagent.output.len() > subagent.max_output_size {
                    let keep_from = subagent.output.len() - subagent.max_output_size;
                    subagent.output = subagent.output[keep_from..].to_string();
                }
            }
        }
    }

    /// Mark a subagent as completed
    pub fn complete_subagent(&mut self, chat: &mut [Message], task_id: &str, result: &str) {
        if let Some(&idx) = self.subagent_indices.get(task_id) {
            if let Some(Message::Subagent(subagent)) = chat.get_mut(idx) {
                subagent.status = SubagentStatus::Completed;
                subagent.output = result.to_string();
            }
        }
    }
}

/// A single chat session with Dave
pub struct ChatSession {
    pub id: SessionId,
    pub title: String,
    pub chat: Vec<Message>,
    pub input: String,
    pub incoming_tokens: Option<Receiver<DaveApiResponse>>,
    /// Handle to the background task processing this session's AI requests.
    /// Aborted on drop to clean up the subprocess.
    pub task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Cached status for the agent (derived from session state)
    cached_status: AgentStatus,
    /// Set when cached_status changes, cleared after publishing state event
    pub state_dirty: bool,
    /// Whether this session's input should be focused on the next frame
    pub focus_requested: bool,
    /// AI interaction mode for this session (Chat vs Agentic)
    pub ai_mode: AiMode,
    /// Agentic-mode specific data (None in Chat mode)
    pub agentic: Option<AgenticSessionData>,
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}

impl ChatSession {
    pub fn new(id: SessionId, cwd: PathBuf, ai_mode: AiMode) -> Self {
        let agentic = match ai_mode {
            AiMode::Agentic => Some(AgenticSessionData::new(id, cwd)),
            AiMode::Chat => None,
        };

        ChatSession {
            id,
            title: "New Chat".to_string(),
            chat: vec![],
            input: String::new(),
            incoming_tokens: None,
            task_handle: None,
            cached_status: AgentStatus::Idle,
            state_dirty: false,
            focus_requested: false,
            ai_mode,
            agentic,
        }
    }

    /// Create a new session that resumes an existing Claude conversation
    pub fn new_resumed(
        id: SessionId,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        ai_mode: AiMode,
    ) -> Self {
        let mut session = Self::new(id, cwd, ai_mode);
        if let Some(ref mut agentic) = session.agentic {
            agentic.resume_session_id = Some(resume_session_id);
        }
        session.title = title;
        session
    }

    // === Helper methods for accessing agentic data ===

    /// Get agentic data, panics if not in agentic mode (use in agentic-only code paths)
    pub fn agentic(&self) -> &AgenticSessionData {
        self.agentic
            .as_ref()
            .expect("agentic data only available in Agentic mode")
    }

    /// Get mutable agentic data
    pub fn agentic_mut(&mut self) -> &mut AgenticSessionData {
        self.agentic
            .as_mut()
            .expect("agentic data only available in Agentic mode")
    }

    /// Check if session has agentic capabilities
    pub fn is_agentic(&self) -> bool {
        self.agentic.is_some()
    }

    /// Check if session has pending permission requests
    pub fn has_pending_permissions(&self) -> bool {
        self.agentic
            .as_ref()
            .is_some_and(|a| !a.pending_permissions.is_empty())
    }

    /// Check if session is in plan mode
    pub fn is_plan_mode(&self) -> bool {
        self.agentic
            .as_ref()
            .is_some_and(|a| a.permission_mode == PermissionMode::Plan)
    }

    /// Get the working directory (agentic only)
    pub fn cwd(&self) -> Option<&PathBuf> {
        self.agentic.as_ref().map(|a| &a.cwd)
    }

    /// Update a subagent's output (appending new content, keeping only the tail)
    pub fn update_subagent_output(&mut self, task_id: &str, new_output: &str) {
        if let Some(ref mut agentic) = self.agentic {
            agentic.update_subagent_output(&mut self.chat, task_id, new_output);
        }
    }

    /// Mark a subagent as completed
    pub fn complete_subagent(&mut self, task_id: &str, result: &str) {
        if let Some(ref mut agentic) = self.agentic {
            agentic.complete_subagent(&mut self.chat, task_id, result);
        }
    }

    /// Update the session title from the last message (user or assistant)
    pub fn update_title_from_last_message(&mut self) {
        for msg in self.chat.iter().rev() {
            let text: &str = match msg {
                Message::User(text) => text,
                Message::Assistant(msg) => msg.text(),
                _ => continue,
            };
            // Use first ~30 chars of last message as title
            let title: String = text.chars().take(30).collect();
            let new_title = if text.len() > 30 {
                format!("{}...", title)
            } else {
                title
            };
            if new_title != self.title {
                self.title = new_title;
                self.state_dirty = true;
            }
            break;
        }
    }

    /// Get the current status of this session/agent
    pub fn status(&self) -> AgentStatus {
        self.cached_status
    }

    /// Update the cached status based on current session state.
    /// Sets `state_dirty` when the status actually changes.
    pub fn update_status(&mut self) {
        let new_status = self.derive_status();
        if new_status != self.cached_status {
            self.cached_status = new_status;
            self.state_dirty = true;
        }
    }

    /// Derive status from the current session state
    fn derive_status(&self) -> AgentStatus {
        // Check for pending permission requests (needs input) - agentic only
        if self.has_pending_permissions() {
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
                    Message::Assistant(_) | Message::CompactionComplete(_) => {
                        return AgentStatus::Done
                    }
                    Message::User(_) => return AgentStatus::Idle, // Waiting for response
                    Message::Error(_) => return AgentStatus::Error,
                    _ => continue,
                }
            }
        }

        AgentStatus::Idle
    }
}

/// Tracks a pending external editor process
pub struct EditorJob {
    /// The spawned editor process
    pub child: std::process::Child,
    /// Path to the temp file being edited
    pub temp_path: PathBuf,
    /// Session ID that initiated the editor
    pub session_id: SessionId,
}

/// Manages multiple chat sessions
pub struct SessionManager {
    sessions: HashMap<SessionId, ChatSession>,
    order: Vec<SessionId>, // Sorted by recency (most recent first)
    active: Option<SessionId>,
    next_id: SessionId,
    /// Pending external editor job (only one at a time)
    pub pending_editor: Option<EditorJob>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        SessionManager {
            sessions: HashMap::new(),
            order: Vec::new(),
            active: None,
            next_id: 1,
            pending_editor: None,
        }
    }

    /// Create a new session with the given cwd and make it active
    pub fn new_session(&mut self, cwd: PathBuf, ai_mode: AiMode) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session = ChatSession::new(id, cwd, ai_mode);
        self.sessions.insert(id, session);
        self.order.insert(0, id); // Most recent first
        self.active = Some(id);

        id
    }

    /// Create a new session that resumes an existing Claude conversation
    pub fn new_resumed_session(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        ai_mode: AiMode,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session = ChatSession::new_resumed(id, cwd, resume_session_id, title, ai_mode);
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
    /// Returns true if the session was deleted, false if it didn't exist.
    /// If the last session is deleted, active will be None and the caller
    /// should open the directory picker to create a new session.
    pub fn delete_session(&mut self, id: SessionId) -> bool {
        if self.sessions.remove(&id).is_some() {
            self.order.retain(|&x| x != id);

            // If we deleted the active session, switch to another
            if self.active == Some(id) {
                self.active = self.order.first().copied();
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
