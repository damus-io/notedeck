//! Convert parsed JSONL lines into kind-1988 nostr events.
//!
//! Each JSONL line becomes one or more nostr events. Assistant messages with
//! mixed content (text + tool_use blocks) are split into separate events.
//! Events are threaded using NIP-10 `e` tags with root/reply markers.

use crate::config::{RunConfig, AI_RUN_CONFIG_KIND};
use crate::session_jsonl::{self, ContentBlock, JsonlLine};
use nostrdb::{NoteBuildOptions, NoteBuilder};
use std::collections::HashMap;
use std::path::PathBuf;

/// Nostr event kind for AI conversation notes.
pub const AI_CONVERSATION_KIND: u32 = 1988;

/// Nostr event kind for source-data companion events (archive).
/// Each 1989 event carries the raw JSONL for one line, linked to the
/// corresponding 1988 event via an `e` tag.
pub const AI_SOURCE_DATA_KIND: u32 = 1989;

/// Nostr event kind for AI session state (parameterized replaceable, NIP-33).
/// One event per session, auto-replaced by nostrdb on update.
/// `d` tag = claude_session_id.
pub const AI_SESSION_STATE_KIND: u32 = 31988;

/// Nostr event kind for AI session commands (parameterized replaceable, NIP-33).
/// Fire-and-forget commands to create sessions on remote hosts.
/// `d` tag = command UUID.
pub const AI_SESSION_COMMAND_KIND: u32 = 31989;

/// Extract the value of a named tag from a note.
pub fn get_tag_value<'a>(note: &'a nostrdb::Note<'a>, tag_name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }
        let Some(name) = tag.get_str(0) else {
            continue;
        };
        if name != tag_name {
            continue;
        }
        return tag.get_str(1);
    }
    None
}

/// A built nostr event ready for ingestion and relay publishing.
#[derive(Debug)]
pub struct BuiltEvent {
    /// The bare event JSON `{…}` — for relay publishing and ndb ingestion.
    pub note_json: String,
    /// The 32-byte note ID (from the signed event).
    pub note_id: [u8; 32],
    /// The nostr event kind (1988 or 1989).
    pub kind: u32,
}

impl BuiltEvent {
    /// Format as `["EVENT", {…}]` for ndb ingestion via `process_event_with`.
    pub fn to_event_json(&self) -> String {
        format!("[\"EVENT\", {}]", self.note_json)
    }
}

/// Wrap an inner event in a kind-1080 PNS envelope for relay publishing.
///
/// The encrypt + sign-1080 construction lives in [`enostr::pns::wrap`]; this
/// adapts it to agentium's `Result<String, EventBuildError>` by returning the
/// wrapper's event JSON.
pub fn wrap_pns(
    inner_json: &str,
    pns_keys: &enostr::pns::PnsKeys,
) -> Result<String, EventBuildError> {
    enostr::pns::wrap(pns_keys, inner_json, now_secs())
        .ok_or_else(|| EventBuildError::Serialize("PNS wrap failed".to_string()))?
        .json()
        .map_err(|e| EventBuildError::Serialize(format!("PNS wrap json: {e}")))
}

/// Maintains threading state across a session's events.
pub struct ThreadingState {
    /// Maps JSONL uuid → nostr note ID (32 bytes).
    uuid_to_note_id: HashMap<String, [u8; 32]>,
    /// The note ID of the first event in the session (root).
    root_note_id: Option<[u8; 32]>,
    /// The note ID of the most recently built event.
    last_note_id: Option<[u8; 32]>,
    /// Monotonic sequence counter for unambiguous ordering.
    seq: u32,
    /// Last seen session ID (carried forward for lines that lack it).
    session_id: Option<String>,
    /// Last seen timestamp in seconds (carried forward for lines that lack it).
    last_timestamp: Option<u64>,
    /// Last seen timestamp in milliseconds (carried forward for lines that lack
    /// it). Kept alongside [`last_timestamp`](Self::last_timestamp) so the
    /// sub-second ordering tag stays consistent with the second-resolution
    /// `created_at`.
    last_timestamp_millis: Option<u64>,
}

impl Default for ThreadingState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadingState {
    pub fn new() -> Self {
        Self {
            uuid_to_note_id: HashMap::new(),
            root_note_id: None,
            last_note_id: None,
            seq: 0,
            session_id: None,
            last_timestamp: None,
            last_timestamp_millis: None,
        }
    }

    /// The current sequence number.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Update session context from a JSONL line (session_id, timestamp).
    fn update_context(&mut self, line: &JsonlLine) {
        if let Some(sid) = line.session_id() {
            self.session_id = Some(sid.to_string());
        }
        if let Some(ts) = line.timestamp_secs() {
            self.last_timestamp = Some(ts);
        }
        if let Some(ms) = line.timestamp_millis() {
            self.last_timestamp_millis = Some(ms);
        }
    }

    /// Get the session ID for the current line, falling back to the last seen.
    fn session_id_for(&self, line: &JsonlLine) -> Option<String> {
        line.session_id()
            .map(|s| s.to_string())
            .or_else(|| self.session_id.clone())
    }

    /// Get the timestamp for the current line, falling back to the last seen.
    fn timestamp_for(&self, line: &JsonlLine) -> Option<u64> {
        line.timestamp_secs().or(self.last_timestamp)
    }

    /// Get the millisecond timestamp for the current line, falling back to the
    /// last seen. Used to stamp the sub-second ordering tag.
    fn timestamp_millis_for(&self, line: &JsonlLine) -> Option<u64> {
        line.timestamp_millis().or(self.last_timestamp_millis)
    }

    /// Seed threading state from existing events (e.g. loaded from ndb).
    ///
    /// Sets root and last note IDs so that subsequent live events thread
    /// correctly (NIP-10) as replies to the existing conversation. This is now
    /// purely about the reply chain — display order is keyed on wall-clock time
    /// (see [`crate::session_loader::EventOrder`]), so the seeded `seq` no
    /// longer needs to be globally coherent and the counter simply increments
    /// from wherever it is via [`record`](Self::record).
    ///
    /// Safe to call repeatedly, including against a *partial* ndb snapshot
    /// during a fresh resync: `last_note_id` is the loader's most-recent event
    /// by time, and any event this host has already emitted carries a `now`
    /// timestamp that dominates the backfilled history — so a re-seed can never
    /// regress the reply target below a live emit. The conversation root is the
    /// thread's first event and never changes once known, so it is set only
    /// while still unset.
    pub fn seed(&mut self, root_note_id: [u8; 32], last_note_id: [u8; 32]) {
        if self.root_note_id.is_none() {
            self.root_note_id = Some(root_note_id);
        }
        self.last_note_id = Some(last_note_id);
    }

    /// Record a built event's note ID, associated with a JSONL uuid.
    ///
    /// `can_be_root`: if true, this event may become the conversation root.
    /// Metadata events (queue-operation, progress, etc.) should pass false
    /// so they don't become the root of the threading chain.
    pub fn record(&mut self, uuid: Option<&str>, note_id: [u8; 32], can_be_root: bool) {
        if can_be_root && self.root_note_id.is_none() {
            self.root_note_id = Some(note_id);
        }
        if let Some(uuid) = uuid {
            self.uuid_to_note_id.insert(uuid.to_string(), note_id);
        }
        self.last_note_id = Some(note_id);
        self.seq += 1;
    }
}

/// Get the current Unix timestamp in seconds.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The `created_at` for the next kind-31988 state event of a session, kept
/// strictly greater than the last one published for it.
///
/// kind-31988 is a parameterized-replaceable event ordered only by
/// second-resolution `created_at`, so two status transitions within one second
/// would tie and resolve by event id (~random) — letting a stale status win
/// (the "NeedsInput floats to top" regression). Advancing to `last + 1` when
/// wall-clock hasn't ticked guarantees the newest revision always compares
/// newest, in both nostrdb's replaceable resolution and the receive-side
/// `remote_status_ts` guard. Drift above real time is bounded and self-heals
/// once wall-clock passes it.
pub fn next_state_created_at(now: u64, last_published: u64) -> u64 {
    now.max(last_published + 1)
}

/// Get the current Unix timestamp in milliseconds.
///
/// Live events stamp this into the `ms` ordering tag so a turn's burst of
/// events — all sharing one `created_at` second — orders sub-second without
/// leaning on the `seq` counter (see [`crate::session_loader`]).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Stamp the `ms` sub-second ordering tag (unix milliseconds) when the event's
/// millisecond timestamp is known.
///
/// `created_at` is only second-resolution, so it cannot order the several
/// events a single turn emits within one second. `ms` carries the full
/// millisecond timestamp and is the loader's primary same-second tiebreak,
/// consistent across the archive (`convert`) and live paths because both derive
/// it from the same instant as `created_at`.
fn stamp_ms_tag(builder: NoteBuilder<'_>, timestamp_ms: Option<u64>) -> NoteBuilder<'_> {
    match timestamp_ms {
        Some(ms) => builder.start_tag().tag_str("ms").tag_str(&ms.to_string()),
        None => builder,
    }
}

/// Initialize a NoteBuilder with kind, content, and optional timestamp.
fn init_note_builder(kind: u32, content: &str, timestamp: Option<u64>) -> NoteBuilder<'_> {
    let mut builder = NoteBuilder::new()
        .kind(kind)
        .content(content)
        .options(NoteBuildOptions::default());

    if let Some(ts) = timestamp {
        builder = builder.created_at(ts);
    }

    builder
}

