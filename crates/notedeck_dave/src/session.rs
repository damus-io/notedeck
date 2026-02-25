use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
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
    /// User-set title that takes precedence over the auto-generated one.
    pub custom_title: Option<String>,
    pub hostname: String,
    pub cwd: Option<PathBuf>,
    /// Home directory of the machine where this session originated.
    /// Used to abbreviate cwd paths for remote sessions.
    pub home_dir: String,
}

impl SessionDetails {
    /// Returns custom_title if set, otherwise the auto-generated title.
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.title)
    }
}

/// Tracks the "Compact & Approve" lifecycle.
///
/// Button click → `WaitingForCompaction` (intent recorded).
/// CompactionComplete → `ReadyToProceed` (compaction finished, safe to send).
/// Stream-end (local) or compaction_complete event (remote) → consume and fire.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum CompactAndProceedState {
    /// No compact-and-proceed in progress.
    #[default]
    Idle,
    /// User clicked "Compact & Approve"; waiting for compaction to finish.
    WaitingForCompaction,
    /// Compaction finished; send "Proceed" on the next safe opportunity
    /// (stream-end for local, immediately for remote).
    ReadyToProceed,
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
    /// Tracks the "Compact & Approve" lifecycle.
    pub compact_and_proceed: CompactAndProceedState,
    /// Accumulated usage metrics across queries in this session.
    pub usage: crate::messages::UsageInfo,
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
            compact_and_proceed: CompactAndProceedState::Idle,
            usage: Default::default(),
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
        let Some(parent_id) = result.parent_task_id.as_ref() else {
            return Some(result);
        };
        let Some(&idx) = self.subagent_indices.get(parent_id) else {
            return Some(result);
        };
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
    /// Number of trailing user messages that were dispatched in the current
    /// stream. Used by `append_token` to insert the assistant response
    /// after all dispatched messages but before any newly queued ones.
    pub dispatched_user_count: usize,
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
    /// Which backend this session uses (Claude, Codex, etc.)
    pub backend_type: BackendType,
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }
}

impl ChatSession {
    pub fn new(id: SessionId, cwd: PathBuf, ai_mode: AiMode, backend_type: BackendType) -> Self {
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
            dispatched_user_count: 0,
            cached_status: AgentStatus::Idle,
            state_dirty: false,
            focus_requested: false,
            ai_mode,
            agentic,
            source: SessionSource::Local,
            details: SessionDetails {
                title: "New Chat".to_string(),
                custom_title: None,
                hostname: String::new(),
                cwd: details_cwd,
                home_dir: dirs::home_dir()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_default(),
            },
            backend_type,
        }
    }

    /// Create a new session that resumes an existing Claude conversation
    pub fn new_resumed(
        id: SessionId,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        ai_mode: AiMode,
        backend_type: BackendType,
    ) -> Self {
        let mut session = Self::new(id, cwd, ai_mode, backend_type);
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
    /// Cached agent grouping by hostname. Each entry is (hostname, session IDs
    /// in recency order). Rebuilt via `rebuild_host_groups()` when sessions or
    /// hostnames change.
    host_groups: Vec<(String, Vec<SessionId>)>,
    /// Cached chat session IDs in recency order. Rebuilt alongside host_groups.
    chat_ids: Vec<SessionId>,
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
            host_groups: Vec::new(),
            chat_ids: Vec::new(),
        }
    }

    /// Create a new session with the given cwd and make it active
    pub fn new_session(
        &mut self,
        cwd: PathBuf,
        ai_mode: AiMode,
        backend_type: BackendType,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session = ChatSession::new(id, cwd, ai_mode, backend_type);
        self.sessions.insert(id, session);
        self.order.insert(0, id); // Most recent first
        self.active = Some(id);
        self.rebuild_host_groups();

        id
    }

    /// Create a new session that resumes an existing Claude conversation
    pub fn new_resumed_session(
        &mut self,
        cwd: PathBuf,
        resume_session_id: String,
        title: String,
        ai_mode: AiMode,
        backend_type: BackendType,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session =
            ChatSession::new_resumed(id, cwd, resume_session_id, title, ai_mode, backend_type);
        self.sessions.insert(id, session);
        self.order.insert(0, id); // Most recent first
        self.active = Some(id);
        self.rebuild_host_groups();

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
            self.rebuild_host_groups();
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

    /// Get cached agent session groups by hostname.
    /// Each entry is (hostname, session IDs in recency order).
    pub fn host_groups(&self) -> &[(String, Vec<SessionId>)] {
        &self.host_groups
    }

    /// Get cached chat session IDs in recency order.
    pub fn chat_ids(&self) -> &[SessionId] {
        &self.chat_ids
    }

    /// Get a session's index in the recency-ordered list (for keyboard shortcuts).
    pub fn session_index(&self, id: SessionId) -> Option<usize> {
        self.order.iter().position(|&oid| oid == id)
    }

    /// Rebuild the cached hostname groups from current sessions and order.
    /// Call after adding/removing sessions or changing a session's hostname.
    pub fn rebuild_host_groups(&mut self) {
        self.host_groups.clear();
        self.chat_ids.clear();

        for &id in &self.order {
            if let Some(session) = self.sessions.get(&id) {
                if session.ai_mode != AiMode::Agentic {
                    if session.ai_mode == AiMode::Chat {
                        self.chat_ids.push(id);
                    }
                    continue;
                }
                let hostname = &session.details.hostname;
                if let Some(group) = self.host_groups.iter_mut().find(|(h, _)| h == hostname) {
                    group.1.push(id);
                } else {
                    self.host_groups.push((hostname.clone(), vec![id]));
                }
            }
        }

        // Sort groups by hostname for stable ordering
        self.host_groups.sort_by(|a, b| a.0.cmp(&b.0));
    }
}

