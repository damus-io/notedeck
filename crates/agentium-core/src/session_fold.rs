//! An incremental fold of one account's agent sessions: the latest kind-31988
//! state per session id, plus a per-session last-activity time advanced by both
//! state revisions and kind-1988 conversation messages.
//!
//! This is the pure-data half of Dave's realtime session cache, lifted here so
//! any consumer can keep a live session table without re-querying nostrdb per
//! row. Dave drives it through notedeck's generic `RealtimeCache` (its
//! `session_cache` module is only that adapter); `agentium watch` drives it from
//! an [`Engine::watch_activity`](crate::Engine::watch_activity) subscription. The
//! fold is framework-free — it knows only nostrdb notes — so it unit-tests
//! against bare notes and a bare [`Ndb`].
//!
//! # Two agentium specifics
//! - **Replaceable identity.** A session's kind-31988 state event is *replaceable*:
//!   its event id churns on every status/title update, so a word-id is built from
//!   the stable `claude_session_id` d-tag (SHA-256'd — see [`crate::wordid`]), not
//!   the event id. nostrdb keeps *every* revision, so the fold keeps the
//!   highest-`created_at` revision per session id, tombstones included, so a
//!   re-delivered older revision can't resurrect a deleted session.
//! - **Two kinds, one fold.** The live feed ([`session_feed_filter`]) spans both
//!   the kind-31988 state events (the title/status projection) and the kind-1988
//!   conversation messages. Only the newest message *time* per session is kept,
//!   folded into [`SessionView::last_activity`], so a session that streams
//!   messages without republishing its status still reads fresh — and a reader
//!   gets that memoized timestamp instead of querying ndb for it.

use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use std::collections::HashMap;

use crate::session_events::{get_tag_value, AI_CONVERSATION_KIND, AI_SESSION_STATE_KIND};
use crate::session_loader::{SessionState, DELETED_STATUS};

/// Upper bound on session-state events folded in one seed query. Far above any
/// realistic per-account session count; the incremental path folds later
/// arrivals as deltas rather than re-querying.
const SEED_QUERY_LIMIT: i32 = 10_000;

/// One session's current folded state plus the note id of the kind-31988 event it
/// came from. Dave's parser resolves a word-id to [`note_id`](Self::note_id); its
/// renderer folds the current [`state`](Self::state) (title/status) off the same
/// entry, so a live status update shows on the chip.
#[derive(Clone)]
pub struct SessionView {
    /// The note id of the *current* (highest-`created_at`) kind-31988 state event.
    /// Drifts as the replaceable event is revised, so a reader tracks the latest.
    pub note_id: NoteId,
    /// The current folded session state (title, status, …).
    pub state: SessionState,
    /// Unix seconds of the session's last activity: the newest `created_at` across
    /// *both* its state revisions (kind-31988) and its conversation messages
    /// (kind-1988), maintained incrementally as either kind folds in. Memoized
    /// here so a per-frame or per-redraw reader never queries ndb for it. Spanning
    /// kind-1988 is what keeps a session that's actively streaming messages
    /// (without republishing its status) reading as fresh rather than only
    /// tracking status publishes.
    pub last_activity: u64,
}