/// Sign, build, and serialize a NoteBuilder into a BuiltEvent.
fn finalize_built_event(
    builder: NoteBuilder,
    secret_key: &[u8; 32],
    kind: u32,
) -> Result<BuiltEvent, EventBuildError> {
    let note = builder
        .sign(secret_key)
        .build()
        .ok_or_else(|| EventBuildError::Build("NoteBuilder::build returned None".to_string()))?;

    let note_id: [u8; 32] = *note.id();

    let note_json = note
        .json()
        .map_err(|e| EventBuildError::Serialize(format!("{:?}", e)))?;

    Ok(BuiltEvent {
        note_json,
        note_id,
        kind,
    })
}

/// Whether a role represents a conversation message (not metadata).
pub fn is_conversation_role(role: &str) -> bool {
    matches!(role, "user" | "assistant" | "tool_call" | "tool_result")
}

/// Build nostr events from a single JSONL line.
///
/// Returns one or more events. Assistant messages with mixed content blocks
/// (text + tool_use) are split into multiple events, one per block.
///
/// `secret_key` is the 32-byte secret key for signing events.
pub fn build_events(
    line: &JsonlLine,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<Vec<BuiltEvent>, EventBuildError> {
    // Resolve session_id and timestamp with fallback to last seen values,
    // then update context for subsequent lines.
    let session_id = threading.session_id_for(line);
    let timestamp = threading.timestamp_for(line);
    let timestamp_ms = threading.timestamp_millis_for(line);
    threading.update_context(line);

    let msg = line.message();
    let is_assistant = line.line_type() == Some("assistant");

    // Check if this is an assistant message with multiple content blocks
    // that should be split into separate events
    let blocks: Vec<ContentBlock<'_>> = if is_assistant {
        msg.as_ref().map(|m| m.content_blocks()).unwrap_or_default()
    } else {
        vec![]
    };

    let should_split = is_assistant && blocks.len() > 1;

    let mut events = if should_split {
        // Build one event per content block
        let total = blocks.len();
        let mut events = Vec::with_capacity(total);
        for (i, block) in blocks.iter().enumerate() {
            let content = session_jsonl::display_content_for_block(block);
            let role = match block {
                ContentBlock::Text(_) => "assistant",
                ContentBlock::ToolUse { .. } => "tool_call",
                ContentBlock::ToolResult { .. } => "tool_result",
            };
            let tool_id = match block {
                ContentBlock::ToolUse { id, .. } => Some(*id),
                ContentBlock::ToolResult { tool_use_id, .. } => Some(*tool_use_id),
                _ => None,
            };
            let tool_name = match block {
                ContentBlock::ToolUse { name, .. } => Some(*name),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    // Look up tool name from a prior ToolUse block with matching id
                    blocks.iter().find_map(|b| match b {
                        ContentBlock::ToolUse { id, name, .. } if *id == *tool_use_id => {
                            Some(*name)
                        }
                        _ => None,
                    })
                }
                _ => None,
            };

            let event = build_single_event(
                Some(line),
                &content,
                role,
                "claude-code",
                Some((i, total)),
                tool_id,
                tool_name,
                session_id.as_deref(),
                None,
                timestamp,
                timestamp_ms,
                threading,
                secret_key,
            )?;
            threading.record(line.uuid(), event.note_id, is_conversation_role(role));
            events.push(event);
        }
        events
    } else {
        // Single event for the line
        let content = session_jsonl::extract_display_content(line);
        let role = line.role().unwrap_or("unknown");

        // Extract tool_id and tool_name from single-block messages
        let (tool_id, tool_name) = msg
            .as_ref()
            .and_then(|m| {
                let blocks = m.content_blocks();
                if blocks.len() == 1 {
                    match &blocks[0] {
                        ContentBlock::ToolUse { id, name, .. } => {
                            Some((id.to_string(), Some(name.to_string())))
                        }
                        ContentBlock::ToolResult { tool_use_id, .. } => {
                            Some((tool_use_id.to_string(), None))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .map_or((None, None), |(id, name)| (Some(id), name));

        let event = build_single_event(
            Some(line),
            &content,
            role,
            "claude-code",
            None,
            tool_id.as_deref(),
            tool_name.as_deref(),
            session_id.as_deref(),
            None,
            timestamp,
            timestamp_ms,
            threading,
            secret_key,
        )?;
        threading.record(line.uuid(), event.note_id, is_conversation_role(role));
        vec![event]
    };

    // Build a kind-1989 source-data companion event linked to the first 1988 event.
    let first_note_id = events[0].note_id;
    let source_data_event = build_source_data_event(
        line,
        &first_note_id,
        threading.seq() - 1,
        session_id.as_deref(),
        timestamp,
        secret_key,
    )?;
    events.push(source_data_event);

    Ok(events)
}

#[derive(Debug)]
pub enum EventBuildError {
    Build(String),
    Serialize(String),
}

impl std::fmt::Display for EventBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventBuildError::Build(e) => write!(f, "failed to build note: {}", e),
            EventBuildError::Serialize(e) => write!(f, "failed to serialize event: {}", e),
        }
    }
}

/// Build a kind-1989 source-data companion event.
///
/// Contains the raw JSONL line and links to the corresponding 1988 event.
/// Does NOT participate in threading (no root/reply, no seq increment).
fn build_source_data_event(
    line: &JsonlLine,
    conversation_note_id: &[u8; 32],
    seq: u32,
    session_id: Option<&str>,
    timestamp: Option<u64>,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let raw_json = line.to_json();
    let seq_str = seq.to_string();

    let mut builder = init_note_builder(AI_SOURCE_DATA_KIND, "", timestamp);

    // Link to the corresponding 1988 event
    builder = builder
        .start_tag()
        .tag_str("e")
        .tag_id(conversation_note_id);

    if let Some(session_id) = session_id {
        builder = builder.start_tag().tag_str("d").tag_str(session_id);
    }

    // Same seq as the first 1988 event from this line
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    // The raw JSONL data
    builder = builder
        .start_tag()
        .tag_str("source-data")
        .tag_str(&raw_json);

    finalize_built_event(builder, secret_key, AI_SOURCE_DATA_KIND)
}

/// Build a single kind-1988 nostr event.
///
/// When `line` is provided (archive path), extracts slug, version, model,
/// line_type, and cwd from the JSONL line. When `None` (live path), only
/// uses the explicitly passed parameters.
///
/// `split_index`: `Some((i, total))` when this event is part of a split
/// assistant message.
///
/// `tool_id`: The tool use/result ID for tool_call and tool_result events.
///
/// `tool_name`: The tool name (e.g. "Bash", "Read") for tool_call and tool_result events.
#[allow(clippy::too_many_arguments)]
fn build_single_event(
    line: Option<&JsonlLine>,
    content: &str,
    role: &str,
    source: &str,
    split_index: Option<(usize, usize)>,
    tool_id: Option<&str>,
    tool_name: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
    timestamp: Option<u64>,
    timestamp_ms: Option<u64>,
    threading: &ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = init_note_builder(AI_CONVERSATION_KIND, content, timestamp);

    // -- Session identity tags --
    if let Some(session_id) = session_id {
        builder = builder.start_tag().tag_str("d").tag_str(session_id);
    }
    if let Some(slug) = line.and_then(|l| l.slug()) {
        builder = builder.start_tag().tag_str("session-slug").tag_str(slug);
    }

    // -- Threading tags (NIP-10) --
    if let Some(root_id) = threading.root_note_id {
        builder = builder
            .start_tag()
            .tag_str("e")
            .tag_id(&root_id)
            .tag_str("")
            .tag_str("root");
    }
    if let Some(reply_id) = threading.last_note_id {
        builder = builder
            .start_tag()
            .tag_str("e")
            .tag_id(&reply_id)
            .tag_str("")
            .tag_str("reply");
    }

    // -- Ordering tags --
    // `ms` (sub-second wall-clock) is the primary same-second tiebreak the
    // loader uses; `seq` is retained as a per-session monotonic index for the
    // JSONL reconstructor and out-of-order detection (see `session_loader`).
    builder = stamp_ms_tag(builder, timestamp_ms);
    let seq_str = threading.seq.to_string();
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    // -- Message metadata tags --
    builder = builder.start_tag().tag_str("source").tag_str(source);

    if let Some(version) = line.and_then(|l| l.version()) {
        builder = builder
            .start_tag()
            .tag_str("source-version")
            .tag_str(version);
    }

    builder = builder.start_tag().tag_str("role").tag_str(role);

    // Model tag (for assistant messages)
    if let Some(model) = line.and_then(|l| l.message()).and_then(|m| m.model()) {
        builder = builder.start_tag().tag_str("model").tag_str(model);
    }

    if let Some(line_type) = line.and_then(|l| l.line_type()) {
        builder = builder.start_tag().tag_str("turn-type").tag_str(line_type);
    }

    // -- CWD tag --
    let resolved_cwd = cwd.or_else(|| line.and_then(|l| l.cwd()));
    if let Some(cwd) = resolved_cwd {
        builder = builder.start_tag().tag_str("cwd").tag_str(cwd);
    }

    // -- Split tag (for split assistant messages) --
    if let Some((i, total)) = split_index {
        let split_str = format!("{}/{}", i, total);
        builder = builder.start_tag().tag_str("split").tag_str(&split_str);
    }

    // -- Tool ID tag --
    if let Some(tid) = tool_id {
        builder = builder.start_tag().tag_str("tool-id").tag_str(tid);
    }

    // -- Tool name tag --
    if let Some(tn) = tool_name {
        builder = builder.start_tag().tag_str("tool-name").tag_str(tn);
    }

    // -- Discoverability --
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");

    finalize_built_event(builder, secret_key, AI_CONVERSATION_KIND)
}

/// Build a kind-1988 event for a live conversation message.
///
/// Unlike `build_events()` which works from JSONL lines, this builds directly
/// from role + content strings. No kind-1989 source-data events are created.
///
/// Calls `threading.record()` internally.
#[allow(clippy::too_many_arguments)]
pub fn build_live_event(
    content: &str,
    role: &str,
    session_id: &str,
    cwd: Option<&str>,
    tool_id: Option<&str>,
    tool_name: Option<&str>,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    // One instant for both `created_at` (seconds) and the `ms` ordering tag so
    // they never straddle a second boundary.
    let now_ms = now_millis();
    let event = build_single_event(
        None,
        content,
        role,
        "notedeck-dave",
        None,
        tool_id,
        tool_name,
        Some(session_id),
        cwd,
        Some(now_ms / 1000),
        Some(now_ms),
        threading,
        secret_key,
    )?;

    threading.record(None, event.note_id, true);
    Ok(event)
}

/// Build a kind-1988 permission request event.
///
/// Published to relays so remote clients (phone) can see pending permission
/// requests and respond. Tags include `perm-id` (UUID), `tool-name`, and
/// `t: ai-permission` for filtering.
///
/// Maximum serialized size for tool_input in permission request events.
/// Keeps the final PNS-wrapped event well under typical relay limits
/// (~64KB). Budget: 40KB content + ~500B inner event overhead + ~500B
/// PNS outer overhead, with 1.33x base64 expansion ≈ 54KB total.
const MAX_TOOL_INPUT_BYTES: usize = 40_000;

/// Truncate large string values in a tool_input JSON object so that the
/// serialized result fits within `max_bytes`.
///
/// Only modifies top-level string fields in an Object value.  Fields are
/// truncated proportionally based on how much they exceed their share of
/// the budget.  A `"_truncated": true` flag is added when any field is
/// shortened.
fn truncate_tool_input(tool_input: &serde_json::Value, max_bytes: usize) -> serde_json::Value {
    use serde_json::Value;

    let serialized = serde_json::to_string(tool_input).unwrap_or_default();
    if serialized.len() <= max_bytes {
        return tool_input.clone();
    }

    let obj = match tool_input.as_object() {
        Some(o) => o,
        None => return tool_input.clone(),
    };

    // Collect string field names sorted largest-first so we know what to trim
    let mut string_fields: Vec<(&str, usize)> = obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s.len())))
        .collect();
    string_fields.sort_by_key(|field| std::cmp::Reverse(field.1));

    if string_fields.is_empty() {
        return tool_input.clone();
    }

    let excess = serialized.len() - max_bytes;
    let suffix = "\n... (truncated)";
    let suffix_len = suffix.len();
    // Safety margin for JSON escaping differences and the added _truncated field
    let trim_target = excess + suffix_len * string_fields.len() + 64;

    // Distribute the trim proportionally among string fields
    let total_string_bytes: usize = string_fields.iter().map(|(_, len)| len).sum();
    let mut trim_amounts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (key, len) in &string_fields {
        let share = (*len as f64 / total_string_bytes as f64 * trim_target as f64).ceil() as usize;
        trim_amounts.insert(key, share.min(*len));
    }

    let mut result = serde_json::Map::new();
    let mut did_truncate = false;
    for (key, val) in obj {
        if let Some(s) = val.as_str() {
            let trim = trim_amounts.get(key.as_str()).copied().unwrap_or(0);
            if trim > 0 && s.len() > suffix_len + trim {
                let keep = s.len() - trim;
                let cut = crate::util::floor_char_boundary(s, keep);
                let truncated = format!("{}{}", &s[..cut], suffix);
                result.insert(key.clone(), Value::String(truncated));
                did_truncate = true;
            } else {
                result.insert(key.clone(), val.clone());
            }
        } else {
            result.insert(key.clone(), val.clone());
        }
    }

    if did_truncate {
        result.insert("_truncated".to_string(), Value::Bool(true));
    }
    Value::Object(result)
}