impl ChatSession {
    /// Whether the session is actively streaming a response from the backend.
    pub fn is_streaming(&self) -> bool {
        self.incoming_tokens.is_some()
    }

    /// Append a streaming token to the current assistant message.
    ///
    /// If the last message is an Assistant, append there. Otherwise
    /// search backwards through only trailing User messages (queued
    /// ones) for a still-streaming Assistant. If none is found,
    /// create a new Assistant — inserted after the dispatched user
    /// message but before any queued ones.
    ///
    /// We intentionally do NOT search past ToolCalls, ToolResponse,
    /// or other non-User messages. When Claude sends text → tool
    /// call → more text, the post-tool tokens must go into a NEW
    /// Assistant so the tool call appears between the two text blocks.
    pub fn append_token(&mut self, token: &str) {
        // Fast path: last message is the active assistant response
        if let Some(Message::Assistant(msg)) = self.chat.last_mut() {
            msg.push_token(token);
            return;
        }

        // Slow path: look backwards through only trailing User messages.
        // If we find a streaming Assistant just before them, append there.
        let mut appended = false;
        for m in self.chat.iter_mut().rev() {
            match m {
                Message::User(_) => continue, // skip queued user messages
                Message::Assistant(msg) if msg.is_streaming() => {
                    msg.push_token(token);
                    appended = true;
                    break;
                }
                _ => break, // stop at ToolCalls, ToolResponse, finalized Assistant, etc.
            }
        }

        if !appended {
            // No streaming assistant reachable — start a new one.
            // Insert after the dispatched user messages but before
            // any newly queued ones so the response appears in the
            // right order and queued messages trigger redispatch.
            let mut msg = crate::messages::AssistantMessage::new();
            msg.push_token(token);

            let trailing_start = self
                .chat
                .iter()
                .rposition(|m| !matches!(m, Message::User(_)))
                .map(|i| i + 1)
                .unwrap_or(0);

            // Skip past the dispatched user messages (default 1 for
            // single dispatch, more for batch redispatch)
            let skip = self.dispatched_user_count.max(1);
            let insert_pos = (trailing_start + skip).min(self.chat.len());
            self.chat.insert(insert_pos, Message::Assistant(msg));
        }
    }

    /// Finalize the last assistant message (cache parsed markdown, etc).
    ///
    /// Searches backwards because queued user messages may appear after
    /// the assistant response in the chat.
    pub fn finalize_last_assistant(&mut self) {
        for msg in self.chat.iter_mut().rev() {
            if let Message::Assistant(assistant) = msg {
                assistant.finalize();
                return;
            }
        }
    }

