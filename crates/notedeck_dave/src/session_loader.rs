//! Load a previous session's conversation from nostr events in ndb.
//!
//! Queries for kind-1988 events with a matching `d` tag (session ID),
//! orders them by their monotonic `seq` tag, and converts them into
//! `Message` variants for populating the chat UI.

use crate::messages::{AssistantMessage, ExecutedTool, PermissionRequest};
use crate::session::PermissionTracker;
use crate::session_events::{
    decode_permission_response, get_tag_value, is_conversation_role, AI_CONVERSATION_KIND,
};
use crate::tools::ToolResponse;
use crate::Message;
use nostrdb::{Filter, Ndb, NoteKey, Transaction};
use std::collections::{HashMap, HashSet};

/// Query replaceable events via `ndb.fold`, deduplicating by `d` tag.
///
/// nostrdb doesn't deduplicate replaceable events internally, so multiple
/// revisions of the same (kind, pubkey, d-tag) tuple may exist. This
/// folds over all matching notes and keeps only the one with the highest
/// `created_at` for each unique `d` tag value.
///
/// Returns a Vec of `NoteKey`s for the winning notes (one per unique d-tag).
pub fn query_replaceable(ndb: &Ndb, txn: &Transaction, filters: &[Filter]) -> Vec<NoteKey> {
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
    // Fold: for each d-tag value, track the latest created_at and optionally
    // a NoteKey (only if the latest revision passes the predicate).
    // Notes may arrive in any order from ndb.fold, so we always track the
    // highest timestamp and only keep a key when that revision is valid.
    let best = ndb.fold(
        txn,
        filters,
        std::collections::HashMap::<String, (u64, Option<NoteKey>)>::new(),
        |mut acc, note| {
            let Some(d_tag) = get_tag_value(&note, "d") else {
                return acc;
            };

            let created_at = note.created_at();

            if let Some((existing_ts, _)) = acc.get(d_tag) {
                if created_at <= *existing_ts {
                    return acc;
                }
            }

            let key = if predicate(&note) {
                Some(note.key().expect("note key"))
            } else {
                None
            };

            acc.insert(d_tag.to_string(), (created_at, key));
            acc
        },
    );

    match best {
        Ok(map) => map.into_values().filter_map(|(_, key)| key).collect(),
        Err(_) => vec![],
    }
}

/// Result of loading session messages, including threading info for live events.
pub struct LoadedSession {
    pub messages: Vec<Message>,
    pub root_note_id: Option<[u8; 32]>,
    pub last_note_id: Option<[u8; 32]>,
    pub event_count: u32,
    /// Permission state loaded from events (responded set + request note IDs).
    pub permissions: PermissionTracker,
    /// All note IDs found, for seeding dedup in live polling.
    pub note_ids: HashSet<[u8; 32]>,
}

/// Load conversation messages from ndb for a given session ID.
///
/// This queries for kind-1988 events with a `d` tag matching the session ID,
/// sorts them by `seq`, and converts relevant roles into Messages.
pub fn load_session_messages(ndb: &Ndb, txn: &Transaction, session_id: &str) -> LoadedSession {
    load_session_messages_with_author(ndb, txn, session_id, None)
}

/// Load conversation messages for one author-scoped Dave session.
pub fn load_session_messages_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
    session_id: &str,
) -> LoadedSession {
    load_session_messages_with_author(ndb, txn, session_id, Some(author))
}