/// Participates in threading so that permission events are correctly
/// ordered relative to tool_call / assistant events when reconstructed.
pub fn build_permission_request_event(
    perm_id: &uuid::Uuid,
    tool_name: &str,
    tool_input: &serde_json::Value,
    session_id: &str,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    // Truncate large string values so the event fits within relay size
    // limits after PNS wrapping.  The local UI keeps the full tool_input.
    let tool_input_for_event = truncate_tool_input(tool_input, MAX_TOOL_INPUT_BYTES);

    let content = serde_json::json!({
        "tool_name": tool_name,
        "tool_input": tool_input_for_event,
    })
    .to_string();

    let perm_id_str = perm_id.to_string();

    let now_ms = now_millis();
    let mut builder = init_note_builder(AI_CONVERSATION_KIND, &content, Some(now_ms / 1000));

    // Session identity
    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Ordering tags: `ms` sub-second (primary tiebreak), `seq` monotonic index.
    builder = stamp_ms_tag(builder, Some(now_ms));
    let seq_str = threading.seq.to_string();
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    // Permission-specific tags
    builder = builder.start_tag().tag_str("perm-id").tag_str(&perm_id_str);
    builder = builder.start_tag().tag_str("tool-name").tag_str(tool_name);
    builder = builder
        .start_tag()
        .tag_str("role")
        .tag_str("permission_request");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    // Discoverability
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");
    builder = builder.start_tag().tag_str("t").tag_str("ai-permission");

    let event = finalize_built_event(builder, secret_key, AI_CONVERSATION_KIND)?;
    threading.record(None, event.note_id, false);
    Ok(event)
}

/// Build a kind-1988 permission response event.
///
/// Published by remote clients (phone) to allow/deny a permission request.
/// The desktop subscribes for these and routes them through the existing
/// oneshot channel, racing with the local UI.
///
/// Tags include `perm-id` (matching the request), `e` tag linking to the
/// request event, and `t: ai-permission` for filtering.
#[allow(clippy::too_many_arguments)]
pub fn build_permission_response_event(
    perm_id: &uuid::Uuid,
    request_note_id: &[u8; 32],
    allowed: bool,
    message: Option<&str>,
    cancel_turn: bool,
    auto_accepted: bool,
    session_id: &str,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    // Keep the legacy `interrupt` key on the wire for compatibility with
    // sessions that may still decode the earlier payload shape. `auto` records
    // whether the runtime allowlist / Auto Accept All resolved this without a
    // user click, so a remote session rebuilt from ndb can start the responded
    // row expanded for after-the-fact review (see `PermissionRequest`).
    let content = serde_json::json!({
        "decision": if allowed { "allow" } else { "deny" },
        "message": message.unwrap_or(""),
        "interrupt": cancel_turn,
        "auto": auto_accepted,
    })
    .to_string();

    let perm_id_str = perm_id.to_string();

    let now_ms = now_millis();
    let mut builder = init_note_builder(AI_CONVERSATION_KIND, &content, Some(now_ms / 1000));

    // Session identity
    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Ordering tags. `ms` (sub-second wall-clock) is the loader's primary
    // same-second tiebreak; `seq` remains a per-session monotonic index. Every
    // kind-1988 conversation event carries a `seq` — see the
    // `no_conversation_event_is_seqless` invariant test.
    builder = stamp_ms_tag(builder, Some(now_ms));
    let seq_str = threading.seq.to_string();
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    // Link to the request event
    builder = builder.start_tag().tag_str("e").tag_id(request_note_id);

    // Permission-specific tags
    builder = builder.start_tag().tag_str("perm-id").tag_str(&perm_id_str);
    builder = builder
        .start_tag()
        .tag_str("role")
        .tag_str("permission_response");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    // Discoverability
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");
    builder = builder.start_tag().tag_str("t").tag_str("ai-permission");

    let event = finalize_built_event(builder, secret_key, AI_CONVERSATION_KIND)?;
    threading.record(None, event.note_id, false);
    Ok(event)
}

