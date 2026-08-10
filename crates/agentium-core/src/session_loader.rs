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

/// Total ordering key for a conversation event, at millisecond wall-clock
/// resolution.
///
/// `millis` — the sub-second `ms` tag, or `created_at * 1000` for events synced
/// before that tag existed — is authoritative. Time is used rather than `seq`
/// alone because a session mixes two independent `seq` counters whose ranges
/// diverge (the live `ThreadingState` and `convert_session_to_events`), so
/// `seq`-first ordering floats live-typed user messages to the bottom. `seq` is
/// kept as a same-second tiebreak for legacy events that predate `ms`.
///
/// `id` is the final tiebreak, making this a **total order**: two distinct
/// events can collide on both `ms` and `seq` (a session mixes two independent
/// `seq` counters — live and convert — that both restart at 0, so the same
/// `seq` recurs, and their `ms` can coincide), and without a deterministic last
/// key the tie falls through the stable `sort_by_key` to nostrdb's
/// ingestion/query order, which differs machine-to-machine (the fresh-machine
/// backfill regression). Ordering on the note id — intrinsic to the event, not
/// a stateful counter — is identical everywhere the same event set is loaded.
/// Field order (millis, then seq, then id) is the comparison order.
///
/// This is the single source of truth for conversation ordering: the loader,
/// the live poll-batch sorter, and out-of-order delivery detection all key off
/// it, so they can never drift apart.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct EventOrder {
    millis: u64,
    seq: u32,
    id: [u8; 32],
}

impl EventOrder {
    /// Derive the ordering key from a conversation note's `ms` and `seq` tags,
    /// falling back to the note id as the final total-order tiebreak.
    pub fn from_note(note: &nostrdb::Note) -> Self {
        let millis = get_tag_value(note, "ms")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| note.created_at() * 1000);
        let seq = get_tag_value(note, "seq")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(u32::MAX);
        EventOrder {
            millis,
            seq,
            id: *note.id(),
        }
    }
}

/// Result of loading session messages, including threading info for live events.
pub struct LoadedSession {
    pub messages: Vec<Message>,
    pub root_note_id: Option<[u8; 32]>,
    pub last_note_id: Option<[u8; 32]>,
    /// Permission state loaded from events (responded set + request note IDs).
    pub permissions: PermissionTracker,
    /// All note IDs found, for seeding dedup in live polling.
    pub note_ids: HashSet<[u8; 32]>,
    /// Highest [`EventOrder`] among the loaded notes, for seeding the live
    /// poll-merge tail so it knows what's already displayed. `None` when empty.
    pub max_order: Option<EventOrder>,
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
                permissions: PermissionTracker::new(),
                note_ids: HashSet::new(),
                max_order: None,
            };
        }
    };

    // Collect notes with their created_at for sorting
    let mut notes: Vec<_> = results
        .iter()
        .filter_map(|qr| ndb.get_note_by_key(txn, qr.note_key).ok())
        .collect();

    // Sort by wall-clock time at millisecond resolution — see [`EventOrder`]
    // for why time, not `seq`, is the authoritative axis.
    notes.sort_by_key(|note| EventOrder::from_note(note));

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

    // Highest ordering key present, for seeding the live-merge tail (notes are
    // sorted, so the last one is the max).
    let max_order = notes.last().map(EventOrder::from_note);

    // Second pass: convert to messages via the shared renderer.
    let mut messages = Vec::new();
    for note in &notes {
        if let Some(msg) = render_conversation_note(note, &permissions.responded) {
            messages.push(msg);
        }
    }

    LoadedSession {
        messages,
        root_note_id,
        last_note_id,
        permissions,
        note_ids,
        max_order,
    }
}