fn load_session_messages_with_author(
    ndb: &Ndb,
    txn: &Transaction,
    session_id: &str,
    author: Option<&enostr::Pubkey>,
) -> LoadedSession {
    let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]);
    let filter = if let Some(author) = author {
        filter.authors([author.bytes()])
    } else {
        filter
    };
    let filter = filter.tags([session_id], 'd').build();

    let results = match ndb.query(txn, &[filter], 10000) {
        Ok(r) => r,
        Err(_) => {
            return LoadedSession {
                messages: vec![],
                root_note_id: None,
                last_note_id: None,
                event_count: 0,
                permissions: PermissionTracker::new(),
                note_ids: HashSet::new(),
            };
        }
    };

    // Collect notes with their created_at for sorting
    let mut notes: Vec<_> = results
        .iter()
        .filter_map(|qr| ndb.get_note_by_key(txn, qr.note_key).ok())
        .collect();

    // Sort by `seq` first, falling back to `created_at` as a tiebreaker.
    //
    // This query is scoped to a single session (`d` tag), and within one
    // session `seq` is a unique, monotonic counter assigned in event order —
    // it is the authoritative ordering (see `session_reconstructor`, which
    // rebuilds JSONL purely by `seq`). `created_at` is only second-resolution
    // and mixes backfilled JSONL timestamps with live `now_secs()` values, so
    // sorting by it first scrambles events when many arrive in the same second
    // (e.g. a synced backlog), which would float late events like a pending
    // permission request to the wrong position. Only fall back to `created_at`
    // for events missing a `seq` tag.
    notes.sort_by_key(|note| {
        let seq = get_tag_value(note, "seq")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(u32::MAX);
        (seq, note.created_at())
    });

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
    let mut permissions = PermissionTracker::new();
    for note in &notes {
        let role = get_tag_value(note, "role");
        if role == Some("permission_response") {
            if let Some(perm_id_str) = get_tag_value(note, "perm-id") {
                if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                    let (response_type, _, _) = decode_permission_response(note.content());
                    permissions.responded.insert(perm_id, response_type);
                }
            }
        } else if role == Some("permission_request") {
            if let Some(perm_id_str) = get_tag_value(note, "perm-id") {
                if let Ok(perm_id) = uuid::Uuid::parse_str(perm_id_str) {
                    permissions.request_note_ids.insert(perm_id, *note.id());
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
            Some("user") => Some(Message::User(content.to_string().into())),
            Some("assistant") | Some("tool_call") => Some(Message::Assistant(
                AssistantMessage::from_text(content.to_string()),
            )),
            Some("tool_result") => {
                let summary = truncate(content, 200);
                Some(Message::ToolResponse(ToolResponse::executed_tool(
                    ExecutedTool {
                        tool_name: get_tag_value(note, "tool-name")
                            .unwrap_or("tool")
                            .to_string(),
                        summary,
                        parent_task_id: None,
                        file_update: None,
                    },
                )))
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

                    let response = permissions.responded.get(&perm_id).copied();

                    Some(Message::PermissionRequest(PermissionRequest::new(
                        perm_id, tool_name, tool_input, None, response, None,
                    )))
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
        permissions,
        note_ids,
    }
}

/// A persisted session state from a kind-31988 event.
pub struct SessionState {
    pub claude_session_id: String,
    pub title: String,
    pub custom_title: Option<String>,
    pub cwd: String,
    pub status: String,
    pub indicator: Option<String>,
    pub hostname: String,
    pub home_dir: String,
    pub backend: Option<String>,
    pub permission_mode: Option<String>,
    pub created_at: u64,
    /// Real CLI session ID when the d-tag is a provisional UUID.
    /// Present only for sessions created via spawn commands.
    /// Empty string means the backend hasn't started yet.
    pub cli_session_id: Option<String>,
    /// Spawn command UUID linking this session to the request that created it.
    pub spawn_id: Option<String>,
}

impl SessionState {
    /// Build a SessionState from a kind-31988 note's tags.
    ///
    /// Returns None if the note has no d-tag (session ID).
    pub fn from_note(note: &nostrdb::Note, session_id: Option<&str>) -> Option<Self> {
        let claude_session_id = session_id
            .map(|s| s.to_string())
            .or_else(|| get_tag_value(note, "d").map(|s| s.to_string()))?;

        Some(SessionState {
            claude_session_id,
            title: get_tag_value(note, "title")
                .unwrap_or("Untitled")
                .to_string(),
            custom_title: get_tag_value(note, "custom_title").map(|s| s.to_string()),
            cwd: get_tag_value(note, "cwd").unwrap_or("").to_string(),
            status: get_tag_value(note, "status").unwrap_or("idle").to_string(),
            indicator: get_tag_value(note, "indicator").map(|s| s.to_string()),
            hostname: get_tag_value(note, "hostname").unwrap_or("").to_string(),
            home_dir: get_tag_value(note, "home_dir").unwrap_or("").to_string(),
            backend: get_tag_value(note, "backend").map(|s| s.to_string()),
            permission_mode: get_tag_value(note, "permission-mode").map(|s| s.to_string()),
            created_at: note.created_at(),
            cli_session_id: get_tag_value(note, "cli_session").map(|s| s.to_string()),
            spawn_id: get_tag_value(note, "spawn_id").map(|s| s.to_string()),
        })
    }
}

/// Load all session states from kind-31988 events in ndb.
///
/// Uses `query_replaceable_filtered` to deduplicate by d-tag, keeping
/// only the most recent non-deleted revision of each session state.
pub fn load_session_states(ndb: &Ndb, txn: &Transaction) -> Vec<SessionState> {
    load_session_states_with_author(ndb, txn, None)
}

/// Load session state events signed by the selected Dave account.
pub fn load_session_states_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
) -> Vec<SessionState> {
    load_session_states_with_author(ndb, txn, Some(author))
}

fn load_session_states_with_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: Option<&enostr::Pubkey>,
) -> Vec<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let mut filter = Filter::new().kinds([AI_SESSION_STATE_KIND as u64]);
    if let Some(author) = author {
        filter = filter.authors([author.bytes()]);
    }
    let filter = filter.build();

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

        let Some(state) = SessionState::from_note(&note, None) else {
            continue;
        };
        states.push(state);
    }

    states
}