/// Decode a permission response from its JSON content string.
///
/// Returns the decision as a `PermissionResponseType` and an optional
/// human-readable message plus whether the denial should interrupt the
/// current turn. Defaults to `Denied`/`false` if the content cannot be
/// parsed or has no `"decision"` field.
/// Decoded fields of a permission-response event's JSON content.
///
/// A named struct rather than a bare tuple because the payload now carries two
/// adjacent booleans (`cancel_turn`, `auto_accepted`) that are easy to transpose
/// positionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPermissionResponse {
    /// Whether the request was allowed or denied.
    pub response_type: crate::messages::PermissionResponseType,
    /// Optional human-readable message (e.g. the question-set answer payload).
    pub message: Option<String>,
    /// Whether a denial should interrupt the current turn.
    pub cancel_turn: bool,
    /// Whether the runtime allowlist / Auto Accept All resolved this without a
    /// user click. Reconstructs [`PermissionRequest::auto_accepted`] on load.
    pub auto_accepted: bool,
}

pub fn decode_permission_response(content: &str) -> DecodedPermissionResponse {
    use crate::messages::PermissionResponseType;

    let denied = DecodedPermissionResponse {
        response_type: PermissionResponseType::Denied,
        message: None,
        cancel_turn: false,
        auto_accepted: false,
    };

    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        return denied;
    };

    let allowed = v.get("decision").and_then(|d| d.as_str()).unwrap_or("deny") == "allow";
    let message = v
        .get("message")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let cancel_turn = v
        .get("interrupt")
        .and_then(|i| i.as_bool())
        .unwrap_or(false);
    let auto_accepted = v.get("auto").and_then(|a| a.as_bool()).unwrap_or(false);
    let response_type = if allowed {
        PermissionResponseType::Allowed
    } else {
        PermissionResponseType::Denied
    };
    DecodedPermissionResponse {
        response_type,
        message,
        cancel_turn,
        auto_accepted,
    }
}

/// Build a kind-31988 session state event (parameterized replaceable).
///
/// Published on every status change so remote clients and startup restore
/// can discover active sessions. nostrdb auto-replaces older versions
/// with same (kind, pubkey, d-tag).
///
/// `created_at` is supplied by the caller (rather than stamped internally) so
/// it can be kept **strictly monotonic per session** — see
/// [`next_state_created_at`]. Both nostrdb's replaceable resolution and the
/// receive-side `remote_status_ts` guard key on this second-resolution
/// timestamp; if two status transitions shared a second the tie was broken by
/// event id (~random), so a stale status could win and the session would stick
/// (e.g. `NeedsInput` floating to the top of the sidebar). Monotonic
/// `created_at` makes the latest revision always compare newest.
#[allow(clippy::too_many_arguments)]
pub fn build_session_state_event(
    event_session_id: &str,
    title: &str,
    custom_title: Option<&str>,
    cwd: &str,
    status: &str,
    indicator: Option<&str>,
    hostname: &str,
    home_dir: &str,
    backend: &str,
    permission_mode: &str,
    cli_session_id: Option<&str>,
    spawn_id: Option<&str>,
    created_at: u64,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = init_note_builder(AI_SESSION_STATE_KIND, "", Some(created_at));

    // Session identity (makes this a parameterized replaceable event)
    builder = builder.start_tag().tag_str("d").tag_str(event_session_id);

    // Session metadata as tags
    builder = builder.start_tag().tag_str("title").tag_str(title);
    if let Some(ct) = custom_title {
        builder = builder.start_tag().tag_str("custom_title").tag_str(ct);
    }
    builder = builder.start_tag().tag_str("cwd").tag_str(cwd);
    builder = builder.start_tag().tag_str("status").tag_str(status);
    if let Some(ind) = indicator {
        builder = builder.start_tag().tag_str("indicator").tag_str(ind);
    }
    builder = builder.start_tag().tag_str("hostname").tag_str(hostname);
    builder = builder.start_tag().tag_str("home_dir").tag_str(home_dir);
    builder = builder.start_tag().tag_str("backend").tag_str(backend);
    builder = builder
        .start_tag()
        .tag_str("permission-mode")
        .tag_str(permission_mode);

    // Real Claude CLI session ID for backend --resume.
    // Empty string means the backend hasn't started yet.
    // Absent (old events) means the d-tag itself is the CLI ID.
    builder = builder
        .start_tag()
        .tag_str("cli_session")
        .tag_str(cli_session_id.unwrap_or(""));

    // Spawn command UUID linking this session to the request that created it.
    if let Some(sid) = spawn_id {
        builder = builder.start_tag().tag_str("spawn_id").tag_str(sid);
    }

    // Discoverability
    builder = builder.start_tag().tag_str("t").tag_str("ai-session-state");
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    finalize_built_event(builder, secret_key, AI_SESSION_STATE_KIND)
}

/// Resume parameters for a spawn command that reopens an *existing* session
/// instead of creating a fresh one.
///
/// When present on [`build_spawn_command_event`], the command's `command` tag
/// becomes `"resume_session"` and it carries the target session's stable
/// identity plus the CLI session id needed for `claude --resume`. The host peels
/// these back in `poll_session_command_events` to revive the tombstoned session
/// rather than minting a new one.
pub struct ResumeSpawn<'a> {
    /// The kind-31988 d-tag of the session to revive (its stable Nostr identity,
    /// i.e. the `agentium:` ref). The host reopens *this* session.
    pub target_session_id: &'a str,
    /// The real CLI session id (the `cli_session` tag value) that the backend
    /// feeds to `claude --resume` to reconstruct the conversation.
    pub cli_session_id: &'a str,
}

/// Build a kind-31989 spawn command event.
///
/// This is a fire-and-forget command that tells a remote host to create a
/// session. The target host discovers the command via its ndb subscription,
/// materializes the session locally, and publishes a kind-31988 state event.
///
/// With `resume` = `None` this is a plain spawn (`command = "spawn_session"`,
/// a new session). With `resume` = `Some`, it is a resume command
/// (`command = "resume_session"`) that reopens the session named by
/// [`ResumeSpawn::target_session_id`] — see [`ResumeSpawn`].
pub fn build_spawn_command_event(
    target_host: &str,
    cwd: &str,
    backend: &str,
    spawn_id: &str,
    resume: Option<&ResumeSpawn<'_>>,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let command_id = uuid::Uuid::new_v4().to_string();
    let mut builder = init_note_builder(AI_SESSION_COMMAND_KIND, "", Some(now_secs()));

    let command = if resume.is_some() {
        "resume_session"
    } else {
        "spawn_session"
    };

    builder = builder.start_tag().tag_str("d").tag_str(&command_id);
    builder = builder.start_tag().tag_str("command").tag_str(command);
    builder = builder
        .start_tag()
        .tag_str("target_host")
        .tag_str(target_host);
    builder = builder.start_tag().tag_str("cwd").tag_str(cwd);
    builder = builder.start_tag().tag_str("backend").tag_str(backend);
    builder = builder.start_tag().tag_str("spawn_id").tag_str(spawn_id);

    // Resume commands carry the identity of the session to revive plus the CLI
    // session id for `claude --resume`. Absent on a plain spawn.
    if let Some(resume) = resume {
        builder = builder
            .start_tag()
            .tag_str("session_id")
            .tag_str(resume.target_session_id);
        builder = builder
            .start_tag()
            .tag_str("resume_session_id")
            .tag_str(resume.cli_session_id);
    }

    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-session-command");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    finalize_built_event(builder, secret_key, AI_SESSION_COMMAND_KIND)
}

/// Build a kind-31991 run-config event (parameterized replaceable, NIP-33).
///
/// One event per individual config. The d-tag is the config's stable UUID,
/// which survives renames, command edits, reloads, and cross-device sync.
pub fn build_run_config_event(
    config: &RunConfig,
    cwd: &str,
    hostname: &str,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = init_note_builder(AI_RUN_CONFIG_KIND, "", None);

    builder = builder.start_tag().tag_str("d").tag_str(&config.id);
    builder = builder.start_tag().tag_str("cwd").tag_str(cwd);
    builder = builder.start_tag().tag_str("hostname").tag_str(hostname);
    builder = builder.start_tag().tag_str("name").tag_str(&config.name);
    builder = builder
        .start_tag()
        .tag_str("command")
        .tag_str(&config.command);

    finalize_built_event(builder, secret_key, AI_RUN_CONFIG_KIND)
}

/// Build a tombstone kind-31991 event to delete a run config.
///
/// Publishes an event with the same d-tag (config UUID) but with a `deleted`
/// tag. This replaces the live config in nostrdb via NIP-33.
pub fn build_run_config_delete_event(
    config_id: &str,
    cwd: &str,
    hostname: &str,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = init_note_builder(AI_RUN_CONFIG_KIND, "", None);

    builder = builder.start_tag().tag_str("d").tag_str(config_id);
    builder = builder.start_tag().tag_str("cwd").tag_str(cwd);
    builder = builder.start_tag().tag_str("hostname").tag_str(hostname);
    builder = builder.start_tag().tag_str("deleted").tag_str("true");

    finalize_built_event(builder, secret_key, AI_RUN_CONFIG_KIND)
}