/// Render one conversation note into a chat [`Message`], or `None` for roles
/// that carry no display message (permission_response, progress, …).
///
/// This is the **single** note→message mapping, shared by the loader (a
/// from-scratch rebuild) and the live poll-merge append path in notedeck_dave,
/// so the two can never render the same event differently (path agreement).
/// `responded` supplies the decision to show on a `permission_request`.
pub fn render_conversation_note(
    note: &nostrdb::Note,
    responded: &HashMap<uuid::Uuid, crate::messages::PermissionResponseType>,
) -> Option<Message> {
    let content = note.content();
    match get_tag_value(note, "role") {
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
            let content_json = serde_json::from_str::<serde_json::Value>(content).ok()?;
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
            let response = responded.get(&perm_id).copied();
            Some(Message::PermissionRequest(PermissionRequest::new(
                perm_id, tool_name, tool_input, None, response, None,
            )))
        }
        Some("compaction_complete") => {
            let pre_tokens = content.parse::<u64>().unwrap_or(0);
            Some(Message::CompactionComplete(
                crate::messages::CompactionInfo { pre_tokens },
            ))
        }
        // Skip permission_response, progress, queue-operation, etc.
        _ => None,
    }
}

/// The `status` tag value marking a soft-deleted session.
///
/// Deletion is a tombstone, not an erasure: a fresh kind-31988 revision carries
/// this status so it wins nostrdb's replaceable resolution, while every earlier
/// revision (and the session's kind-1988 messages / kind-1989 archive) stays
/// queryable. Loaders key their include/exclude decision off this one string, so
/// it lives in one place rather than being spelled `"deleted"` at each site.
pub const DELETED_STATUS: &str = "deleted";

/// A persisted session state from a kind-31988 event.
#[derive(Debug, Clone, serde::Serialize)]
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

    /// The title to show: the user's custom title when set and non-empty, else
    /// the derived one. Shared by the CLI's row rendering and the selector's
    /// title-substring matching so both agree on what "the title" is.
    pub fn display_title(&self) -> &str {
        match self.custom_title.as_deref() {
            Some(t) if !t.is_empty() => t,
            _ => &self.title,
        }
    }

    /// Whether this is a tombstoned (soft-deleted) session — its latest revision
    /// carries [`DELETED_STATUS`]. Such sessions are kept out of the default list
    /// but stay resolvable so a durable `agentium:` ref can still reopen them.
    pub fn is_deleted(&self) -> bool {
        self.status == DELETED_STATUS
    }
}

/// Resolve a user-typed session selector to exactly one session.
///
/// The shared resolver behind every session-scoped command, modeled on
/// headway's `resolve_card` / notebook's `resolve_node`. Accepts, in precedence
/// order:
/// 1. an exact `claude_session_id` (the kind-31988 d-tag),
/// 2. an exact `cli_session_id` (the real CLI id, when the d-tag is provisional),
/// 3. a word-id — `agentium:word-word-word` or the bare words — matched by
///    re-encoding each session's stable id,
/// 4. a unique prefix of a session/cli id, or a unique case-insensitive
///    substring of the display title.
///
/// Exact and word-id matches win outright. Otherwise a fuzzy form must identify
/// exactly one session: zero matches gives `no session matching '<sel>'`, and
/// two or more gives an error listing the candidates by word-id and title so the
/// caller can retype a longer, unique selector.
pub fn resolve_session<'a>(
    sessions: &'a [SessionState],
    sel: &str,
) -> Result<&'a SessionState, String> {
    // 1. exact session id (the d-tag) — unambiguous, wins outright.
    if let Some(s) = sessions.iter().find(|s| s.claude_session_id == sel) {
        return Ok(s);
    }
    // 2. exact real cli-session id.
    if let Some(s) = sessions
        .iter()
        .find(|s| s.cli_session_id.as_deref() == Some(sel))
    {
        return Ok(s);
    }
    // 3. word-id, with an optional `agentium:` scheme prefix stripped.
    let words = sel
        .strip_prefix(&format!("{}:", crate::wordid::SCHEME))
        .unwrap_or(sel);
    let by_word: Vec<&SessionState> = sessions
        .iter()
        .filter(|s| crate::wordid::encode_session_id(&s.claude_session_id) == words)
        .collect();
    // Word-ids are unique per session, but guard against a 33-bit collision.
    match by_word.as_slice() {
        [one] => return Ok(one),
        [_, _, ..] => return Err(ambiguous_session(sel, &by_word)),
        [] => {}
    }
    // 4. unique id-prefix or display-title substring.
    let needle = sel.to_lowercase();
    let fuzzy: Vec<&SessionState> = sessions
        .iter()
        .filter(|s| {
            s.claude_session_id.starts_with(sel)
                || s.cli_session_id
                    .as_deref()
                    .is_some_and(|c| c.starts_with(sel))
                || s.display_title().to_lowercase().contains(&needle)
        })
        .collect();
    match fuzzy.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no session matching '{sel}'")),
        many => Err(ambiguous_session(sel, many)),
    }
}

