//! Convert parsed JSONL lines into kind-1988 nostr events.
//!
//! Each JSONL line becomes one or more nostr events. Assistant messages with
//! mixed content (text + tool_use blocks) are split into separate events.
//! Events are threaded using NIP-10 `e` tags with root/reply markers.

use crate::session_jsonl::{self, ContentBlock, JsonlLine};
use nostrdb::{NoteBuildOptions, NoteBuilder};
use std::collections::HashMap;

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

    /// Seed threading state from existing events (e.g. loaded from ndb).
    ///
    /// Sets root and last note IDs so that subsequent live events
    /// thread correctly as replies to the existing conversation.
    pub fn seed(&mut self, root_note_id: [u8; 32], last_note_id: [u8; 32], event_count: u32) {
        self.root_note_id = Some(root_note_id);
        self.last_note_id = Some(last_note_id);
        self.seq = event_count;
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

            let event = build_single_event(
                Some(line),
                &content,
                role,
                "claude-code",
                Some((i, total)),
                tool_id,
                session_id.as_deref(),
                None,
                timestamp,
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

        // Extract tool_id from single-block messages
        let tool_id = msg.as_ref().and_then(|m| {
            let blocks = m.content_blocks();
            if blocks.len() == 1 {
                match &blocks[0] {
                    ContentBlock::ToolUse { id, .. } => Some(id.to_string()),
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.to_string()),
                    _ => None,
                }
            } else {
                None
            }
        });

        let event = build_single_event(
            Some(line),
            &content,
            role,
            "claude-code",
            None,
            tool_id.as_deref(),
            session_id.as_deref(),
            None,
            timestamp,
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

    let mut builder = NoteBuilder::new()
        .kind(AI_SOURCE_DATA_KIND)
        .content("")
        .options(NoteBuildOptions::default());

    if let Some(ts) = timestamp {
        builder = builder.created_at(ts);
    }

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
        kind: AI_SOURCE_DATA_KIND,
    })
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
fn build_single_event(
    line: Option<&JsonlLine>,
    content: &str,
    role: &str,
    source: &str,
    split_index: Option<(usize, usize)>,
    tool_id: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
    timestamp: Option<u64>,
    threading: &ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = NoteBuilder::new()
        .kind(AI_CONVERSATION_KIND)
        .content(content)
        .options(NoteBuildOptions::default());

    if let Some(ts) = timestamp {
        builder = builder.created_at(ts);
    }

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

    // -- Sequence number (monotonic, for unambiguous ordering) --
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

    // -- Discoverability --
    builder = builder.start_tag().tag_str("t").tag_str("ai-conversation");

    // Sign and build
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
        kind: AI_CONVERSATION_KIND,
    })
}

/// Build a kind-1988 event for a live conversation message.
///
/// Unlike `build_events()` which works from JSONL lines, this builds directly
/// from role + content strings. No kind-1989 source-data events are created.
///
/// Calls `threading.record()` internally.
pub fn build_live_event(
    content: &str,
    role: &str,
    session_id: &str,
    cwd: Option<&str>,
    tool_id: Option<&str>,
    threading: &mut ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let event = build_single_event(
        None,
        content,
        role,
        "notedeck-dave",
        None,
        tool_id,
        Some(session_id),
        cwd,
        Some(now),
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
/// Does NOT participate in threading — permission events are ancillary.
pub fn build_permission_request_event(
    perm_id: &uuid::Uuid,
    tool_name: &str,
    tool_input: &serde_json::Value,
    session_id: &str,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Content is a JSON summary for display on remote clients
    let content = serde_json::json!({
        "tool_name": tool_name,
        "tool_input": tool_input,
    })
    .to_string();

    let perm_id_str = perm_id.to_string();

    let mut builder = NoteBuilder::new()
        .kind(AI_CONVERSATION_KIND)
        .content(&content)
        .options(NoteBuildOptions::default())
        .created_at(now);

    // Session identity
    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Permission-specific tags
    builder = builder
        .start_tag()
        .tag_str("perm-id")
        .tag_str(&perm_id_str);
    builder = builder
        .start_tag()
        .tag_str("tool-name")
        .tag_str(tool_name);
    builder = builder
        .start_tag()
        .tag_str("role")
        .tag_str("permission_request");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    // Discoverability
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-conversation");
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-permission");

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
        kind: AI_CONVERSATION_KIND,
    })
}