/// Parse a kind-31991 run-config note into a single `RunConfig`.
///
/// Returns `None` if this is a tombstone (has `deleted` tag) or if
/// required tags (`d`, `cwd`, `name`, `command`) are missing.
pub fn parse_run_config_event(note: &nostrdb::Note) -> Option<(PathBuf, RunConfig)> {
    // Tombstone — treat as deleted
    if get_tag_value(note, "deleted").is_some() {
        return None;
    }
    let id = get_tag_value(note, "d")?.to_string();
    let cwd = get_tag_value(note, "cwd")?;
    if cwd.is_empty() {
        return None;
    }
    let name = get_tag_value(note, "name")
        .filter(|s| !s.is_empty())?
        .to_string();
    let command = get_tag_value(note, "command")
        .filter(|s| !s.is_empty())?
        .to_string();

    Some((
        PathBuf::from(cwd),
        RunConfig {
            id,
            name,
            command,
            updated_at: note.created_at(),
        },
    ))
}

/// Extract the d-tag from a kind-31991 note. Used to identify tombstones
/// so the caller can remove the config by ID even when `parse_run_config_event`
/// returns `None`.
pub fn run_config_event_id(note: &nostrdb::Note) -> Option<String> {
    get_tag_value(note, "d").map(|s| s.to_string())
}

/// Check whether a kind-31991 note is a deletion tombstone.
pub fn is_run_config_deleted(note: &nostrdb::Note) -> bool {
    get_tag_value(note, "deleted").is_some()
}

/// Build a kind-1988 command event to set permission mode on a remote session.
///
/// Published by remote observers to request a permission mode change on the host.
/// The host subscribes for these and applies them via its local backend.
pub fn build_set_permission_mode_event(
    mode: &str,
    session_id: &str,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let content = serde_json::json!({
        "mode": mode,
    })
    .to_string();

    let now_ms = now_millis();
    let mut builder = init_note_builder(AI_CONVERSATION_KIND, &content, Some(now_ms / 1000));

    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Ordering tags: `ms` sub-second (primary tiebreak), `seq` monotonic index.
    // Every kind-1988 conversation event carries a `seq` — see the
    // `no_conversation_event_is_seqless` invariant test.
    builder = stamp_ms_tag(builder, Some(now_ms));
    let seq_str = threading.seq.to_string();
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    builder = builder
        .start_tag()
        .tag_str("role")
        .tag_str("set_permission_mode");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");
    builder = builder.start_tag().tag_str("t").tag_str("ai-command");

    let event = finalize_built_event(builder, secret_key, AI_CONVERSATION_KIND)?;
    threading.record(None, event.note_id, false);
    Ok(event)
}

