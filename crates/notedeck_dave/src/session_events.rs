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

/// A built nostr event ready for ingestion, with its note ID.
#[derive(Debug)]
pub struct BuiltEvent {
    /// The full JSON string: `["EVENT", {…}]`
    pub json: String,
    /// The 32-byte note ID (from the signed event).
    pub note_id: [u8; 32],
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
        }
    }

    /// The current sequence number.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Record a built event's note ID, associated with a JSONL uuid.
    fn record(&mut self, uuid: Option<&str>, note_id: [u8; 32]) {
        if self.root_note_id.is_none() {
            self.root_note_id = Some(note_id);
        }
        if let Some(uuid) = uuid {
            self.uuid_to_note_id.insert(uuid.to_string(), note_id);
        }
        self.last_note_id = Some(note_id);
        self.seq += 1;
    }
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

    if should_split {
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
                line,
                &content,
                role,
                Some((i, total)),
                tool_id,
                threading,
                secret_key,
            )?;
            threading.record(line.uuid(), event.note_id);
            events.push(event);
        }
        Ok(events)
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
            line,
            &content,
            role,
            None,
            tool_id.as_deref(),
            threading,
            secret_key,
        )?;
        threading.record(line.uuid(), event.note_id);
        Ok(vec![event])
    }
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