/// Load all run configurations from kind-31991 events in ndb.
///
/// Each event is one config (d-tag = config UUID). Uses `query_replaceable`
/// to deduplicate by d-tag, keeping only the most recent revision. Tombstoned
/// events (with a `deleted` tag) are excluded. Only events whose `hostname`
/// tag matches `local_hostname` are loaded.
///
/// Returns a map from CWD to sorted config list.
pub(crate) fn load_run_configs_from_ndb(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
    local_hostname: &str,
) -> std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>> {
    use crate::config::{RunConfig, AI_RUN_CONFIG_KIND};
    use crate::session_events::{get_tag_value, parse_run_config_event};

    let filter = Filter::new()
        .kinds([AI_RUN_CONFIG_KIND as u64])
        .authors([author.bytes()])
        .build();
    let note_keys = query_replaceable(ndb, txn, &[filter]);

    let mut map: std::collections::HashMap<std::path::PathBuf, Vec<crate::config::RunConfig>> =
        std::collections::HashMap::new();
    for key in note_keys {
        let Ok(note) = ndb.get_note_by_key(txn, key) else {
            continue;
        };
        if get_tag_value(&note, "hostname") != Some(local_hostname) {
            continue;
        }
        // parse_run_config_event returns None for tombstones
        if let Some((cwd, config)) = parse_run_config_event(&note) {
            map.entry(cwd).or_default().push(config);
        }
    }
    // Sort each CWD's configs by name for deterministic UI order
    for configs in map.values_mut() {
        RunConfig::sort_by_name(configs);
    }
    map
}

/// Look up the latest valid revision of a single session by d-tag.
///
/// PNS wrapping causes relays to store all revisions of replaceable
/// events. This queries for the latest revision and returns it only
/// if it's non-deleted and in the current format.
pub fn latest_valid_session(
    ndb: &Ndb,
    txn: &Transaction,
    session_id: &str,
) -> Option<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let filter = Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .tags([session_id], 'd')
        .limit(1)
        .build();

    let results = ndb.query(txn, &[filter], 1).ok()?;
    let note = &results.first()?.note;

    if get_tag_value(note, "status") == Some("deleted") {
        return None;
    }
    if note.content().starts_with('{') {
        return None;
    }

    SessionState::from_note(note, Some(session_id))
}

/// Look up the latest valid revision of a selected-account session by d-tag.
pub fn latest_valid_session_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
    session_id: &str,
) -> Option<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let filter = Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .authors([author.bytes()])
        .tags([session_id], 'd')
        .limit(1)
        .build();
    let results = ndb.query(txn, &[filter], 1).ok()?;
    let note = &results.first()?.note;

    if get_tag_value(note, "status") == Some("deleted") {
        return None;
    }
    if note.content().starts_with('{') {
        return None;
    }

    SessionState::from_note(note, Some(session_id))
}

/// Extract recent working directories grouped by hostname from kind-31988
/// session state events.
///
/// Returns up to `MAX_RECENT_PER_HOST` unique paths per hostname, ordered
/// by most recently seen first. Useful for populating the directory picker
/// with previously used paths (both local and remote hosts).
pub fn load_recent_paths_by_host(
    ndb: &Ndb,
    txn: &Transaction,
) -> HashMap<String, Vec<std::path::PathBuf>> {
    load_recent_paths_by_host_with_author(ndb, txn, None)
}

/// Extract recent paths only from session states signed by the selected account.
pub fn load_recent_paths_by_host_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
) -> HashMap<String, Vec<std::path::PathBuf>> {
    load_recent_paths_by_host_with_author(ndb, txn, Some(author))
}

