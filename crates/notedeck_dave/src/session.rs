use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::agent_status::AgentStatus;
use crate::config::AiMode;
use crate::git_status::GitStatusCache;
use crate::messages::{
    AnswerSummary, CompactionInfo, ExecutedTool, PermissionResponse, PermissionResponseType,
    QuestionAnswer, SessionInfo, SubagentStatus,
};
use crate::session_events::ThreadingState;
use crate::{DaveApiResponse, Message};
use claude_agent_sdk_rs::PermissionMode;
use tokio::sync::oneshot;
use uuid::Uuid;

pub type SessionId = u32;

/// Whether this session runs locally or is observed remotely via relays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSource {
    /// Local Claude process running on this machine.
    #[default]
    Local,
    /// Remote session observed via relay events (no local process).
    Remote,
}

/// Session metadata for display in chat headers
pub struct SessionDetails {
    pub title: String,
    pub hostname: String,
    pub cwd: Option<PathBuf>,
}

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

/// Consolidated permission tracking for a session.
///
/// Bundles the local oneshot channels (for local sessions), the note-ID
/// mapping (for linking relay responses), and the already-responded set
/// (for remote sessions) into a single struct.
pub struct PermissionTracker {
    /// Local oneshot senders waiting for the user to allow/deny.
    pub pending: HashMap<Uuid, oneshot::Sender<PermissionResponse>>,
    /// Maps permission-request UUID → nostr note ID of the published request.
    pub request_note_ids: HashMap<Uuid, [u8; 32]>,
    /// Permission UUIDs that have already been responded to.
    pub responded: HashSet<Uuid>,
}

impl PermissionTracker {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            request_note_ids: HashMap::new(),
            responded: HashSet::new(),
        }
    }

    /// Whether there are unresolved local permission requests.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Resolve a permission request. This is the ONLY place resolution state
    /// is updated — both `handle_permission_response` and
    /// `handle_question_response` funnel through here.
    pub fn resolve(
        &mut self,
        chat: &mut [Message],
        request_id: Uuid,
        response_type: PermissionResponseType,
        answer_summary: Option<AnswerSummary>,
        is_remote: bool,
        oneshot_response: Option<PermissionResponse>,
    ) {
        // 1. Update the PermissionRequest message in chat
        for msg in chat.iter_mut() {
            if let Message::PermissionRequest(req) = msg {
                if req.id == request_id {
                    req.response = Some(response_type);
                    if answer_summary.is_some() {
                        req.answer_summary = answer_summary;
                    }
                    break;
                }
            }
        }

        // 2. Update PermissionTracker state
        if is_remote {
            self.responded.insert(request_id);
        } else if let Some(response) = oneshot_response {
            if let Some(sender) = self.pending.remove(&request_id) {
                if sender.send(response).is_err() {
                    tracing::error!(
                        "failed to send permission response for request {}",
                        request_id
                    );
                }
            } else {
                tracing::warn!("no pending permission found for request {}", request_id);
            }
        }
    }

    /// Merge loaded permission state from restored events.
    pub fn merge_loaded(
        &mut self,
        responded: HashSet<Uuid>,
        request_note_ids: HashMap<Uuid, [u8; 32]>,
    ) {
        self.responded = responded;
        self.request_note_ids.extend(request_note_ids);
    }
}

impl Default for PermissionTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Agentic-mode specific session data (Claude backend only)
pub struct AgenticSessionData {
    /// Permission state (pending channels, note IDs, responded set)
    pub permissions: PermissionTracker,
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
    /// Subscription for remote permission response events (kind-1988, t=ai-permission).
    /// Set up once when the session's claude_session_id becomes known.
    pub perm_response_sub: Option<nostrdb::Subscription>,
    /// Status as reported by the remote desktop's kind-31988 event.
    /// Only meaningful when session source is Remote.
    pub remote_status: Option<AgentStatus>,
    /// Timestamp of the kind-31988 event that last set `remote_status`.
    /// Used to ignore older replaceable event revisions that arrive out of order.
    pub remote_status_ts: u64,
    /// Subscription for live kind-1988 conversation events from relays.
    /// Used by remote sessions to receive new messages in real-time.
    pub live_conversation_sub: Option<nostrdb::Subscription>,
    /// Note IDs we've already processed from live conversation polling.
    /// Prevents duplicate messages when events are loaded during restore
    /// and then appear again via the subscription.
    pub seen_note_ids: HashSet<[u8; 32]>,
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
            permissions: PermissionTracker::new(),
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
            perm_response_sub: None,
            remote_status: None,
            remote_status_ts: 0,
            live_conversation_sub: None,
            seen_note_ids: HashSet::new(),
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

    /// Try to fold a tool result into its parent subagent.
    /// Returns None if folded, Some(result) if it couldn't be folded.
    pub fn fold_tool_result(
        &self,
        chat: &mut [Message],
        result: ExecutedTool,
    ) -> Option<ExecutedTool> {
        let parent_id = result.parent_task_id.as_ref()?;
        let &idx = self.subagent_indices.get(parent_id)?;
        if let Some(Message::Subagent(subagent)) = chat.get_mut(idx) {
            subagent.tool_results.push(result);
            None
        } else {
            Some(result)
        }
    }
}

/// A single chat session with Dave
pub struct ChatSession {
    pub id: SessionId,
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
    /// Whether this session is local (has a Claude process) or remote (relay-only).
    pub source: SessionSource,
    /// Session metadata for display (title, hostname, cwd)
    pub details: SessionDetails,
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
        let details_cwd = if ai_mode == AiMode::Agentic {
            Some(cwd.clone())
        } else {
            None
        };
        let agentic = match ai_mode {
            AiMode::Agentic => Some(AgenticSessionData::new(id, cwd)),
            AiMode::Chat => None,
        };

