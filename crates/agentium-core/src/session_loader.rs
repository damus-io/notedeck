//! Load a previous session's conversation from nostr events in ndb.
//!
//! Queries for kind-1988 events with a matching `d` tag (session ID),
//! orders them by their monotonic `seq` tag, and converts them into
//! `Message` variants for populating the chat UI.

use crate::messages::{AssistantMessage, ExecutedTool, Message, PermissionRequest};
use crate::session::PermissionTracker;
use crate::session_events::{
    decode_permission_response, get_tag_value, is_conversation_role, AI_CONVERSATION_KIND,
};
use crate::tools::ToolResponse;
use nostrdb::{Filter, Ndb, Transaction};
use std::collections::{HashMap, HashSet};

// `query_replaceable` / `query_replaceable_filtered` now live in `enostr`, so
// every consumer (calendar sync, Horizon, Dave sessions) resolves replaceable
// events the same way. Re-exported here for the call sites below and any users
// of this module. (Eventual home is nostrdb itself.)
pub use enostr::{query_replaceable, query_replaceable_filtered};

/// Result of loading session messages, including threading info for live events.
pub struct LoadedSession {
    pub messages: Vec<Message>,
    pub root_note_id: Option<[u8; 32]>,
    pub last_note_id: Option<[u8; 32]>,
    /// The `seq` the next emitted event should carry: the highest `seq` among
    /// loaded events plus one (0 for an empty session). This is deliberately
    /// **not** the note count — some conversation events (`permission_response`,
    /// `set_permission_mode`) carry no `seq` tag, so counting notes overshoots
    /// the real sequence space and would seed the live counter ahead of the
    /// turn's own events, floating user messages below the turn they trigger.
    pub next_seq: u32,
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
                next_seq: 0,
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

    // Sort by `created_at` first, using `seq` only as a same-second tiebreaker.
    //
    // `created_at` is the authoritative order: every event carries real
    // wall-clock time — convert preserves the original JSONL timestamp, live
    // events use `now_secs()` — and a live event is always emitted at or after
    // the conversation it follows, so `created_at` never inverts logical order
    // across seconds. `seq` is NOT reliable as the primary key: a session mixes
    // events from two independent counters (the live `ThreadingState` seeded
    // from ndb, and `convert_session_to_events`, which restarts at 0), so their
    // `seq` ranges diverge and live-typed user messages float to the bottom
    // when sorted by `seq`. `created_at` has no such split.
    //
    // `seq` still matters within a single second: `created_at` is only
    // second-resolution, so a turn's burst of events (or a synced backlog) can
    // share one timestamp, and there `seq` — the per-turn monotonic counter —
    // orders them (e.g. keeps a pending permission request after the assistant
    // text it follows). Events missing a `seq` tag tiebreak last (`u32::MAX`).
    notes.sort_by_key(|note| {
        let seq = get_tag_value(note, "seq")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(u32::MAX);
        (note.created_at(), seq)
    });

    // The next event's `seq` is one past the highest `seq` actually assigned,
    // ignoring seq-less events (see `LoadedSession::next_seq`). Using the note
    // count instead would overshoot by the number of seq-less events.
    let next_seq = notes
        .iter()
        .filter_map(|n| get_tag_value(n, "seq").and_then(|s| s.parse::<u32>().ok()))
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
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
                let summary = crate::util::truncate(content, 200);
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
        next_seq,
        permissions,
        note_ids,
    }
}