/// Resolve `sel` for a resume-oriented command, falling back to tombstoned
/// sessions when the live set has no match.
///
/// Reopening work — a `resume`, or opening a durable `agentium:` ref quoted long
/// after the session was closed — must still find a session after it's been
/// soft-deleted. This tries the live `sessions` first, so their errors (including
/// an ambiguous live selector) are reported against the clean set, and only on a
/// miss retries against `deleted`. The two sets are disjoint by session id
/// (nostrdb keeps a single latest revision per `d` tag, either live or a
/// tombstone), so the searches can't collide. When both miss, the live-set error
/// is returned — it's the one the caller most likely meant.
pub fn resolve_session_including_deleted<'a>(
    sessions: &'a [SessionState],
    deleted: &'a [SessionState],
    sel: &str,
) -> Result<&'a SessionState, String> {
    match resolve_session(sessions, sel) {
        Ok(s) => Ok(s),
        Err(live_err) => resolve_session(deleted, sel).map_err(|_| live_err),
    }
}

/// Format an "ambiguous selector" error listing each candidate by its sayable
/// word-id and title, so the user can retype a longer, unique selector.
fn ambiguous_session(sel: &str, hits: &[&SessionState]) -> String {
    let mut msg = format!(
        "ambiguous session '{sel}' — matches {} sessions:",
        hits.len()
    );
    for s in hits {
        msg.push_str(&format!(
            "\n  {}  {}",
            crate::wordid::session_ref(&s.claude_session_id),
            s.display_title()
        ));
    }
    msg
}

/// Load all session states from kind-31988 events in ndb.
///
/// Uses `query_replaceable_filtered` to deduplicate by d-tag, keeping
/// only the most recent non-deleted revision of each session state.
pub fn load_session_states(ndb: &Ndb, txn: &Transaction) -> Vec<SessionState> {
    load_session_states_with_author(ndb, txn, None, SessionScope::Live)
}

/// Load session state events signed by the selected Dave account.
pub fn load_session_states_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
) -> Vec<SessionState> {
    load_session_states_with_author(ndb, txn, Some(author), SessionScope::Live)
}

/// Load only the *tombstoned* (soft-deleted) sessions for the selected account.
///
/// The counterpart to [`load_session_states_for_author`]: it keeps the sessions
/// that one drops, so a resume-oriented path can fall back to them when a durable
/// `agentium:` ref no longer matches anything live (see
/// [`resolve_session_including_deleted`]). Deliberately kept separate — and out of
/// the default list — so the live list stays clean; a caller wanting both merges
/// the two loads.
pub fn load_deleted_session_states_for_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
) -> Vec<SessionState> {
    load_session_states_with_author(ndb, txn, Some(author), SessionScope::Deleted)
}

/// Which revisions the session-state loader keeps once each `d`-tag has been
/// folded to its latest revision.
#[derive(Clone, Copy)]
enum SessionScope {
    /// Live sessions only — tombstones dropped. The default list.
    Live,
    /// Only tombstoned ([`DELETED_STATUS`]) sessions — the resume / `--deleted` set.
    Deleted,
}

