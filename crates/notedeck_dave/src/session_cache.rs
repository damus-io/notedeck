//! A realtime, per-account fold of the current agent-session state (kind-31988),
//! shared between Dave's foreground UI and the inline `agentium:<word-id>` chips
//! it registers (its [`AgentiumRefParser`](crate::reference::AgentiumRefParser)
//! and [`AgentiumSessionRenderer`](crate::render::AgentiumSessionRenderer)).
//!
//! An `agentium:` reference written in a note or a Dave chat message must render a
//! chip of the session's *live* title/status — one that updates in realtime as the
//! session runs — and clicking it must open that session in Dave. Both the chip
//! and Dave's own session view must read the *same* folded state, so a status
//! update the app's per-frame pump folds in shows on the chip too, not just the
//! open surface. This module is that shared fold: it rides notedeck's generic
//! [`RealtimeCache`](notedeck::RealtimeCache) — subscribe once per author, seed the
//! fold once, absorb later revisions as deltas, memoize the projection, defer
//! notes that postdate the read snapshot — plugging in only the agentium-specific
//! [`SessionReducer`] (kind-31988 → the latest [`SessionState`] per session id).
//!
//! # Two agentium specifics
//! - **Replaceable identity.** A session's kind-31988 state event is *replaceable*:
//!   its event id churns on every status/title update, so a word-id is built from
//!   the stable `claude_session_id` d-tag (SHA-256'd — see
//!   [`agentium_core::wordid`]), not the event id. nostrdb keeps *every* revision,
//!   so the fold keeps the highest-`created_at` revision per session id, tombstones
//!   included, so a re-delivered older revision can't resurrect a deleted session.
//! - **No fan-out.** Unlike the notebook/headway caches, this one is read-only:
//!   Dave already syncs and publishes session-state events through its PNS relay
//!   path, so the pump only *advances* the fold — it never re-publishes
//!   [`fresh`](notedeck::PollResponse::fresh) keys, which would double-write.
//!
//! # Limitation
//! The fold only advances while Dave is *opened* (its `update` pumps it), so a
//! chip drawn in another app stays live only when Dave has run this session.
//! Browser-level sync with the host app closed is out of scope here — see
//! `headway:notedeck/buffalo-change-cook`.

use agentium_core::session_loader::{SessionState, DELETED_STATUS};
use enostr::{NoteId, Pubkey};
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::collections::HashMap;

use agentium_core::session_events::AI_SESSION_STATE_KIND;

/// Upper bound on session-state events folded in one seed query. Far above any
/// realistic per-account session count; the incremental path folds later
/// arrivals as deltas rather than re-querying.
const SEED_QUERY_LIMIT: i32 = 10_000;

/// One session's current folded state plus the note id of the kind-31988 event it
/// came from. The parser resolves a word-id to [`note_id`](Self::note_id); the
/// renderer folds the current [`state`](Self::state) (title/status) off the same
/// entry, so a live status update shows on the chip.
#[derive(Clone)]
pub struct SessionView {
    /// The note id of the *current* (highest-`created_at`) kind-31988 state event.
    /// Drifts as the replaceable event is revised, so the chip tracks the latest.
    pub note_id: NoteId,
    /// The current folded session state (title, status, …).
    pub state: SessionState,
}

/// The pure-data accumulator: the latest kind-31988 revision per session id
/// (`claude_session_id` d-tag), **including** `deleted` tombstones so a stale
/// earlier revision can never resurrect a deleted session. Legacy JSON-content
/// events are ignored. Deliberately egui-free (and `notedeck`-free) so it
/// unit-tests against a bare [`Ndb`]; the [`AgentiumReducer`] newtype supplies the
/// framework glue.
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
}

impl SessionReducer {
    /// Fold one kind-31988 note, keeping the highest-`created_at` revision per
    /// session id (tombstones included). A no-op for a note with no `d` tag, one in
    /// the superseded JSON-content format, or an older revision than one already
    /// held — which makes the fold commutative and idempotent (a re-delivered
    /// revision changes nothing), as the [`RealtimeCache`](notedeck::RealtimeCache)
    /// requires.
    fn ingest(&mut self, note: &Note) {
        // The superseded JSON-content format predates the tag layout `from_note`
        // reads; the loader skips it too (see `load_session_states_with_author`).
        if note.content().starts_with('{') {
            return;
        }
        let Some(state) = SessionState::from_note(note, None) else {
            return; // no d-tag: not a session-state event we can key.
        };
        let held = self.latest.get(&state.claude_session_id);
        if held.is_some_and(|held| held.state.created_at > state.created_at) {
            return; // an older revision than the one we hold — ignore.
        }
        // First time we see this session id: compute its word-id once (SHA-256 →
        // BIP-39) and record the reverse mapping. On later revisions the mapping
        // already exists, so `resolve` never re-hashes.
        if held.is_none() {
            self.wordid_index.insert(
                agentium_core::wordid::encode_session_id(&state.claude_session_id),
                state.claude_session_id.clone(),
            );
        }
        let note_id = NoteId::new(*note.id());
        self.latest.insert(
            state.claude_session_id.clone(),
            SessionView { note_id, state },
        );
    }

