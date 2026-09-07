use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::agent_status::AgentStatus;
use crate::backend::BackendType;
use crate::config::AiMode;
use crate::focus_queue::FocusPriority;
use crate::git_status::GitStatusCache;
use crate::messages::{
    CompactionInfo, ExecutedTool, QuestionAnswer, RunningTool, SessionInfo, SubagentStatus,
};
use crate::session_events::ThreadingState;
use crate::{DaveApiResponse, Message};
use claude_agent_sdk_rs::PermissionMode;
use uuid::Uuid;

pub type SessionId = u32;

/// Current wall-clock time as unix seconds, matching the resolution of nostr
/// `created_at`. Used to stamp and age `ChatSession::last_activity`.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Convert PermissionMode to a stable string for nostr tags.
pub fn permission_mode_to_str(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::Plan => "plan",
        PermissionMode::AcceptEdits => "accept_edits",
        PermissionMode::Auto => "auto",
        PermissionMode::BypassPermissions => "bypass",
    }
}

/// Parse PermissionMode from a nostr tag string.
pub fn permission_mode_from_str(s: &str) -> PermissionMode {
    match s {
        "plan" => PermissionMode::Plan,
        "accept_edits" => PermissionMode::AcceptEdits,
        "auto" => PermissionMode::Auto,
        "bypass" => PermissionMode::BypassPermissions,
        _ => PermissionMode::Default,
    }
}

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
    /// Display slug of the project the cwd belongs to (git repo root basename).
    /// `None` until resolved; grouping then falls back to deriving it from `cwd`.
    /// See [`crate::worktree::project_for`].
    pub project_slug: Option<String>,
    /// Git repo root shared by all the project's worktrees — the grouping key
    /// that keeps worktrees of one repo under a single sidebar project instead of
    /// scattering per-cwd. `None` until resolved (grouping falls back to `cwd`).
    pub project_root: Option<PathBuf>,
    /// User-requested model override for new backend requests and clones.
    ///
    /// `None` means "let the backend choose its default model".
    pub requested_model: Option<String>,
    /// Model currently reported by the backend for display.
    ///
    /// This may differ from `requested_model` if the backend resolved an alias
    /// to a concrete version or fell back to a different model.
    pub model: Option<String>,
}

impl SessionDetails {
    /// Returns custom_title if set, otherwise the auto-generated title.
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.title)
    }

    /// Returns a human-friendly model name for display.
    ///
    /// Converts raw model IDs like "claude-opus-4-6-20250514" to "Opus 4.6",
    /// "gpt-5.2-codex" to "GPT-5.2 Codex", etc.
    pub fn display_model(&self) -> Option<&str> {
        self.model.as_deref().map(friendly_model_name)
    }

    /// Resolve the model to use for an API request.
    ///
    /// Returns the user-selected model if set, otherwise `None` to let
    /// the backend use its own default.
    pub fn resolve_model(&self) -> Option<String> {
        self.requested_model.clone()
    }
}

/// Table mapping model ID prefixes to human-friendly display names.
///
/// Entries are checked in order; the first matching prefix wins.
/// If no prefix matches, the raw model ID is returned as-is.
const MODEL_DISPLAY_NAMES: &[(&str, &str)] = &[
    // Claude Opus
    ("claude-opus-4-6", "Opus 4.6"),
    ("claude-opus-4-5", "Opus 4.5"),
    ("claude-opus-4", "Opus 4"),
    // Claude Sonnet
    ("claude-sonnet-4-6", "Sonnet 4.6"),
    ("claude-sonnet-4-5", "Sonnet 4.5"),
    ("claude-sonnet-4.5", "Sonnet 4.5"),
    ("claude-sonnet-4", "Sonnet 4"),
    ("claude-3-5-sonnet", "Sonnet 3.5"),
    ("claude-3-sonnet", "Sonnet 3"),
    // Claude Haiku
    ("claude-haiku-4-5", "Haiku 4.5"),
    ("claude-3-5-haiku", "Haiku 3.5"),
    ("claude-3-haiku", "Haiku 3"),
];

/// Convert a raw model ID to a human-friendly display name.
///
/// Falls back to the raw ID if no known prefix matches.
pub fn friendly_model_name(model: &str) -> &str {
    for &(prefix, display) in MODEL_DISPLAY_NAMES {
        if model.starts_with(prefix) {
            return display;
        }
    }
    model
}

/// Unified compaction intent — replaces the old `CompactAndProceedState`
/// enum *and* the separate `is_compacting: bool` field.
///
/// `None` = idle (no compaction in progress, no compact-and-proceed pending).
/// Each variant captures exactly one phase of the compaction lifecycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompactIntent {
    /// Manual compact (Compact button pressed); no "Proceed" afterwards.
    Manual,
    /// "Compact & Approve" clicked; waiting for the current stream to end
    /// so we can dispatch the compact query.
    ProceedAfterStreamEnd,
    /// Compact query dispatched; waiting for CompactionComplete.
    ProceedAfterCompaction,
    /// Compaction finished; send "Proceed" at next opportunity
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

// PermissionTracker is platform-neutral session state; it now lives in the
// agentium-core engine. Keep it reachable as `crate::session::PermissionTracker`.
pub use agentium_core::session::PermissionTracker;

/// Agentic-mode specific session data (Claude backend only)
pub struct AgenticSessionData {
    /// Permission state (pending channels, note IDs, responded set)
    pub permissions: PermissionTracker,
    /// Position in the RTS scene, as plain `(x, y)` scene coordinates.
    /// Kept egui-free so `AgenticSessionData` stays platform-neutral; the UI
    /// converts to/from `egui::Vec2` at the rendering boundary.
    pub scene_position: (f32, f32),
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
    /// Indices of in-flight `Message::ToolRunning` rows in chat, keyed by the
    /// originating `tool_use` id. A row is upgraded in place to its completed
    /// `ToolResponse` when the matching result lands (`place_tool_result`), and
    /// any still-running row is finalized at turn end (`finalize_running_tools`).
    pub running_tool_indices: HashMap<String, usize>,
    /// Compaction lifecycle state. `None` = idle.
    pub compact_intent: Option<CompactIntent>,
    /// Info from the last completed compaction (for display)
    pub last_compaction: Option<CompactionInfo>,
    /// Claude session ID to resume (UUID from Claude CLI's session storage)
    /// When set, the backend will use --resume to continue this session
    pub resume_session_id: Option<String>,
    /// Git status cache for this session's working directory
    pub git_status: GitStatusCache,
    /// Threading state for live kind-1988 event generation.
    pub live_threading: ThreadingState,
    /// Status as reported by the remote desktop's kind-31988 event.
    /// Only meaningful when session source is Remote.
    pub remote_status: Option<AgentStatus>,
    /// Timestamp of the kind-31988 event that last set `remote_status`.
    /// Used to ignore older replaceable event revisions that arrive out of order.
    pub remote_status_ts: u64,
    /// Note IDs we've already processed from live conversation polling.
    /// Prevents duplicate messages when events are loaded during restore
    /// and then appear again via the subscription.
    pub seen_note_ids: HashSet<[u8; 32]>,
    /// Highest [`EventOrder`](agentium_core::session_loader::EventOrder) already
    /// reflected in `chat` for a remote session — the fast-path tail marker.
    /// When a poll batch's new notes all sort after this, they are appended in
    /// order (O(batch)) instead of triggering a full rebuild (O(n)); a note at
    /// or before it forces a rebuild. Seeded from the loader on every rebuild
    /// (see `rebuild_remote_chat`); `None` conservatively forces a rebuild, so a
    /// missed seeding can never misorder — it only costs one extra rebuild.
    pub tail_order: Option<agentium_core::session_loader::EventOrder>,
    /// Accumulated usage metrics across queries in this session.
    pub usage: crate::messages::UsageInfo,
    /// Runtime allowlist for auto-accepting permissions this session.
    /// For Bash: stores binary names (first word of command).
    /// For other tools: stores the tool name.
    pub runtime_allows: HashSet<String>,
    /// Stable Nostr event identity for this session (d-tag for kind-31988
    /// and kind-1988 events).  Generated at creation, never changes.
    /// Separate from the Claude CLI session ID used for `--resume`.
    pub event_id: String,
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
            scene_position: (x, y),
            permission_mode: PermissionMode::Auto,
            permission_message_state: PermissionMessageState::None,
            question_answers: HashMap::new(),
            question_index: HashMap::new(),
            cwd,
            session_info: None,
            subagent_indices: HashMap::new(),
            running_tool_indices: HashMap::new(),
            compact_intent: None,
            last_compaction: None,
            resume_session_id: None,
            git_status,
            live_threading: ThreadingState::new(),
            remote_status: None,
            remote_status_ts: 0,
            seen_note_ids: HashSet::new(),
            tail_order: None,
            usage: Default::default(),
            runtime_allows: HashSet::new(),
            event_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Extract the runtime allow key from a permission request.
    /// For Bash: first word of the command (binary name).
    /// For other tools: the tool name itself.
    fn runtime_allow_key(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
        if tool_name == "Bash" {
            tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .and_then(|cmd| cmd.split_whitespace().next())
                .map(|s| s.to_string())
        } else {
            Some(tool_name.to_string())
        }
    }

    /// Check if a permission request should be auto-accepted this session
    /// because the tool matches the per-session runtime allowlist (an
    /// "allow this command for the rest of the session" grant). This is the
    /// single dave-side checkpoint every backend's permission requests flow
    /// through, so it is backend-agnostic.
    pub fn should_runtime_allow(&self, tool_name: &str, tool_input: &serde_json::Value) -> bool {
        // Decision-type prompts (AskUserQuestion / ExitPlanMode plan review)
        // always need a real user decision — a question set needs a selection
        // and a plan review needs approval, neither is a yes/no tool grant — so
        // never auto-accept them.
        if crate::messages::PermissionView::is_decision_tool(tool_name) {
            return false;
        }
        if let Some(key) = Self::runtime_allow_key(tool_name, tool_input) {
            self.runtime_allows.contains(&key)
        } else {
            false
        }
    }

    /// Add a runtime allow rule from a permission request.
    /// Returns the key that was added (for logging).
    pub fn add_runtime_allow(
        &mut self,
        tool_name: &str,
        tool_input: &serde_json::Value,
    ) -> Option<String> {
        let key = Self::runtime_allow_key(tool_name, tool_input)?;
        self.runtime_allows.insert(key.clone());
        Some(key)
    }

    /// Stable Nostr event identity (d-tag for kind-1988 / kind-31988).
    ///
    /// This is always available — every session gets a UUID at creation.
    /// It is independent of the Claude CLI session ID.
    pub fn event_session_id(&self) -> &str {
        &self.event_id
    }

    /// Whether a compaction operation is currently in-flight.
    pub fn is_compacting(&self) -> bool {
        matches!(
            self.compact_intent,
            Some(CompactIntent::Manual | CompactIntent::ProceedAfterCompaction)
        )
    }