/// A persisted session state from a kind-31988 event.
#[derive(serde::Serialize)]
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
pub fn load_run_configs_from_ndb(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_events::{build_events, build_permission_request_event, ThreadingState};
    use crate::session_jsonl::JsonlLine;
    use nostrdb::{Config, IngestMetadata, Ndb, NoteBuildOptions, NoteBuilder};
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

    /// Hand-build a signed kind-1988 event JSON with an explicit `created_at`
    /// and `seq`, bypassing the live builders so tests control both axes
    /// independently. `content` is the raw note content (JSON for a
    /// permission_request, plain text otherwise); `extra` adds trailing
    /// `(key, value)` tags (e.g. `perm-id`).
    fn build_1988_event_json(
        sk: &[u8; 32],
        session_id: &str,
        role: &str,
        content: &str,
        created_at: u64,
        seq: u32,
        extra: &[(&str, &str)],
    ) -> String {
        let seq_str = seq.to_string();
        let mut builder = NoteBuilder::new()
            .kind(AI_CONVERSATION_KIND)
            .content(content)
            .options(NoteBuildOptions::default())
            .created_at(created_at)
            .start_tag()
            .tag_str("d")
            .tag_str(session_id)
            .start_tag()
            .tag_str("role")
            .tag_str(role)
            .start_tag()
            .tag_str("seq")
            .tag_str(&seq_str);
        for (k, v) in extra {
            builder = builder.start_tag().tag_str(k).tag_str(v);
        }
        let note = builder.sign(sk).build().unwrap();
        format!("[\"EVENT\", {}]", note.json().unwrap())
    }

    async fn ingest_all(ndb: &Ndb, filter: &Filter, events: &[String]) {
        for event in events {
            let sub_id = ndb.subscribe(std::slice::from_ref(filter)).unwrap();
            ndb.process_event_with(event, IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _ = ndb.wait_for_notes(sub_id, 1).await.unwrap();
        }
    }

    /// Within a single wall-clock second, `seq` breaks the tie so a pending
    /// permission request stays after the assistant text it follows and does
    /// not float to the top (regression for the remote-sync "NeedsInput floats
    /// to top" bug, dave#pledge-grief-close). `created_at` is second-resolution,
    /// so a turn's burst of events shares one timestamp; only `seq` can order
    /// them, and the loader uses it as the same-second tiebreaker.
    #[tokio::test]
    async fn same_second_events_order_by_seq() {
        let sk = test_secret_key();
        let session_id = "same-second-test";
        let t = 1_770_000_000; // one shared second for every event

        let perm_content = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/test"}}"#;
        let perm_id = uuid::Uuid::new_v4().to_string();
        let events = [
            build_1988_event_json(&sk, session_id, "user", "run a command", t, 0, &[]),
            build_1988_event_json(&sk, session_id, "assistant", "sure, running it", t, 1, &[]),
            build_1988_event_json(
                &sk,
                session_id,
                "permission_request",
                perm_content,
                t,
                2,
                &[("perm-id", &perm_id)],
            ),
        ];

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        // Ingest in reverse to mimic out-of-order relay delivery.
        let reversed: Vec<String> = events.iter().rev().cloned().collect();
        ingest_all(&ndb, &filter, &reversed).await;

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, session_id);

        assert_eq!(loaded.messages.len(), 3);
        assert!(
            matches!(loaded.messages[0], Message::User(_)),
            "user prompt must sort first (seq 0): {:?}",
            loaded.messages
        );
        assert!(
            matches!(loaded.messages.last(), Some(Message::PermissionRequest(_))),
            "permission request must stay last (seq 2), not float to the top: {:?}",
            loaded.messages
        );
    }

    /// Across different seconds, `created_at` wins over `seq`. A session mixes
    /// two independent seq counters — the live `ThreadingState` (seeded from
    /// ndb) and `convert_session_to_events` (restarts at 0) — so a live-typed
    /// user message can carry a much *higher* `seq` than the events that
    /// chronologically follow it. Sorting by `seq` sank those user messages to
    /// the very bottom of the chat; sorting by `created_at` keeps them in place.
    #[tokio::test]
    async fn divergent_seq_orders_by_created_at() {
        let sk = test_secret_key();
        let session_id = "divergent-seq-test";

        // The user message is EARLIER in wall-clock time but carries an
        // inflated seq (490, the live counter); the assistant reply is LATER
        // but carries a low seq (360, the convert counter). created_at must win.
        let events = [
            build_1988_event_json(&sk, session_id, "user", "/compact reorder", 1_000, 490, &[]),
            build_1988_event_json(&sk, session_id, "assistant", "on it", 1_001, 360, &[]),
        ];

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        ingest_all(&ndb, &filter, &events).await;

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, session_id);

        assert_eq!(loaded.messages.len(), 2);
        assert!(
            matches!(loaded.messages[0], Message::User(_)),
            "earlier user message must sort first by created_at despite its \
             higher seq (490 vs 360) — sorting by seq sinks it to the bottom: {:?}",
            loaded.messages
        );
        assert!(
            matches!(loaded.messages[1], Message::Assistant(_)),
            "later assistant reply must sort second: {:?}",
            loaded.messages
        );
    }

    /// `next_seq` must be one past the highest assigned `seq`, ignoring any
    /// event that carries no `seq` tag. Current builders always stamp `seq`
    /// (see `no_conversation_event_is_seqless`), but sessions synced before that
    /// fix still hold seq-less notes in ndb. Counting notes instead of taking
    /// `max(seq) + 1` would overshoot by those legacy events and seed the live
    /// counter ahead of the next turn, floating a user message below the turn it
    /// triggers — the long-standing "resume drift" bug.
    #[tokio::test]
    async fn next_seq_ignores_seqless_events() {
        let sk = test_secret_key();
        let mut threading = ThreadingState::new();
        let session_id = "next-seq-test";

        let user_line = JsonlLine::parse(&format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{session_id}","timestamp":"2024-02-09T20:00:01Z","cwd":"/tmp","version":"2.0.64","message":{{"role":"user","content":"run a command"}}}}"#,
        ))
        .unwrap();
        let user_events = build_events(&user_line, &mut threading, &sk).unwrap(); // seq 0

        let perm_id = uuid::Uuid::new_v4();
        let perm_event = build_permission_request_event(
            &perm_id,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            session_id,
            &mut threading,
            &sk,
        )
        .unwrap(); // seq 1

        // A legacy seq-less note (as emitted before the seq invariant landed).
        // Hand-built to bypass the current builders, which always stamp `seq`.
        let legacy_note = NoteBuilder::new()
            .kind(AI_CONVERSATION_KIND)
            .content("legacy response with no seq tag")
            .options(NoteBuildOptions::default())
            .start_tag()
            .tag_str("d")
            .tag_str(session_id)
            .start_tag()
            .tag_str("role")
            .tag_str("permission_response")
            .sign(&sk)
            .build()
            .unwrap();
        let legacy_event_json = format!("[\"EVENT\", {}]", legacy_note.json().unwrap());

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        for event_json in [
            user_events[0].to_event_json(),
            perm_event.to_event_json(),
            legacy_event_json,
        ] {
            let sub_id = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event_json, IngestMetadata::new().client(true))
                .expect("ingest failed");
            let _ = ndb.wait_for_notes(sub_id, 1).await.unwrap();
        }

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, session_id);

        // Three notes ingested (user=0, request=1, legacy=none). The highest
        // assigned seq is 1, so the next event must be seq 2 — not the note
        // count (3), which the seq-less legacy note would inflate.
        assert_eq!(
            loaded.next_seq, 2,
            "next_seq must be max-seq + 1 (2), not the note count (3)"
        );
    }
}