    /// The current (non-deleted) sessions. Tombstones are kept in the accumulator
    /// to block resurrection but never projected.
    fn views(&self) -> Vec<SessionView> {
        self.latest
            .values()
            .filter(|v| v.state.status != DELETED_STATUS)
            .cloned()
            .collect()
    }

    /// The state-event note id of the session whose word-id is `words`,
    /// **including tombstones**. A session's stable identity is its
    /// `claude_session_id`, whose word-id (SHA-256 → BIP-39 — see
    /// [`agentium_core::wordid::encode_session_id`]) is precomputed into
    /// [`wordid_index`](Self::wordid_index) at fold time, so this is an O(1) map
    /// lookup — no per-call scan or re-hash. Called per visible chip per frame in
    /// the immediate-mode UI, so it must not hash or allocate.
    ///
    /// Unlike [`views`](Self::views), this resolves every folded revision, deleted
    /// ones included: a durable `agentium:` ref (e.g. quoted in a headway
    /// done-comment) must keep resolving so a closed session can still be reopened.
    /// `None` if no session matches. Reads the fold directly rather than the
    /// projected views precisely because the projection drops tombstones.
    fn resolve_wordid_including_deleted(&self, words: &str) -> Option<NoteId> {
        let session_id = self.wordid_index.get(words)?;
        self.latest.get(session_id).map(|v| v.note_id)
    }
}

/// The subscription/seed filter selecting `author`'s kind-31988 session-state
/// events — the one filter both the live subscription and the seed fold use.
pub fn session_state_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .kinds([AI_SESSION_STATE_KIND as u64])
        .authors([author.bytes()])
        .build()
}