/// The pure-data accumulator: the latest kind-31988 revision per session id
/// (`claude_session_id` d-tag), **including** `deleted` tombstones so a stale
/// earlier revision can never resurrect a deleted session. Legacy JSON-content
/// events are ignored.
///
/// Seed it with [`fold_sessions`], then advance it with [`reduce_delta`] (or
/// [`ingest`](Self::ingest) per note) as a [`session_feed_filter`] subscription
/// delivers new notes; read it with [`views`](Self::views).
#[derive(Default)]
pub struct SessionReducer {
    /// Keyed by `claude_session_id`; the value is the latest revision seen.
    latest: HashMap<String, SessionView>,
    /// Reverse index `word-id → claude_session_id`, so
    /// [`resolve_wordid_including_deleted`](Self::resolve_wordid_including_deleted)
    /// is an O(1) lookup rather than a per-call linear scan that re-hashes every
    /// folded session. A word-id is a deterministic SHA-256 → BIP-39 encoding of
    /// the stable `claude_session_id`, so an entry is computed exactly once — when
    /// a session id is first folded — and never changes as the session's
    /// replaceable revision (and its note id) churns. Tombstones stay indexed:
    /// a session id is only ever added, never removed, mirroring [`latest`](Self::latest).
    wordid_index: HashMap<String, String>,
    /// Newest kind-1988 conversation-message `created_at` seen per session id. Only
    /// the timestamp is kept (never the message), so this is a tiny per-session
    /// `u64` regardless of conversation volume. Merged with each session's state
    /// `created_at` into [`SessionView::last_activity`], so a session streaming
    /// messages without republishing its status still reads fresh. Kept separate
    /// from [`latest`](Self::latest) so a message that arrives *before* its
    /// session's state event isn't lost — the state ingest folds it in when it lands.
    last_msg_activity: HashMap<String, u64>,
}

impl SessionReducer {
    /// Fold one note from the session feed, dispatching on kind: a kind-1988
    /// conversation message advances only last-activity
    /// ([`ingest_activity`](Self::ingest_activity)); anything else is treated as a
    /// kind-31988 state revision ([`ingest_state`](Self::ingest_state)). Both paths
    /// are commutative and idempotent (a re-delivered or older event changes
    /// nothing), as an incremental cache requires.
    ///
    /// Returns whether the fold changed, so a redraw-on-change reader can skip a
    /// batch that only re-delivered notes it already holds.
    pub fn ingest(&mut self, note: &Note) -> bool {
        // A conversation message only advances last-activity; a state event carries
        // the full session projection. Everything else the broadened subscription
        // could deliver falls through to the state path and is rejected there.
        if note.kind() == AI_CONVERSATION_KIND {
            return self.ingest_activity(note);
        }
        self.ingest_state(note)
    }

    /// Fold a kind-31988 session-state event: keep the highest-`created_at`
    /// revision per session id (tombstones included) and compute its
    /// [`last_activity`](SessionView::last_activity) across both kinds. Returns
    /// whether the held revision changed.
    fn ingest_state(&mut self, note: &Note) -> bool {
        // The superseded JSON-content format predates the tag layout `from_note`
        // reads; the loader skips it too (see `load_session_states_with_author`).
        if note.content().starts_with('{') {
            return false;
        }
        let Some(state) = SessionState::from_note(note, None) else {
            return false; // no d-tag: not a session-state event we can key.
        };
        let held = self.latest.get(&state.claude_session_id);
        if held.is_some_and(|held| held.state.created_at > state.created_at) {
            return false; // an older revision than the one we hold — ignore.
        }
        // First time we see this session id: compute its word-id once (SHA-256 →
        // BIP-39) and record the reverse mapping. On later revisions the mapping
        // already exists, so `resolve` never re-hashes.
        if held.is_none() {
            self.wordid_index.insert(
                crate::wordid::encode_session_id(&state.claude_session_id),
                state.claude_session_id.clone(),
            );
        }
        let note_id = NoteId::new(*note.id());
        // A same-second revision still replaces the held one (unchanged tie
        // behaviour), but re-delivering the very note we hold is not a change.
        let changed = held.is_none_or(|held| held.note_id != note_id);
        // Last activity spans both kinds: fold in the newest conversation message
        // already seen for this session (which may have arrived before this state),
        // so an actively-streaming session doesn't read as stale.
        let last_activity = self
            .last_msg_activity
            .get(&state.claude_session_id)
            .copied()
            .unwrap_or(0)
            .max(state.created_at);
        self.latest.insert(
            state.claude_session_id.clone(),
            SessionView {
                note_id,
                state,
                last_activity,
            },
        );
        changed
    }