    /// Get the CLI session ID for backend `--resume`.
    ///
    /// Returns the real Claude CLI session ID.  `None` means the backend
    /// hasn't started yet (no session to resume).
    pub fn cli_resume_id(&self) -> Option<&str> {
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
                // Keep only the most recent content up to max_output_size.
                // Must find a valid UTF-8 char boundary to avoid panics.
                if subagent.output.len() > subagent.max_output_size {
                    let mut keep_from = subagent.output.len() - subagent.max_output_size;
                    while !subagent.output.is_char_boundary(keep_from) {
                        keep_from += 1;
                    }
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

    /// Mark a subagent as failed
    pub fn fail_subagent(&mut self, chat: &mut [Message], task_id: &str, error: &str) {
        if let Some(&idx) = self.subagent_indices.get(task_id) {
            if let Some(Message::Subagent(subagent)) = chat.get_mut(idx) {
                subagent.status = SubagentStatus::Failed;
                subagent.output = error.to_string();
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

/// Tracks the lifecycle of a dispatch to the AI backend.
///
/// Transitions:
/// - `Idle → AwaitingResponse` when `send_user_message_for()` dispatches
/// - `AwaitingResponse → Streaming` when the backend produces content
/// - `Streaming | AwaitingResponse → Idle` at stream end
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DispatchState {
    /// No active dispatch.
    #[default]
    Idle,
    /// Dispatched `count` trailing user messages; backend hasn't
    /// produced visible content yet.
    AwaitingResponse { count: usize },
    /// Backend is actively producing content for this dispatch.
    Streaming { dispatched_count: usize },
}

impl DispatchState {
    /// Number of user messages that were dispatched in the current batch.
    /// Used by `append_token` for insert position and UI for queued indicator.
    pub fn dispatched_count(&self) -> usize {
        match self {
            DispatchState::Idle => 0,
            DispatchState::AwaitingResponse { count } => *count,
            DispatchState::Streaming { dispatched_count } => *dispatched_count,
        }
    }

    /// Transition: backend produced content.
    /// `AwaitingResponse → Streaming`; other states unchanged.
    pub fn backend_responded(&mut self) {
        if let DispatchState::AwaitingResponse { count } = *self {
            *self = DispatchState::Streaming {
                dispatched_count: count,
            };
        }
    }

    /// Transition: stream ended. Resets to `Idle`.
    pub fn stream_ended(&mut self) {
        *self = DispatchState::Idle;
    }
}

/// Boundary in `chat` between this turn's content (dispatched user message(s)
/// plus assistant/tool/todo output) and any queued (not-yet-dispatched) user
/// messages — i.e. the index the next piece of content is inserted at, and
/// equivalently where the queued-user run begins.
///
/// Queued user messages are always kept as the trailing run of `chat` (see
/// [`ChatSession::insert_turn_content`]). The boundary sits after the last
/// non-user message; before this turn has produced any content, the trailing
/// user run still begins with the dispatched message(s), so those are skipped.
/// `turn_has_content` is the single signal that distinguishes those two cases —
/// a tool call can be a turn's first output without any token, so the state
/// alone is not enough.
fn turn_content_boundary(
    chat: &[Message],
    dispatch_state: DispatchState,
    turn_has_content: bool,
) -> usize {
    let after_content = chat
        .iter()
        .rposition(|m| !matches!(m, Message::User(_)))
        .map(|i| i + 1)
        .unwrap_or(0);
    let skip = if turn_has_content {
        0
    } else {
        dispatch_state.dispatched_count().max(1)
    };
    (after_content + skip).min(chat.len())
}

/// Index into `chat` at which queued (not-yet-dispatched) user messages start,
/// or `None` when nothing is queued.
///
/// This is exactly the [`turn_content_boundary`] when a queued message sits past
/// it, so the "queued" indicator marks precisely the messages that will be
/// redispatched. Reads `turn_has_content` so it stays correct through a
/// tool-using turn, where the last non-user message is a tool row rather than a
/// streaming assistant.
///
/// This drives the "queued" indicator in `DaveUi::render_chat`. Tests must call
/// it rather than reimplement it: a private copy in the test module cannot
/// notice the production path changing, or going away.
pub fn queued_from(
    chat: &[Message],
    is_working: bool,
    dispatch_state: DispatchState,
    turn_has_content: bool,
) -> Option<usize> {
    if !is_working {
        return None;
    }

    let queued_start = turn_content_boundary(chat, dispatch_state, turn_has_content);
    (queued_start < chat.len()).then_some(queued_start)
}

/// A single chat session with Dave
pub struct ChatSession {
    pub id: SessionId,
    pub chat: Vec<Message>,
    pub input: String,
    /// Images staged for the next message send, cleared after dispatch.
    pub pending_images: Vec<crate::messages::ImageAttachment>,
    pub incoming_tokens: Option<Receiver<DaveApiResponse>>,
    /// Handle to the background task processing this session's AI requests.
    /// Aborted on drop to clean up the subprocess.
    pub task_handle: Option<tokio::task::JoinHandle<()>>,
    /// Tracks the dispatch lifecycle for redispatch and insert-position logic.
    pub dispatch_state: DispatchState,
    /// Whether the current turn has produced any content (assistant text, a tool
    /// row, a todo, …) yet. Reset in [`mark_dispatched`](Self::mark_dispatched)
    /// and set the first time this turn inserts content via
    /// [`insert_turn_content`](Self::insert_turn_content). Drives where new
    /// content lands relative to the dispatched user message(s): before any
    /// content exists, content must skip past the dispatched user(s); afterwards
    /// it appends after the prior content but still before queued user messages.
    turn_has_content: bool,
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
    /// When the last activity was seen, as wall-clock unix seconds (for
    /// "5m ago" display). Set from the local token stream for local sessions
    /// and from ingested note/state `created_at` for remote sessions, so a
    /// wall-clock value is required rather than a monotonic `Instant`.
    pub last_activity: Option<u64>,
    /// Focus indicator dot state (persisted in kind-31988 note).
    /// Set on status transitions, cleared when user dismisses it.
    pub indicator: Option<FocusPriority>,
    /// When set, this session is a pending placeholder waiting for the remote
    /// host to respond with a real kind-31988 session state event.
    /// Cleared when matched to an incoming session event.
    pub pending_created_at: Option<Instant>,
    /// Spawn command UUID linking this session to the kind-31989 that created it.
    /// Set on both the placeholder (sender) and the spawned session (receiver),
    /// echoed in kind-31988 events so the sender can match the response.
    pub spawn_id: Option<String>,
    /// For a *resume* placeholder: the kind-31988 d-tag of the session being
    /// revived on another host. The revived state always comes back on this
    /// d-tag (unlike a freshly-minted `spawn_id`), so the discovery fold
    /// correlates the placeholder by it — see
    /// [`Dave::pending_placeholder_for`](crate::Dave). `None` for a spawn
    /// placeholder (no session exists yet) and for real sessions.
    pub pending_resume_target: Option<String>,
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
        // Resolve the project (git repo) the cwd belongs to once, at creation —
        // `project_for` spawns git, so it must never run from a per-frame path.
        // Chat sessions have no cwd and no project.
        let project = (ai_mode == AiMode::Agentic).then(|| crate::worktree::project_for(&cwd));
        let agentic = match ai_mode {
            AiMode::Agentic => Some(AgenticSessionData::new(id, cwd)),
            AiMode::Chat => None,
        };

        ChatSession {
            id,
            chat: vec![],
            input: String::new(),
            pending_images: vec![],
            incoming_tokens: None,
            task_handle: None,
            dispatch_state: DispatchState::Idle,
            turn_has_content: false,
            cached_status: AgentStatus::Idle,
            state_dirty: true,
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
                project_slug: project.as_ref().map(|p| p.slug.clone()),
                project_root: project.map(|p| p.root),
                requested_model: None,
                model: None,
            },
            backend_type,
            last_activity: None,
            indicator: None,
            pending_created_at: None,
            spawn_id: None,
            pending_resume_target: None,
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
            if !resume_session_id.is_empty() {
                agentic.resume_session_id = Some(resume_session_id);
            }
        }
        session.details.title = title;
        session
    }

    /// Create a lightweight pending placeholder for a remote spawn/resume command.
    /// Skips `AgenticSessionData` (no git status, no threading, no subscriptions)
    /// since the placeholder only exists until the real session event arrives.
    ///
    /// `resume_target` is the d-tag of the session being revived for a *resume*
    /// placeholder (so the discovery fold can correlate the revived state by its
    /// stable id), or `None` for a *spawn* placeholder (correlated by `spawn_id`).
    pub fn new_pending_placeholder(
        id: SessionId,
        cwd: PathBuf,
        hostname: String,
        backend_type: BackendType,
        spawn_id: String,
        resume_target: Option<String>,
    ) -> Self {
        ChatSession {
            id,
            chat: vec![],
            input: String::new(),
            incoming_tokens: None,
            task_handle: None,
            dispatch_state: DispatchState::Idle,
            turn_has_content: false,
            cached_status: AgentStatus::Pending,
            state_dirty: false, // placeholder should not publish state events
            focus_requested: false,
            ai_mode: AiMode::Agentic,
            agentic: None, // no agentic data — placeholder only
            source: SessionSource::Remote,
            details: SessionDetails {
                title: "Connecting...".to_string(),
                custom_title: None,
                hostname,
                cwd: Some(cwd),
                home_dir: String::new(),
                // Placeholder: the real project is filled in on hydration from the
                // arriving state event (remote) or recomputed from git (local).
                project_slug: None,
                project_root: None,
                requested_model: None,
                model: None,
            },
            backend_type,
            last_activity: None,
            indicator: None,
            pending_images: vec![],
            pending_created_at: Some(Instant::now()),
            spawn_id: Some(spawn_id),
            pending_resume_target: resume_target,
        }
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

    /// Record host/agent activity at `created_at` (wall-clock unix seconds),
    /// keeping the newest so out-of-order remote notes never move it backwards.
    pub fn mark_activity(&mut self, created_at: u64) {
        self.last_activity = Some(
            self.last_activity
                .map_or(created_at, |cur| cur.max(created_at)),
        );
    }

    /// Check if session has pending permission requests that genuinely
    /// need user input (i.e. would NOT be auto-accepted by the runtime
    /// allowlist).
    pub fn has_pending_permissions(&self) -> bool {
        if self.is_remote() {
            // Remote: check for unresponded PermissionRequest messages in chat,
            // but skip any that the runtime allowlist would auto-accept.
            let agentic = self.agentic.as_ref();
            let responded = agentic.map(|a| &a.permissions.responded);
            return self.chat.iter().any(|msg| {
                if let Message::PermissionRequest(req) = msg {
                    if req.response.is_some() {
                        return false;
                    }
                    if responded.is_some_and(|ids| ids.contains_key(&req.id)) {
                        return false;
                    }
                    // Skip if runtime allowlist would auto-accept
                    if agentic
                        .is_some_and(|a| a.should_runtime_allow(&req.tool_name, &req.tool_input))
                    {
                        return false;
                    }
                    true
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

    /// Auto-resolve any pending local permissions that now match the
    /// runtime allowlist (e.g. after the user clicked "Allow Always"
    /// and the allowlist was updated).  Returns the number resolved.
    pub fn auto_resolve_runtime_allowed(&mut self) -> usize {
        let Some(agentic) = &self.agentic else {
            return 0;
        };
        if agentic.permissions.pending.is_empty() {
            return 0;
        }

        // Collect IDs of pending permissions whose tool matches the allowlist
        let to_resolve: Vec<uuid::Uuid> = self
            .chat
            .iter()
            .filter_map(|msg| {
                if let Message::PermissionRequest(req) = msg {
                    if req.response.is_none()
                        && agentic.permissions.pending.contains_key(&req.id)
                        && agentic.should_runtime_allow(&req.tool_name, &req.tool_input)
                    {
                        return Some(req.id);
                    }
                }
                None
            })
            .collect();

        if to_resolve.is_empty() {
            return 0;
        }

        // The allowlist accepted these without a user click, so flag them as
        // auto-accepted — their responded rows start expanded for review.
        for msg in self.chat.iter_mut() {
            if let Message::PermissionRequest(req) = msg {
                if to_resolve.contains(&req.id) {
                    req.auto_accepted = true;
                }
            }
        }

        // Resolve each: send Allow on the oneshot and mark in chat
        let agentic = self.agentic.as_mut().unwrap();
        for id in &to_resolve {
            agentic.permissions.resolve(
                &mut self.chat,
                *id,
                crate::messages::PermissionResponseType::Allowed,
                None,
                false,
                Some(crate::messages::PermissionResponse::Allow { message: None }),
            );
        }

        to_resolve.len()
    }

    /// Check if session is in plan mode
    pub fn is_plan_mode(&self) -> bool {
        self.agentic
            .as_ref()
            .is_some_and(|a| a.permission_mode == PermissionMode::Plan)
    }

    /// Get the current permission mode (defaults to Default for non-agentic)
    pub fn permission_mode(&self) -> PermissionMode {
        self.agentic
            .as_ref()
            .map(|a| a.permission_mode)
            .unwrap_or(PermissionMode::Default)
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

    /// Mark a subagent as failed
    pub fn fail_subagent(&mut self, task_id: &str, error: &str) {
        if let Some(ref mut agentic) = self.agentic {
            agentic.fail_subagent(&mut self.chat, task_id, error);
        }
    }

    /// Whether any background subagent is still running.
    ///
    /// A background subagent (`run_in_background`) keeps executing after the
    /// foreground turn that launched it completes; the CLI resumes the session
    /// with a wake-up turn once it finishes. Until then the session is still
    /// doing work even though no foreground turn is in flight, so
    /// [`status`](Self::status) reports `Working`.
    pub fn has_running_background_subagent(&self) -> bool {
        let Some(agentic) = &self.agentic else {
            return false;
        };
        agentic.subagent_indices.values().any(|&idx| {
            matches!(
                self.chat.get(idx),
                Some(Message::Subagent(info))
                    if info.background && info.status == SubagentStatus::Running
            )
        })
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

    /// Push an in-flight `Message::ToolRunning` row and record its chat index so
    /// the matching result can upgrade it in place. Mirrors how a subagent row
    /// is pushed on spawn (`handle_subagent_spawned`): the index is tracked only
    /// when agentic state exists, which is where running tools originate.
    pub fn push_running_tool(&mut self, running: RunningTool) {
        let tool_use_id = running.tool_use_id.clone();
        // Insert before any queued user messages so they stay trailing, and
        // record the position the row actually landed at.
        let idx = self.insert_turn_content(Message::ToolRunning(running));
        if let Some(agentic) = &mut self.agentic {
            agentic.running_tool_indices.insert(tool_use_id, idx);
        }
    }

    /// Place a completed foreground tool result into chat. When it correlates to
    /// an in-flight running row (by `tool_use_id`), that row is upgraded in
    /// place — no index shift, so sibling `subagent_indices` /
    /// `running_tool_indices` stay valid and message ordering is preserved.
    /// Otherwise (no running row, or a stale index) the result is appended.
    pub fn place_tool_result(&mut self, result: ExecutedTool) {
        let running_idx = result.tool_use_id.as_ref().and_then(|id| {
            self.agentic
                .as_mut()
                .and_then(|agentic| agentic.running_tool_indices.remove(id))
        });
        let message = Message::ToolResponse(crate::tools::ToolResponse::executed_tool(result));
        match running_idx {
            Some(idx) if matches!(self.chat.get(idx), Some(Message::ToolRunning(_))) => {
                self.chat[idx] = message;
            }
            // No running row to upgrade (auto-accepted tool, or a stale index):
            // insert before queued user messages rather than at the very end.
            _ => {
                self.insert_turn_content(message);
            }
        }
    }

    /// Resolve any still-running tool rows at a turn boundary. A tool whose
    /// result never arrived (an interrupted turn) would otherwise keep spinning
    /// forever; convert each dangling `Message::ToolRunning` to its terminal
    /// static `ToolResponse` in place and clear the index map.
    pub fn finalize_running_tools(&mut self) {
        let Some(agentic) = &mut self.agentic else {
            return;
        };
        // Collect first: draining the map while indexing `self.chat` would be a
        // double &mut borrow. This runs at the turn boundary, not per frame, so
        // the small allocation is fine.
        let dangling: Vec<usize> = agentic
            .running_tool_indices
            .drain()
            .map(|(_, i)| i)
            .collect();
        for idx in dangling {
            if let Some(Message::ToolRunning(running)) = self.chat.get(idx) {
                let executed = running.to_executed();
                self.chat[idx] =
                    Message::ToolResponse(crate::tools::ToolResponse::executed_tool(executed));
            }
        }
    }

    /// Update the session title from the last message (user or assistant)
    pub fn update_title_from_last_message(&mut self) {
        for msg in self.chat.iter().rev() {
            let text: &str = match msg {
                Message::User(msg) => {
                    let t = msg.as_str();
                    if t.is_empty() && !msg.images.is_empty() {
                        "[Image]"
                    } else {
                        t
                    }
                }
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
    /// Also sets the focus indicator when transitioning to a notable state.
    pub fn update_status(&mut self) {
        let new_status = self.derive_status();
        if new_status != self.cached_status {
            self.cached_status = new_status;
            if let Some(priority) = FocusPriority::from_status(new_status) {
                // Set indicator when entering a notable state
                self.indicator = Some(priority);
            } else if self.indicator.is_some() {
                // Clear stale indicator when agent resumes work
                self.indicator = None;
            }
            self.state_dirty = true;
        }
    }

    /// Derive status from the current session state
    fn derive_status(&self) -> AgentStatus {
        // Pending placeholder sessions always show Pending
        if self.pending_created_at.is_some() {
            return AgentStatus::Pending;
        }

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

        // A background subagent is still running: the foreground turn has ended
        // (no task handle) but a wake-up turn will resume the session when the
        // task finishes, so it's still Working.
        if self.has_running_background_subagent() {
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
    /// Cached agent grouping: host → project → cwd → sessions.
    /// Rebuilt via `rebuild_groups()` when sessions change.
    host_groups: Vec<HostGroup>,
    /// Whether host grouping cache must be rebuilt before reads.
    host_groups_dirty: bool,
    /// Cached chat session IDs in recency order.
    chat_ids: Vec<SessionId>,
    /// Whether chat ID cache must be rebuilt before reads.
    chat_ids_dirty: bool,
}

/// A group of sessions under a single hostname.
#[derive(Clone)]
pub struct HostGroup {
    pub hostname: String,
    pub project_groups: Vec<ProjectGroup>,
}

/// A group of workspaces (cwds/worktrees) belonging to one project.
///
/// A project is the git repository shared by all its worktrees, so every
/// worktree lands under one sidebar entry instead of scattering per-cwd. A cwd
/// that isn't in a git repo forms its own single-workspace project.
#[derive(Clone)]
pub struct ProjectGroup {
    /// Display slug — the git repo root basename (or cwd basename for non-git).
    pub slug: String,
    /// Grouping key — the project root shared by the worktrees, or the cwd itself.
    pub root: PathBuf,
    pub cwd_groups: Vec<CwdGroup>,
}

impl ProjectGroup {
    /// A project renders "flat" — sessions directly under the project header,
    /// with no workspace sub-level — when it has a single workspace located at
    /// the project root (the common single-checkout case). Multi-workspace
    /// projects (worktrees) and single workspaces sitting in a subdirectory keep
    /// the workspace level so their paths stay distinguishable.
    pub fn is_flat(&self) -> bool {
        self.cwd_groups.len() == 1 && self.cwd_groups[0].cwd == self.root
    }
}

/// A group of sessions sharing a working directory.
#[derive(Clone)]
pub struct CwdGroup {
    pub display_cwd: String,
    pub cwd: PathBuf,
    pub session_ids: Vec<SessionId>,
}

/// Fallback project slug for a root that carries no persisted slug (old events /
/// non-git cwds): the root's basename, or the whole path if it has no final
/// component. A pure alternative to [`crate::worktree::project_for`] that never
/// spawns git, safe to call from the grouping rebuild.
fn project_slug_from_root(root: &std::path::Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

/// Append a workspace's sessions to `ids` unless it's a collapsible
/// (multi-session) folder that's currently collapsed.
///
/// A single-session workspace renders inline with no folder header (see
/// `cwd_section_ui`), so it has no collapse UI and stays visible regardless of
/// a stale `is_cwd_collapsed` entry. Only multi-session workspaces get a
/// collapsible folder. This is the one rule shared by flat projects and the
/// workspaces inside a multi-workspace project, keeping `visual_order` in lock
/// step with what `session_list` renders.
fn push_visible_cwd(
    ids: &mut Vec<SessionId>,
    collapse: &crate::collapse_state::CollapseState,
    hostname: &str,
    cwd_group: &CwdGroup,
) {
    let collapsible = cwd_group.session_ids.len() > 1;
    if collapsible && collapse.is_cwd_collapsed(hostname, &cwd_group.cwd) {
        return;
    }
    ids.extend_from_slice(&cwd_group.session_ids);
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
            host_groups_dirty: false,
            chat_ids: Vec::new(),
            chat_ids_dirty: false,
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
        self.rebuild_groups();

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
        self.rebuild_groups();

        id
    }

    /// Create a lightweight pending placeholder session for a remote spawn or
    /// resume. `resume_target` names the session being revived (resume) or is
    /// `None` (spawn) — see [`ChatSession::new_pending_placeholder`].
    pub fn new_pending_placeholder(
        &mut self,
        cwd: PathBuf,
        hostname: String,
        backend_type: BackendType,
        spawn_id: String,
        resume_target: Option<String>,
    ) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;

        let session = ChatSession::new_pending_placeholder(
            id,
            cwd,
            hostname,
            backend_type,
            spawn_id,
            resume_target,
        );
        self.sessions.insert(id, session);
        self.order.insert(0, id);
        self.active = Some(id);
        self.rebuild_groups();

        id
    }

    /// Get a reference to the active session
    pub fn get_active(&self) -> Option<&ChatSession> {
        self.active.and_then(|id| self.sessions.get(&id))
    }

    /// Get a mutable reference to the active session
    pub fn get_active_mut(&mut self) -> Option<&mut ChatSession> {
        self.mark_grouping_cache_dirty();
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
            self.rebuild_groups();
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
            self.mark_grouping_cache_dirty();
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
        self.mark_grouping_cache_dirty();
        self.sessions.get_mut(&id)
    }

    /// Iterate over all sessions
    pub fn iter(&self) -> impl Iterator<Item = &ChatSession> {
        self.sessions.values()
    }

    /// Iterate over all sessions mutably
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut ChatSession> {
        self.mark_grouping_cache_dirty();
        self.sessions.values_mut()
    }

    /// Update status for all sessions.
    ///
    /// First drains any pending permissions that now match the runtime
    /// allowlist (e.g. after "Allow Always"), then derives status.
    pub fn update_all_statuses(&mut self) {
        for session in self.sessions.values_mut() {
            session.auto_resolve_runtime_allowed();
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

    /// Get cached agent session groups: host → cwd → sessions.
    pub fn host_groups(&mut self) -> &[HostGroup] {
        self.ensure_grouping_cache();
        &self.host_groups
    }

    /// Collect unique remote hostnames from all sessions.
    pub fn remote_hostnames(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .sessions
            .values()
            .filter(|session| session.is_remote())
            .map(|session| session.details.hostname.clone())
            .filter(|hostname| !hostname.is_empty())
            .collect();
        hosts.sort_unstable();
        hosts.dedup();
        hosts
    }

    /// Get cached chat session IDs in recency order.
    pub fn chat_ids(&mut self) -> &[SessionId] {
        self.ensure_grouping_cache();
        &self.chat_ids
    }

    /// Session IDs in visual/display order (host → project → cwd groups, then
    /// chats), filtered by collapse state. Sessions inside a collapsed host,
    /// project, or workspace folder are excluded — this must mirror what
    /// `session_list` renders so keyboard nav lands only on visible rows.
    pub fn visual_order(
        &mut self,
        collapse: &crate::collapse_state::CollapseState,
    ) -> Vec<SessionId> {
        self.ensure_grouping_cache();
        let mut ids = Vec::new();
        for host_group in &self.host_groups {
            if collapse.is_host_collapsed(&host_group.hostname) {
                continue;
            }
            for project in &host_group.project_groups {
                if project.is_flat() {
                    // Flat projects render flush (no project header): the single
                    // workspace inlines a lone session or folds multiple.
                    push_visible_cwd(
                        &mut ids,
                        collapse,
                        &host_group.hostname,
                        &project.cwd_groups[0],
                    );
                    continue;
                }

                // Multi-workspace project: a collapsible slug header wrapping its
                // workspaces. Each workspace follows the same inline-single /
                // folder-multi rule as a flat project, so a single-session
                // workspace stays visible even with a stale collapse entry.
                if collapse.is_project_collapsed(&host_group.hostname, &project.root) {
                    continue;
                }
                for cwd_group in &project.cwd_groups {
                    push_visible_cwd(&mut ids, collapse, &host_group.hostname, cwd_group);
                }
            }
        }
        ids.extend_from_slice(&self.chat_ids);
        ids
    }

    /// Get a session's index in the recency-ordered list (for keyboard shortcuts).
    pub fn session_index(&self, id: SessionId) -> Option<usize> {
        self.order.iter().position(|&oid| oid == id)
    }

    /// Rebuild the cached host → project → cwd groups from current sessions.
    /// Call after adding/removing sessions or changing a session's cwd/project.
    pub fn rebuild_groups(&mut self) {
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

                let hostname = session.details.hostname.clone();
                let cwd = session.cwd().or(session.details.cwd.as_ref());
                let raw_cwd = cwd.cloned().unwrap_or_default();
                let cwd_display = match cwd {
                    Some(cwd) => {
                        let home = &session.details.home_dir;
                        if home.is_empty() {
                            crate::path_utils::abbreviate_path(cwd)
                        } else {
                            crate::path_utils::abbreviate_with_home(cwd, home)
                        }
                    }
                    None => "(unknown)".to_string(),
                };

                // Project identity: the persisted/computed repo root groups all
                // worktrees together. Old events / non-git cwds have no project,
                // so fall back to the cwd itself as a single-workspace project.
                let project_root = session
                    .details
                    .project_root
                    .clone()
                    .unwrap_or_else(|| raw_cwd.clone());
                let project_slug = session
                    .details
                    .project_slug
                    .clone()
                    .unwrap_or_else(|| project_slug_from_root(&project_root));

                // Find or create host group
                let host_group = if let Some(hg) = self
                    .host_groups
                    .iter_mut()
                    .find(|hg| hg.hostname == hostname)
                {
                    hg
                } else {
                    self.host_groups.push(HostGroup {
                        hostname: hostname.clone(),
                        project_groups: Vec::new(),
                    });
                    self.host_groups.last_mut().unwrap()
                };

                // Find or create project group within host (keyed by root)
                let project_group = if let Some(pg) = host_group
                    .project_groups
                    .iter_mut()
                    .find(|pg| pg.root == project_root)
                {
                    pg
                } else {
                    host_group.project_groups.push(ProjectGroup {
                        slug: project_slug,
                        root: project_root,
                        cwd_groups: Vec::new(),
                    });
                    host_group.project_groups.last_mut().unwrap()
                };

                // Find or create cwd group within project
                if let Some(cg) = project_group
                    .cwd_groups
                    .iter_mut()
                    .find(|cg| cg.cwd == raw_cwd)
                {
                    cg.session_ids.push(id);
                } else {
                    project_group.cwd_groups.push(CwdGroup {
                        display_cwd: cwd_display,
                        cwd: raw_cwd,
                        session_ids: vec![id],
                    });
                }
            }
        }

        // Sort host groups alphabetically (empty hostname = local, sorts first)
        self.host_groups.sort_by(|a, b| a.hostname.cmp(&b.hostname));

        for host_group in &mut self.host_groups {
            // Sort projects by slug (then root, so same-named repos stay stable)
            host_group
                .project_groups
                .sort_by(|a, b| a.slug.cmp(&b.slug).then_with(|| a.root.cmp(&b.root)));

            for project in &mut host_group.project_groups {
                // Sort workspaces, and sessions within each
                project
                    .cwd_groups
                    .sort_by(|a, b| a.display_cwd.cmp(&b.display_cwd));

                for cwd_group in &mut project.cwd_groups {
                    cwd_group.session_ids.sort_by(|a, b| {
                        let title_a = self
                            .sessions
                            .get(a)
                            .map(|s| s.details.display_title())
                            .unwrap_or("");
                        let title_b = self
                            .sessions
                            .get(b)
                            .map(|s| s.details.display_title())
                            .unwrap_or("");
                        title_a.cmp(title_b).then(a.cmp(b))
                    });
                }
            }
        }

        self.host_groups_dirty = false;
        self.chat_ids_dirty = false;
    }

    /// Mark cached grouping state dirty after mutable session access.
    fn mark_grouping_cache_dirty(&mut self) {
        self.host_groups_dirty = true;
        self.chat_ids_dirty = true;
    }

    /// Ensure host/cwd and chat caches are rebuilt before read access.
    fn ensure_grouping_cache(&mut self) {
        if self.host_groups_dirty || self.chat_ids_dirty {
            self.rebuild_groups();
        }
    }
}

impl ChatSession {
    /// Whether the session is actively streaming a response from the backend.
    pub fn is_streaming(&self) -> bool {
        self.incoming_tokens.is_some()
    }

    /// Whether a dispatch is active (message sent to backend, waiting for
    /// or receiving response). This is more reliable than `is_streaming()`
    /// because it covers the window between dispatch and first token arrival.
    pub fn is_dispatched(&self) -> bool {
        !matches!(self.dispatch_state, DispatchState::Idle)
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
        // Content arrived — transition AwaitingResponse → Streaming.
        self.dispatch_state.backend_responded();
        self.mark_activity(now_unix());

        // Fast path: last message is the ACTIVE (still-streaming) assistant
        // response. A finalized assistant must NOT be extended — e.g. when a
        // spontaneous wake-up turn begins with no separating user message, the
        // last message is the previous turn's finalized assistant, and its
        // parser is already dropped. Fall through to start a new message.
        if let Some(Message::Assistant(msg)) = self.chat.last_mut() {
            if msg.is_streaming() {
                msg.push_token(token);
                return;
            }
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
            // No streaming assistant reachable — start a new one. Route through
            // `insert_turn_content` so it lands after the dispatched user
            // message(s) and this turn's prior content, but before any queued
            // user messages (which must stay trailing to trigger redispatch).
            let mut msg = crate::messages::AssistantMessage::new();
            msg.push_token(token);
            self.insert_turn_content(Message::Assistant(msg));
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
    /// the backend right now. Returns false if a dispatch is already
    /// active — the message is already in chat and will be picked up
    /// when the current stream finishes.
    pub fn should_dispatch_remote_message(&self) -> bool {
        !self.is_dispatched() && self.has_pending_user_message()
    }

    /// Mark the current trailing user messages as dispatched to the backend.
    /// Call this when starting a new stream for this session.
    pub fn mark_dispatched(&mut self) {
        let count = self.trailing_user_count();
        self.dispatch_state = DispatchState::AwaitingResponse { count };
        // A fresh turn has produced nothing yet, so its first content must skip
        // past the just-dispatched user message(s).
        self.turn_has_content = false;
    }

    /// Index at which this turn's next content (assistant text, a tool row, a
    /// todo, an error, a subagent, …) should be inserted so it lands after the
    /// dispatched user message(s) and this turn's earlier content, but BEFORE any
    /// queued (trailing) user messages.
    ///
    /// Keeping queued user messages as the trailing run of `chat` is load-bearing:
    /// [`needs_redispatch_after_stream_end`](Self::needs_redispatch_after_stream_end)
    /// and [`queued_from`] read the tail to find them, and the redispatch prompt
    /// collects trailing user messages. Content pushed to the end would bury a
    /// queued message and silently drop it.
    fn turn_content_pos(&self) -> usize {
        turn_content_boundary(&self.chat, self.dispatch_state, self.turn_has_content)
    }

    /// Whether the current turn has produced any content yet (drives the queued
    /// indicator's insert-boundary — see [`queued_from`]).
    pub fn turn_has_content(&self) -> bool {
        self.turn_has_content
    }

    /// Insert this turn's content at [`turn_content_pos`](Self::turn_content_pos),
    /// preserving the trailing queued-user invariant, and return the index it
    /// landed at so callers that track message positions (running tools,
    /// subagents) can record it. New content always lands after this turn's prior
    /// content, so previously-recorded indices never shift.
    pub fn insert_turn_content(&mut self, message: Message) -> usize {
        let pos = self.turn_content_pos();
        self.chat.insert(pos, message);
        self.turn_has_content = true;
        pos
    }

    /// Count trailing user messages at the end of the chat.
    pub fn trailing_user_count(&self) -> usize {
        self.chat
            .iter()
            .rev()
            .take_while(|m| matches!(m, Message::User(_)))
            .count()
    }

    /// Whether the session needs a re-dispatch after a stream ends.
    /// This catches user messages that arrived while we were streaming.
    ///
    /// Uses `dispatch_state` to distinguish genuinely new messages from
    /// messages that were already dispatched:
    ///
    /// - `Streaming`: backend responded, so any trailing user messages
    ///   are genuinely new (queued during the response).
    /// - `AwaitingResponse`: backend returned empty. Only redispatch if
    ///   NEW messages arrived beyond what was dispatched (prevents the
    ///   infinite loop on empty responses).
    /// - `Idle`: nothing to redispatch.
    pub fn needs_redispatch_after_stream_end(&self) -> bool {
        match self.dispatch_state {
            DispatchState::Streaming { .. } => self.has_pending_user_message(),
            DispatchState::AwaitingResponse { count } => self.trailing_user_count() > count,
            DispatchState::Idle => false,
        }
    }

    /// If "Compact & Approve" has reached ReadyToProceed, consume the state,
    /// push a "Proceed" user message, and return true.
    ///
    /// Called from:
    /// - Local sessions: at stream-end in process_events()
    /// - Remote sessions: on compaction_complete in poll_remote_conversation_events()
    pub fn take_compact_and_proceed(&mut self) -> bool {
        let ready = self
            .agentic
            .as_ref()
            .is_some_and(|a| a.compact_intent == Some(CompactIntent::ReadyToProceed));

        if !ready {
            return false;
        }

        self.agentic.as_mut().unwrap().compact_intent = None;
        self.chat
            .push(Message::User("Proceed with implementing the plan.".into()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::collapse_state::CollapseState;
    use crate::config::AiMode;
    use crate::messages::AssistantMessage;
    use std::sync::mpsc;

    #[test]
    fn runtime_allowlist_grants_persist_per_session() {
        let mut agentic = AgenticSessionData::new(1, PathBuf::from("/tmp"));

        // Off: a command not on the allowlist is not auto-accepted.
        let ls = serde_json::json!({ "command": "ls -la" });
        assert!(!agentic.should_runtime_allow("Bash", &ls));

        // Granting `ls` (by binary name) auto-accepts future `ls` invocations
        // this session, but not other binaries.
        agentic.add_runtime_allow("Bash", &ls);
        assert!(agentic.should_runtime_allow("Bash", &serde_json::json!({ "command": "ls foo" })));
        assert!(
            !agentic.should_runtime_allow("Bash", &serde_json::json!({ "command": "rm -rf /" }))
        );
    }

    #[test]
    fn decision_tools_never_auto_accepted() {
        let mut agentic = AgenticSessionData::new(1, PathBuf::from("/tmp"));

        let question = serde_json::json!({
            "questions": [{
                "header": "Approach",
                "question": "Which one?",
                "options": [{ "label": "A" }, { "label": "B" }],
            }],
        });
        let plan = serde_json::json!({ "plan": "# Do the thing" });

        // A question set / plan review needs a real user decision, so it is
        // never auto-accepted — not even if it were somehow on the allowlist.
        assert!(!agentic.should_runtime_allow("AskUserQuestion", &question));
        assert!(!agentic.should_runtime_allow("ExitPlanMode", &plan));

        agentic.add_runtime_allow("AskUserQuestion", &question);
        agentic.add_runtime_allow("ExitPlanMode", &plan);
        assert!(!agentic.should_runtime_allow("AskUserQuestion", &question));
        assert!(!agentic.should_runtime_allow("ExitPlanMode", &plan));
    }

    fn test_session() -> ChatSession {
        ChatSession::new(
            1,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        )
    }

    #[test]
    fn mark_activity_keeps_the_newest() {
        let mut session = test_session();
        assert_eq!(session.last_activity, None);

        session.mark_activity(1_000);
        assert_eq!(session.last_activity, Some(1_000));

        // A newer timestamp advances it.
        session.mark_activity(2_000);
        assert_eq!(session.last_activity, Some(2_000));

        // An older (out-of-order) timestamp does not move it backwards.
        session.mark_activity(1_500);
        assert_eq!(session.last_activity, Some(2_000));
    }

    fn create_grouped_session(
        mgr: &mut SessionManager,
        hostname: &str,
        cwd: &str,
        title: &str,
        ai_mode: AiMode,
    ) -> SessionId {
        let backend = match ai_mode {
            AiMode::Agentic => BackendType::Claude,
            AiMode::Chat => BackendType::OpenAI,
        };
        let id = mgr.new_session(PathBuf::from(cwd), ai_mode, backend);
        let session = mgr.get_mut(id).expect("session should exist");
        session.details.hostname = hostname.to_string();
        session.details.title = title.to_string();
        session.details.custom_title = None;
        session.details.home_dir = "/home/tester".to_string();
        id
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

        // Dispatch and start streaming
        let _tx = make_streaming(&mut session);

        // New user message arrives while streaming
        session.chat.push(Message::User("another".into()));
        assert!(!session.should_dispatch_remote_message());
    }

    #[test]
    fn redispatch_after_stream_ends_with_pending_user_message() {
        let mut session = test_session();
        session.chat.push(Message::User("msg1".into()));

        // Dispatch and start streaming
        let tx = make_streaming(&mut session);

        // Assistant responds via append_token (transitions to Streaming)
        session.append_token("response");
        session.finalize_last_assistant();

        // New user message arrives while stream is still open
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

        // Dispatch and start streaming
        let tx = make_streaming(&mut session);

        // Backend responds
        session.append_token("done");
        session.finalize_last_assistant();

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

        // Dispatch and start streaming
        let tx = make_streaming(&mut session);

        // Messages arrive one per frame while streaming
        session.chat.push(Message::User("msg2".into()));
        assert!(!session.should_dispatch_remote_message());

        session.chat.push(Message::User("msg3".into()));
        assert!(!session.should_dispatch_remote_message());

        // Stream ends (backend didn't produce content — e.g. connection dropped)
        drop(tx);
        session.incoming_tokens = None;

        // Should redispatch — new messages arrived beyond what was dispatched
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

        // User sends initial message, dispatched and streaming starts
        session.chat.push(Message::User("hello".into()));
        let _tx = make_streaming(&mut session);
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
        let _tx = make_streaming(&mut session);
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

        // User sends a new message (primary, not queued). The real send path
        // (`send_user_message_for`) marks it dispatched before streaming, which
        // is what distinguishes a dispatched user message from a queued one.
        session.chat.push(Message::User("follow up".into()));
        session.mark_dispatched();

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

    /// A spontaneous wake-up turn (a background task completing) begins with no
    /// separating user message: the last chat message is the PREVIOUS turn's
    /// finalized assistant. Its tokens must start a NEW assistant bubble, not
    /// extend the finalized one (whose parser is already dropped, so appended
    /// text would neither render nor belong to that turn).
    #[test]
    fn wakeup_tokens_after_finalized_assistant_start_new_bubble() {
        let mut session = test_session();

        // Complete turn 1 (user + finalized assistant) with no queued message.
        session
            .chat
            .push(Message::User("start a background task".into()));
        session.append_token("started it");
        session.finalize_last_assistant();
        session.dispatch_state.stream_ended();

        let before = session.chat.len();

        // Wake-up turn tokens arrive with the finalized assistant last and no
        // user message in between.
        session.append_token("bg task ");
        session.append_token("done");

        // A new assistant bubble was created for the wake-up turn.
        assert_eq!(session.chat.len(), before + 1);
        assert!(matches!(session.chat.last(), Some(Message::Assistant(_))));
        assert_eq!(session.last_assistant_text().unwrap(), "bg task done");

        // The previous turn's assistant is preserved untouched.
        let preserved = session
            .chat
            .iter()
            .any(|m| matches!(m, Message::Assistant(msg) if msg.text() == "started it"));
        assert!(preserved, "previous assistant should be preserved");
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
        session.mark_dispatched();

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
                Message::User(s) if s.text == "follow up" => Some("U:follow up"),
                Message::User(s) if s.text == "queued msg" => Some("U:queued msg"),
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

        // Asserting the queued message survived says nothing: this function
        // can't add, remove or reorder messages. Assert what it is for — the
        // assistant sitting *behind* the queued user message got finalized.
        let assistant = session
            .chat
            .iter()
            .find_map(|m| match m {
                Message::Assistant(a) => Some(a),
                _ => None,
            })
            .expect("the streamed assistant is in the chat");
        assert!(
            !assistant.is_streaming(),
            "the assistant behind the queued user message must be finalized"
        );
    }

    // ---- status tests ----

    /// Helper to put a session into "streaming" state.
    /// Also calls `mark_dispatched()` to mirror what `send_user_message_for()`
    /// does in real code — the trailing user messages are marked as dispatched.
    fn make_streaming(session: &mut ChatSession) -> mpsc::Sender<DaveApiResponse> {
        session.mark_dispatched();
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
        assert!(session.should_dispatch_remote_message());

        // Backend starts streaming (mark_dispatched called by make_streaming)
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
        let tx = make_streaming(&mut session);
        session.append_token("response 1");
        session.chat.push(Message::User("queued A".into()));
        session.chat.push(Message::User("queued B".into()));
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;
        assert!(session.needs_redispatch_after_stream_end());

        // Turn 2: batch redispatch handles both queued messages
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

    /// Reproduction: a message queued *during* a tool-using turn must still be
    /// redispatched after the turn ends. Any mid-turn message that lands at the
    /// end of `chat` (a second tool's running row, a todo update, an error)
    /// buries the queued user message so it is no longer `chat.last()`, and the
    /// redispatch check (which looks at the last message) silently drops it.
    ///
    /// This is the concrete failure behind "queued sending doesn't work": a
    /// plain-text turn works, but the common agentic case — queueing behind a
    /// turn that keeps using tools — loses the queued message entirely.
    #[test]
    fn queued_message_survives_tool_activity_after_queue() {
        let mut session = test_session();

        // Turn 1 dispatched.
        session.chat.push(Message::User("do the thing".into()));
        let _tx = make_streaming(&mut session);

        // Assistant text, then a tool starts running.
        session.append_token("On it. ");
        session.push_running_tool(running_tool("t1", "Read", "hostname"));

        // User queues a follow-up while the first tool is in flight.
        session
            .chat
            .push(Message::User("also check the tests".into()));

        // First tool completes — resolved in place (fine).
        session.place_tool_result(executed_tool("t1", "Read"));

        // Claude keeps working: a SECOND tool. `push_running_tool` appends to the
        // end of chat, landing *after* the queued user message and burying it.
        session.push_running_tool(running_tool("t2", "Bash", "cargo test"));
        session.place_tool_result(executed_tool("t2", "Bash"));
        session.append_token("Done.");

        // Turn ends.
        session.finalize_last_assistant();
        session.finalize_running_tools();

        // The queued message must still be redispatched and its text recoverable.
        assert!(
            session.needs_redispatch_after_stream_end(),
            "queued message must trigger redispatch after a tool-using turn; \
             chat tail = {:?}",
            session.chat.last().map(std::mem::discriminant)
        );
        let prompt = crate::backend::shared::get_pending_user_messages(&session.chat);
        assert!(
            prompt.contains("also check the tests"),
            "queued prompt was lost; got {prompt:?}"
        );
    }

    /// A message queued mid-turn followed by a `TodoUpdate` (pushed to the end
    /// of chat in `process_events`) must likewise survive to redispatch.
    #[test]
    fn queued_message_survives_todo_update_after_queue() {
        let mut session = test_session();

        session.chat.push(Message::User("start".into()));
        let _tx = make_streaming(&mut session);
        session.append_token("working");

        // Queue a follow-up, then a todo update lands. `process_events` routes
        // TodoUpdate through `insert_turn_content` so it must not bury the queue.
        session.chat.push(Message::User("one more thing".into()));
        session.insert_turn_content(Message::TodoUpdate(serde_json::json!({"todos": []})));

        session.finalize_last_assistant();

        assert!(
            session.needs_redispatch_after_stream_end(),
            "queued message must survive a trailing TodoUpdate"
        );
    }

    /// When the backend returns immediately with no content (e.g. a
    /// skill command it can't handle), the dispatched user message is
    /// still the last in chat. Without the trailing-count guard this
    /// would trigger an infinite redispatch loop.
    #[test]
    fn empty_response_prevents_redispatch_loop() {
        let mut session = test_session();

        session
            .chat
            .push(Message::User("/refactor something".into()));
        let tx = make_streaming(&mut session);

        // Backend returns immediately — no tokens, no tools, nothing
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;

        assert!(
            !session.needs_redispatch_after_stream_end(),
            "should not redispatch already-dispatched messages with empty response"
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
                Message::User(s) => format!("U:{}", s.text),
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

        // Both queued messages, not just one: "late queue" trails the answer
        // for any insert position, so an `||` here passes even when the
        // assistant is appended at the very end.
        assert!(
            question_pos < answer_pos && answer_pos < early_pos && early_pos < late_pos,
            "expected question < answer < early queue < late queue, got {types:?}"
        );

        // Finalize and check redispatch
        session.finalize_last_assistant();
        drop(tx);
        session.incoming_tokens = None;
        assert!(session.needs_redispatch_after_stream_end());
    }

    /// The user messages the UI would tag "queued", by text.
    ///
    /// Calls the production `queued_from` and applies the same predicate
    /// `render_chat` does (`i >= queued_from`, for `Message::User` only), so
    /// these cases fail when the production path changes.
    fn queued_texts(
        session: &ChatSession,
        is_working: bool,
        dispatch_state: DispatchState,
    ) -> Vec<&str> {
        let Some(qi) = queued_from(
            &session.chat,
            is_working,
            dispatch_state,
            session.turn_has_content(),
        ) else {
            return vec![];
        };
        session.chat[qi..]
            .iter()
            .filter_map(|m| match m {
                Message::User(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
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

        // Single dispatch
        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 1 });
        assert_eq!(
            queued,
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

        // Dispatch state doesn't matter here — streaming assistant
        // branch doesn't use the dispatched count
        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 1 });
        assert_eq!(
            queued,
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

        let queued = queued_texts(&session, false, DispatchState::Idle);
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

        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 1 });
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

        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 1 });
        assert_eq!(queued, vec!["queued"]);
    }

    /// The queued indicator must still mark a message queued when the last
    /// non-user message is a tool row (not a streaming assistant). This is the
    /// same tool-burial scenario as `queued_message_survives_tool_activity_after_queue`,
    /// viewed from the UI: the badge marks exactly what will be redispatched.
    #[test]
    fn queued_indicator_after_tool_row() {
        let mut session = test_session();
        session.chat.push(Message::User("do the thing".into()));
        let _tx = make_streaming(&mut session);
        session.append_token("On it. ");
        session.push_running_tool(running_tool("t1", "Read", "hostname"));
        session.chat.push(Message::User("queued".into()));

        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 1 });
        assert_eq!(
            queued,
            vec!["queued"],
            "a message queued behind a tool row must still be marked queued"
        );
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
        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 3 });
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
        let queued = queued_texts(&session, true, DispatchState::AwaitingResponse { count: 3 });
        assert_eq!(
            queued,
            vec!["new queued"],
            "only the message after the batch should be queued"
        );
    }

    /// A working session whose dispatch state is `Idle` — nothing ever recorded
    /// a count — must still treat the last trailing user message as dispatched.
    /// That is what the `.max(1)` clamp is for; without it the message actually
    /// being worked on renders as "queued".
    #[test]
    fn queued_indicator_working_without_dispatch_count() {
        let mut session = test_session();
        session
            .chat
            .push(Message::Assistant(AssistantMessage::from_text(
                "prev reply".into(),
            )));
        session.chat.push(Message::User("dispatched".into()));

        assert!(
            queued_texts(&session, true, DispatchState::Idle).is_empty(),
            "the trailing user message is being worked on, not queued"
        );

        session.chat.push(Message::User("queued".into()));
        assert_eq!(
            queued_texts(&session, true, DispatchState::Idle),
            vec!["queued"],
            "only the message after the dispatched one should be queued"
        );
    }

    #[test]
    fn test_friendly_model_name_known_models() {
        for model in BackendType::Claude.available_models() {
            if let Some(id) = model.to_model_id() {
                let friendly = friendly_model_name(id);
                assert_ne!(
                    friendly, id,
                    "Claude model {id:?} should have a friendly display name"
                );
            }
        }
    }

    // ---- remote session tests ----

    fn test_remote_session() -> ChatSession {
        let mut session = ChatSession::new(
            99,
            PathBuf::from("/tmp"),
            AiMode::Agentic,
            BackendType::Claude,
        );
        session.source = SessionSource::Remote;
        session
    }

    #[test]
    fn remote_session_source() {
        let session = test_remote_session();
        assert!(session.is_remote());
    }

    #[test]
    fn local_session_is_not_remote() {
        let session = test_session();
        assert!(!session.is_remote());
    }

    #[test]
    fn remote_hostnames_only_returns_unique_sorted_remote_hosts() {
        let mut mgr = SessionManager::new();

        let local_id =
            create_grouped_session(&mut mgr, "local-host", "/work/a", "Local", AiMode::Agentic);
        let remote_b =
            create_grouped_session(&mut mgr, "beta-host", "/srv/b", "Remote B", AiMode::Agentic);
        let remote_a = create_grouped_session(
            &mut mgr,
            "alpha-host",
            "/srv/a",
            "Remote A",
            AiMode::Agentic,
        );
        let remote_dup = create_grouped_session(
            &mut mgr,
            "beta-host",
            "/srv/other",
            "Remote B 2",
            AiMode::Agentic,
        );

        mgr.get_mut(local_id).expect("local session").source = SessionSource::Local;
        mgr.get_mut(remote_a).expect("remote session").source = SessionSource::Remote;
        mgr.get_mut(remote_b).expect("remote session").source = SessionSource::Remote;
        mgr.get_mut(remote_dup).expect("remote session").source = SessionSource::Remote;

        assert_eq!(
            mgr.remote_hostnames(),
            vec!["alpha-host".to_string(), "beta-host".to_string()]
        );
    }

    #[test]
    fn remote_dispatch_idle_with_user_message() {
        let mut session = test_remote_session();
        session.chat.push(Message::User("hello".into()));
        assert!(session.should_dispatch_remote_message());
    }

    #[test]
    fn remote_no_dispatch_without_user_message() {
        let session = test_remote_session();
        assert!(!session.should_dispatch_remote_message());
    }

    #[test]
    fn remote_no_dispatch_while_streaming() {
        let mut session = test_remote_session();
        session.chat.push(Message::User("hello".into()));
        let _tx = make_streaming(&mut session);
        session.chat.push(Message::User("another".into()));
        assert!(!session.should_dispatch_remote_message());
    }

    // ---- subagent lifecycle tests ----

    fn make_subagent(task_id: &str, desc: &str) -> crate::messages::SubagentInfo {
        crate::messages::SubagentInfo {
            task_id: task_id.to_string(),
            description: desc.to_string(),
            subagent_type: "Explore".to_string(),
            status: crate::messages::SubagentStatus::Running,
            output: String::new(),
            max_output_size: 1000,
            tool_results: vec![],
            background: false,
        }
    }

    #[test]
    fn subagent_output_updates() {
        let mut session = test_session();
        let subagent = make_subagent("task-1", "exploring");
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        session.update_subagent_output(&task_id, "first ");
        session.update_subagent_output(&task_id, "second");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert_eq!(s.output, "first second");
            assert_eq!(s.status, crate::messages::SubagentStatus::Running);
        } else {
            panic!("expected Subagent message at index {}", idx);
        }
    }

    #[test]
    fn subagent_completion() {
        let mut session = test_session();
        let subagent = make_subagent("task-1", "exploring");
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        session.update_subagent_output(&task_id, "partial output");
        session.complete_subagent(&task_id, "final result");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert_eq!(s.status, crate::messages::SubagentStatus::Completed);
            assert_eq!(s.output, "final result");
        } else {
            panic!("expected Subagent message at index {}", idx);
        }
    }

    #[test]
    fn running_background_subagent_keeps_session_working() {
        let mut session = test_session();
        // A finished foreground turn that launched a background subagent: the
        // task handle is cleared, but the subagent is still running.
        session
            .chat
            .push(Message::User("do background work".into()));
        let mut subagent = make_subagent("toolu_root", "background task");
        subagent.background = true;
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic
                .subagent_indices
                .insert("toolu_root".to_string(), idx);
        }
        session.task_handle = None;

        session.update_status();
        assert_eq!(
            session.status(),
            AgentStatus::Working,
            "a running background subagent should keep the session Working"
        );

        // Once it completes, the session is no longer Working on its account.
        session.complete_subagent("toolu_root", "done");
        session.update_status();
        assert_ne!(session.status(), AgentStatus::Working);
    }

    #[test]
    fn foreground_subagent_does_not_keep_session_working() {
        let mut session = test_session();
        session.chat.push(Message::User("explore".into()));
        // A foreground subagent (background: false) must not by itself hold the
        // session in Working after the turn ends.
        let subagent = make_subagent("toolu_fg", "foreground task");
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert("toolu_fg".to_string(), idx);
        }
        session.task_handle = None;

        session.update_status();
        assert_ne!(session.status(), AgentStatus::Working);
    }

    #[test]
    fn subagent_failure() {
        let mut session = test_session();
        let subagent = make_subagent("task-1", "exploring");
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        session.fail_subagent(&task_id, "it crashed");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert_eq!(s.status, crate::messages::SubagentStatus::Failed);
            assert_eq!(s.output, "it crashed");
        } else {
            panic!("expected Subagent message at index {}", idx);
        }
    }

    #[test]
    fn subagent_output_truncation() {
        let mut session = test_session();
        let mut subagent = make_subagent("task-1", "exploring");
        subagent.max_output_size = 20;
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        // Push output that exceeds max_output_size
        session
            .update_subagent_output(&task_id, "a long output that is way too big for the buffer");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert!(
                s.output.len() <= 20,
                "output should be truncated to max_output_size, got len {}",
                s.output.len()
            );
        } else {
            panic!("expected Subagent message");
        }
    }

    /// Truncation must not panic on multi-byte UTF-8 characters.
    /// Before the fix, slicing at arbitrary byte offsets would panic
    /// with "byte index X is not a char boundary".
    #[test]
    fn subagent_output_truncation_utf8_emoji() {
        let mut session = test_session();
        let mut subagent = make_subagent("task-emoji", "exploring");
        subagent.max_output_size = 7;
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        // "OK🌍" = 6 bytes (O=1, K=1, 🌍=4)
        session.update_subagent_output(&task_id, "OK🌍");
        // "More🎉test" = 11 bytes
        // Total: 17 bytes, max: 7, keep_from: 10
        // Byte 10 is mid-emoji — must not panic
        session.update_subagent_output(&task_id, "More🎉test");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            // 18 bytes total, max 7, so the cut is at byte 11 — inside the
            // 🎉 (bytes 10..13). Walking forward to the next boundary drops the
            // partial char and keeps the tail from byte 14.
            assert_eq!(s.output, "test");
        } else {
            panic!("expected Subagent message");
        }
    }

    #[test]
    fn test_friendly_model_name_unknown() {
        for model in BackendType::OpenAI.available_models() {
            if let Some(id) = model.to_model_id() {
                assert_eq!(
                    friendly_model_name(id),
                    id,
                    "OpenAI model should pass through as-is"
                );
            }
        }
        assert_eq!(
            friendly_model_name("some-unknown-model"),
            "some-unknown-model"
        );
    }

    #[test]
    fn subagent_output_truncation_utf8_cjk() {
        let mut session = test_session();
        let mut subagent = make_subagent("task-cjk", "exploring");
        subagent.max_output_size = 8;
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        // "你好" = 6 bytes (3 per CJK char)
        session.update_subagent_output(&task_id, "你好");
        // "世界" = 6 bytes
        // Total: 12 bytes, max: 8, keep_from: 4 — mid-char boundary
        session.update_subagent_output(&task_id, "世界");

        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert!(s.output.len() <= 8, "got len {}", s.output.len());
        } else {
            panic!("expected Subagent message");
        }
    }

    #[test]
    fn fold_tool_result_into_subagent() {
        let mut session = test_session();
        let subagent = make_subagent("task-1", "exploring");
        let task_id = subagent.task_id.clone();
        let idx = session.chat.len();
        session.chat.push(Message::Subagent(subagent));
        if let Some(ref mut agentic) = session.agentic {
            agentic.subagent_indices.insert(task_id.clone(), idx);
        }

        let result = crate::messages::ExecutedTool {
            tool_name: "Read".to_string(),
            summary: "42 lines".to_string(),
            output: None,
            parent_task_id: Some("task-1".to_string()),
            file_update: None,
            tool_use_id: None,
        };

        // Should be folded (returns None)
        let folded = session.fold_tool_result(result);
        assert!(
            folded.is_none(),
            "result with matching parent should be folded"
        );

        // Verify it was added to the subagent's tool_results
        if let Some(Message::Subagent(s)) = session.chat.get(idx) {
            assert_eq!(s.tool_results.len(), 1);
            assert_eq!(s.tool_results[0].tool_name, "Read");
        } else {
            panic!("expected Subagent message");
        }
    }

    #[test]
    fn fold_tool_result_no_parent() {
        let mut session = test_session();
        let result = crate::messages::ExecutedTool {
            tool_name: "Bash".to_string(),
            summary: "exit 0".to_string(),
            output: None,
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        };

        // Should NOT be folded (returns Some)
        let not_folded = session.fold_tool_result(result);
        assert!(
            not_folded.is_some(),
            "result without parent should not be folded"
        );
    }

    // ---- in-flight running tool rows ----

    fn running_tool(id: &str, name: &str, summary: &str) -> crate::messages::RunningTool {
        crate::messages::RunningTool {
            tool_use_id: id.to_string(),
            tool_name: name.to_string(),
            summary: summary.to_string(),
        }
    }

    fn executed_tool(id: &str, name: &str) -> crate::messages::ExecutedTool {
        crate::messages::ExecutedTool {
            tool_name: name.to_string(),
            summary: format!("{name} done"),
            output: None,
            parent_task_id: None,
            file_update: None,
            tool_use_id: Some(id.to_string()),
        }
    }

    #[test]
    fn running_tool_is_resolved_in_place_by_its_result() {
        let mut session = test_session();
        session.push_running_tool(running_tool("t1", "Read", "hostname"));

        let idx = 0;
        assert!(matches!(
            session.chat.get(idx),
            Some(Message::ToolRunning(_))
        ));
        assert_eq!(
            session
                .agentic
                .as_ref()
                .unwrap()
                .running_tool_indices
                .get("t1"),
            Some(&idx)
        );

        session.place_tool_result(executed_tool("t1", "Read"));

        // Upgraded in place: same length, same slot, now a completed response,
        // and the index map is cleared.
        assert_eq!(session.chat.len(), 1, "no new row is appended");
        assert!(matches!(
            session.chat.get(idx),
            Some(Message::ToolResponse(_))
        ));
        assert!(session
            .agentic
            .as_ref()
            .unwrap()
            .running_tool_indices
            .is_empty());
    }

    #[test]
    fn tool_result_without_a_running_row_is_appended() {
        let mut session = test_session();
        session.place_tool_result(executed_tool("x", "Bash"));
        assert_eq!(session.chat.len(), 1);
        assert!(matches!(
            session.chat.get(0),
            Some(Message::ToolResponse(_))
        ));
    }

    #[test]
    fn finalize_running_tools_stops_a_dangling_spinner() {
        let mut session = test_session();
        session.push_running_tool(running_tool("t1", "Bash", "sleep 5"));
        assert!(matches!(session.chat.get(0), Some(Message::ToolRunning(_))));

        // An interrupted turn ends with no result for the tool; finalize turns
        // the spinner into a static completed row.
        session.finalize_running_tools();
        assert!(matches!(
            session.chat.get(0),
            Some(Message::ToolResponse(_))
        ));
        assert!(session
            .agentic
            .as_ref()
            .unwrap()
            .running_tool_indices
            .is_empty());
    }

    // ---- edge case: silent failures ----

    #[test]
    fn subagent_output_nonexistent_task_no_panic() {
        let mut session = test_session();
        // Should silently do nothing — no matching task_id in indices
        session.update_subagent_output("nonexistent-id", "output");
        assert!(session.chat.is_empty());
    }

    #[test]
    fn complete_subagent_nonexistent_task_no_panic() {
        let mut session = test_session();
        session.complete_subagent("nonexistent-id", "result");
        assert!(session.chat.is_empty());
    }

    #[test]
    fn fold_tool_result_nonexistent_parent_returns_result() {
        let mut session = test_session();
        let result = crate::messages::ExecutedTool {
            tool_name: "Read".to_string(),
            summary: "42 lines".to_string(),
            output: None,
            parent_task_id: Some("nonexistent-task".to_string()),
            file_update: None,
            tool_use_id: None,
        };
        // Parent doesn't exist — should return the result unfolded
        let not_folded = session.fold_tool_result(result);
        assert!(
            not_folded.is_some(),
            "result with nonexistent parent should not be folded"
        );
    }

    #[test]
    fn session_manager_touch_nonexistent_no_panic() {
        let mut mgr = SessionManager::new();
        let id = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        // Touch a non-existent ID — should be a silent no-op
        mgr.touch(999);
        // Assert on the raw order, not on `sessions_ordered()`: that projection
        // is `order.iter().filter_map(|id| sessions.get(id))`, so a bogus id
        // pushed into `order` is filtered straight back out and the corruption
        // this guards against is invisible through it.
        assert_eq!(mgr.session_ids(), vec![id]);
    }

    #[test]
    fn session_manager_delete_active_clears_active() {
        let mut mgr = SessionManager::new();
        let id = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        // id should be active after creation
        assert_eq!(mgr.active_id(), Some(id));
        mgr.delete_session(id);
        // After deleting the only (active) session, both active and get should be gone
        assert!(mgr.active_id().is_none());
        assert!(mgr.get(id).is_none());
        assert!(mgr.is_empty());
    }

    // ---- session manager tests ----

    #[test]
    fn session_manager_create_and_get() {
        let mut mgr = SessionManager::new();
        let id = mgr.new_session(PathBuf::from("/tmp"), AiMode::Agentic, BackendType::Claude);
        let session = mgr.get(id).expect("session should exist after creation");
        assert_eq!(session.id, id);
        assert_eq!(session.ai_mode, AiMode::Agentic);
        assert!(mgr.get_mut(id).is_some());
        assert_eq!(mgr.active_id(), Some(id));
    }

    #[test]
    fn session_manager_delete() {
        let mut mgr = SessionManager::new();
        let id1 = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        let id2 = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        assert_eq!(mgr.len(), 2);
        mgr.delete_session(id1);
        assert!(mgr.get(id1).is_none());
        assert!(mgr.get(id2).is_some());
        assert_eq!(mgr.len(), 1);
    }

    #[test]
    fn session_manager_ordering() {
        let mut mgr = SessionManager::new();
        let id1 = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        let _id2 = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);
        let id3 = mgr.new_session(PathBuf::from("/tmp"), AiMode::Chat, BackendType::OpenAI);

        // Most recent (first in order) should be last created
        let ordered = mgr.sessions_ordered();
        assert_eq!(ordered[0].id, id3);

        // Touch id1 to make it most recent
        mgr.touch(id1);
        let ordered = mgr.sessions_ordered();
        assert_eq!(ordered[0].id, id1);

        // Verify all three are still present
        assert_eq!(ordered.len(), 3);
    }

    #[test]
    fn chat_id_cache_refreshes_after_touch_without_manual_rebuild() {
        let mut mgr = SessionManager::new();
        let id1 = mgr.new_session(PathBuf::from("/tmp/one"), AiMode::Chat, BackendType::OpenAI);
        let id2 = mgr.new_session(PathBuf::from("/tmp/two"), AiMode::Chat, BackendType::OpenAI);

        assert_eq!(mgr.chat_ids(), &[id2, id1]);

        mgr.touch(id1);
        assert_eq!(mgr.chat_ids(), &[id1, id2]);
    }

    #[test]
    fn host_group_cache_refreshes_after_get_mut_hostname_change() {
        let mut mgr = SessionManager::new();
        let id = mgr.new_session(
            PathBuf::from("/tmp/work"),
            AiMode::Agentic,
            BackendType::Claude,
        );

        {
            let session = mgr.get_mut(id).expect("session should exist");
            session.details.hostname = "remote-a".to_string();
        }

        let groups = mgr.host_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].hostname, "remote-a");
    }

    #[test]
    fn pending_placeholder_group_uses_requested_cwd() {
        let mut mgr = SessionManager::new();
        let requested_cwd = PathBuf::from("/srv/project");
        mgr.new_pending_placeholder(
            requested_cwd.clone(),
            "remote-a".to_string(),
            BackendType::Claude,
            "spawn-1".to_string(),
            None,
        );

        let groups = mgr.host_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].hostname, "remote-a");
        // A placeholder has no persisted project, so its cwd forms its own
        // single-workspace project keyed by the cwd itself.
        assert_eq!(groups[0].project_groups.len(), 1);
        let cwd_groups = &groups[0].project_groups[0].cwd_groups;
        assert_eq!(cwd_groups.len(), 1);
        assert_eq!(cwd_groups[0].cwd, requested_cwd);
        assert_eq!(cwd_groups[0].display_cwd, "/srv/project");
    }

    #[test]
    fn rebuild_groups_groups_hosts_cwds_and_sessions_deterministically() {
        let mut mgr = SessionManager::new();

        let local_zulu =
            create_grouped_session(&mut mgr, "", "/work/alpha", "Zulu task", AiMode::Agentic);
        let local_alpha =
            create_grouped_session(&mut mgr, "", "/work/alpha", "Alpha task", AiMode::Agentic);
        let remote_beta = create_grouped_session(
            &mut mgr,
            "beta-host",
            "/srv/backend",
            "Beta host task",
            AiMode::Agentic,
        );
        let remote_zulu = create_grouped_session(
            &mut mgr,
            "zulu-host",
            "/srv/api",
            "Zulu host task",
            AiMode::Agentic,
        );
        let chat_id = create_grouped_session(&mut mgr, "", "/chat/ignored", "Chat", AiMode::Chat);

        mgr.rebuild_groups();

        let groups = mgr.host_groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].hostname, "");
        assert_eq!(groups[1].hostname, "beta-host");
        assert_eq!(groups[2].hostname, "zulu-host");

        // Non-git cwds each become their own single-workspace project.
        let local_group = &groups[0];
        assert_eq!(local_group.project_groups.len(), 1);
        let local_cwds = &local_group.project_groups[0].cwd_groups;
        assert_eq!(local_cwds.len(), 1);
        assert_eq!(local_cwds[0].display_cwd, "/work/alpha");
        assert_eq!(local_cwds[0].session_ids, vec![local_alpha, local_zulu]);

        assert_eq!(
            groups[1].project_groups[0].cwd_groups[0].session_ids,
            vec![remote_beta]
        );
        assert_eq!(
            groups[2].project_groups[0].cwd_groups[0].session_ids,
            vec![remote_zulu]
        );

        assert_eq!(
            mgr.visual_order(&CollapseState::new()),
            vec![local_alpha, local_zulu, remote_beta, remote_zulu, chat_id]
        );
    }

    #[test]
    fn rebuild_groups_sorts_multiple_projects_within_a_host() {
        let mut mgr = SessionManager::new();

        let alpha_first = create_grouped_session(
            &mut mgr,
            "remote-a",
            "/srv/alpha",
            "Alpha first",
            AiMode::Agentic,
        );
        let zeta_only = create_grouped_session(
            &mut mgr,
            "remote-a",
            "/srv/zeta",
            "Zeta only",
            AiMode::Agentic,
        );
        let alpha_second = create_grouped_session(
            &mut mgr,
            "remote-a",
            "/srv/alpha",
            "Alpha second",
            AiMode::Agentic,
        );

        mgr.rebuild_groups();

        // Two distinct non-git cwds → two single-workspace projects, sorted by
        // slug (basename): "alpha" before "zeta".
        let groups = mgr.host_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].hostname, "remote-a");
        let projects = &groups[0].project_groups;
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].slug, "alpha");
        assert_eq!(projects[0].cwd_groups[0].display_cwd, "/srv/alpha");
        assert_eq!(projects[1].slug, "zeta");
        assert_eq!(projects[1].cwd_groups[0].display_cwd, "/srv/zeta");
        assert_eq!(
            projects[0].cwd_groups[0].session_ids,
            vec![alpha_first, alpha_second]
        );
        assert_eq!(projects[1].cwd_groups[0].session_ids, vec![zeta_only]);
        assert_eq!(
            mgr.visual_order(&CollapseState::new()),
            vec![alpha_first, alpha_second, zeta_only]
        );
    }

    #[test]
    fn visual_order_skips_collapsed_hosts_and_cwds_but_keeps_chats() {
        let mut mgr = SessionManager::new();

        // Two sessions in /work/a so the cwd is collapsible (single-session
        // cwds always stay visible regardless of collapse state).
        let _local_a1 =
            create_grouped_session(&mut mgr, "", "/work/a", "Local A1", AiMode::Agentic);
        let _local_a2 =
            create_grouped_session(&mut mgr, "", "/work/a", "Local A2", AiMode::Agentic);
        let local_b = create_grouped_session(&mut mgr, "", "/work/b", "Local B", AiMode::Agentic);
        let remote_a = create_grouped_session(
            &mut mgr,
            "remote-a",
            "/srv/keep",
            "Remote A",
            AiMode::Agentic,
        );
        let _remote_b = create_grouped_session(
            &mut mgr,
            "remote-b",
            "/srv/hide",
            "Remote B",
            AiMode::Agentic,
        );
        let chat_id = create_grouped_session(&mut mgr, "", "/chat/ignored", "Chat", AiMode::Chat);

        mgr.rebuild_groups();

        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("", std::path::Path::new("/work/a"));
        collapse.toggle_host("remote-b");

        assert_eq!(
            mgr.visual_order(&collapse),
            vec![local_b, remote_a, chat_id]
        );
    }

    #[test]
    fn visual_order_keeps_single_session_cwd_visible_even_if_marked_collapsed() {
        let mut mgr = SessionManager::new();

        // Only one session in /work/solo — its cwd cannot be collapsed in the
        // UI (no folder header is rendered), so a stale `is_cwd_collapsed`
        // entry must not hide it from keyboard navigation.
        let solo = create_grouped_session(&mut mgr, "", "/work/solo", "Solo", AiMode::Agentic);

        mgr.rebuild_groups();

        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("", std::path::Path::new("/work/solo"));

        assert_eq!(mgr.visual_order(&collapse), vec![solo]);
    }

    /// Set an explicit shared project on a session, simulating two worktrees of
    /// one repo (which resolve to the same `project_root` but different cwds).
    fn set_project(mgr: &mut SessionManager, id: SessionId, root: &str, slug: &str) {
        let session = mgr.get_mut(id).expect("session should exist");
        session.details.project_root = Some(PathBuf::from(root));
        session.details.project_slug = Some(slug.to_string());
    }

    #[test]
    fn worktrees_of_one_repo_group_under_a_single_project() {
        let mut mgr = SessionManager::new();
        // Main checkout and a linked worktree: different cwds, same repo root.
        let main = create_grouped_session(&mut mgr, "", "/dev/repo", "Main", AiMode::Agentic);
        let wt = create_grouped_session(
            &mut mgr,
            "",
            "/dev/repo-feature",
            "Feature",
            AiMode::Agentic,
        );
        set_project(&mut mgr, main, "/dev/repo", "repo");
        set_project(&mut mgr, wt, "/dev/repo", "repo");
        mgr.rebuild_groups();

        let groups = mgr.host_groups();
        assert_eq!(groups.len(), 1);
        // Both sessions land under ONE project with two workspaces (not two
        // scattered top-level cwd groups) — the whole point of the redesign.
        let projects = &groups[0].project_groups;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].slug, "repo");
        assert_eq!(projects[0].cwd_groups.len(), 2);
        assert!(
            !projects[0].is_flat(),
            "a two-workspace project is not flat"
        );

        // Collapsing the project hides both workspaces from keyboard nav.
        let mut collapse = CollapseState::new();
        collapse.toggle_project("", std::path::Path::new("/dev/repo"));
        assert!(mgr.visual_order(&collapse).is_empty());
    }

    #[test]
    fn single_session_workspace_in_project_stays_visible_when_marked_collapsed() {
        let mut mgr = SessionManager::new();
        // A multi-workspace (non-flat) project: one workspace has a single
        // session (renders inline, no folder), the other has two (a folder).
        let main = create_grouped_session(&mut mgr, "", "/dev/repo", "Main", AiMode::Agentic);
        let feat_a = create_grouped_session(
            &mut mgr,
            "",
            "/dev/repo-feature",
            "Feature A",
            AiMode::Agentic,
        );
        let feat_b = create_grouped_session(
            &mut mgr,
            "",
            "/dev/repo-feature",
            "Feature B",
            AiMode::Agentic,
        );
        for id in [main, feat_a, feat_b] {
            set_project(&mut mgr, id, "/dev/repo", "repo");
        }
        mgr.rebuild_groups();
        assert!(!mgr.host_groups()[0].project_groups[0].is_flat());

        // Marking the single-session workspace collapsed must NOT hide it — it
        // has no folder UI to collapse (mirrors the flat single-session rule).
        let mut collapse = CollapseState::new();
        collapse.toggle_cwd("", std::path::Path::new("/dev/repo"));
        assert_eq!(
            mgr.visual_order(&collapse),
            vec![main, feat_a, feat_b],
            "single-session workspace stays visible; folder still expanded"
        );

        // Collapsing the multi-session workspace hides just its two sessions.
        collapse.toggle_cwd("", std::path::Path::new("/dev/repo-feature"));
        assert_eq!(mgr.visual_order(&collapse), vec![main]);
    }

    #[test]
    fn single_workspace_at_root_is_flat() {
        let mut mgr = SessionManager::new();
        let id = create_grouped_session(&mut mgr, "", "/dev/repo", "Only", AiMode::Agentic);
        set_project(&mut mgr, id, "/dev/repo", "repo");
        mgr.rebuild_groups();

        let projects = &mgr.host_groups()[0].project_groups;
        assert_eq!(projects.len(), 1);
        assert!(
            projects[0].is_flat(),
            "one workspace at the repo root renders flush"
        );
    }

    // ---- compact_intent / take_compact_and_proceed tests ----

    #[test]
    fn take_compact_and_proceed_none_returns_false() {
        let mut session = test_session();
        // Agentic session starts with compact_intent = None
        assert!(!session.take_compact_and_proceed());
        assert!(session.chat.is_empty());
    }

    #[test]
    fn take_compact_and_proceed_waiting_for_stream_end_returns_false() {
        let mut session = test_session();
        session.agentic.as_mut().unwrap().compact_intent =
            Some(CompactIntent::ProceedAfterStreamEnd);
        assert!(!session.take_compact_and_proceed());
        assert!(session.chat.is_empty());
    }

    #[test]
    fn take_compact_and_proceed_waiting_for_compaction_returns_false() {
        let mut session = test_session();
        session.agentic.as_mut().unwrap().compact_intent =
            Some(CompactIntent::ProceedAfterCompaction);
        assert!(!session.take_compact_and_proceed());
        assert!(session.chat.is_empty());
    }

    #[test]
    fn take_compact_and_proceed_ready_returns_true_and_pushes_message() {
        let mut session = test_session();
        session.agentic.as_mut().unwrap().compact_intent = Some(CompactIntent::ReadyToProceed);

        assert!(session.take_compact_and_proceed());

        // Should have pushed a user "Proceed" message
        assert_eq!(session.chat.len(), 1);
        assert!(matches!(session.chat[0], Message::User(ref s) if s.text.contains("Proceed")));

        // State should be reset to None
        assert_eq!(session.agentic.as_ref().unwrap().compact_intent, None);
    }

    #[test]
    fn take_compact_and_proceed_only_fires_once() {
        let mut session = test_session();
        session.agentic.as_mut().unwrap().compact_intent = Some(CompactIntent::ReadyToProceed);

        assert!(session.take_compact_and_proceed());
        // Second call should return false — state was consumed
        assert!(!session.take_compact_and_proceed());
        // Only one "Proceed" message
        assert_eq!(session.chat.len(), 1);
    }

    #[test]
    fn compact_and_proceed_full_lifecycle() {
        let mut session = test_session();
        let agentic = session.agentic.as_mut().unwrap();

        // 1. User clicks "Compact & Approve" → ProceedAfterStreamEnd
        agentic.compact_intent = Some(CompactIntent::ProceedAfterStreamEnd);
        assert!(!session.take_compact_and_proceed());

        // 2. Stream ends → caller dispatches compact, sets ProceedAfterCompaction
        let agentic = session.agentic.as_mut().unwrap();
        assert_eq!(
            agentic.compact_intent,
            Some(CompactIntent::ProceedAfterStreamEnd)
        );
        agentic.compact_intent = Some(CompactIntent::ProceedAfterCompaction);
        assert!(!session.take_compact_and_proceed());

        // 3. Compaction completes → advance to ReadyToProceed
        let agentic = session.agentic.as_mut().unwrap();
        agentic.compact_intent = Some(CompactIntent::ReadyToProceed);

        // 4. Compact stream ends → take_compact_and_proceed fires → sends "Proceed"
        assert!(session.take_compact_and_proceed());
        assert!(
            matches!(session.chat.last(), Some(Message::User(ref s)) if s.text.contains("Proceed"))
        );

        // 5. State is back to None
        assert_eq!(session.agentic.as_ref().unwrap().compact_intent, None);
    }

    #[test]
    fn is_compacting_derived_from_compact_intent() {
        let mut session = test_session();
        let agentic = session.agentic.as_mut().unwrap();

        // None → not compacting
        agentic.compact_intent = None;
        assert!(!agentic.is_compacting());

        // Manual → compacting
        agentic.compact_intent = Some(CompactIntent::Manual);
        assert!(agentic.is_compacting());

        // ProceedAfterStreamEnd → not yet compacting (waiting for stream end)
        agentic.compact_intent = Some(CompactIntent::ProceedAfterStreamEnd);
        assert!(!agentic.is_compacting());

        // ProceedAfterCompaction → compacting
        agentic.compact_intent = Some(CompactIntent::ProceedAfterCompaction);
        assert!(agentic.is_compacting());

        // ReadyToProceed → compaction finished, not compacting
        agentic.compact_intent = Some(CompactIntent::ReadyToProceed);
        assert!(!agentic.is_compacting());
    }
}