/// Seed a reducer by folding all of `author`'s existing kind-31988 events — the
/// one-time seed the cache performs on first touch. `None` on a query error, so
/// the cache re-attempts on the next touch.
fn fold_sessions(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<SessionReducer> {
    let results = ndb
        .query(txn, &[session_state_filter(author)], SEED_QUERY_LIMIT)
        .ok()?;
    let mut reducer = SessionReducer::default();
    for result in &results {
        reducer.ingest(&result.note);
    }
    Some(reducer)
}

/// Fold a batch of freshly-arrived notes into `reducer`, returning the keys not
/// yet visible under `txn` (committed after its snapshot) so the cache retries
/// them on a later advance rather than dropping a cross-device edit. See
/// [`notedeck::Reducer::reduce_delta`].
fn reduce_delta(
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

/// notedeck_dave's [`notedeck::Reducer`] adapter over [`SessionReducer`], so a
/// generic [`notedeck::RealtimeCache`] can drive the session fold. A newtype
/// rather than a direct `impl notedeck::Reducer for SessionReducer` so the fold
/// layer stays free of any `notedeck` dependency; the trait methods forward to the
/// free functions above and [`SessionReducer::views`].
struct AgentiumReducer(SessionReducer);

impl notedeck::Reducer for AgentiumReducer {
    type View = SessionView;

    fn filter(author: &Pubkey) -> Filter {
        session_state_filter(author)
    }

    fn fold(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Self> {
        fold_sessions(ndb, txn, author).map(AgentiumReducer)
    }

    fn reduce_delta(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Vec<NoteKey> {
        reduce_delta(&mut self.0, ndb, txn, keys)
    }

    fn finalize(&self) -> Vec<SessionView> {
        self.0.views()
    }
}

/// The session-data engine for Dave: a per-account [`notedeck::RealtimeCache`]
/// over the kind-31988 session fold. Shared (behind `Rc<RefCell<…>>`) between the
/// app's per-frame pump and the inline `agentium:` parser + renderer, so a chip
/// drawn in a note/Dave-chat reads the *same* realtime state — a status update the
/// pump folds in is immediately visible on the chip, not just the open session.
///
/// A thin wrapper: the seed-once fold, the post-snapshot deferral, the
/// once-per-fold projection and the pump all live in the generic cache; this only
/// adds agentium-specific convenience reads (word-id resolution and a single
/// session lookup).
#[derive(Default)]
pub struct AgentiumSessionCache {
    authors: notedeck::RealtimeCache<AgentiumReducer>,
}

impl AgentiumSessionCache {
    /// Advance `author`'s session fold and report the change — the per-frame pump
    /// called from Dave's `update`. Read-only: unlike the notebook/headway caches
    /// this does *not* fan [`fresh`](notedeck::PollResponse::fresh) out (Dave's PNS
    /// path owns publishing); the caller only needs [`changed`](notedeck::PollResponse::changed)
    /// to schedule a repaint so chips stay live.
    #[profiling::function]
    pub fn poll(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
    ) -> notedeck::PollResponse {
        self.authors.poll(ndb, txn, author)
    }

    /// Advance `author`'s fold, project it *at most once per fold*, and run `read`
    /// against the memoized [`SessionView`]s. `None` until the reducer seeds.
    /// Delegates to [`RealtimeCache::with_views`](notedeck::RealtimeCache::with_views):
    /// the parser and renderer both resolve through it, so on a steady frame the
    /// first read projects and the rest reuse the memo. `read` extracts owned data
    /// so the cache borrow drops before drawing.
    pub fn with_sessions<R>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&[SessionView]) -> R,
    ) -> Option<R> {
        self.authors.with_views(ndb, txn, author, read)
    }

    /// The current state-event note id of the session whose word-id is `words`,
    /// **including tombstoned sessions**, seeding the fold on first touch. `None`
    /// before the first fold or when no session matches.
    ///
    /// A durable `agentium:` ref must keep resolving after its session is
    /// soft-deleted, so this reads the raw fold (via
    /// [`with_reducer`](notedeck::RealtimeCache::with_reducer)) rather than the
    /// projected views, which drop tombstones to keep the live scene clean. Going
    /// through the shared cache still means a chip drawn the same frame reuses this
    /// fold.
    #[profiling::function]
    pub fn resolve_wordid(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        words: &str,
    ) -> Option<NoteId> {
        self.authors
            .with_reducer(ndb, txn, author, |r| {
                r.0.resolve_wordid_including_deleted(words)
            })
            .flatten()
    }

    /// The current folded state for `session_id` (a `claude_session_id` d-tag), if
    /// held and not deleted — the renderer's live-state read for a chip's title and
    /// status. `None` before the first fold or when no such session is folded
    /// locally (a not-yet-synced session, or another account's).
    #[profiling::function]
    pub fn session(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        session_id: &str,
    ) -> Option<SessionView> {
        self.with_sessions(ndb, txn, author, |sessions| {
            sessions
                .iter()
                .find(|v| v.state.claude_session_id == session_id)
                .cloned()
        })
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentium_core::session_events::AI_SESSION_STATE_KIND;
    use enostr::FullKeypair;
    use futures::StreamExt;
    use nostrdb::{Config, Ndb, NoteBuilder, SubscriptionStream};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A headless harness driving an [`AgentiumSessionCache`] against a bare `Ndb`,
    /// so a test folds real kind-31988 events the same way the app pumps them.
    /// Ingest is async, so each write awaits a side subscription before polling.
    struct TestCache {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        cache: Rc<RefCell<AgentiumSessionCache>>,
        stream: SubscriptionStream,
    }

    impl TestCache {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            let kp = FullKeypair::generate();
            let sub = ndb.subscribe(&[session_state_filter(&kp.pubkey)]).unwrap();
            let stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(64);
            Self {
                ndb,
                _dir: dir,
                kp,
                cache: Rc::new(RefCell::new(AgentiumSessionCache::default())),
                stream,
            }
        }

        /// Write a kind-31988 session-state event for `session_id` and ingest it
        /// locally (mirroring the store's client-ingest path). `created_at` orders
        /// revisions; `status` drives the deleted-tombstone case.
        fn write_state(&self, session_id: &str, title: &str, status: &str, created_at: u64) {
            let note = NoteBuilder::new()
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
                .sign(&self.kp.secret_key.secret_bytes())
                .build()
                .unwrap();
            let frame = enostr::ClientMessage::event(&note)
                .unwrap()
                .to_json()
                .unwrap();
            self.ndb
                .process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
                .unwrap();
        }

        /// Await `n` committed notes on the side subscription (writes commit async).
        async fn await_notes(&mut self, n: usize) {
            let mut seen = 0;
            while seen < n {
                seen += self.stream.next().await.expect("subscription open").len();
            }
        }

        /// Pump the shared cache under a fresh read txn.
        fn poll(&mut self) -> notedeck::PollResponse {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .poll(&self.ndb, &txn, &self.kp.pubkey)
        }

        /// Resolve a word-id against the shared cache under a fresh read txn.
        fn resolve(&mut self, words: &str) -> Option<NoteId> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .resolve_wordid(&self.ndb, &txn, &self.kp.pubkey, words)
        }

        /// The folded state for a session id under a fresh read txn.
        fn session(&mut self, session_id: &str) -> Option<SessionView> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache
                .borrow_mut()
                .session(&self.ndb, &txn, &self.kp.pubkey, session_id)
        }
    }

    /// A word-id for a real session resolves to its *current* kind-31988 note id,
    /// and that note id drifts to the newer revision when the session updates —
    /// the replaceable-identity guarantee (resolve tracks the latest state event).
    #[tokio::test]
    async fn resolve_tracks_the_latest_state_event() {
        let mut t = TestCache::new();
        let sid = "session-alpha";
        let words = agentium_core::wordid::encode_session_id(sid);

        t.write_state(sid, "First title", "working", 1_000);
        t.await_notes(1).await;
        let first = loop {
            t.poll();
            if let Some(id) = t.resolve(&words) {
                break id;
            }
        };

        // A newer revision (a title/status update) replaces the resolved note id.
        t.write_state(sid, "Renamed", "idle", 2_000);
        t.await_notes(1).await;
        let second = loop {
            t.poll();
            if let Some(id) = t.resolve(&words) {
                if id != first {
                    break id;
                }
            }
        };
        assert_ne!(first, second, "resolve should track the newer revision");

        // The live state reflects the newer title.
        assert_eq!(t.session(sid).unwrap().state.display_title(), "Renamed");

        // A well-formed but unknown word-id resolves to nothing.
        assert!(t.resolve("maple-river-canyon").is_none());
    }

    /// The reverse word-id index keeps distinct sessions distinct: each word-id
    /// resolves to its own session's note id, never another's — a guard against
    /// the index conflating sessions once resolution stopped scanning-and-hashing.
    #[tokio::test]
    async fn distinct_sessions_resolve_through_the_index() {
        let mut t = TestCache::new();
        let sid_a = "session-a";
        let sid_b = "session-b";
        let words_a = agentium_core::wordid::encode_session_id(sid_a);
        let words_b = agentium_core::wordid::encode_session_id(sid_b);

        t.write_state(sid_a, "Alpha", "working", 1_000);
        t.write_state(sid_b, "Bravo", "working", 1_000);
        t.await_notes(2).await;
        let (id_a, id_b) = loop {
            t.poll();
            if let (Some(a), Some(b)) = (t.resolve(&words_a), t.resolve(&words_b)) {
                break (a, b);
            }
        };
        assert_ne!(id_a, id_b, "each session resolves to its own note id");
        assert_eq!(t.session(sid_a).unwrap().state.display_title(), "Alpha");
        assert_eq!(t.session(sid_b).unwrap().state.display_title(), "Bravo");
    }

    /// A `deleted` tombstone drops a session from the *projected* views (so it
    /// leaves the live scene) but keeps it *resolvable*: a durable `agentium:` ref
    /// must still resolve so a closed session can be reopened. A re-delivered older
    /// (non-deleted) revision does not resurrect it into the projection.
    #[tokio::test]
    async fn deleted_session_stays_resolvable_but_unprojected() {
        let mut t = TestCache::new();
        let sid = "session-beta";
        let words = agentium_core::wordid::encode_session_id(sid);

        t.write_state(sid, "Beta", "working", 1_000);
        t.await_notes(1).await;
        loop {
            t.poll();
            if t.resolve(&words).is_some() {
                break;
            }
        }

        // A newer `deleted` revision tombstones the session: it leaves the
        // projection (`session` is None) but stays resolvable, now pointing at the
        // tombstone revision's note id.
        t.write_state(sid, "Beta", DELETED_STATUS, 2_000);
        t.await_notes(1).await;
        let tombstone_id = loop {
            t.poll();
            if t.session(sid).is_none() {
                break t.resolve(&words).expect("a deleted session still resolves");
            }
        };

        // Re-delivering the original (older) creation revision must not resurrect
        // it: the fold holds the newer tombstone, so the older revision is ignored —
        // the session stays unprojected and resolves to the tombstone, not the
        // stale working revision.
        t.write_state(sid, "Beta", "working", 1_000);
        // The re-delivered older event may not commit (nostrdb drops a stale
        // replaceable revision), so poll a few times rather than await a commit.
        for _ in 0..5 {
            t.poll();
        }
        assert!(
            t.session(sid).is_none(),
            "a stale older revision must not resurrect a deleted session"
        );
        assert_eq!(
            t.resolve(&words),
            Some(tombstone_id),
            "resolve must stay pinned to the tombstone, not the stale revision"
        );
    }
}
