//! Parse claude-code session JSONL lines with lossless round-trip support.
//!
//! Follows the `ProfileState` pattern from `enostr::profile` — wraps a
//! `serde_json::Value` with typed accessors so we can read the fields we
//! need for nostr event construction while preserving the raw JSON for
//! the `source-data` tag.

use serde_json::Value;

/// A single line from a claude-code session JSONL file.
///
/// Wraps the raw JSON value to preserve all fields for lossless round-trip
/// via the `source-data` nostr tag.
#[derive(Debug, Clone)]
pub struct JsonlLine(Value);

impl JsonlLine {
    /// Parse a JSONL line from a string.
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        let value: Value = serde_json::from_str(line)?;
        Ok(Self(value))
    }

    /// The raw JSON value.
    pub fn value(&self) -> &Value {
        &self.0
    }

    /// Serialize back to a JSON string (lossless).
    pub fn to_json(&self) -> String {
        // serde_json::to_string on a Value is infallible
        serde_json::to_string(&self.0).unwrap()
    }

    // -- Top-level field accessors --

    fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    /// The JSONL line type: "user", "assistant", "progress", "queue-operation",
    /// "file-history-snapshot"
    pub fn line_type(&self) -> Option<&str> {
        self.get_str("type")
    }

    pub fn uuid(&self) -> Option<&str> {
        self.get_str("uuid")
    }

    pub fn parent_uuid(&self) -> Option<&str> {
        self.get_str("parentUuid")
    }

    pub fn session_id(&self) -> Option<&str> {
        self.get_str("sessionId")
    }

    pub fn timestamp(&self) -> Option<&str> {
        self.get_str("timestamp")
    }

    /// Parse the timestamp as a unix timestamp (seconds).
    pub fn timestamp_secs(&self) -> Option<u64> {
        let ts_str = self.timestamp()?;
        let dt = chrono::DateTime::parse_from_rfc3339(ts_str).ok()?;
        Some(dt.timestamp() as u64)
    }

    pub fn cwd(&self) -> Option<&str> {
        self.get_str("cwd")
    }

    pub fn git_branch(&self) -> Option<&str> {
        self.get_str("gitBranch")
    }

    pub fn version(&self) -> Option<&str> {
        self.get_str("version")
    }

    pub fn slug(&self) -> Option<&str> {
        self.get_str("slug")
    }

    /// The `message` object, if present.
    pub fn message(&self) -> Option<JsonlMessage<'_>> {
        self.0.get("message").map(JsonlMessage)
    }

    /// For queue-operation lines: the operation type ("dequeue", etc.)
    pub fn operation(&self) -> Option<&str> {
        self.get_str("operation")
    }

    /// Determine the role string for nostr event tagging.
    ///
    /// This maps the JSONL structure to the design spec's role values:
    /// user (text) → "user", user (tool_result) → "tool_result",
    /// assistant (text) → "assistant", assistant (tool_use) → "tool_call",
    /// progress → "progress", etc.
    pub fn role(&self) -> Option<&str> {
        match self.line_type()? {
            "user" => {
                // Check if the content is a tool_result array
                if let Some(msg) = self.message() {
                    if msg.has_tool_result_content() {
                        return Some("tool_result");
                    }
                }
                Some("user")
            }
            "assistant" => Some("assistant"),
            "progress" => Some("progress"),
            "queue-operation" => Some("queue-operation"),
            "file-history-snapshot" => Some("file-history-snapshot"),
            _ => None,
        }
    }
}

/// A borrowed view into the `message` object of a JSONL line.
#[derive(Debug, Clone, Copy)]
pub struct JsonlMessage<'a>(&'a Value);

impl<'a> JsonlMessage<'a> {
    fn get_str(&self, key: &str) -> Option<&'a str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    pub fn role(&self) -> Option<&'a str> {
        self.get_str("role")
    }

    pub fn model(&self) -> Option<&'a str> {
        self.get_str("model")
    }

    /// The raw content value — can be a string or an array of content blocks.
    pub fn content(&self) -> Option<&'a Value> {
        self.0.get("content")
    }

    /// Check if content contains tool_result blocks (user messages with tool results).
    pub fn has_tool_result_content(&self) -> bool {
        match self.content() {
            Some(Value::Array(arr)) => arr
                .iter()
                .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
            _ => false,
        }
    }

    /// Extract the content blocks as an iterator.
    pub fn content_blocks(&self) -> Vec<ContentBlock<'a>> {
        match self.content() {
            Some(Value::String(s)) => vec![ContentBlock::Text(s.as_str())],
            Some(Value::Array(arr)) => arr.iter().filter_map(ContentBlock::from_value).collect(),
            _ => vec![],
        }
    }

    /// Extract just the text from text blocks, concatenated.
    pub fn text_content(&self) -> Option<String> {
        let blocks = self.content_blocks();
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(*t),
                _ => None,
            })
            .collect();

        if texts.is_empty() {
            None
        } else {
            Some(texts.join(""))
        }
    }
}

