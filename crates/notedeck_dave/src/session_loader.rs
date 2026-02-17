//! Load a previous session's conversation from nostr events in ndb.
//!
//! Queries for kind-1988 events with a matching `d` tag (session ID),
//! orders them by created_at, and converts them into `Message` variants
//! for populating the chat UI.

use crate::messages::{AssistantMessage, PermissionRequest, PermissionResponseType, ToolResult};
use crate::session_events::{get_tag_value, is_conversation_role, AI_CONVERSATION_KIND};
use crate::Message;
use nostrdb::{Filter, Ndb, Transaction};
use std::collections::HashSet;

/// Result of loading session messages, including threading info for live events.
pub struct LoadedSession {
    pub messages: Vec<Message>,
    /// Root note ID of the conversation (first event chronologically).
    pub root_note_id: Option<[u8; 32]>,
    /// Last note ID of the conversation (most recent event).
    pub last_note_id: Option<[u8; 32]>,
    /// Total number of events found.
    pub event_count: u32,
    /// Permission IDs that already have response events.
    pub responded_perm_ids: HashSet<uuid::Uuid>,
    /// Map of perm_id -> note_id for permission request events.
    /// Used by remote sessions to link responses back to requests.
    pub perm_request_note_ids: std::collections::HashMap<uuid::Uuid, [u8; 32]>,
    /// All note IDs found, for seeding dedup in live polling.
    pub note_ids: HashSet<[u8; 32]>,
}

/// Load conversation messages from ndb for a given session ID.
///
/// Returns messages in chronological order, suitable for populating
/// `ChatSession.chat` before streaming begins. Also returns note IDs
/// for seeding live threading state.
pub fn load_session_messages(ndb: &Ndb, txn: &Transaction, session_id: &str) -> LoadedSession {
    let filter = Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .tags([session_id], 'd')
        .limit(10000)
        .build();

    let results = match ndb.query(txn, &[filter], 10000) {
        Ok(r) => r,
        Err(_) => {
            return LoadedSession {
                messages: vec![],
                root_note_id: None,
                last_note_id: None,
                event_count: 0,
                responded_perm_ids: HashSet::new(),
                perm_request_note_ids: std::collections::HashMap::new(),
                note_ids: HashSet::new(),
            }
        }
    };

    // Collect notes with their created_at for sorting
    let mut notes: Vec<_> = results
        .iter()
        .filter_map(|qr| ndb.get_note_by_key(txn, qr.note_key).ok())
        .collect();

    // Sort by created_at (chronological order)
    notes.sort_by_key(|note| note.created_at());

    let event_count = notes.len() as u32;
    let note_ids: HashSet<[u8; 32]> = notes.iter().map(|n| *n.id()).collect();

    // Find the first conversation note (skip metadata like queue-operation)
    // so the threading root is a real message.
    let root_note_id = notes
        .iter()
        .find(|n| {
            get_tag_value(n, "role")
                .map(is_conversation_role)
                .unwrap_or(false)
        })
        .map(|n| *n.id());
    let last_note_id = notes.last().map(|n| *n.id());

    // First pass: collect responded perm IDs and request note IDs
    let mut responded_perm_ids: HashSet<uuid::Uuid> = HashSet::new();
    let mut perm_request_note_ids: std::collections::HashMap<uuid::Uuid, [u8; 32]> =
        std::collections::HashMap::new();

    for note in &notes {
        let role = get_tag_value(note, "role");
        if role == Some("permission_response") {
            if let Some(perm_id_str) = get_tag_value(note, "perm-id") {
                if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                    responded_perm_ids.insert(perm_id);
                }
            }
        } else if role == Some("permission_request") {
            if let Some(perm_id_str) = get_tag_value(note, "perm-id") {
                if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                    perm_request_note_ids.insert(perm_id, *note.id());
                }
            }
        }
    }

    // Second pass: build messages
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
            Some("permission_request") => {
                if let Ok(content_json) = serde_json::from_str::<serde_json::Value>(content) {
                    let tool_name = content_json["tool_name"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string();
                    let tool_input = content_json
                        .get("tool_input")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let perm_id = get_tag_value(note, "perm-id")
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                        .unwrap_or_else(uuid::Uuid::new_v4);

                    let response = if responded_perm_ids.contains(&perm_id) {
                        Some(PermissionResponseType::Allowed)
                    } else {
                        None
                    };

                    Some(Message::PermissionRequest(PermissionRequest {
                        id: perm_id,
                        tool_name,
                        tool_input,
                        response,
                        answer_summary: None,
                        cached_plan: None,
                    }))
                } else {
                    None
                }
            }
            // Skip permission_response, progress, queue-operation, etc.
            _ => None,
        };

        if let Some(msg) = msg {
            messages.push(msg);
        }
    }

    LoadedSession {
        messages,
        root_note_id,
        last_note_id,
        event_count,
        responded_perm_ids,
        perm_request_note_ids,
        note_ids,
    }
}

/// A persisted session state from a kind-31988 event.
pub struct SessionState {
    pub claude_session_id: String,
    pub title: String,
    pub cwd: String,
    pub status: String,
}

/// Load all session states from kind-31988 events in ndb.
///
/// Returns one `SessionState` per unique session. Since these are
/// parameterized replaceable events, nostrdb keeps only the latest
/// version for each (kind, pubkey, d-tag) tuple.
pub fn load_session_states(ndb: &Ndb, txn: &Transaction) -> Vec<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let filter = Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .tags(["ai-session-state"], 't')
        .build();

    let results = match ndb.query(txn, &[filter], 100) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut states = Vec::new();
    for qr in &results {
        let Ok(note) = ndb.get_note_by_key(txn, qr.note_key) else {
            continue;
        };

        let content = note.content();
        let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
            continue;
        };

        let Some(claude_session_id) = json["claude_session_id"].as_str() else {
            continue;
        };
        let title = json["title"].as_str().unwrap_or("Untitled").to_string();
        let cwd = json["cwd"].as_str().unwrap_or("").to_string();
        let status = json["status"].as_str().unwrap_or("idle").to_string();

        states.push(SessionState {
            claude_session_id: claude_session_id.to_string(),
            title,
            cwd,
            status,
        });
    }

    states
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}