    /// Get the text of the last assistant message.
    ///
    /// Searches backwards because queued user messages may appear after
    /// the assistant response in the chat.
    pub fn last_assistant_text(&self) -> Option<String> {
        self.chat.iter().rev().find_map(|m| match m {
            Message::Assistant(msg) => {
                let text = msg.text().to_string();
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            _ => None,
        })
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

    /// If "Compact & Approve" has reached ReadyToProceed, consume the state,
    /// push a "Proceed" user message, and return true.
    ///
    /// Called from:
    /// - Local sessions: at stream-end in process_events()
    /// - Remote sessions: on compaction_complete in poll_remote_conversation_events()
    pub fn take_compact_and_proceed(&mut self) -> bool {
        let dominated = self
            .agentic
            .as_ref()
            .is_none_or(|a| a.compact_and_proceed != CompactAndProceedState::ReadyToProceed);

        if dominated {
            return false;
        }

        self.agentic.as_mut().unwrap().compact_and_proceed = CompactAndProceedState::Idle;
        self.chat
            .push(Message::User("Proceed with implementing the plan.".into()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiMode;
    use crate::messages::AssistantMessage;
    use std::sync::mpsc;

    fn test_session() -> ChatSession {
        ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        )
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

    // ---- append_token tests ----

    #[test]
    fn append_token_creates_assistant_when_empty() {
        let mut session = test_session();
        session.append_token("hello");
        assert!(matches!(session.chat.last(), Some(Message::Assistant(_))));
        assert_eq!(session.last_assistant_text().unwrap(), "hello");
    }

    #[test]
    fn append_token_extends_existing_assistant() {
        let mut session = test_session();
        session.chat.push(Message::User("hi".into()));
        session.append_token("hel");
        session.append_token("lo");
        assert_eq!(session.last_assistant_text().unwrap(), "hello");
        assert!(matches!(session.chat.last(), Some(Message::Assistant(_))));
    }

    /// The key bug this prevents: tokens arriving after a queued user
    /// message must NOT create a new Assistant that buries the queued
    /// message. They should append to the existing Assistant before it.
    #[test]
    fn tokens_after_queued_message_dont_bury_it() {
        let mut session = test_session();

        // User sends initial message, assistant starts responding
        session.chat.push(Message::User("hello".into()));
        session.append_token("Sure, ");
        session.append_token("I can ");

        // User queues a follow-up while streaming
        session.chat.push(Message::User("also do this".into()));

        // More tokens arrive from the CURRENT stream (not the queued msg)
        session.append_token("help!");

        // The queued user message must still be last
        assert!(
            matches!(session.chat.last(), Some(Message::User(_))),
            "queued user message should still be the last message"
        );
        assert!(session.has_pending_user_message());

        // Tokens should have been appended to the existing assistant
        assert_eq!(session.last_assistant_text().unwrap(), "Sure, I can help!");

        // After stream ends, redispatch should fire
        assert!(session.needs_redispatch_after_stream_end());
    }

    /// Multiple queued messages: all should remain after the assistant
    /// response, and redispatch should still trigger.
    #[test]
    fn multiple_queued_messages_preserved() {
        let mut session = test_session();

        session.chat.push(Message::User("first".into()));
        session.append_token("response");

        // Queue two messages
        session.chat.push(Message::User("second".into()));
        session.chat.push(Message::User("third".into()));

        // More tokens arrive
        session.append_token(" done");

        // Last message should still be the queued user message
        assert!(session.has_pending_user_message());
        assert!(session.needs_redispatch_after_stream_end());

        // Assistant text should be the combined response
        assert_eq!(session.last_assistant_text().unwrap(), "response done");
    }

    /// After a turn is finalized, a new user message is sent and Claude
    /// responds. Tokens for the NEW response must create a new Assistant
    /// after the user message, not append to the finalized old one.
    /// This was the root cause of the infinite redispatch loop.
    #[test]
    fn tokens_after_finalized_turn_create_new_assistant() {
        let mut session = test_session();

        // Complete turn 1
        session.chat.push(Message::User("hello".into()));
        session.append_token("first response");
        session.finalize_last_assistant();

        // User sends a new message (primary, not queued)
        session.chat.push(Message::User("follow up".into()));

        // Tokens arrive from Claude's new response
        session.append_token("second ");
        session.append_token("response");

        // The new tokens must be in a NEW assistant after the user message
        assert!(
            matches!(session.chat.last(), Some(Message::Assistant(_))),
            "new assistant should be the last message"
        );
        assert_eq!(session.last_assistant_text().unwrap(), "second response");

        // The old assistant should still have its original text
        let first_assistant_text = session
            .chat
            .iter()
            .find_map(|m| match m {
                Message::Assistant(msg) => {
                    let t = msg.text().to_string();
                    if t == "first response" {
                        Some(t)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .expect("original assistant should still exist");
        assert_eq!(first_assistant_text, "first response");

        // No pending user message — assistant is last
        assert!(!session.has_pending_user_message());
    }

    /// When a queued message arrives before the first token, the new
    /// Assistant must be inserted between the dispatched user message
    /// and the queued one, not after the queued one.
    #[test]
    fn queued_before_first_token_ordering() {
        let mut session = test_session();

        // Turn 1 complete
        session.chat.push(Message::User("hello".into()));
        session.append_token("response 1");
        session.finalize_last_assistant();

        // User sends a new message, dispatched to Claude (single dispatch)
        session.chat.push(Message::User("follow up".into()));
        session.dispatched_user_count = 1;

        // User queues another message BEFORE any tokens arrive
        session.chat.push(Message::User("queued msg".into()));

        // Now first token arrives from Claude's response to "follow up"
        session.append_token("response ");
        session.append_token("2");

        // Expected order: User("follow up"), Assistant("response 2"), User("queued msg")
        let msgs: Vec<&str> = session
            .chat
            .iter()
            .filter_map(|m| match m {
                Message::User(s) if s == "follow up" => Some("U:follow up"),
                Message::User(s) if s == "queued msg" => Some("U:queued msg"),
                Message::Assistant(a) if a.text() == "response 2" => Some("A:response 2"),
                _ => None,
            })
            .collect();
        assert_eq!(
            msgs,
            vec!["U:follow up", "A:response 2", "U:queued msg"],
            "assistant response should appear between dispatched and queued messages"
        );

        // Queued message should still be last → triggers redispatch
        assert!(session.has_pending_user_message());
    }

    /// Text → tool call → more text: post-tool tokens must create a
    /// new Assistant so the tool call appears between the two text blocks,
    /// not get appended to the pre-tool Assistant (which would push the
    /// tool call to the bottom).
    #[test]
    fn tokens_after_tool_call_create_new_assistant() {
        let mut session = test_session();

        session.chat.push(Message::User("do something".into()));
        session.append_token("Let me read that file.");

        // Tool call arrives mid-stream
        let tool = crate::tools::ToolCall::invalid(
            "call-1".into(),
            Some("Read".into()),
            None,
            "test".into(),
        );
        session.chat.push(Message::ToolCalls(vec![tool]));
        session
            .chat
            .push(Message::ToolResponse(crate::tools::ToolResponse::error(
                "call-1".into(),
                "test result".into(),
            )));

        // More tokens arrive after the tool call
        session.append_token("Here is what I found.");

        // Verify ordering: Assistant, ToolCalls, ToolResponse, Assistant
        let labels: Vec<&str> = session
            .chat
            .iter()
            .map(|m| match m {
                Message::User(_) => "User",
                Message::Assistant(_) => "Assistant",
                Message::ToolCalls(_) => "ToolCalls",
                Message::ToolResponse(_) => "ToolResponse",
                _ => "Other",
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "User",
                "Assistant",
                "ToolCalls",
                "ToolResponse",
                "Assistant"
            ],
            "post-tool tokens should be in a new assistant, not appended to the first"
        );

        // Verify content of each assistant
        let assistants: Vec<String> = session
            .chat
            .iter()
            .filter_map(|m| match m {
                Message::Assistant(a) => Some(a.text().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(assistants[0], "Let me read that file.");
        assert_eq!(assistants[1], "Here is what I found.");
    }

    // ---- finalize_last_assistant tests ----

    #[test]
    fn finalize_finds_assistant_before_queued_messages() {
        let mut session = test_session();

        session.chat.push(Message::User("hi".into()));
        session.append_token("response");
        session.chat.push(Message::User("queued".into()));

        // Should finalize without panicking, even though last() is User
        session.finalize_last_assistant();

        // Verify the queued message is still there
        assert!(session.has_pending_user_message());
    }

    // ---- status tests ----

    /// Helper to put a session into "streaming" state
    fn make_streaming(session: &mut ChatSession) -> mpsc::Sender<DaveApiResponse> {
        let (tx, rx) = mpsc::channel::<DaveApiResponse>();
        session.incoming_tokens = Some(rx);
        tx
    }

    #[test]
    fn status_idle_initially() {
        let session = test_session();
        assert_eq!(session.status(), AgentStatus::Idle);
    }

    #[test]
    fn status_idle_with_pending_user_message() {
        let mut session = test_session();
        session.chat.push(Message::User("hello".into()));
        session.update_status();
        // No task handle or incoming tokens → Idle
        assert_eq!(session.status(), AgentStatus::Idle);
    }

    #[test]
    fn status_done_when_assistant_is_last() {
        let mut session = test_session();
        session.chat.push(Message::User("hello".into()));
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "reply".into(),
            )));
        session.update_status();
        assert_eq!(session.status(), AgentStatus::Done);
    }

    // ---- batch redispatch lifecycle tests ----

    /// Simulates the full lifecycle of queued message batch dispatch:
    /// 1. User sends message → dispatched
    /// 2. While streaming, user queues 3 more messages
    /// 3. Stream ends → needs_redispatch is true
    /// 4. On redispatch, get_pending_user_messages collects all 3
    /// 5. After redispatch, new tokens create response after all queued msgs
    #[test]
    fn batch_redispatch_full_lifecycle() {
        let mut session = test_session();
        use crate::backend::shared;

        // Step 1: User sends first message, it gets dispatched (single)
        session.chat.push(Message::User("hello".into()));
        session.dispatched_user_count = 1;
        assert!(session.should_dispatch_remote_message());

        // Backend starts streaming
        let tx = make_streaming(&mut session);
        assert!(session.is_streaming());
        assert!(!session.should_dispatch_remote_message());

        // First tokens arrive
        session.append_token("Sure, ");
        session.append_token("I can help.");

        // Step 2: User queues 3 messages while streaming
        session.chat.push(Message::User("also".into()));
        session.chat.push(Message::User("do this".into()));
        session.chat.push(Message::User("and this".into()));

        // Should NOT dispatch while streaming
        assert!(!session.should_dispatch_remote_message());

        // More tokens arrive — should append to the streaming assistant,
        // not create new ones after the queued messages
        session.append_token(" Let me ");
        session.append_token("check.");

        // Verify the assistant text is continuous
        assert_eq!(
            session.last_assistant_text().unwrap(),
            "Sure, I can help. Let me check."
        );

        // Queued messages should still be at the end
        assert!(session.has_pending_user_message());

        // Step 3: Stream ends
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;

        assert!(!session.is_streaming());
        assert!(session.needs_redispatch_after_stream_end());

        // Step 4: At redispatch time, get_pending_user_messages should
        // collect ALL trailing user messages
        let prompt = shared::get_pending_user_messages(&session.chat);
        assert_eq!(prompt, "also\ndo this\nand this");

        // Step 5: Backend dispatches with the batch prompt (3 messages)
        session.dispatched_user_count = 3;
        let _tx2 = make_streaming(&mut session);

        // New tokens arrive — should create a new assistant after ALL
        // dispatched messages (since they were all sent in the batch)
        session.append_token("OK, doing all three.");

        // Verify chat order: response 2 should come after all 3
        // batch-dispatched user messages
        let types: Vec<&str> = session
            .chat
            .iter()
            .map(|m| match m {
                Message::User(_) => "User",
                Message::Assistant(_) => "Assistant",
                _ => "?",
            })
            .collect();
        assert_eq!(
            types,
            // Turn 1: User → Assistant
            // Turn 2: User, User, User (batch) → Assistant
            vec!["User", "Assistant", "User", "User", "User", "Assistant"],
        );
        // Verify the second assistant has the right text
        assert_eq!(
            session.last_assistant_text().unwrap(),
            "OK, doing all three."
        );
    }

    /// When all queued messages are batch-dispatched, no redispatch
    /// should be needed after the second stream completes (assuming
    /// no new messages arrive).
    #[test]
    fn no_double_redispatch_after_batch() {
        let mut session = test_session();

        // Turn 1: single dispatch
        session.chat.push(Message::User("first".into()));
        session.dispatched_user_count = 1;
        let tx = make_streaming(&mut session);
        session.append_token("response 1");
        session.chat.push(Message::User("queued A".into()));
        session.chat.push(Message::User("queued B".into()));
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;
        assert!(session.needs_redispatch_after_stream_end());

        // Turn 2: batch redispatch handles both queued messages
        session.dispatched_user_count = 2;
        let tx2 = make_streaming(&mut session);
        session.append_token("response 2");
        session.finalize_last_assistant();
        drop(tx2);
        session.incoming_tokens = None;

        // No more pending user messages after the assistant response
        assert!(
            !session.needs_redispatch_after_stream_end(),
            "should not need another redispatch when no new messages arrived"
        );
    }

    /// When a stream ends with an error (no tokens produced), the
    /// Error message should prevent infinite redispatch.
    #[test]
    fn error_prevents_redispatch_loop() {
        let mut session = test_session();

        session.chat.push(Message::User("hello".into()));
        session.dispatched_user_count = 1;
        let tx = make_streaming(&mut session);

        // Error arrives (no tokens were sent)
        session
            .chat
            .push(Message::Error("context window exceeded".into()));

        // Stream ends
        drop(tx);
        session.incoming_tokens = None;

        assert!(
            !session.needs_redispatch_after_stream_end(),
            "error should prevent redispatch"
        );
    }

    /// Verify chat ordering when queued messages arrive before any
    /// tokens, and after tokens, across a full batch lifecycle.
    #[test]
    fn chat_ordering_with_mixed_timing() {
        let mut session = test_session();

        // Turn 1 complete
        session.chat.push(Message::User("hello".into()));
        session.append_token("hi there");
        session.finalize_last_assistant();

        // User sends new message (single dispatch)
        session.chat.push(Message::User("question".into()));
        session.dispatched_user_count = 1;
        let tx = make_streaming(&mut session);

        // Queued BEFORE first token
        session.chat.push(Message::User("early queue".into()));

        // First token arrives
        session.append_token("answer ");

        // Queued AFTER first token
        session.chat.push(Message::User("late queue".into()));

        // More tokens
        session.append_token("here");

        // Verify: assistant response should be between dispatched
        // user and the queued messages
        let types: Vec<String> = session
            .chat
            .iter()
            .map(|m| match m {
                Message::User(s) => format!("U:{}", s),
                Message::Assistant(a) => format!("A:{}", a.text()),
                _ => "?".into(),
            })
            .collect();

        // The key constraint: "answer here" must appear after
        // "question" and before the queued messages
        let answer_pos = types.iter().position(|t| t == "A:answer here").unwrap();
        let question_pos = types.iter().position(|t| t == "U:question").unwrap();
        let early_pos = types.iter().position(|t| t == "U:early queue").unwrap();
        let late_pos = types.iter().position(|t| t == "U:late queue").unwrap();

        assert!(
            answer_pos > question_pos,
            "answer should come after the dispatched question"
        );
        assert!(
            early_pos > answer_pos || late_pos > answer_pos,
            "at least one queued message should be after the answer"
        );

        // Finalize and check redispatch
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;
        assert!(session.needs_redispatch_after_stream_end());
    }

    /// Queued indicator detection: helper that mimics what the UI does
    /// to find which messages are "queued".
    fn find_queued_indices(
        chat: &[Message],
        is_working: bool,
        dispatched_user_count: usize,
    ) -> Vec<usize> {
        if !is_working {
            return vec![];
        }
        let last_non_user = chat.iter().rposition(|m| !matches!(m, Message::User(_)));
        let queued_from = match last_non_user {
            Some(i) if matches!(chat[i], Message::Assistant(ref m) if m.is_streaming()) => {
                let first_trailing = i + 1;
                if first_trailing < chat.len() {
                    Some(first_trailing)
                } else {
                    None
                }
            }
            Some(i) => {
                let first_trailing = i + 1;
                let skip = dispatched_user_count.max(1);
                let queued_start = first_trailing + skip;
                if queued_start < chat.len() {
                    Some(queued_start)
                } else {
                    None
                }
            }
            None => None,
        };
        match queued_from {
            Some(qi) => (qi..chat.len())
                .filter(|&i| matches!(chat[i], Message::User(_)))
                .collect(),
            None => vec![],
        }
    }

    #[test]
    fn queued_indicator_before_first_token() {
        // Chat: [...finalized Asst], User("dispatched"), User("queued")
        // No streaming assistant yet → dispatched is being processed,
        // only "queued" should show the indicator.
        let mut session = test_session();
        session.chat.push(Message::User("prev".into()));
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "prev reply".into(),
            )));
        session.chat.push(Message::User("dispatched".into()));
        session.chat.push(Message::User("queued 1".into()));
        session.chat.push(Message::User("queued 2".into()));

        // dispatched_user_count=1: single dispatch
        let queued = find_queued_indices(&session.chat, true, 1);
        let queued_texts: Vec<&str> = queued
            .iter()
            .map(|&i| match &session.chat[i] {
                Message::User(s) => s.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(
            queued_texts,
            vec!["queued 1", "queued 2"],
            "dispatched message should not be marked as queued"
        );
    }

    #[test]
    fn queued_indicator_during_streaming() {
        // Chat: User("dispatched"), Assistant(streaming), User("queued")
        // Streaming assistant separates dispatched from queued.
        let mut session = test_session();
        session.chat.push(Message::User("dispatched".into()));
        session.append_token("streaming...");
        session.chat.push(Message::User("queued 1".into()));
        session.chat.push(Message::User("queued 2".into()));

        // dispatched_user_count doesn't matter here — streaming
        // assistant branch doesn't use it
        let queued = find_queued_indices(&session.chat, true, 1);
        let queued_texts: Vec<&str> = queued
            .iter()
            .map(|&i| match &session.chat[i] {
                Message::User(s) => s.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(
            queued_texts,
            vec!["queued 1", "queued 2"],
            "all user messages after streaming assistant should be queued"
        );
    }

    #[test]
    fn queued_indicator_not_working() {
        // When not working, nothing should be marked as queued
        let mut session = test_session();
        session.chat.push(Message::User("msg 1".into()));
        session.chat.push(Message::User("msg 2".into()));

        let queued = find_queued_indices(&session.chat, false, 0);
        assert!(
            queued.is_empty(),
            "nothing should be queued when not working"
        );
    }

    #[test]
    fn queued_indicator_no_queued_messages() {
        // Working but only one user message → nothing queued
        let mut session = test_session();
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "prev".into(),
            )));
        session.chat.push(Message::User("only one".into()));

        let queued = find_queued_indices(&session.chat, true, 1);
        assert!(
            queued.is_empty(),
            "single dispatched message should not be queued"
        );
    }

    #[test]
    fn queued_indicator_after_tool_call_with_streaming() {
        // Chat: User, Asst, ToolCalls, ToolResponse, Asst(streaming), User(queued)
        let mut session = test_session();
        session.chat.push(Message::User("do something".into()));
        session.append_token("Let me check.");

        let tool =
            crate::tools::ToolCall::invalid("c1".into(), Some("Read".into()), None, "test".into());
        session.chat.push(Message::ToolCalls(vec![tool]));
        session
            .chat
            .push(Message::ToolResponse(crate::tools::ToolResponse::error(
                "c1".into(),
                "result".into(),
            )));

        // Post-tool tokens create new streaming assistant
        session.append_token("Found it.");
        session.chat.push(Message::User("queued".into()));

        let queued = find_queued_indices(&session.chat, true, 1);
        let queued_texts: Vec<&str> = queued
            .iter()
            .map(|&i| match &session.chat[i] {
                Message::User(s) => s.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(queued_texts, vec!["queued"]);
    }

    /// Batch dispatch: when 3 messages were dispatched together,
    /// none should show "queued" before the first token arrives.
    #[test]
    fn queued_indicator_batch_dispatch_no_queued() {
        let mut session = test_session();
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "prev reply".into(),
            )));
        session.chat.push(Message::User("a".into()));
        session.chat.push(Message::User("b".into()));
        session.chat.push(Message::User("c".into()));

        // All 3 were batch-dispatched
        let queued = find_queued_indices(&session.chat, true, 3);
        assert!(
            queued.is_empty(),
            "all 3 messages were dispatched — none should show queued"
        );
    }

    /// Batch dispatch with new message queued after: 3 dispatched,
    /// then 1 more arrives. Only the new one should be "queued".
    #[test]
    fn queued_indicator_batch_with_new_queued() {
        let mut session = test_session();
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "prev reply".into(),
            )));
        session.chat.push(Message::User("a".into()));
        session.chat.push(Message::User("b".into()));
        session.chat.push(Message::User("c".into()));
        session.chat.push(Message::User("new queued".into()));

        // 3 were dispatched, 1 new arrival
        let queued = find_queued_indices(&session.chat, true, 3);
        let queued_texts: Vec<&str> = queued
            .iter()
            .map(|&i| match &session.chat[i] {
                Message::User(s) => s.as_str(),
                _ => "?",
            })
            .collect();
        assert_eq!(
            queued_texts,
            vec!["new queued"],
            "only the message after the batch should be queued"
        );
    }
}
