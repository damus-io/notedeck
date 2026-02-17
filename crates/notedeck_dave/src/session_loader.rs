//! Load a previous session's conversation from nostr events in ndb.
//!
//! Queries for kind-1988 events with a matching `d` tag (session ID),
//! orders them by created_at, and converts them into `Message` variants
//! for populating the chat UI.

use crate::messages::{AssistantMessage, PermissionRequest, PermissionResponseType, ToolResult};
use crate::session_events::{get_tag_value, is_conversation_role, AI_CONVERSATION_KIND};
use crate::Message;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use std::collections::HashSet;

/// Query replaceable events via `ndb.fold`, deduplicating by `d` tag.
///
/// nostrdb doesn't deduplicate replaceable events internally, so multiple
/// revisions of the same (kind, pubkey, d-tag) tuple may exist. This
/// folds over all matching notes and keeps only the one with the highest
/// `created_at` for each unique `d` tag value.
///
/// Returns a Vec of `NoteKey`s for the winning notes (one per unique d-tag).
pub fn query_replaceable(
    ndb: &Ndb,
    txn: &Transaction,
    filters: &[Filter],
) -> Vec<NoteKey> {
    query_replaceable_filtered(ndb, txn, filters, |_| true)
}

/// Like `query_replaceable`, but with a predicate to filter notes.
///
/// The predicate is called on the latest revision of each d-tag group.
/// If it returns false, that d-tag is removed from results (even if an
/// older revision would have passed).
pub fn query_replaceable_filtered(
    ndb: &Ndb,
    txn: &Transaction,
    filters: &[Filter],
    predicate: impl Fn(&nostrdb::Note) -> bool,
) -> Vec<NoteKey> {
    // Fold: for each d-tag value, track (created_at, NoteKey) of the latest
    let best = ndb.fold(
        txn,
        filters,
        std::collections::HashMap::<String, (u64, NoteKey)>::new(),
        |mut acc, note| {
            let Some(d_tag) = get_tag_value(&note, "d") else {
                return acc;
            };

            let created_at = note.created_at() as u64;

            if let Some((existing_ts, _)) = acc.get(d_tag) {
                if created_at <= *existing_ts {
                    return acc;
                }
            }

            if predicate(&note) {
                acc.insert(d_tag.to_string(), (created_at, note.key().expect("note key")));
            } else {
                // Latest revision rejected — remove any older revision we kept
                acc.remove(d_tag);
            }

            acc
        },
    );

    match best {
        Ok(map) => map.into_values().map(|(_, key)| key).collect(),
        Err(_) => vec![],
    }
}

/// Result of loading session messages, including threading info for live events.
pub struct LoadedSession {
    pub messages: Vec<Message>,
    pub root_note_id: Option<[u8; 32]>,
    pub last_note_id: Option<[u8; 32]>,
    pub event_count: u32,
    /// Set of perm-id UUIDs that have already been responded to.
    /// Used by remote sessions to know which permission requests are already handled.
    pub responded_perm_ids: HashSet<uuid::Uuid>,
    /// Map of perm_id -> note_id for permission request events.
    /// Used by remote sessions to link responses back to requests.
    pub perm_request_note_ids: std::collections::HashMap<uuid::Uuid, [u8; 32]>,
    /// All note IDs found, for seeding dedup in live polling.
    pub note_ids: HashSet<[u8; 32]>,
}

/// Load conversation messages from ndb for a given session ID.
///
/// This queries for kind-1988 events with a `d` tag matching the session ID,
/// sorts them chronologically, and converts relevant roles into Messages.
pub fn load_session_messages(ndb: &Ndb, txn: &Transaction, session_id: &str) -> LoadedSession {
    let filter = Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .tags([session_id], 'd')
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

    // First pass: collect responded permission IDs and perm request note IDs
    let mut responded_perm_ids = HashSet::new();
    let mut perm_request_note_ids = std::collections::HashMap::new();
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

    // Second pass: convert to messages
    let mut messages = Vec::new();
    for note in &notes {
        let content = note.content();
        let role = get_tag_value(note, "role");

        let msg = match role {
            Some("user") => Some(Message::User(content.to_string())),
            Some("assistant") | Some("tool_call") => Some(Message::Assistant(
                AssistantMessage::from_text(content.to_string()),
            )),
            Some("tool_result") => {
                let summary = truncate(content, 200);
                Some(Message::ToolResult(ToolResult {
                    tool_name: get_tag_value(note, "tool-name")
                        .unwrap_or("tool")
                        .to_string(),
                    summary,
                }))
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
/// Uses `query_replaceable_filtered` to deduplicate by d-tag, keeping
/// only the most recent non-deleted revision of each session state.
pub fn load_session_states(ndb: &Ndb, txn: &Transaction) -> Vec<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let filter = Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .tags(["ai-session-state"], 't')
        .build();

    let is_valid = |note: &nostrdb::Note| {
        // Skip deleted sessions
        if get_tag_value(note, "status") == Some("deleted") {
            return false;
        }
        // Skip old JSON-content format events
        if note.content().starts_with('{') {
            return false;
        }
        true
    };

    let note_keys = query_replaceable_filtered(ndb, txn, &[filter], is_valid);

    let mut states = Vec::new();
    for key in note_keys {
        let Ok(note) = ndb.get_note_by_key(txn, key) else {
            continue;
        };

        let Some(claude_session_id) = get_tag_value(&note, "d") else {
            continue;
        };

        states.push(SessionState {
            claude_session_id: claude_session_id.to_string(),
            title: get_tag_value(&note, "title").unwrap_or("Untitled").to_string(),
            cwd: get_tag_value(&note, "cwd").unwrap_or("").to_string(),
            status: get_tag_value(&note, "status").unwrap_or("idle").to_string(),
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