/// A content block from an assistant or user message.
#[derive(Debug, Clone)]
pub enum ContentBlock<'a> {
    /// Plain text content.
    Text(&'a str),
    /// A tool use request (assistant → tool).
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a Value,
    },
    /// A tool result (tool → user message).
    ToolResult {
        tool_use_id: &'a str,
        content: &'a Value,
    },
}

impl<'a> ContentBlock<'a> {
    fn from_value(value: &'a Value) -> Option<Self> {
        let block_type = value.get("type")?.as_str()?;
        match block_type {
            "text" => {
                let text = value.get("text")?.as_str()?;
                Some(ContentBlock::Text(text))
            }
            "tool_use" => {
                let id = value.get("id")?.as_str()?;
                let name = value.get("name")?.as_str()?;
                let input = value.get("input")?;
                Some(ContentBlock::ToolUse { id, name, input })
            }
            "tool_result" => {
                let tool_use_id = value.get("tool_use_id")?.as_str()?;
                let content = value.get("content")?;
                Some(ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                })
            }
            _ => None,
        }
    }
}

/// Human-readable content extraction for the nostr event `content` field.
///
/// This produces the text that goes into the nostr event content,
/// suitable for rendering in any nostr client.
pub fn extract_display_content(line: &JsonlLine) -> String {
    match line.line_type() {
        Some("user") => {
            if let Some(msg) = line.message() {
                if msg.has_tool_result_content() {
                    // Tool result content — summarize
                    let blocks = msg.content_blocks();
                    let summaries: Vec<String> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolResult { content, .. } => match content {
                                Value::String(s) => {
                                    Some(truncate_str(s, 500))
                                }
                                _ => Some("[tool result]".to_string()),
                            },
                            _ => None,
                        })
                        .collect();
                    summaries.join("\n")
                } else if let Some(text) = msg.text_content() {
                    // Strip "Human: " prefix if present (claude-code adds it)
                    text.strip_prefix("Human: ").unwrap_or(&text).to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        Some("assistant") => {
            if let Some(msg) = line.message() {
                // For assistant messages, we'll produce content for each block.
                // The caller handles splitting into multiple events for mixed content.
                if let Some(text) = msg.text_content() {
                    text
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        Some("progress") => line
            .message()
            .and_then(|m| m.text_content())
            .unwrap_or_else(|| "[progress]".to_string()),
        Some("queue-operation") => {
            let op = line.operation().unwrap_or("unknown");
            format!("[queue: {}]", op)
        }
        Some("file-history-snapshot") => "[file history snapshot]".to_string(),
        _ => String::new(),
    }
}

/// Extract display content for a single content block (for assistant messages
/// that need to be split into multiple events).
pub fn display_content_for_block(block: &ContentBlock<'_>) -> String {
    match block {
        ContentBlock::Text(text) => text.to_string(),
        ContentBlock::ToolUse { name, input, .. } => {
            // Compact summary: tool name + truncated input
            let input_str = serde_json::to_string(input).unwrap_or_default();
            let input_preview = truncate_str(&input_str, 200);
            format!("Tool: {} {}", name, input_preview)
        }
        ContentBlock::ToolResult { content, .. } => match content {
            Value::String(s) => truncate_str(s, 500),
            _ => "[tool result]".to_string(),
        },
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_text_message() {
        let line = r#"{"parentUuid":null,"cwd":"/Users/jb55/dev/notedeck","sessionId":"abc-123","version":"2.0.64","gitBranch":"main","type":"user","message":{"role":"user","content":"Human: Hello world\n\n"},"uuid":"uuid-1","timestamp":"2026-02-09T20:43:35.675Z"}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        assert_eq!(parsed.line_type(), Some("user"));
        assert_eq!(parsed.uuid(), Some("uuid-1"));
        assert_eq!(parsed.parent_uuid(), None);
        assert_eq!(parsed.session_id(), Some("abc-123"));
        assert_eq!(parsed.cwd(), Some("/Users/jb55/dev/notedeck"));
        assert_eq!(parsed.version(), Some("2.0.64"));
        assert_eq!(parsed.role(), Some("user"));

        let msg = parsed.message().unwrap();
        assert_eq!(msg.role(), Some("user"));
        assert_eq!(msg.text_content(), Some("Human: Hello world\n\n".to_string()));

        let content = extract_display_content(&parsed);
        assert_eq!(content, "Hello world\n\n");
    }

    #[test]
    fn test_parse_assistant_text_message() {
        let line = r#"{"parentUuid":"uuid-1","cwd":"/Users/jb55/dev/notedeck","sessionId":"abc-123","version":"2.0.64","message":{"model":"claude-opus-4-5-20251101","role":"assistant","content":[{"type":"text","text":"I can help with that."}]},"type":"assistant","uuid":"uuid-2","timestamp":"2026-02-09T20:43:38.421Z"}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        assert_eq!(parsed.line_type(), Some("assistant"));
        assert_eq!(parsed.role(), Some("assistant"));

        let msg = parsed.message().unwrap();
        assert_eq!(msg.model(), Some("claude-opus-4-5-20251101"));
        assert_eq!(
            msg.text_content(),
            Some("I can help with that.".to_string())
        );
    }

    #[test]
    fn test_parse_assistant_tool_use() {
        let line = r#"{"parentUuid":"uuid-1","cwd":"/tmp","sessionId":"abc","version":"2.0.64","message":{"model":"claude-opus-4-5-20251101","role":"assistant","content":[{"type":"tool_use","id":"toolu_123","name":"Read","input":{"file_path":"/tmp/test.rs"}}]},"type":"assistant","uuid":"uuid-3","timestamp":"2026-02-09T20:43:38.421Z"}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        let msg = parsed.message().unwrap();
        let blocks = msg.content_blocks();
        assert_eq!(blocks.len(), 1);

        match &blocks[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(*id, "toolu_123");
                assert_eq!(*name, "Read");
                assert_eq!(input.get("file_path").unwrap().as_str(), Some("/tmp/test.rs"));
            }
            _ => panic!("expected ToolUse block"),
        }
    }

    #[test]
    fn test_parse_user_tool_result() {
        let line = r#"{"parentUuid":"uuid-3","cwd":"/tmp","sessionId":"abc","version":"2.0.64","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_123","type":"tool_result","content":"file contents here"}]},"uuid":"uuid-4","timestamp":"2026-02-09T20:43:38.476Z"}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        assert_eq!(parsed.role(), Some("tool_result"));

        let msg = parsed.message().unwrap();
        assert!(msg.has_tool_result_content());

        let blocks = msg.content_blocks();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => {
                assert_eq!(*tool_use_id, "toolu_123");
                assert_eq!(content.as_str(), Some("file contents here"));
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    #[test]
    fn test_parse_queue_operation() {
        let line = r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-02-09T20:43:35.669Z","sessionId":"abc-123"}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        assert_eq!(parsed.line_type(), Some("queue-operation"));
        assert_eq!(parsed.operation(), Some("dequeue"));
        assert_eq!(parsed.role(), Some("queue-operation"));

        let content = extract_display_content(&parsed);
        assert_eq!(content, "[queue: dequeue]");
    }

    #[test]
    fn test_lossless_roundtrip() {
        // The key property: parse → to_json should preserve all fields
        let original = r#"{"type":"user","uuid":"abc","parentUuid":null,"sessionId":"sess","timestamp":"2026-02-09T20:43:35.675Z","cwd":"/tmp","gitBranch":"main","version":"2.0.64","isSidechain":false,"userType":"external","message":{"role":"user","content":"hello"},"unknownField":"preserved"}"#;

        let parsed = JsonlLine::parse(original).unwrap();
        let roundtripped = parsed.to_json();

        // Parse both as Value to compare (field order may differ)
        let orig_val: Value = serde_json::from_str(original).unwrap();
        let rt_val: Value = serde_json::from_str(&roundtripped).unwrap();
        assert_eq!(orig_val, rt_val);
    }

    #[test]
    fn test_timestamp_secs() {
        let line = r#"{"type":"user","timestamp":"2026-02-09T20:43:35.675Z","sessionId":"abc"}"#;
        let parsed = JsonlLine::parse(line).unwrap();
        assert!(parsed.timestamp_secs().is_some());
    }

    #[test]
    fn test_mixed_assistant_content() {
        let line = r#"{"type":"assistant","uuid":"u1","sessionId":"s","timestamp":"2026-02-09T20:00:00Z","message":{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{"type":"text","text":"Here is what I found:"},{"type":"tool_use","id":"t1","name":"Glob","input":{"pattern":"**/*.rs"}}]}}"#;

        let parsed = JsonlLine::parse(line).unwrap();
        let msg = parsed.message().unwrap();
        let blocks = msg.content_blocks();
        assert_eq!(blocks.len(), 2);

        // First block is text
        assert!(matches!(blocks[0], ContentBlock::Text("Here is what I found:")));

        // Second block is tool use
        match &blocks[1] {
            ContentBlock::ToolUse { name, .. } => assert_eq!(*name, "Glob"),
            _ => panic!("expected ToolUse"),
        }

        // display_content_for_block should work on each
        assert_eq!(
            display_content_for_block(&blocks[0]),
            "Here is what I found:"
        );
        assert!(display_content_for_block(&blocks[1]).starts_with("Tool: Glob"));
    }
}