    /// Fold a kind-1988 conversation message: advance its session's newest-message
    /// timestamp (keeping only the max, never the message body) and, when that
    /// session is already folded, bump its [`last_activity`](SessionView::last_activity).
    /// This is the whole reason the fold observes kind-1988 — a session streaming
    /// messages without republishing its status must still read fresh, without a
    /// reader querying ndb for it. Idempotent: a re-delivered or older message
    /// can't move the max backwards. Returns whether the max advanced.
    fn ingest_activity(&mut self, note: &Note) -> bool {
        let Some(session_id) = get_tag_value(note, "d") else {
            return false; // a conversation message with no session id — nothing to key.
        };
        let created_at = note.created_at();
        let newer = self
            .last_msg_activity
            .get(session_id)
            .is_none_or(|&seen| created_at > seen);
        if !newer {
            return false;
        }
        self.last_msg_activity
            .insert(session_id.to_string(), created_at);
        if let Some(view) = self.latest.get_mut(session_id) {
            view.last_activity = view.last_activity.max(created_at);
        }
        true
    }

    /// The current (non-deleted) sessions, in no particular order. Tombstones are
    /// kept in the accumulator to block resurrection but never projected.
    pub fn views(&self) -> Vec<SessionView> {
        self.latest
            .values()
            .filter(|v| v.state.status != DELETED_STATUS)
            .cloned()
            .collect()
    }

    /// The state-event note id of the session whose word-id is `words`,
    /// **including tombstones**. A session's stable identity is its
    /// `claude_session_id`, whose word-id (SHA-256 → BIP-39 — see
    /// [`crate::wordid::encode_session_id`]) is precomputed into
    /// [`wordid_index`](Self::wordid_index) at fold time, so this is an O(1) map
    /// lookup — no per-call scan or re-hash. Dave calls it per visible chip per
    /// frame in its immediate-mode UI, so it must not hash or allocate.
    ///
    /// Unlike [`views`](Self::views), this resolves every folded revision, deleted
    /// ones included: a durable `agentium:` ref (e.g. quoted in a headway
    /// done-comment) must keep resolving so a closed session can still be reopened.
    /// `None` if no session matches. Reads the fold directly rather than the
    /// projected views precisely because the projection drops tombstones.
    pub fn resolve_wordid_including_deleted(&self, words: &str) -> Option<NoteId> {
        let session_id = self.wordid_index.get(words)?;
        self.latest.get(session_id).map(|v| v.note_id)
    }
}

/// The seed filter selecting `author`'s kind-31988 session-state events — the
/// projection-bearing half of the fold (title/status/word-id). Kept kind-scoped
/// (rather than the combined [`session_feed_filter`]) so the seed query for the
/// session *set* can't be crowded out of [`SEED_QUERY_LIMIT`] by conversation
/// volume, and so tests can await state commits alone.
pub fn session_state_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .authors([author.bytes()])
        .build()
}

/// The seed filter selecting `author`'s kind-1988 conversation messages — the
/// last-activity half of the fold. Queried newest-first and capped, so it seeds
/// recent activity (all a staleness read needs) without walking the whole
/// message history.
pub fn session_conversation_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .authors([author.bytes()])
        .build()
}

/// The live subscription filter: `author`'s session-state (kind-31988) *and*
/// conversation (kind-1988) events in one filter, so both advance the fold. State
/// events drive the title/status projection; conversation events advance
/// [`SessionView::last_activity`] so a streaming session reads fresh without a
/// reader querying ndb. Seeding is split across the two kind-scoped filters above
/// (see [`fold_sessions`]).
pub fn session_feed_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64, AI_SESSION_STATE_KIND as u64])
        .authors([author.bytes()])
        .build()
}

