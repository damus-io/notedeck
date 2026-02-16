//! Load a previous session's conversation from nostr events in ndb.
//!
//! Queries for kind-1988 events with a matching `d` tag (session ID),
//! orders them by created_at, and converts them into `Message` variants
//! for populating the chat UI.

use crate::messages::{AssistantMessage, ToolResult};
use crate::session_events::{get_tag_value, AI_CONVERSATION_KIND};
use crate::Message;
use nostrdb::{Filter, Ndb, Transaction};

/// Load conversation messages from ndb for a given session ID.
///
/// Returns messages in chronological order, suitable for populating
/// `ChatSession.chat` before streaming begins.
pub fn load_session_messages(ndb: &Ndb, txn: &Transaction, session_id: &str) -> Vec<Message> {
    let filter = Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .tags([session_id], 'd')
        .limit(10000)
        .build();

    let results = match ndb.query(txn, &[filter], 10000) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    // Collect notes with their created_at for sorting
    let mut notes: Vec<_> = results
        .iter()
        .filter_map(|qr| ndb.get_note_by_key(txn, qr.note_key).ok())
        .collect();

    // Sort by created_at (chronological order)
    notes.sort_by_key(|note| note.created_at());

    let mut messages = Vec::new();
    for note in &notes {
        let content = note.content();
        let role = get_tag_value(note, "role");

        let msg = match role {
            Some("user") => Some(Message::User(content.to_string())),
            Some("assistant") => Some(Message::Assistant(AssistantMessage::from_text(
                content.to_string(),
            ))),
            Some("tool_call") => {
                // Tool calls are displayed as assistant messages in the UI
                Some(Message::Assistant(AssistantMessage::from_text(
                    content.to_string(),
                )))
            }
            Some("tool_result") => {
                // Extract tool name from content if possible
                // Content format is the tool output text
                let tool_name = "tool".to_string();
                let summary = truncate(content, 100);
                Some(Message::ToolResult(ToolResult { tool_name, summary }))
            }
            // Skip progress, queue-operation, file-history-snapshot for UI
            _ => None,
        };

        if let Some(msg) = msg {
            messages.push(msg);
        }
    }

    messages
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}