/// Build a single nostr event from a JSONL line.
///
/// `split_index`: `Some((i, total))` when this event is part of a split
/// assistant message. Only the first event in a split group gets source-data.
///
/// `tool_id`: The tool use/result ID for tool_call and tool_result events.
fn build_single_event(
    line: &JsonlLine,
    content: &str,
    role: &str,
    split_index: Option<(usize, usize)>,
    tool_id: Option<&str>,
    threading: &ThreadingState,
    secret_key: &[u8; 32],
) -> Result<BuiltEvent, EventBuildError> {
    let mut builder = NoteBuilder::new()
        .kind(AI_CONVERSATION_KIND)
        .content(content)
        .options(NoteBuildOptions::default());

    // Set timestamp from JSONL
    if let Some(ts) = line.timestamp_secs() {
        builder = builder.created_at(ts);
    }

    // -- Session identity tags --
    if let Some(session_id) = line.session_id() {
        builder = builder.start_tag().tag_str("d").tag_str(session_id);
    }
    if let Some(slug) = line.slug() {
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
    builder = builder.start_tag().tag_str("source").tag_str("claude-code");

    if let Some(version) = line.version() {
        builder = builder
            .start_tag()
            .tag_str("source-version")
            .tag_str(version);
    }

    builder = builder.start_tag().tag_str("role").tag_str(role);

    // Model tag (for assistant messages)
    if let Some(msg) = line.message() {
        if let Some(model) = msg.model() {
            builder = builder.start_tag().tag_str("model").tag_str(model);
        }
    }

    if let Some(line_type) = line.line_type() {
        builder = builder.start_tag().tag_str("turn-type").tag_str(line_type);
    }

    // -- CWD tag --
    if let Some(cwd) = line.cwd() {
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

    // -- Source data (lossless) --
    // Only include source-data on non-split events or first event of a split group.
    // Store raw JSON verbatim (no path normalization).
    let include_source_data = match split_index {
        Some((i, _)) => i == 0,
        None => true,
    };
    if include_source_data {
        let raw_json = line.to_json();
        builder = builder
            .start_tag()
            .tag_str("source-data")
            .tag_str(&raw_json);
    }

    // Sign and build
    let note = builder
        .sign(secret_key)
        .build()
        .ok_or_else(|| EventBuildError::Build("NoteBuilder::build returned None".to_string()))?;

    let note_id: [u8; 32] = *note.id();

    let event = enostr::ClientMessage::event(&note)
        .map_err(|e| EventBuildError::Serialize(format!("{:?}", e)))?;

    let json = event
        .to_json()
        .map_err(|e| EventBuildError::Serialize(format!("{:?}", e)))?;

    Ok(BuiltEvent { json, note_id })
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

        assert_eq!(events.len(), 1);
        assert!(threading.root_note_id.is_some());
        assert_eq!(threading.root_note_id, Some(events[0].note_id));

        // Verify the JSON contains our kind and tags
        let json = &events[0].json;
        assert!(json.contains("1988"));
        assert!(json.contains("source"));
        assert!(json.contains("claude-code"));
        assert!(json.contains("role"));
        assert!(json.contains("user"));
        assert!(json.contains("source-data"));
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
        assert_eq!(events.len(), 1);

        let json = &events[0].json;
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

        // Should produce 2 events: one text, one tool_call
        assert_eq!(events.len(), 2);

        // Both should have unique note IDs
        assert_ne!(events[0].note_id, events[1].note_id);
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

        assert_eq!(all_events.len(), 3);

        // First event should be root (no e tags)
        // Subsequent events should reference root + previous
        // We can't easily inspect the binary note, but we can verify
        // the JSON contains "root" and "reply" markers
        assert!(!all_events[0].json.contains("root"));
        assert!(all_events[1].json.contains("root"));
        assert!(all_events[1].json.contains("reply"));
        assert!(all_events[2].json.contains("root"));
        assert!(all_events[2].json.contains("reply"));
    }

    #[test]
    fn test_source_data_preserves_raw_json() {
        let line = JsonlLine::parse(
            r#"{"type":"user","uuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:00Z","cwd":"/Users/jb55/dev/notedeck","version":"2.0.64","message":{"role":"user","content":"check /Users/jb55/dev/notedeck/src/main.rs"}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();

        let json = &events[0].json;
        assert!(json.contains("source-data"));
        // Raw paths should be preserved (no normalization)
        assert!(json.contains("/Users/jb55/dev/notedeck"));
    }

    #[test]
    fn test_queue_operation_event() {
        let line = JsonlLine::parse(
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-02-09T20:43:35.669Z","sessionId":"sess1"}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        assert_eq!(events.len(), 1);

        let json = &events[0].json;
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
        assert_eq!(events.len(), 1);
        assert_eq!(threading.seq(), 1);
        // First event should have seq=0
        assert!(events[0].json.contains(r#""seq","0"#));

        let line = JsonlLine::parse(lines[1]).unwrap();
        let events = build_events(&line, &mut threading, &sk).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(threading.seq(), 2);
        // Second event should have seq=1
        assert!(events[0].json.contains(r#""seq","1"#));
    }

    #[test]
    fn test_split_tags_and_source_data() {
        let line = JsonlLine::parse(
            r#"{"type":"assistant","uuid":"u3","sessionId":"sess1","timestamp":"2026-02-09T20:00:00Z","cwd":"/tmp","version":"2.0.64","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/test.rs"}}]}}"#,
        )
        .unwrap();

        let mut threading = ThreadingState::new();
        let events = build_events(&line, &mut threading, &test_secret_key()).unwrap();
        assert_eq!(events.len(), 2);

        // First event (text): split 0/2, has source-data
        assert!(events[0].json.contains(r#""split","0/2"#));
        assert!(events[0].json.contains("source-data"));

        // Second event (tool_call): split 1/2, NO source-data, has tool-id
        assert!(events[1].json.contains(r#""split","1/2"#));
        assert!(!events[1].json.contains("source-data"));
        assert!(events[1].json.contains(r#""tool-id","t1"#));
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
            .json
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
        assert_eq!(events.len(), 1);
        assert!(events[0].json.contains(r#""tool-id","toolu_abc"#));
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
            .kinds([AI_CONVERSATION_KIND as u64])
            .build();

        for line_str in &jsonl_lines {
            let line = JsonlLine::parse(line_str).unwrap();
            let events = build_events(&line, &mut threading, &sk).unwrap();
            for event in &events {
                let sub_id = ndb.subscribe(&[filter.clone()]).unwrap();
                ndb.process_event_with(&event.json, IngestMetadata::new().client(true))
                    .expect("ingest failed");
                let _keys = ndb.wait_for_notes(sub_id, 1).await.unwrap();
                total_events += 1;
            }
        }

        // The split assistant message (line 3) produces 2 events,
        // others produce 1 each = 4 + 2 = 6
        assert_eq!(total_events, 6);

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
}