fn load_session_states_with_author(
    ndb: &Ndb,
    txn: &Transaction,
    author: Option<&enostr::Pubkey>,
    scope: SessionScope,
) -> Vec<SessionState> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let mut filter = Filter::new().kinds([AI_SESSION_STATE_KIND as u64]);
    if let Some(author) = author {
        filter = filter.authors([author.bytes()]);
    }
    let filter = filter.build();

    let is_valid = |note: &nostrdb::Note| {
        // Skip old JSON-content format events regardless of scope.
        if note.content().starts_with('{') {
            return false;
        }
        // Keep or drop the latest revision by whether it's a tombstone, per scope.
        let deleted = get_tag_value(note, "status") == Some(DELETED_STATUS);
        match scope {
            SessionScope::Live => !deleted,
            SessionScope::Deleted => deleted,
        }
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

    if get_tag_value(note, "status") == Some(DELETED_STATUS) {
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

    if get_tag_value(note, "status") == Some(DELETED_STATUS) {
        return None;
    }
    if note.content().starts_with('{') {
        return None;
    }

    SessionState::from_note(note, Some(session_id))
}

/// The `created_at` of the newest persisted kind-31988 state revision for a
/// session, if any.
///
/// Callers stamp the next state event with `max(now, this + 1)` (see
/// [`crate::session_events::next_state_created_at`]) so each revision is
/// strictly newer than what ndb already holds — the latest status then always
/// wins nostrdb's replaceable resolution and the receive-side `remote_status_ts`
/// guard, even for two transitions in one wall-clock second. The authoritative
/// timestamp lives in ndb (queried here), not duplicated in session state.
pub fn latest_state_created_at(
    ndb: &Ndb,
    txn: &Transaction,
    author: &enostr::Pubkey,
    session_id: &str,
) -> Option<u64> {
    use crate::session_events::AI_SESSION_STATE_KIND;

    let filter = Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .authors([author.bytes()])
        .tags([session_id], 'd')
        .limit(1)
        .build();
    let results = ndb.query(txn, &[filter], 1).ok()?;
    Some(results.first()?.note.created_at())
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
        if get_tag_value(note, "status") == Some(DELETED_STATUS) {
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

    /// Within a single wall-clock second, the sub-second `ms` tag wins over
    /// `seq`. This is the case only millisecond time can resolve: two events
    /// share one `created_at`, but their `seq` values come from the two
    /// independent counters (live vs convert) and so disagree with real order.
    /// The event with the *earlier* `ms` must sort first even though it carries
    /// the *higher* `seq`.
    #[tokio::test]
    async fn same_second_events_order_by_ms() {
        let sk = test_secret_key();
        let session_id = "same-second-ms-test";
        let t = 1_770_000_000u64; // one shared second for both events

        // "first" is earlier by ms (t+0.100s) but carries the higher seq (9);
        // "second" is later by ms (t+0.900s) but the lower seq (2). If `seq`
        // won, "second" would sort first — `ms` must override that.
        let first_ms = (t * 1000 + 100).to_string();
        let second_ms = (t * 1000 + 900).to_string();
        let events = [
            build_1988_event_json(&sk, session_id, "user", "first", t, 9, &[("ms", &first_ms)]),
            build_1988_event_json(
                &sk,
                session_id,
                "assistant",
                "second",
                t,
                2,
                &[("ms", &second_ms)],
            ),
        ];

        let tmp_dir = TempDir::new().unwrap();
        let ndb = Ndb::new(tmp_dir.path().to_str().unwrap(), &test_config()).unwrap();
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        // Ingest reversed to mimic out-of-order relay delivery.
        let reversed: Vec<String> = events.iter().rev().cloned().collect();
        ingest_all(&ndb, &filter, &reversed).await;

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, session_id);

        assert_eq!(loaded.messages.len(), 2);
        assert!(
            matches!(loaded.messages[0], Message::User(_)),
            "earlier-ms event must sort first despite its higher seq (9 vs 2): {:?}",
            loaded.messages
        );
        assert!(
            matches!(loaded.messages[1], Message::Assistant(_)),
            "later-ms event must sort second despite its lower seq: {:?}",
            loaded.messages
        );
    }

    /// Render a loaded transcript to comparable `(role, content)` strings so two
    /// loads can be asserted equal regardless of how the events were ingested.
    fn transcript(messages: &[Message]) -> Vec<(String, String)> {
        messages
            .iter()
            .map(|m| match m {
                Message::User(c) => ("user".to_string(), c.as_str().to_string()),
                Message::Assistant(a) => ("assistant".to_string(), a.text().to_string()),
                Message::ToolResponse(_) => ("tool".to_string(), String::new()),
                Message::PermissionRequest(p) => ("permission".to_string(), p.tool_name.clone()),
                Message::CompactionComplete(info) => {
                    ("compaction".to_string(), info.pre_tokens.to_string())
                }
                other => (format!("{other:?}"), String::new()),
            })
            .collect()
    }

    /// INGESTION-PERMUTATION INVARIANCE: the displayed order must be a pure
    /// function of the event SET, independent of the order events are ingested
    /// into ndb (the fresh-machine negentropy-backfill case). This includes two
    /// events that collide on BOTH `ms` and `seq` — a real possibility because a
    /// session mixes two independent `seq` counters (live vs convert) that both
    /// restart at 0, so the same `seq` can recur, and their `ms` can coincide.
    /// Such a `(ms, seq)` tie has no ordering signal left, so a stable
    /// `sort_by_key` falls through to nostrdb's ingestion/query order, which
    /// differs machine-to-machine. `EventOrder` must break the tie on the note
    /// id so both ingestion orders yield the identical transcript.
    #[tokio::test]
    async fn ingestion_permutation_invariance() {
        let sk = test_secret_key();
        let session_id = "permutation-test";
        let t = 1_770_000_000u64;
        let ms = |off: u64| (t * 1000 + off).to_string();

        // A realistic post-Aug-6 burst, all carrying `ms`. The two `collide-*`
        // events share the SAME (ms, seq) — the tie only the note-id can break.
        let collide_ms = ms(500);
        let events = [
            build_1988_event_json(&sk, session_id, "user", "hello", t, 0, &[("ms", &ms(100))]),
            build_1988_event_json(
                &sk,
                session_id,
                "assistant",
                "collide-A",
                t,
                7,
                &[("ms", &collide_ms)],
            ),
            build_1988_event_json(
                &sk,
                session_id,
                "assistant",
                "collide-B",
                t,
                7,
                &[("ms", &collide_ms)],
            ),
            build_1988_event_json(
                &sk,
                session_id,
                "user",
                "and more",
                t,
                3,
                &[("ms", &ms(900))],
            ),
            // A compaction event must render (loader is a superset of the live
            // merge path) and stay permutation-invariant.
            build_1988_event_json(
                &sk,
                session_id,
                "compaction_complete",
                "12345",
                t,
                4,
                &[("ms", &ms(950))],
            ),
        ];

        // Load the same set from two fresh ndbs: one ingested in authored order,
        // one reversed. Both transcripts must be identical.
        let load_order = |order: Vec<String>| async move {
            let tmp = TempDir::new().unwrap();
            let ndb = Ndb::new(tmp.path().to_str().unwrap(), &test_config()).unwrap();
            let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
            ingest_all(&ndb, &filter, &order).await;
            let txn = Transaction::new(&ndb).unwrap();
            transcript(&load_session_messages(&ndb, &txn, session_id).messages)
        };

        let authored = load_order(events.to_vec()).await;
        let reversed = load_order(events.iter().rev().cloned().collect()).await;

        assert_eq!(
            authored, reversed,
            "displayed order must be independent of ingestion order, even for a \
             (ms, seq) collision: authored={authored:?} reversed={reversed:?}"
        );
    }

    /// A minimal SessionState for resolver tests: only the fields the selector
    /// looks at (id, cli id, title) are meaningful; the rest are inert defaults.
    fn sess(id: &str, cli: Option<&str>, title: &str) -> SessionState {
        SessionState {
            claude_session_id: id.to_string(),
            title: title.to_string(),
            custom_title: None,
            cwd: String::new(),
            status: "idle".to_string(),
            indicator: None,
            hostname: String::new(),
            home_dir: String::new(),
            backend: None,
            permission_mode: None,
            created_at: 0,
            cli_session_id: cli.map(|s| s.to_string()),
            spawn_id: None,
        }
    }

    #[test]
    fn resolve_exact_ids_win() {
        let sessions = vec![
            sess("aaaa-1111", Some("cli-abc"), "Refactor auth"),
            sess("bbbb-2222", None, "Fix relay reconnect"),
        ];
        // exact d-tag
        assert_eq!(
            resolve_session(&sessions, "aaaa-1111")
                .unwrap()
                .claude_session_id,
            "aaaa-1111"
        );
        // exact cli-session id
        assert_eq!(
            resolve_session(&sessions, "cli-abc")
                .unwrap()
                .claude_session_id,
            "aaaa-1111"
        );
    }

    #[test]
    fn resolve_word_id_forms() {
        let sessions = vec![sess("aaaa-1111", None, "Refactor auth")];
        let words = crate::wordid::encode_session_id("aaaa-1111");
        // the full URI ref and the bare words both resolve to the same session.
        for sel in [format!("agentium:{words}"), words.clone()] {
            assert_eq!(
                resolve_session(&sessions, &sel).unwrap().claude_session_id,
                "aaaa-1111",
                "selector {sel:?} should resolve"
            );
        }
    }

    #[test]
    fn resolve_prefix_and_title_substring() {
        let sessions = vec![
            sess("aaaa-1111", None, "Refactor auth"),
            sess("bbbb-2222", None, "Fix relay reconnect"),
        ];
        // unique id prefix
        assert_eq!(
            resolve_session(&sessions, "aaaa")
                .unwrap()
                .claude_session_id,
            "aaaa-1111"
        );
        // unique case-insensitive title substring
        assert_eq!(
            resolve_session(&sessions, "RELAY")
                .unwrap()
                .claude_session_id,
            "bbbb-2222"
        );
    }

    #[test]
    fn resolve_none_and_ambiguous() {
        let sessions = vec![
            sess("aaaa-1111", None, "Refactor auth module"),
            sess("aaaa-9999", None, "Refactor billing"),
        ];
        // nothing matches
        let err = resolve_session(&sessions, "zzz").unwrap_err();
        assert!(err.contains("no session matching 'zzz'"), "{err}");
        // both share the "aaaa" prefix and the "Refactor" title word
        let err = resolve_session(&sessions, "aaaa").unwrap_err();
        assert!(err.starts_with("ambiguous session 'aaaa'"), "{err}");
        assert!(err.contains("agentium:"), "candidates are listed: {err}");
    }

    /// A tombstoned session (whose `sess` fixture we mark deleted) that no longer
    /// appears in the live set is still resolvable via the deleted fallback — the
    /// durability the card is about — while a live match always wins over it.
    #[test]
    fn resolve_including_deleted_falls_back() {
        let live = vec![sess("aaaa-1111", None, "Refactor auth")];
        let mut gone = sess("bbbb-2222", Some("cli-old"), "Closed spike");
        gone.status = DELETED_STATUS.to_string();
        let deleted = vec![gone];

        // A live selector resolves against the live set and never reaches deleted.
        assert_eq!(
            resolve_session_including_deleted(&live, &deleted, "aaaa-1111")
                .unwrap()
                .claude_session_id,
            "aaaa-1111"
        );

        // A deleted session's id, cli-id, and word-id all resolve via the fallback,
        // even though it's absent from the live set.
        let words = crate::wordid::encode_session_id("bbbb-2222");
        for sel in ["bbbb-2222", "cli-old", &words, &format!("agentium:{words}")] {
            let s = resolve_session_including_deleted(&live, &deleted, sel).unwrap_or_else(|e| {
                panic!("selector {sel:?} should resolve a deleted session: {e}")
            });
            assert_eq!(s.claude_session_id, "bbbb-2222");
            assert!(s.is_deleted());
        }

        // Missing in both sets reports the live-set error, not the deleted one.
        let err = resolve_session_including_deleted(&live, &deleted, "zzz").unwrap_err();
        assert!(err.contains("no session matching 'zzz'"), "{err}");
    }
}