/// Build a kind-1988 command event to interrupt a remote session's in-flight turn.
///
/// Published by a remote observer to abort whatever turn/tool loop the host is
/// currently running, mirroring the local Escape interrupt. The host subscribes
/// for these (`role = "interrupt"`, `t = ai-command`) and applies them by sending
/// `SessionCommand::Interrupt` to its local backend actor. Carries no payload —
/// the session is identified by the `d` tag.
pub fn build_interrupt_event(
    session_id: &str,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let now_ms = now_millis();
    let mut builder = init_note_builder(AI_CONVERSATION_KIND, "{}", Some(now_ms / 1000));

    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Ordering tags: `ms` sub-second (primary tiebreak), `seq` monotonic index.
    // Every kind-1988 conversation event carries a `seq` — see the
    // `no_conversation_event_is_seqless` invariant test.
    builder = stamp_ms_tag(builder, Some(now_ms));
    let seq_str = threading.seq.to_string();
    builder = builder.start_tag().tag_str("seq").tag_str(&seq_str);

    builder = builder.start_tag().tag_str("role").tag_str("interrupt");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");
    builder = builder.start_tag().tag_str("t").tag_str("ai-command");

    let event = finalize_built_event(builder, secret_key, AI_CONVERSATION_KIND)?;
    threading.record(None, event.note_id, false);
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> nostrdb::Config {
        if cfg!(target_os = "windows") {
            nostrdb::Config::new().set_mapsize(32 * 1024 * 1024)
        } else {
            nostrdb::Config::new()
        }
    }

    // Test secret key (32 bytes, not for real use)
    fn test_secret_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = 1; // non-zero so signing works
        key
    }

    #[test]
    fn test_build_user_text_event() {
        let line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess1","timestamp":"2026-02-09T20:43:35.675Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"user","content":"Human: hello world\n\n"}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();

        // 1 conversation event (1988) + 1 source-data event (1989)
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, AI_CONVERSATION_KIND);
        assert_eq!(events[1].kind, AI_SOURCE_DATA_KIND);
        assert!(threading.root_note_id.is_some());
        assert_eq!(threading.root_note_id, Some(events[0].note_id));

        // 1988 event has kind and tags but NO source-data
        let json = &events[0].note_json;
        assert!(json.contains("1988"));
        assert!(json.contains("source"));
        assert!(json.contains("claude-code"));
        assert!(json.contains("role"));
        assert!(json.contains("user"));
        assert!(!json.contains("source-data"));

        // 1989 event has source-data
        assert!(events[1].note_json.contains("source-data"));
    }

    #[test]
    fn test_build_assistant_text_event() {
        let line = JsonlLine::parse(
            r#"{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"sess1","timestamp":"2026-02-09T20:43:38.421Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"I can help with that."}]}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        // Simulate a prior event
        threading.root_note_id = Some([1u8; 32]);
        threading.last_note_id = Some([1u8; 32]);

        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        // 1 conversation (1988) + 1 source-data (1989)
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, AI_CONVERSATION_KIND);
        assert_eq!(events[1].kind, AI_SOURCE_DATA_KIND);

        let json = &events[0].note_json;
        assert!(json.contains("assistant"));
        assert!(json.contains("claude-opus-4-5-20251101")); // model tag
    }

    #[test]
    fn test_build_split_assistant_mixed_content() {
        let line = JsonlLine::parse(
            r#"{"type":"assistant","uuid":"u3","sessionId":"sess1","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/test.rs"}}]}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();

        // 2 conversation events (1988) + 1 source-data (1989)
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, AI_CONVERSATION_KIND);
        assert_eq!(events[1].kind, AI_CONVERSATION_KIND);
        assert_eq!(events[2].kind, AI_SOURCE_DATA_KIND);

        // All should have unique note IDs
        assert_ne!(events[0].note_id, events[1].note_id);
        assert_ne!(events[0].note_id, events[2].note_id);
    }

    #[test]
    fn test_threading_chain() {
        let lines = vec![
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"user","uuid":"u3","parentUuid":"u2","sessionId":"s","timestamp":"2026-02-09T20:00:02Z","cwd":"/tmp","version":"2.0.64","message":{"role":"user","content":"bye"}}"#,
        ];

        let mut threading = ThreadingState::new();
        let sk = test_secret_key();
        let mut all_events = vec![];

        for line_str in &lines {
            let line = JsonlLine::parse(line_str).unwrap();
            let events = build_events(&line, &mut threading, &sk).unwrap();
            all_events.extend(events);
        }

        // 3 lines × (1 conversation + 1 source-data) = 6 events
        assert_eq!(all_events.len(), 6);

        // Filter to only 1988 events for threading checks
        let conv_events: Vec<_> = all_events
            .iter()
            .filter(|e| e.kind == AI_CONVERSATION_KIND)
            .collect();
        assert_eq!(conv_events.len(), 3);

        // First event should be root (no e tags)
        // Subsequent events should reference root + previous
        assert!(!conv_events[0].note_json.contains("root"));
        assert!(conv_events[1].note_json.contains("root"));
        assert!(conv_events[1].note_json.contains("reply"));
        assert!(conv_events[2].note_json.contains("root"));
        assert!(conv_events[2].note_json.contains("reply"));
    }

    #[test]
    fn test_source_data_preserves_raw_json() {
        let line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:00Z","cwd":"/Users/jb55/dev/notedeck","version":"2.0.64","message":{"role":"user","content":"check /Users/jb55/dev/notedeck/src/main.rs"}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();

        // 1988 event should NOT have source-data
        assert!(!events[0].note_json.contains("source-data"));

        // 1989 event should have source-data with raw paths preserved
        let sd_event = events
            .iter()
            .find(|e| e.kind == AI_SOURCE_DATA_KIND)
            .unwrap();
        assert!(sd_event.note_json.contains("source-data"));
        assert!(sd_event.note_json.contains("/Users/jb55/dev/notedeck"));
    }

    #[test]
    fn test_queue_operation_event() {
        let line = JsonlLine::parse(
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-02-09T20:43:35.669Z","sessionId":"sess1"}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        // 1 conversation (1988) + 1 source-data (1989)
        assert_eq!(events.len(), 2);

        let json = &events[0].note_json;
        assert!(json.contains("queue-operation"));
    }

    #[test]
    fn test_seq_counter_increments() {
        let lines = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
        ];

        let mut threading = ThreadingState::new();
        let sk = test_secret_key();

        assert_eq!(threading.seq(), 0);

        let line = JsonlLine::parse(lines[0]).unwrap();
        let events = build_events(&line, &mut threading, &sk).unwrap();
        // 1 conversation + 1 source-data
        assert_eq!(events.len(), 2);
        assert_eq!(threading.seq(), 1);
        // First 1988 event should have seq=0
        assert!(events[0].note_json.contains(r#""seq","0"#));
        // 1989 event should also have seq=0 (matches its 1988 event)
        assert!(events[1].note_json.contains(r#""seq","0"#));

        let line = JsonlLine::parse(lines[1]).unwrap();
        let events = build_events(&line, &mut threading, &sk).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(threading.seq(), 2);
        // Second 1988 event should have seq=1
        assert!(events[0].note_json.contains(r#""seq","1"#));
    }

    #[test]
    fn test_split_tags_and_source_data() {
        let line = JsonlLine::parse(
            r#"{"type":"assistant","uuid":"u3","sessionId":"sess1","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/test.rs"}}]}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        // 2 conversation (1988) + 1 source-data (1989)
        assert_eq!(events.len(), 3);

        // First event (text): split 0/2, NO source-data (moved to 1989)
        assert!(events[0].note_json.contains(r#""split","0/2"#));
        assert!(!events[0].note_json.contains("source-data"));

        // Second event (tool_call): split 1/2, NO source-data, has tool-id
        assert!(events[1].note_json.contains(r#""split","1/2"#));
        assert!(!events[1].note_json.contains("source-data"));
        assert!(events[1].note_json.contains(r#""tool-id","t1"#));

        // Third event (1989): has source-data
        assert_eq!(events[2].kind, AI_SOURCE_DATA_KIND);
        assert!(events[2].note_json.contains("source-data"));
    }

    #[test]
    fn test_cwd_tag() {
        let line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:00Z","cwd":"/Users/jb55/dev/notedeck","version":"2.0.64","message":{"role":"user","content":"hello"}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();

        assert!(events[0]
            .note_json
            .contains(r#""cwd","/Users/jb55/dev/notedeck"#));
    }

    #[test]
    fn test_tool_result_has_tool_id() {
        let line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u4","parentUuid":"u3","cwd":"/tmp","sessionId":"s","version":"2.0.64","timestamp":"2026-02-09T20:00:03Z","message":{"role":"user","content":[{"tool_use_id":"toolu_abc","type":"tool_result","content":"file contents"}]}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        // 1 conversation + 1 source-data
        assert_eq!(events.len(), 2);
        assert!(events[0].note_json.contains(r#""tool-id","toolu_abc"#));
    }

    #[tokio::test]
    async fn test_full_roundtrip() {
        use crate::session_reconstructor;
        use nostrdb::{IngestMetadata, Ndb, Transaction};
        use serde_json::Value;
        use tempfile::TempDir;

        // Sample JSONL lines covering different message types
        let jsonl_lines = vec![
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-02-09T20:00:00Z","sessionId":"roundtrip-test"}"#,
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"roundtrip-test","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"user","content":"Human: hello world\n\n"}}"#,
            r#"{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"roundtrip-test","timestamp":"2026-02-09T20:00:02Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"Let me check that file."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/tmp/project/main.rs"}}]}}"#,
            r#"{"type":"user","uuid":"u3","parentUuid":"u2","sessionId":"roundtrip-test","timestamp":"2026-02-09T20:00:03Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"fn main() {}"}]}}"#,
            r#"{"type":"assistant","uuid":"u4","parentUuid":"u3","sessionId":"roundtrip-test","timestamp":"2026-02-09T20:00:04Z","cwd":"/tmp/project","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"That's a simple main function."}]}}"#,
        ];

        // Set up ndb
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        // Build and ingest events one at a time, waiting for each
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let mut total_events = 0;

        let filter = nostrdb::Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64, AI_SOURCE_DATA_KIND as u64])
            .build();

        for line_str in &jsonl_lines {
            let line = JsonlLine::parse(line_str).unwrap();
            let events = build_events(&line, &mut threading, &sk).unwrap();
            for event in &events {
                let sub_id = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
                ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                    .expect("ingest failed");
                let _keys = ndb.wait_for_notes(sub_id, 1).await.unwrap();
                total_events += 1;
            }
        }

        // Each JSONL line produces N conversation events + 1 source-data event.
        // Line 1 (queue-op): 1 conv + 1 sd = 2
        // Line 2 (user): 1 conv + 1 sd = 2
        // Line 3 (assistant split): 2 conv + 1 sd = 3
        // Line 4 (user tool_result): 1 conv + 1 sd = 2
        // Line 5 (assistant): 1 conv + 1 sd = 2
        // Total: 11
        assert_eq!(total_events, 11);

        // Reconstruct JSONL from ndb
        let txn = Transaction::new(&ndb).unwrap();
        let reconstructed =
            session_reconstructor::reconstruct_jsonl_lines(&ndb, &txn, "roundtrip-test").unwrap();

        // Should get back one JSONL line per original line
        assert_eq!(
            reconstructed.len(),
            jsonl_lines.len(),
            "expected {} lines, got {}",
            jsonl_lines.len(),
            reconstructed.len()
        );

        // Compare each line as serde_json::Value for order-independent equality
        for (i, (original, reconstructed)) in
            jsonl_lines.iter().zip(reconstructed.iter()).enumerate()
        {
            let orig_val: Value = serde_json::from_str(original).unwrap();
            let recon_val: Value = serde_json::from_str(reconstructed).unwrap();
            assert_eq!(
                orig_val, recon_val,
                "line {} mismatch.\noriginal:      {}\nreconstructed: {}",
                i, original, reconstructed
            );
        }
    }

    #[test]
    fn test_file_history_snapshot_inherits_context() {
        // file-history-snapshot lines lack sessionId and top-level timestamp.
        // They should inherit session_id from a prior line and get timestamp
        // from snapshot.timestamp.
        let lines = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"ctx-test","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"user","content":"hello"}}"#,
            r#"{"type":"file-history-snapshot","messageId":"abc","snapshot":{"messageId":"abc","trackedFileBackups":{},"timestamp":"2026-02-11T01:29:31.555Z"},"isSnapshotUpdate":false}"#,
        ];

        let mut threading = ThreadingState::new();
        let sk = test_secret_key();

        // First line sets context
        let line = JsonlLine::parse(lines[0]).unwrap();
        let events = build_events(&line, &mut threading, &sk).unwrap();
        assert!(events[0].note_json.contains(r#""d","ctx-test"#));

        // Second line (file-history-snapshot) should inherit session_id
        let line = JsonlLine::parse(lines[1]).unwrap();
        assert!(line.session_id().is_none()); // no top-level sessionId
        let events = build_events(&line, &mut threading, &sk).unwrap();

        // 1988 event should have inherited d tag
        assert!(events[0].note_json.contains(r#""d","ctx-test"#));
        // Should have snapshot timestamp (1770773371), not the user's
        assert!(events[0].note_json.contains(r#""created_at":1770773371"#));
    }

    #[test]
    fn test_build_permission_request_event() {
        let perm_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let tool_input = serde_json::json!({"command": "rm -rf /tmp/test"});
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        // Simulate some prior events so seq is non-zero
        threading.seq = 5;

        let event = build_permission_request_event(
            &perm_id,
            "Bash",
            &tool_input,
            "sess-perm-test",
            &mut threading,
            &sk,
        )
        .unwrap();

        assert_eq!(event.kind, AI_CONVERSATION_KIND);

        let json = &event.note_json;
        // Has permission-specific tags
        assert!(json.contains(r#""perm-id","550e8400-e29b-41d4-a716-446655440000"#));
        assert!(json.contains(r#""tool-name","Bash"#));
        assert!(json.contains(r#""role","permission_request"#));
        // Has session identity
        assert!(json.contains(r#""d","sess-perm-test"#));
        // Has seq tag for ordering
        assert!(json.contains(r#""seq","5"#));
        // Has discoverability tags
        assert!(json.contains(r#""t","ai-conversation"#));
        assert!(json.contains(r#""t","ai-permission"#));
        // Content has tool info
        assert!(json.contains("rm -rf"));
        // Threading state should have advanced
        assert_eq!(threading.seq(), 6);
    }

    /// Seeding sets the reply chain, not `seq`: the root is pinned once and the
    /// `last` note follows the latest seed, while the `seq` counter is left to
    /// advance only via `record`. Display order no longer depends on the seeded
    /// `seq`, so seeding does not touch it (see [`super::EventOrder`]).
    #[test]
    fn seed_sets_reply_chain_not_seq() {
        let root = [1u8; 32];
        let last_a = [2u8; 32];
        let last_b = [3u8; 32];

        let mut threading = ThreadingState::new();

        // First seed pins the root and threads onto `last_a`; `seq` is untouched.
        threading.seed(root, last_a);
        assert_eq!(threading.seq(), 0);
        assert_eq!(threading.root_note_id, Some(root));
        assert_eq!(threading.last_note_id, Some(last_a));

        // A re-seed updates the reply target to the loader's latest note. This
        // is safe against a partial snapshot because a live emit's `now`
        // timestamp dominates backfilled history, so the loader's `last` is
        // never older than an already-emitted event.
        threading.seed(root, last_b);
        assert_eq!(threading.last_note_id, Some(last_b));
        assert_eq!(threading.seq(), 0);

        // Root is set once and never changes.
        threading.seed([9u8; 32], last_b);
        assert_eq!(threading.root_note_id, Some(root));
    }

    #[test]
    fn test_truncate_tool_input_small() {
        // Small input should pass through unchanged
        let input = serde_json::json!({"command": "ls -la"});
        let result = truncate_tool_input(&input, 1000);
        assert_eq!(input, result);
        assert!(result.get("_truncated").is_none());
    }

    #[test]
    fn test_truncate_tool_input_large_edit() {
        // Large edit should be truncated
        let big_old = "x".repeat(30_000);
        let big_new = "y".repeat(30_000);
        let input = serde_json::json!({
            "file_path": "/some/file.rs",
            "old_string": big_old,
            "new_string": big_new,
        });
        let result = truncate_tool_input(&input, 40_000);

        // Should fit within budget
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(serialized.len() <= 40_000, "got {} bytes", serialized.len());

        // Should be marked as truncated
        assert_eq!(
            result.get("_truncated").and_then(|v| v.as_bool()),
            Some(true)
        );

        // file_path should be preserved
        assert_eq!(
            result.get("file_path").and_then(|v| v.as_str()),
            Some("/some/file.rs")
        );

        // Truncated fields should end with the suffix
        let old = result.get("old_string").and_then(|v| v.as_str()).unwrap();
        assert!(old.ends_with("... (truncated)"));
    }

    #[test]
    fn test_truncate_tool_input_utf8_boundary() {
        // Multibyte chars should not be split
        let big = "\u{1F600}".repeat(10_000); // 4-byte emoji repeated
        let input = serde_json::json!({"content": big});
        let result = truncate_tool_input(&input, 1000);

        let content = result.get("content").and_then(|v| v.as_str()).unwrap();
        // Should be valid UTF-8 (this would panic if not)
        assert!(content.ends_with("... (truncated)"));
    }

    #[test]
    fn test_build_permission_response_event() {
        let perm_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_note_id = [42u8; 32];
        let sk = test_secret_key();

        // Test allow response
        let mut threading = ThreadingState::new();
        threading.seq = 7; // prior events in the conversation
        let event = build_permission_response_event(
            &perm_id,
            &request_note_id,
            true,
            Some("looks safe"),
            false,
            false,
            "sess-perm-test",
            &mut threading,
            &sk,
        )
        .unwrap();

        assert_eq!(event.kind, AI_CONVERSATION_KIND);

        let json = &event.note_json;
        assert!(json.contains(r#""perm-id","550e8400-e29b-41d4-a716-446655440000"#));
        assert!(json.contains(r#""role","permission_response"#));
        assert!(json.contains(r#""d","sess-perm-test"#));
        assert!(json.contains("allow"));
        assert!(json.contains("looks safe"));
        // Has e tag linking to request
        assert!(json.contains(r#""e""#));
        // Carries a seq and advances the counter — no seq-less events.
        assert!(json.contains(r#""seq","7"#));
        assert_eq!(threading.seq(), 8);
    }

    #[test]
    fn test_permission_response_deny() {
        let perm_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_note_id = [42u8; 32];
        let sk = test_secret_key();

        let mut threading = ThreadingState::new();
        let event = build_permission_response_event(
            &perm_id,
            &request_note_id,
            false,
            Some("too dangerous"),
            true,
            false,
            "sess-perm-test",
            &mut threading,
            &sk,
        )
        .unwrap();

        let json = &event.note_json;
        assert!(json.contains("deny"));
        assert!(json.contains("too dangerous"));
        let parsed: serde_json::Value = serde_json::from_str(json).expect("valid event json");
        let content = parsed["content"]
            .as_str()
            .expect("event content should be a string");
        assert!(content.contains(r#""interrupt":true"#));
    }

    /// INVARIANT: every kind-1988 conversation event carries a `seq` tag.
    ///
    /// A seq-less event sorts by the `u32::MAX` fallback and — worse — inflates
    /// the ndb note count that seeds the live sequence counter, seeding it ahead
    /// of the next turn and floating messages out of order. That was the root of
    /// the long-standing conversation-ordering drift. This test exercises every
    /// kind-1988 builder; if you add a new one, it MUST stamp a `seq` through
    /// [`ThreadingState`] and be listed here.
    #[test]
    fn no_conversation_event_is_seqless() {
        let sk = test_secret_key();
        let session_id = "seq-invariant";
        let assert_has_seq = |json: &str| {
            assert!(
                json.contains(r#"["seq","#),
                "kind-1988 event is missing its seq tag: {json}"
            );
        };

        let mut threading = ThreadingState::new();

        // Live conversation roles (assistant, tool, error, compaction, …).
        for role in ["user", "assistant", "tool_call", "tool_result", "error"] {
            let ev = build_live_event(
                "hi",
                role,
                session_id,
                None,
                None,
                None,
                &mut threading,
                &sk,
            )
            .unwrap();
            assert_has_seq(&ev.note_json);
        }

        // Permission request and its response.
        let perm_id = uuid::Uuid::new_v4();
        let req = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            session_id,
            &mut threading,
            &sk,
        )
        .unwrap();
        assert_has_seq(&req.note_json);
        let resp = build_permission_response_event(
            &perm_id,
            &req.note_id,
            true,
            None,
            false,
            false,
            session_id,
            &mut threading,
            &sk,
        )
        .unwrap();
        assert_has_seq(&resp.note_json);

        // Set-permission-mode command.
        let mode =
            build_set_permission_mode_event("plan", session_id, &mut threading, &sk).unwrap();
        assert_has_seq(&mode.note_json);

        // Interrupt command.
        let interrupt = build_interrupt_event(session_id, &mut threading, &sk).unwrap();
        assert_has_seq(&interrupt.note_json);

        // JSONL-derived conversation events (build_events → build_single_event).
        let line = JsonlLine::parse(&format!(
            r#"{{"type":"user","uuid":"x1","parentUuid":null,"sessionId":"{session_id}","timestamp":"2024-01-01T00:00:00Z","cwd":"/tmp","version":"2.0.0","message":{{"role":"user","content":"hello"}}}}"#,
        ))
        .unwrap();
        for ev in build_events(&line, &mut threading, &sk).unwrap() {
            if ev.kind == AI_CONVERSATION_KIND {
                assert_has_seq(&ev.note_json);
            }
        }
    }

    #[test]
    fn test_build_interrupt_event() {
        let session_id = "sess-interrupt";
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();

        let ev = build_interrupt_event(session_id, &mut threading, &sk).unwrap();

        assert_eq!(ev.kind, AI_CONVERSATION_KIND);
        let json = &ev.note_json;
        assert!(json.contains(r#"["role","interrupt"]"#));
        assert!(json.contains(r#"["t","ai-command"]"#));
        assert!(json.contains(&format!(r#"["d","{session_id}"]"#)));
        // No payload beyond the empty object — the session id in the `d` tag is
        // all the host needs.
        assert!(json.contains(r#""content":"{}""#));
    }

    #[test]
    fn test_decode_permission_response_interrupt() {
        let decoded = decode_permission_response(
            r#"{"decision":"deny","message":"stop here","interrupt":true}"#,
        );

        assert_eq!(
            decoded.response_type,
            crate::messages::PermissionResponseType::Denied
        );
        assert_eq!(decoded.message.as_deref(), Some("stop here"));
        assert!(decoded.cancel_turn);
        // No `auto` key → defaults to false (manual decision).
        assert!(!decoded.auto_accepted);
    }

    #[test]
    fn test_decode_permission_response_auto_accepted() {
        // The runtime allowlist / Auto Accept All records `"auto":true` so the
        // responded row can be reconstructed as expanded on a fresh-machine load.
        let decoded =
            decode_permission_response(r#"{"decision":"allow","message":"","auto":true}"#);

        assert_eq!(
            decoded.response_type,
            crate::messages::PermissionResponseType::Allowed
        );
        assert!(decoded.auto_accepted);
    }

    /// Backward compat: a legacy question-set response, where the answers were
    /// serialized to JSON and escaped *inside* `message` (the old
    /// double-encoding), must still decode. The decoder treats `message` as
    /// opaque text, so the escaped JSON round-trips back out as-is — old events
    /// already stored in nostrdb keep working after the encode switched to prose.
    #[test]
    fn test_decode_permission_response_legacy_answers_json() {
        let legacy = r#"{"decision":"allow","message":"{\"answers\":{\"Languages\":{\"selected\":[\"Rust\"]}}}","interrupt":false}"#;
        let decoded = decode_permission_response(legacy);

        assert_eq!(
            decoded.response_type,
            crate::messages::PermissionResponseType::Allowed
        );
        assert_eq!(
            decoded.message.as_deref(),
            Some(r#"{"answers":{"Languages":{"selected":["Rust"]}}}"#),
            "the legacy escaped-JSON message decodes back intact"
        );
        assert!(!decoded.cancel_turn);
        // Legacy events predate the `auto` key → defaults to false.
        assert!(!decoded.auto_accepted);
    }

    #[test]
    fn test_build_session_state_event() {
        let sk = test_secret_key();

        let event = build_session_state_event(
            "sess-state-test",
            "Fix the login bug",
            Some("My Custom Title"),
            "/tmp/project",
            "working",
            Some("needs_input"),
            "my-laptop",
            "/home/testuser",
            "claude",
            "plan",
            None,
            None,
            1_770_000_000,
            &sk,
        )
        .unwrap();

        assert_eq!(event.kind, AI_SESSION_STATE_KIND);

        let json = &event.note_json;
        // Kind 31988 (parameterized replaceable)
        assert!(json.contains("31988"));
        // Has d tag for replacement
        assert!(json.contains(r#""d","sess-state-test"#));
        // Has discoverability tags
        assert!(json.contains(r#""t","ai-session-state"#));
        assert!(json.contains(r#""t","ai-conversation"#));
        // Content has state fields
        assert!(json.contains("Fix the login bug"));
        assert!(json.contains("working"));
        assert!(json.contains("/tmp/project"));
        assert!(json.contains(r#""hostname","my-laptop"#));
        assert!(json.contains(r#""backend","claude"#));
        assert!(json.contains(r#""permission-mode","plan"#));
        // created_at is the caller-supplied value (monotonic per session)
        assert!(json.contains(r#""created_at":1770000000"#), "json: {json}");
    }

    #[test]
    fn spawn_command_has_no_resume_tags() {
        let sk = test_secret_key();
        let event =
            build_spawn_command_event("host-a", "/tmp/proj", "claude", "spawn-1", None, &sk)
                .unwrap();

        assert_eq!(event.kind, AI_SESSION_COMMAND_KIND);
        let json = &event.note_json;
        assert!(json.contains(r#""command","spawn_session"#), "json: {json}");
        assert!(json.contains(r#""target_host","host-a"#));
        assert!(json.contains(r#""cwd","/tmp/proj"#));
        assert!(json.contains(r#""backend","claude"#));
        assert!(json.contains(r#""spawn_id","spawn-1"#));
        // A plain spawn never carries resume identity.
        assert!(!json.contains("resume_session_id"), "json: {json}");
        assert!(!json.contains(r#""session_id"#), "json: {json}");
    }

    #[test]
    fn resume_command_carries_target_and_cli_session() {
        let sk = test_secret_key();
        let resume = ResumeSpawn {
            target_session_id: "d-tag-of-dead-session",
            cli_session_id: "claude-cli-uuid",
        };
        let event = build_spawn_command_event(
            "host-a",
            "/tmp/proj",
            "claude",
            "spawn-2",
            Some(&resume),
            &sk,
        )
        .unwrap();

        let json = &event.note_json;
        // The discriminator flips so the host takes its resume branch.
        assert!(
            json.contains(r#""command","resume_session"#),
            "json: {json}"
        );
        // The session to revive + the id for `claude --resume`.
        assert!(
            json.contains(r#""session_id","d-tag-of-dead-session"#),
            "json: {json}"
        );
        assert!(
            json.contains(r#""resume_session_id","claude-cli-uuid"#),
            "json: {json}"
        );
    }

    /// Monotonic per-session `created_at`: when wall-clock hasn't advanced past
    /// the last published state event, the next one still gets a strictly
    /// greater timestamp so the newest revision always wins replaceable
    /// resolution and the receive-side guard (the "NeedsInput floats" fix).
    #[test]
    fn next_state_created_at_is_strictly_monotonic() {
        // Same second as last publish -> advance past it.
        assert_eq!(next_state_created_at(100, 100), 101);
        assert_eq!(next_state_created_at(100, 105), 106);
        // Wall-clock has moved on -> use real time.
        assert_eq!(next_state_created_at(200, 105), 200);
        // A burst within one second keeps strictly increasing.
        let mut last = 0;
        let mut prev = 0;
        for _ in 0..5 {
            last = next_state_created_at(500, last);
            assert!(last > prev, "must strictly increase: {prev} -> {last}");
            prev = last;
        }
    }

    #[test]
    fn test_wrap_pns() {
        let sk = test_secret_key();
        let pns_keys = enostr::pns::derive_pns_keys(&sk);

        let inner = r#"{"kind":1988,"content":"hello","tags":[],"created_at":0,"pubkey":"abc","id":"def","sig":"ghi"}"#;
        let wrapped = wrap_pns(inner, &pns_keys).unwrap();

        // Outer event should be kind 1080
        assert!(wrapped.contains("1080"));
        // Should NOT contain the plaintext inner content
        assert!(!wrapped.contains("hello"));
        // Should be valid JSON
        assert!(serde_json::from_str::<serde_json::Value>(&wrapped).is_ok());
    }

    /// Verify that permission_request events participate in the seq counter,
    /// producing correct ordering when interleaved with tool_call events.
    #[test]
    fn test_permission_request_seq_interleaves_with_tool_calls() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();

        // Build a tool_call event (simulating an assistant message with a tool use)
        let tool_call_line = JsonlLine::parse(
            r#"{"type":"assistant","uuid":"u1","sessionId":"seq-interleave","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}"#,
        ).unwrap();
        let tool_events = build_events(&tool_call_line, &mut threading, &sk).unwrap();
        // tool_call event should have seq=0
        assert!(
            tool_events[0].note_json.contains(r#""seq","0"#),
            "tool_call should have seq=0"
        );
        assert_eq!(threading.seq(), 1);

        // Build a permission_request event (should get seq=1)
        let perm_id = uuid::Uuid::new_v4();
        let tool_input = serde_json::json!({"command": "rm -rf /tmp/test"});
        let perm_event = build_permission_request_event(
            &perm_id,
            "Bash",
            &tool_input,
            "seq-interleave",
            &mut threading,
            &sk,
        )
        .unwrap();
        assert!(
            perm_event.note_json.contains(r#""seq","1"#),
            "permission_request should have seq=1"
        );
        assert_eq!(threading.seq(), 2);

        // Build a tool_result event (should get seq=2)
        let tool_result_line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u2","parentUuid":"u1","sessionId":"seq-interleave","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"file1.txt\nfile2.txt"}]}}"#,
        ).unwrap();
        let result_events = build_events(&tool_result_line, &mut threading, &sk).unwrap();
        assert!(
            result_events[0].note_json.contains(r#""seq","2"#),
            "tool_result should have seq=2"
        );
        assert_eq!(threading.seq(), 3);
    }

    /// Verify that events with the same created_at but different seq tags
    /// are sorted correctly, simulating the mobile sync scenario.
    #[tokio::test]
    async fn test_reconstruction_ordering_with_permission_requests() {
        use nostrdb::{IngestMetadata, Ndb, Transaction};
        use tempfile::TempDir;

        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id = "ordering-test";

        // Build events: user → tool_call → permission_request → tool_result
        let user_line = JsonlLine::parse(
            &format!(
                r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{}","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{{"role":"user","content":"run a command"}}}}"#,
                session_id
            ),
        ).unwrap();
        let user_events = build_events(&user_line, &mut threading, &sk).unwrap();

        let tool_call_line = JsonlLine::parse(
            &format!(
                r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"{}","timestamp":"2026-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{{"type":"tool_use","id":"toolu_1","name":"Bash","input":{{"command":"rm -rf /tmp/test"}}}}]}}}}"#,
                session_id
            ),
        ).unwrap();
        let tool_call_events = build_events(&tool_call_line, &mut threading, &sk).unwrap();

        let perm_id = uuid::Uuid::new_v4();
        let tool_input = serde_json::json!({"command": "rm -rf /tmp/test"});
        let perm_event = build_permission_request_event(
            &perm_id,
            "Bash",
            &tool_input,
            session_id,
            &mut threading,
            &sk,
        )
        .unwrap();

        // Collect all kind-1988 events
        let mut all_events = Vec::new();
        all_events.extend(
            user_events
                .iter()
                .filter(|e| e.kind == AI_CONVERSATION_KIND),
        );
        all_events.push(&perm_event); // permission_request
        all_events.extend(
            tool_call_events
                .iter()
                .filter(|e| e.kind == AI_CONVERSATION_KIND),
        );

        // Ingest events into ndb in REVERSED order (simulating relay out-of-order delivery)
        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();

        let filter = nostrdb::Filter::new()
            .kinds([AI_CONVERSATION_KIND as u64])
            .build();

        // Ingest in reverse to simulate out-of-order relay delivery
        for event in all_events.iter().rev() {
            let sub_id = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _keys = ndb.wait_for_notes(sub_id, 1).await.unwrap();
        }

        // Query and sort the same way session_loader does: (created_at, seq)
        let txn = Transaction::new(&ndb).unwrap();
        let results = ndb.query(&txn, &[filter], 100).unwrap();
        let mut notes: Vec<_> = results
            .iter()
            .filter_map(|qr| ndb.get_note_by_key(&txn, qr.note_key).ok())
            .collect();

        notes.sort_by_key(|note| {
            let seq = get_tag_value(note, "seq")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(u32::MAX);
            (note.created_at(), seq)
        });

        // Extract roles in sorted order
        let roles: Vec<_> = notes
            .iter()
            .filter_map(|n| get_tag_value(n, "role"))
            .collect();

        // Single-block assistant tool_use keeps role "assistant" (split only
        // happens with multiple content blocks). The key invariant is that
        // permission_request comes AFTER the assistant/tool_call event.
        assert_eq!(
            roles,
            vec!["user", "assistant", "permission_request"],
            "permission_request must come after assistant tool_call, got: {:?}",
            roles
        );
    }
}