        ChatSession {
            id,
            chat: vec![],
            input: String::new(),
            incoming_tokens: None,
            task_handle: None,
            cached_status: AgentStatus::Idle,
            state_dirty: false,
            focus_requested: false,
            ai_mode,
            agentic,
            source: SessionSource::Local,
            details: SessionDetails {
                title: "New Chat".to_string(),
                hostname: String::new(),
                cwd: details_cwd,
            },
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
        session.details.title = title;
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

    /// Check if this is a remote session (observed via relay, no local process)
    pub fn is_remote(&self) -> bool {
        self.source == SessionSource::Remote
    }

    /// Check if session has pending permission requests
    pub fn has_pending_permissions(&self) -> bool {
        if self.is_remote() {
            // Remote: check for unresponded PermissionRequest messages in chat
            let responded = self.agentic.as_ref().map(|a| &a.permissions.responded);
            return self.chat.iter().any(|msg| {
                if let Message::PermissionRequest(req) = msg {
                    req.response.is_none() && responded.is_none_or(|ids| !ids.contains(&req.id))
                } else {
                    false
                }
            });
        }
        // Local: check oneshot senders
        self.agentic
            .as_ref()
            .is_some_and(|a| a.permissions.has_pending())
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

    /// Try to fold a tool result into its parent subagent.
    /// Returns None if folded, Some(result) if it couldn't be folded.
    pub fn fold_tool_result(&mut self, result: ExecutedTool) -> Option<ExecutedTool> {
        if let Some(ref agentic) = self.agentic {
            agentic.fold_tool_result(&mut self.chat, result)
        } else {
            Some(result)
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
            if new_title != self.details.title {
                self.details.title = new_title;
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
        // Remote sessions derive status from the kind-31988 state event,
        // but override to NeedsInput if there are unresponded permission requests.
        if self.is_remote() {
            if self.has_pending_permissions() {
                return AgentStatus::NeedsInput;
            }
            return self
                .agentic
                .as_ref()
                .and_then(|a| a.remote_status)
                .unwrap_or(AgentStatus::Idle);
        }

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
                        return AgentStatus::Done;
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

impl ChatSession {
    /// Whether the session is actively streaming a response from the backend.
    pub fn is_streaming(&self) -> bool {
        self.incoming_tokens.is_some()
    }

    /// Whether the session has an unanswered user message at the end of the
    /// chat that needs to be dispatched to the backend.
    pub fn has_pending_user_message(&self) -> bool {
        matches!(self.chat.last(), Some(Message::User(_)))
    }

    /// Whether a newly arrived remote user message should be dispatched to
    /// the backend right now. Returns false if the session is already
    /// streaming — the message is already in chat and will be picked up
    /// when the current stream finishes.
    pub fn should_dispatch_remote_message(&self) -> bool {
        !self.is_streaming() && self.has_pending_user_message()
    }

    /// Whether the session needs a re-dispatch after a stream ends.
    /// This catches user messages that arrived while we were streaming.
    pub fn needs_redispatch_after_stream_end(&self) -> bool {
        !self.is_streaming() && self.has_pending_user_message()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::messages::AssistantMessage;
    use std::sync::mpsc;

    fn test_session() -> ChatSession {
        ChatSession::new(1, PathBuf::from("/tmp"), AiMode::Agentic)
    }

    #[test]
    fn dispatch_when_idle_with_user_message() {
        let mut session = test_session();
        session.chat.push(Message::User("hello".into()));
        assert!(session.should_dispatch_remote_message());
    }

    #[test]
    fn no_dispatch_while_streaming() {
        let mut session = test_session();
        session.chat.push(Message::User("hello".into()));

        // Start streaming
        let (_tx, rx) = mpsc::channel::<DaveApiResponse>();
        session.incoming_tokens = Some(rx);

        // New user message arrives while streaming
        session.chat.push(Message::User("another".into()));
        assert!(!session.should_dispatch_remote_message());
    }

    #[test]
    fn redispatch_after_stream_ends_with_pending_user_message() {
        let mut session = test_session();
        session.chat.push(Message::User("msg1".into()));

        // Start streaming
        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        session.incoming_tokens = Some(rx);

        // Assistant responds, then more user messages arrive
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "response".into(),
            )));
        session.chat.push(Message::User("msg2".into()));

        // Stream ends
        drop(tx);
        session.incoming_tokens = None;

        assert!(session.needs_redispatch_after_stream_end());
    }

    #[test]
    fn no_redispatch_when_assistant_is_last() {
        let mut session = test_session();
        session.chat.push(Message::User("hello".into()));

        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        session.incoming_tokens = Some(rx);

        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "done".into(),
            )));

        drop(tx);
        session.incoming_tokens = None;

        assert!(!session.needs_redispatch_after_stream_end());
    }

    /// The key bug scenario: multiple remote messages arrive across frames
    /// while streaming. None should trigger dispatch. After stream ends,
    /// the last pending message should trigger redispatch.
    #[test]
    fn multiple_remote_messages_while_streaming() {
        let mut session = test_session();

        // First message — dispatched normally
        session.chat.push(Message::User("msg1".into()));
        assert!(session.should_dispatch_remote_message());

        // Backend starts streaming
        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        session.incoming_tokens = Some(rx);

        // Messages arrive one per frame while streaming
        session.chat.push(Message::User("msg2".into()));
        assert!(!session.should_dispatch_remote_message());

        session.chat.push(Message::User("msg3".into()));
        assert!(!session.should_dispatch_remote_message());

        // Stream ends
        drop(tx);
        session.incoming_tokens = None;

        // Should redispatch — there are unanswered user messages
        assert!(session.needs_redispatch_after_stream_end());
    }
}