fn load_recent_paths_by_host_with_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: Option<&enostr::Pubkey>,
) -> HashMap<String, Vec<std::path::PathBuf>> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    const MAX_RECENT_PER_HOST: usize = 10;

    let mut filter = Filter::new().kinds([AI_SESSION_STATE_KIND as u64]);
    if let Some(author) = author {
        filter = filter.authors([author.bytes()]);
    }
    let filter = filter.build();

    let is_valid = |note: &nostrdb::Note| {
        if get_tag_value(note, "status") == Some("deleted") {
            return false;
        }
        if note.content().starts_with('{') {
            return false;
        }
        true
    };

    let note_keys = query_replaceable_filtered(ndb, txn, &[filter], is_valid);

    // Collect (hostname, cwd, created_at) triples
    let mut entries: Vec<(String, String, u64)> = Vec::new();
    for key in note_keys {
        let Ok(note) = ndb.get_note_by_key(txn, key) else {
            continue;
        };
        let hostname = get_tag_value(&note, "hostname").unwrap_or("").to_string();
        let cwd = get_tag_value(&note, "cwd").unwrap_or("").to_string();
        if cwd.is_empty() {
            continue;
        }
        entries.push((hostname, cwd, note.created_at()));
    }

    // Sort by created_at descending (most recent first)
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.2));

    // Group by hostname, dedup cwds, cap per host
    let mut result: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
    for (hostname, cwd, _) in entries {
        let paths = result.entry(hostname).or_default();
        let path = std::path::PathBuf::from(&cwd);
        if !paths.contains(&path) && paths.len() < MAX_RECENT_PER_HOST {
            paths.push(path);
        }
    }

    result
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
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
    use crate::session_events::{build_events, build_permission_request_event, ThreadingState};
    use crate::session_jsonl::JsonlLine;
    use nostrdb::{Config, IngestMetadata, Ndb};
    use tempfile::TempDir;

    fn test_config() -> Config {
        if cfg!(target_os = "windows") {
            Config::new().set_mapsize(32 * 1024 * 1024)
        } else {
            Config::new()
        }
    }

    fn test_secret_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = 1; // non-zero so signing works
        key
    }

    /// A pending permission request must stay at the end of the conversation
    /// even when its `created_at` is *earlier* than surrounding events.
    ///
    /// This reproduces the remote-sync bug: conversation events carry their
    /// original JSONL timestamps while a live permission request is stamped
    /// with `now_secs()`. When a backlog syncs with future-dated (or simply
    /// out-of-second) timestamps, sorting by `created_at` first floated the
    /// "needs input" permission request to the top. Sorting by `seq` keeps it
    /// in its true position regardless of timestamp skew.
    #[tokio::test]
    async fn permission_request_orders_by_seq_not_created_at() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id = "seq-ordering-test";

        // Conversation events are far-future dated so their created_at exceeds
        // the permission request's now_secs() stamp.
        let user_line = JsonlLine::parse(&format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{session_id}","timestamp":"2099-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{{"role":"user","content":"run a command"}}}}"#,
        ))
        .unwrap();
        let user_events = build_events(&user_line, &mut threading, &sk).unwrap();

        let assistant_line = JsonlLine::parse(&format!(
            r#"{{"type":"assistant","uuid":"u2","parentUuid":"u1","sessionId":"{session_id}","timestamp":"2099-02-09T20:00:02Z","cwd":"/tmp","version":"2.0.64","message":{{"role":"assistant","model":"claude-opus-4-5-20251101","content":[{{"type":"text","text":"sure, running it"}}]}}}}"#,
        ))
        .unwrap();
        let assistant_events = build_events(&assistant_line, &mut threading, &sk).unwrap();

        // Live permission request, stamped with now_secs() (much earlier than 2099).
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

        // Ingest in reverse to mimic out-of-order relay delivery.
        let mut all_events: Vec<_> = Vec::new();
        all_events.extend(
            user_events
                .iter()
                .filter(|e| e.kind == AI_CONVERSATION_KIND),
        );
        all_events.extend(
            assistant_events
                .iter()
                .filter(|e| e.kind == AI_CONVERSATION_KIND),
        );
        all_events.push(&perm_event);

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();

        for event in all_events.iter().rev() {
            let sub_id = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _ = ndb.wait_for_notes(sub_id, 1).await.unwrap();
        }

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, session_id);

        assert_eq!(loaded.messages.len(), 3);
        assert!(
            matches!(loaded.messages[0], Message::User(_)),
            "first message should be the user prompt, got {:?}",
            loaded.messages[0]
        );
        assert!(
            matches!(loaded.messages.last(), Some(Message::PermissionRequest(_))),
            "permission request must sort last (by seq), not float to the top: {:?}",
            loaded.messages
        );
    }
}