/// Seed a reducer by folding all of `author`'s existing session-state (kind-31988)
/// and recent conversation (kind-1988) events — the one-time seed before a
/// subscription takes over. `None` on a query error, so a caller can re-attempt.
///
/// Conversation activity folds *first* so that when a state event lands its
/// [`last_activity`](SessionView::last_activity) already reflects the newest
/// message; the two kinds are queried separately (not through the combined
/// [`session_feed_filter`]) so a busy message history can't push session-state
/// events out of the [`SEED_QUERY_LIMIT`] window and drop sessions from the fold.
pub fn fold_sessions(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<SessionReducer> {
    let mut reducer = SessionReducer::default();
    let activity = ndb
        .query(
            txn,
            &[session_conversation_filter(author)],
            SEED_QUERY_LIMIT,
        )
        .ok()?;
    for result in &activity {
        reducer.ingest(&result.note);
    }
    let states = ndb
        .query(txn, &[session_state_filter(author)], SEED_QUERY_LIMIT)
        .ok()?;
    for result in &states {
        reducer.ingest(&result.note);
    }
    Some(reducer)
}

/// Fold a batch of freshly-arrived notes into `reducer`, returning the keys not
/// yet visible under `txn` (committed after its snapshot) so the caller retries
/// them on a later advance rather than dropping a cross-device edit.
pub fn reduce_delta(
    reducer: &mut SessionReducer,
    ndb: &Ndb,
    txn: &Transaction,
    keys: &[NoteKey],
) -> Vec<NoteKey> {
    let mut deferred = Vec::new();
    for key in keys {
        let Ok(note) = ndb.get_note_by_key(txn, *key) else {
            // Committed after `txn`'s snapshot — retry with a fresher one.
            deferred.push(*key);
            continue;
        };
        reducer.ingest(&note);
    }
    deferred
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{temp_ndb, TEST_SECKEY};
    use futures_util::StreamExt;
    use nostrdb::{NoteBuilder, SubscriptionStream};
    use std::time::Duration;

    /// A signed kind-31988 session-state note for `session_id`. `created_at`
    /// orders revisions; `status` drives the deleted-tombstone case.
    fn state_note(session_id: &str, title: &str, status: &str, created_at: u64) -> Note<'static> {
        NoteBuilder::new()
            .kind(AI_SESSION_STATE_KIND)
            .content("")
            .created_at(created_at)
            .start_tag()
            .tag_str("d")
            .tag_str(session_id)
            .start_tag()
            .tag_str("title")
            .tag_str(title)
            .start_tag()
            .tag_str("status")
            .tag_str(status)
            .sign(&TEST_SECKEY)
            .build()
            .expect("state note")
    }

    /// A signed kind-1988 conversation message for `session_id`. Only its `d`
    /// tag and `created_at` matter to the fold.
    fn message_note(session_id: &str, created_at: u64) -> Note<'static> {
        NoteBuilder::new()
            .kind(AI_CONVERSATION_KIND)
            .content("streamed message")
            .created_at(created_at)
            .start_tag()
            .tag_str("d")
            .tag_str(session_id)
            .sign(&TEST_SECKEY)
            .build()
            .expect("message note")
    }

    /// The projected view for `session_id`, if any.
    fn view(r: &SessionReducer, session_id: &str) -> Option<SessionView> {
        r.views()
            .into_iter()
            .find(|v| v.state.claude_session_id == session_id)
    }

    /// The newest state revision wins regardless of arrival order, and an older
    /// re-delivery changes nothing — word-id resolution follows the winner.
    #[test]
    fn keeps_the_newest_revision_in_any_order() {
        let sid = "session-alpha";
        let words = crate::wordid::encode_session_id(sid);
        let old = state_note(sid, "First", "working", 1_000);
        let new = state_note(sid, "Renamed", "idle", 2_000);

        let mut r = SessionReducer::default();
        assert!(r.ingest(&new));
        assert!(!r.ingest(&old), "an older revision is not taken");
        assert!(
            !r.ingest(&new),
            "re-delivering the held revision is no change"
        );
        assert_eq!(view(&r, sid).unwrap().state.display_title(), "Renamed");
        assert_eq!(
            r.resolve_wordid_including_deleted(&words),
            Some(NoteId::new(*new.id()))
        );
        assert!(r
            .resolve_wordid_including_deleted("maple-river-canyon")
            .is_none());
    }

    /// A message landing before its session's state still counts: the state
    /// ingest folds it into `last_activity`. A later message advances it; a stale
    /// one can't regress it.
    #[test]
    fn messages_advance_last_activity_in_any_order() {
        let sid = "session-live";
        let mut r = SessionReducer::default();

        assert!(r.ingest(&message_note(sid, 1_500)));
        assert!(view(&r, sid).is_none(), "no state yet: nothing to project");
        r.ingest(&state_note(sid, "Live", "working", 1_000));
        assert_eq!(view(&r, sid).unwrap().last_activity, 1_500);

        assert!(r.ingest(&message_note(sid, 2_500)));
        assert!(!r.ingest(&message_note(sid, 2_000)), "stale message");
        let v = view(&r, sid).unwrap();
        assert_eq!(v.last_activity, 2_500);
        assert_eq!(v.state.created_at, 1_000, "state untouched by messages");
    }

    /// A tombstone leaves the projection but stays resolvable, and an older
    /// non-deleted revision can't resurrect it.
    #[test]
    fn tombstone_is_resolvable_but_unprojected() {
        let sid = "session-beta";
        let words = crate::wordid::encode_session_id(sid);
        let tomb = state_note(sid, "Beta", DELETED_STATUS, 2_000);

        let mut r = SessionReducer::default();
        r.ingest(&state_note(sid, "Beta", "working", 1_000));
        r.ingest(&tomb);
        r.ingest(&state_note(sid, "Beta", "working", 1_000));

        assert!(view(&r, sid).is_none(), "tombstoned session not projected");
        assert_eq!(
            r.resolve_wordid_including_deleted(&words),
            Some(NoteId::new(*tomb.id()))
        );
    }

    /// Legacy JSON-content state events are ignored.
    #[test]
    fn ignores_legacy_json_state() {
        let legacy = NoteBuilder::new()
            .kind(AI_SESSION_STATE_KIND)
            .content("{\"title\":\"old\"}")
            .start_tag()
            .tag_str("d")
            .tag_str("legacy")
            .sign(&TEST_SECKEY)
            .build()
            .expect("legacy note");
        let mut r = SessionReducer::default();
        assert!(!r.ingest(&legacy));
        assert!(r.views().is_empty());
    }

    /// The seed fold reads both kinds out of a real ndb, and `reduce_delta`
    /// advances it from subscription keys.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn seeds_from_ndb_then_reduces_deltas() {
        let (_dir, ndb) = temp_ndb();
        let author = nostrdb_net::FullKeypair::from_secret_bytes(&TEST_SECKEY)
            .expect("keypair")
            .pubkey;
        let sub = ndb
            .subscribe(&[session_feed_filter(&author)])
            .expect("subscribe");
        let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
        let ingest = |note: &Note| {
            let frame = nostrdb_net::ClientMessage::event(note)
                .expect("client message")
                .to_json()
                .expect("frame json");
            ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
                .expect("ingest");
        };
        // Pull `n` committed keys off the feed subscription.
        async fn keys(stream: &mut SubscriptionStream, n: usize) -> Vec<NoteKey> {
            let mut out = Vec::new();
            while out.len() < n {
                let batch = tokio::time::timeout(Duration::from_secs(5), stream.next())
                    .await
                    .expect("notes should commit")
                    .expect("stream open");
                out.extend(batch);
            }
            out
        }

        ingest(&state_note("s1", "One", "working", 1_000));
        ingest(&message_note("s1", 1_200));
        keys(&mut stream, 2).await;

        let txn = Transaction::new(&ndb).expect("txn");
        let mut r = fold_sessions(&ndb, &txn, &author).expect("seed");
        drop(txn);
        assert_eq!(view(&r, "s1").unwrap().last_activity, 1_200);

        ingest(&message_note("s1", 3_000));
        ingest(&state_note("s2", "Two", "idle", 2_000));
        let delta = keys(&mut stream, 2).await;
        let txn = Transaction::new(&ndb).expect("txn");
        assert!(reduce_delta(&mut r, &ndb, &txn, &delta).is_empty());
        assert_eq!(view(&r, "s1").unwrap().last_activity, 3_000);
        assert_eq!(view(&r, "s2").unwrap().state.display_title(), "Two");
    }
}