/// Build a kind-1988 permission response event.
///
/// Published by remote clients (phone) to allow/deny a permission request.
/// The desktop subscribes for these and routes them through the existing
/// oneshot channel, racing with the local UI.
///
/// Tags include `perm-id` (matching the request), `e` tag linking to the
/// request event, and `t: ai-permission` for filtering.
pub fn build_permission_response_event(
    perm_id: &uuid::Uuid,
    request_note_id: &[u8; 32],
    allowed: bool,
    message: Option<&str>,
    session_id: &str,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let content = serde_json::json!({
        "decision": if allowed { "allow" } else { "deny" },
        "message": message.unwrap_or(""),
    })
    .to_string();

    let perm_id_str = perm_id.to_string();

    let mut builder = NoteBuilder::new()
        .kind(AI_CONVERSATION_KIND)
        .content(&content)
        .options(NoteBuildOptions::default())
        .created_at(now);

    // Session identity
    builder = builder.start_tag().tag_str("d").tag_str(session_id);

    // Link to the request event
    builder = builder
        .start_tag()
        .tag_str("e")
        .tag_id(request_note_id);

    // Permission-specific tags
    builder = builder
        .start_tag()
        .tag_str("perm-id")
        .tag_str(&perm_id_str);
    builder = builder
        .start_tag()
        .tag_str("role")
        .tag_str("permission_response");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

    // Discoverability
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-conversation");
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-permission");

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
        kind: AI_CONVERSATION_KIND,
    })
}

/// Build a kind-31988 session state event (parameterized replaceable).
///
/// Published on every status change so remote clients and startup restore
/// can discover active sessions. nostrdb auto-replaces older versions
/// with same (kind, pubkey, d-tag).
pub fn build_session_state_event(
    claude_session_id: &str,
    title: &str,
    cwd: &str,
    status: &str,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let content = serde_json::json!({
        "claude_session_id": claude_session_id,
        "title": title,
        "cwd": cwd,
        "status": status,
        "last_active": now,
    })
    .to_string();

    let mut builder = NoteBuilder::new()
        .kind(AI_SESSION_STATE_KIND)
        .content(&content)
        .options(NoteBuildOptions::default())
        .created_at(now);

    // Session identity (makes this a parameterized replaceable event)
    builder = builder.start_tag().tag_str("d").tag_str(claude_session_id);

    // Discoverability
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-session-state");
    builder = builder
        .start_tag()
        .tag_str("t")
        .tag_str("ai-conversation");
    builder = builder
        .start_tag()
        .tag_str("source")
        .tag_str("notedeck-dave");

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
        kind: AI_SESSION_STATE_KIND,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let sd_event = events.iter().find(|e| e.kind == AI_SOURCE_DATA_KIND).unwrap();
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
        let lines = vec![
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
        use nostrdb::{Config, IngestMetadata, Ndb, Transaction};
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
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &Config::new()).unwrap();

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
                let sub_id = ndb.subscribe(&[filter.clone()]).unwrap();
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
        let lines = vec![
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

        let event = build_permission_request_event(
            &perm_id,
            "Bash",
            &tool_input,
            "sess-perm-test",
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
        // Has discoverability tags
        assert!(json.contains(r#""t","ai-conversation"#));
        assert!(json.contains(r#""t","ai-permission"#));
        // Content has tool info
        assert!(json.contains("rm -rf"));
    }

    #[test]
    fn test_build_permission_response_event() {
        let perm_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_note_id = [42u8; 32];
        let sk = test_secret_key();

        // Test allow response
        let event = build_permission_response_event(
            &perm_id,
            &request_note_id,
            true,
            Some("looks safe"),
            "sess-perm-test",
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
    }

    #[test]
    fn test_permission_response_deny() {
        let perm_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let request_note_id = [42u8; 32];
        let sk = test_secret_key();

        let event = build_permission_response_event(
            &perm_id,
            &request_note_id,
            false,
            Some("too dangerous"),
            "sess-perm-test",
            &sk,
        )
        .unwrap();

        let json = &event.note_json;
        assert!(json.contains("deny"));
        assert!(json.contains("too dangerous"));
    }

    #[test]
    fn test_build_session_state_event() {
        let sk = test_secret_key();

        let event = build_session_state_event(
            "sess-state-test",
            "Fix the login bug",
            "/tmp/project",
            "working",
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
    }
}
