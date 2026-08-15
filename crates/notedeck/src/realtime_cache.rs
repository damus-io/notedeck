//! A realtime, per-author fold cache shared between an app's foreground UI and
//! the inline widgets it registers (its
//! [`KindRenderer`](crate::KindRenderer)s and
//! [`ReferenceParser`](crate::ReferenceParser)).
//!
//! Several apps keep an entity live in two places at once: the open app surface,
//! and an inline chip of that same entity drawn *elsewhere* (a `nostr:` link in a
//! note, a Dave chat message). Both must read the *same* folded state, so a
//! realtime edit — a CLI move, a remote sync — that the app's per-frame pump
//! folds in shows on the chip too, not just the open surface. This module is the
//! reusable skeleton for that: subscribe once per author, seed the fold once,
//! absorb later arrivals as deltas, memoize the finalize, and defer notes that
//! arrive after the read transaction's snapshot so a cross-device edit is never
//! lost. The app plugs in only its [`Reducer`] — the accumulator that turns its
//! own kinds into renderable views.
//!
//! The pattern was first proven in `notedeck_headway`'s board cache; this is the
//! extraction so `notedeck_notebook` (canvas nodes) and `notedeck_dave` (agent
//! sessions) ride the same discipline against their own reducers rather than each
//! re-deriving the subtle bits (the seed-once fold, the post-snapshot deferral,
//! the once-per-fold finalize).
//!
//! # What stays app-specific
//! Only the [`Reducer`] and the app's per-frame pump: this cache reports which
//! notes it folded in ([`PollResponse::fresh`]) but does not itself fan them out
//! to relays or drive repaints — the owning app's `update` does that, exactly as
//! before. Multi-writer folds keyed by something other than the author (e.g.
//! headway's shared boards, folded by board coordinate) stay outside this cache
//! as a sibling; the author-keyed leg is what generalizes.

use std::collections::HashMap;

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};

/// An app-owned reduction of one author's nostr events into renderable views.
///
/// The [`RealtimeCache`] owns the subscription/poll/pending/memoize/pump
/// skeleton and calls into this trait for the app-specific parts: which events to
/// pull ([`filter`](Self::filter)), how to seed the fold from history
/// ([`fold`](Self::fold)), how to absorb later arrivals
/// ([`reduce_delta`](Self::reduce_delta)), and how to project the folded state
/// into owned views for drawing ([`finalize`](Self::finalize)).
///
/// The fold must be **commutative and idempotent**: the cache folds the whole
/// history once, then folds only freshly-arrived deltas into that same
/// accumulator, and a relay may re-deliver a note, so re-ingesting an event must
/// not change the result.
pub trait Reducer: Sized {
    /// The owned per-entity view this reducer projects at
    /// [`finalize`](Self::finalize) — a board, a canvas, a session, … The cache
    /// memoizes a `Vec<Self::View>` and hands it to reads.
    type View;

    /// The subscription/seed filter selecting `author`'s events. Used both to
    /// open the live subscription and (implicitly, via [`fold`](Self::fold)) to
    /// seed the reducer, so the two see the same event set.
    fn filter(author: &Pubkey) -> Filter;

    /// Fold all of `author`'s existing events out of `ndb` into a fresh reducer —
    /// the one-time seed. `None` if nothing folded (e.g. a query error); the cache
    /// re-attempts on the next touch.
    ///
    /// Called once on first touch, because a freshly-opened subscription only
    /// reports *future* ingests; thereafter the cache folds deltas via
    /// [`reduce_delta`](Self::reduce_delta) rather than re-walking the history.
    fn fold(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Self>;

    /// Fold a batch of freshly-arrived notes (by `keys`) into the live reducer,
    /// returning the keys **not yet visible** under `txn` — notes whose commit
    /// postdates this transaction's snapshot.
    ///
    /// The cache retains the returned keys and retries them on the next advance
    /// with a fresher snapshot. Dropping them instead would silently lose a
    /// cross-device edit that lands mid-frame until a full re-seed, so an impl
    /// must return (not skip) any key it couldn't read under `txn` — typically the
    /// keys for which `ndb.get_note_by_key(txn, key)` errors.
    fn reduce_delta(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Vec<NoteKey>;

    /// Project the folded state into owned views for drawing. Walked at most once
    /// per fold by the cache (see [`RealtimeCache::with_views`]) and memoized, so
    /// this may allocate; reads borrow the memoized slice.
    fn finalize(&self) -> Vec<Self::View>;
}

/// One author's cached state within a [`RealtimeCache`]: a live subscription to
/// that author's events plus the long-lived [`Reducer`] they fold into, its
/// memoized finalize, and any keys awaiting a fresher snapshot.
struct CachedAuthor<R: Reducer> {
    /// The folded reducer, seeded on first touch and advanced by deltas after.
    reducer: Option<R>,
    /// Live subscription to `R::filter(author)`, opened on first touch and kept
    /// for the cache's lifetime (the touched-author set is tiny, so no eviction).
    sub: Option<Subscription>,
    /// Memoized [`Reducer::finalize`] of `reducer`, rebuilt lazily on the next
    /// read after a fold changed the reducer (see [`RealtimeCache::advance`]).
    /// `finalize` walks the whole reducer into an owned `Vec`; without this memo
    /// every inline reference re-walked it *twice* a frame — once to resolve, once
    /// to render — so N on-screen references cost 2·N finalizes per frame.
    finalized: Option<Vec<R::View>>,
    /// Keys the subscription drained but that weren't yet visible under the read
    /// txn they were polled with (committed after that txn's snapshot — see
    /// [`Reducer::reduce_delta`]). Retried on the next advance with a fresher
    /// snapshot; without this they'd be lost until a full re-seed, which is why a
    /// cross-device edit could otherwise go missing until an app restart.
    pending: Vec<NoteKey>,
    /// Freshly-folded delta keys awaiting the fan-out pump — a per-author outbox.
    ///
    /// The subscription is drained destructively by *every* [`advance`], and
    /// `advance` runs from both the fan-out [`poll`](RealtimeCache::poll) and the
    /// read paths ([`with_views`](RealtimeCache::with_views) /
    /// [`with_reducer`](RealtimeCache::with_reducer)). Whichever runs first takes
    /// the note off the subscription, so if the fan-out relied on its *own* drain
    /// it would miss any note a read swallowed first — the note folds locally but
    /// never publishes to the user's other devices. So `advance` instead appends
    /// each folded key here, and only `poll` drains it: the fan-out pump is
    /// guaranteed every fresh key exactly once, no matter which caller drained the
    /// subscription. Seeds don't enqueue (their keys are historical, not new
    /// ingests — see [`PollResponse::fresh`]).
    unsynced: Vec<NoteKey>,
}

// Derived `Default` would require `R: Default`, which no reducer needs; a manual
// impl seeds an empty entry for any `R`.
impl<R: Reducer> Default for CachedAuthor<R> {
    fn default() -> Self {
        Self {
            reducer: None,
            sub: None,
            finalized: None,
            pending: Vec::new(),
            unsynced: Vec::new(),
        }
    }
}

/// The outcome of advancing an author's reducer one step (see
/// [`RealtimeCache::poll`]).
#[derive(Default)]
pub struct PollResponse {
    /// The reducer was seeded or had new notes folded in this call, so the caller
    /// schedules follow-up repaints (keep waking while edits stream in).
    pub changed: bool,
    /// Note keys folded in *incrementally* since the last [`poll`](RealtimeCache::poll)
    /// — drained from the author's outbox ([`unsynced`](CachedAuthor::unsynced)), so
    /// this includes deltas a read path (`with_views`/`with_reducer`) folded between
    /// polls, not just ones this call drained. Empty on a seed (those keys are
    /// historical, not new ingests) and when nothing folded. The caller fans these
    /// out to the account's private relays (see
    /// [`fan_out_unseen_notes`](crate::fan_out_unseen_notes)).
    pub fresh: Vec<NoteKey>,
}

/// Always-on observability counters for a [`RealtimeCache`]. Two `u32`s bumped on
/// the fold and finalize paths — cheap enough to leave on in release (no
/// allocation, no per-frame cost beyond an increment) and useful both as
/// telemetry (how often a cache re-seeds vs. folds deltas) and as the hook a
/// consumer's tests assert the seed-once / finalize-once-per-fold invariants
/// through, across the crate boundary where `#[cfg(test)]` fields wouldn't be
/// visible.
#[derive(Default, Clone, Copy)]
pub struct RealtimeCacheStats {
    /// Full-history folds ([`Reducer::fold`]) performed — the initial seed plus
    /// any forced re-seed. Steady state is exactly one per author.
    pub full_reloads: u32,
    /// [`Reducer::finalize`] calls spent — at most one per fold, reused by every
    /// read in between.
    pub finalizes: u32,
}

/// A per-author realtime fold cache generic over an app's [`Reducer`].
///
/// One subscription + folded reducer per account, shared (behind
/// `Rc<RefCell<…>>`) between the app and everything it registers for inline
/// display. The foreground surface and every inline widget read the *same*
/// reducer, so a realtime edit the app's per-frame [`poll`](Self::poll) folds in
/// is immediately visible to an inline chip drawn elsewhere, not just to the open
/// app.
///
/// Driven by `&Ndb` (the [`KindRenderer`](crate::KindRenderer) render path has no
/// `&mut Ndb`), so a chip seeds and folds on render even when the owning app
/// isn't foreground. First touch folds the history once to seed; later touches
/// fold in only the freshly-arrived notes — an incremental step, sound because
/// the fold is commutative and idempotent. Subscriptions live for the cache's
/// lifetime; the touched-author set is tiny, so there's no eviction.
pub struct RealtimeCache<R: Reducer> {
    authors: HashMap<Pubkey, CachedAuthor<R>>,
    stats: RealtimeCacheStats,
}

// Manual `Default` (see [`CachedAuthor`]): an empty cache needs no `R: Default`.
impl<R: Reducer> Default for RealtimeCache<R> {
    fn default() -> Self {
        Self {
            authors: HashMap::new(),
            stats: RealtimeCacheStats::default(),
        }
    }
}

impl<R: Reducer> RealtimeCache<R> {
    /// Ensure a live subscription + seeded reducer for `author` and fold in any
    /// freshly-arrived notes, enqueueing each folded delta key on the author's
    /// fan-out outbox ([`unsynced`](CachedAuthor::unsynced)) and returning whether
    /// anything folded. The shared primitive behind [`poll`](Self::poll) and
    /// [`with_views`](Self::with_views): the app's per-frame pump and a chip's lazy
    /// fold-on-render both flow through it, so a reducer is seeded once and
    /// thereafter only ever folds deltas — and, crucially, *any* of those callers
    /// draining the subscription still routes the note to the fan-out pump via the
    /// outbox (only [`poll`](Self::poll) drains it).
    #[profiling::function]
    fn advance(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> bool {
        let entry = self.authors.entry(*author).or_default();
        if entry.sub.is_none() {
            entry.sub = ndb.subscribe(&[R::filter(author)]).ok();
        }
        let (changed, fresh) = match entry.sub {
            // No subscription: fold the whole history each frame so edits still show.
            None => {
                entry.reducer = R::fold(ndb, txn, author);
                (true, Vec::new())
            }
            Some(sub) => {
                let polled = ndb.poll_for_notes(sub, 64);
                if entry.reducer.is_none() {
                    // First touch (a fresh subscription only reports *future*
                    // ingests): fold the existing history once to seed. Notes
                    // just drained by `polled` may postdate this seed's snapshot,
                    // so carry them into `pending` to fold in next advance rather
                    // than lose them.
                    entry.reducer = R::fold(ndb, txn, author);
                    entry.pending = polled;
                    (true, Vec::new())
                } else if polled.is_empty() && entry.pending.is_empty() {
                    (false, Vec::new())
                } else if let Some(reducer) = entry.reducer.as_mut() {
                    // Incremental: fold the new notes (plus any that a staler
                    // snapshot couldn't yet see) into the live reducer, and carry
                    // forward the ones still not visible under this txn.
                    let mut batch = std::mem::take(&mut entry.pending);
                    batch.extend_from_slice(&polled);
                    let deferred = reducer.reduce_delta(ndb, txn, &batch);
                    let fresh: Vec<NoteKey> = batch
                        .into_iter()
                        .filter(|k| !deferred.contains(k))
                        .collect();
                    entry.pending = deferred;
                    // Everything deferred (nothing folded) reads as a no-op: keep
                    // waiting, and crucially don't count it as a full re-seed.
                    (!fresh.is_empty(), fresh)
                } else {
                    (false, Vec::new())
                }
            }
        };
        // A fold happened (seed or delta), so the memoized finalize is stale; drop
        // it and let the next read rebuild it once (see `with_views`).
        if changed {
            entry.finalized = None;
        }
        // A full seed fold has no `fresh` keys (as opposed to an incremental delta,
        // non-empty `fresh`, or a no-op, `!changed`); a seed's keys are historical,
        // not new ingests, so they never enqueue for fan-out.
        let full_seed = changed && fresh.is_empty();
        entry.unsynced.extend(fresh);
        if full_seed {
            self.stats.full_reloads += 1;
        }
        changed
    }

    /// Advance `author`'s reducer and report the change — the per-frame pump
    /// called from the app's `update`. Fan out [`fresh`](PollResponse::fresh) and
    /// wake on [`changed`](PollResponse::changed).
    ///
    /// Drains the author's fan-out outbox ([`unsynced`](CachedAuthor::unsynced))
    /// into [`fresh`](PollResponse::fresh): every delta folded since the last poll,
    /// *including ones a read path drained from the shared subscription first*. A
    /// non-empty drain also reads as `changed`, so the app keeps waking until the
    /// outbox is empty and every key is fanned out.
    pub fn poll(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> PollResponse {
        let changed = self.advance(ndb, txn, author);
        let fresh = self
            .authors
            .get_mut(author)
            .map(|entry| std::mem::take(&mut entry.unsynced))
            .unwrap_or_default();
        PollResponse {
            changed: changed || !fresh.is_empty(),
            fresh,
        }
    }

    /// Advance `author`'s reducer, (re)finalize it *at most once per fold*, and
    /// run `read` against the memoized views — the single place a finalize is
    /// spent. Returns `None` only when the reducer hasn't seeded yet.
    ///
    /// Every per-frame inline read (resolve a ref, render a chip) flows through
    /// here, so on a steady-state frame the first read finalizes and the rest
    /// reuse the cached [`finalized`](CachedAuthor::finalized) vec. `read`
    /// extracts owned data from the borrowed slice so the caller's cache borrow
    /// can drop before it draws.
    #[profiling::function]
    pub fn with_views<T>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&[R::View]) -> T,
    ) -> Option<T> {
        self.advance(ndb, txn, author);
        let entry = self.authors.get_mut(author)?;
        if entry.finalized.is_none() {
            entry.finalized = Some(entry.reducer.as_ref()?.finalize());
            self.stats.finalizes += 1;
        }
        let entry = self.authors.get(author)?;
        Some(read(entry.finalized.as_ref()?))
    }

    /// Advance `author`'s reducer, then run `read` against the *raw* folded
    /// reducer — the fold-free counterpart to [`with_views`](Self::with_views).
    ///
    /// [`with_views`](Self::with_views) only exposes the projected
    /// [`finalize`](Reducer::finalize) views, which deliberately drop state a
    /// consumer sometimes still needs — most notably entries a reducer keeps in
    /// the fold to block resurrection (e.g. tombstones) but hides from the
    /// projection. This hands the reducer itself to a specialized, read-only
    /// lookup so such state stays reachable without changing what `finalize`
    /// projects. It skips the memoized finalize entirely: nothing is projected or
    /// cached here, so the lookup pays only for the advance, not a whole-reducer
    /// walk. `None` until the reducer seeds.
    #[profiling::function]
    pub fn with_reducer<T>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&R) -> T,
    ) -> Option<T> {
        self.advance(ndb, txn, author);
        let entry = self.authors.get(author)?;
        Some(read(entry.reducer.as_ref()?))
    }

    /// The always-on observability counters (see [`RealtimeCacheStats`]).
    pub fn stats(&self) -> RealtimeCacheStats {
        self.stats
    }

    /// The folded reducer for `author`, if it has seeded. Lets a consumer read the
    /// reducer directly (e.g. for a diagnostic or a specialized fold-free query)
    /// without going through the memoized [`with_views`](Self::with_views) path.
    pub fn reducer(&self, author: &Pubkey) -> Option<&R> {
        self.authors.get(author)?.reducer.as_ref()
    }

    /// How many keys are awaiting a fresher snapshot for `author` (see
    /// [`CachedAuthor::pending`]) — zero unless a delta arrived faster than the
    /// read transaction could see it. Exposed for the post-snapshot-deferral
    /// invariant a consumer's tests assert.
    pub fn pending_len(&self, author: &Pubkey) -> usize {
        self.authors
            .get(author)
            .map(|a| a.pending.len())
            .unwrap_or(0)
    }

    /// Drop `author`'s memoized finalize so the next read rebuilds it — the memo
    /// invalidation a fold performs, exposed so a consumer can force it (e.g. when
    /// state it folds *outside* this cache changes, or to exercise the
    /// finalize-once-per-fold path in a test).
    pub fn invalidate(&mut self, author: &Pubkey) {
        if let Some(entry) = self.authors.get_mut(author) {
            entry.finalized = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::{Config, Ndb};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    /// Counts every `fold`/`reduce_delta`/`finalize` a toy reducer performs, so a
    /// test can assert the cache seeded once and finalized once per fold without
    /// reaching into its internals. One shared set of counters across the reducer
    /// (which the cache owns by value) via atomics.
    #[derive(Default)]
    struct Counters {
        folds: AtomicU32,
        deltas: AtomicU32,
        finalizes: AtomicU32,
    }

    // A test writes/reads notes of this kind under one author; the reducer just
    // collects their note ids, which is enough to observe seed-vs-delta folding.
    const TEST_KIND: u64 = 30_777;

    /// A minimal [`Reducer`] over `TEST_KIND` notes: it accumulates the note keys
    /// it has ingested (idempotently) and counts each trait call through a shared
    /// [`Counters`]. `finalize` projects one view per ingested note.
    struct ToyReducer {
        counters: &'static Counters,
        seen: Vec<NoteKey>,
    }

    impl ToyReducer {
        fn ingest(&mut self, key: NoteKey) {
            if !self.seen.contains(&key) {
                self.seen.push(key);
            }
        }
    }

    impl Reducer for ToyReducer {
        type View = NoteKey;

        fn filter(author: &Pubkey) -> Filter {
            Filter::new()
                .authors([author.bytes()])
                .kinds([TEST_KIND])
                .build()
        }

        fn fold(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Self> {
            // Not registered per-instance; the test installs a single static
            // `Counters` and every `ToyReducer` shares it.
            COUNTERS.folds.fetch_add(1, Ordering::SeqCst);
            let mut reducer = ToyReducer {
                counters: &COUNTERS,
                seen: Vec::new(),
            };
            for note in ndb
                .query(txn, &[Self::filter(author)], 1000)
                .unwrap_or_default()
            {
                reducer.ingest(note.note.key().unwrap());
            }
            Some(reducer)
        }

        fn reduce_delta(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Vec<NoteKey> {
            self.counters.deltas.fetch_add(1, Ordering::SeqCst);
            let mut deferred = Vec::new();
            for key in keys {
                match ndb.get_note_by_key(txn, *key) {
                    // Not yet visible under this snapshot — defer, don't drop.
                    Err(_) => deferred.push(*key),
                    Ok(_) => self.ingest(*key),
                }
            }
            deferred
        }

        fn finalize(&self) -> Vec<NoteKey> {
            self.counters.finalizes.fetch_add(1, Ordering::SeqCst);
            self.seen.clone()
        }
    }

    // A single process-wide counter set for the toy reducer, since `fold` is an
    // associated fn with no instance to carry state. Tests run serially against
    // it; each resets it at the top.
    static COUNTERS: Counters = Counters {
        folds: AtomicU32::new(0),
        deltas: AtomicU32::new(0),
        finalizes: AtomicU32::new(0),
    };

    fn reset_counters() {
        COUNTERS.folds.store(0, Ordering::SeqCst);
        COUNTERS.deltas.store(0, Ordering::SeqCst);
        COUNTERS.finalizes.store(0, Ordering::SeqCst);
    }

    fn test_ndb() -> (Ndb, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        (ndb, dir)
    }

    /// Write one `TEST_KIND` note authored by `kp` and ingest it locally,
    /// mirroring the store's client-ingest path (a hand-built relay frame won't
    /// pass validation). Each note carries a distinct `d` tag so it's a distinct
    /// event. Ingest is async; the caller waits on a subscription.
    fn write_note(ndb: &Ndb, kp: &enostr::FullKeypair, tag: &str) {
        let note = nostrdb::NoteBuilder::new()
            .kind(TEST_KIND as u32)
            .content(tag)
            .created_at(1_700_000_000)
            .start_tag()
            .tag_str("d")
            .tag_str(tag)
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .unwrap();
        let frame = enostr::ClientMessage::event(&note)
            .unwrap()
            .to_json()
            .unwrap();
        ndb.process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
            .unwrap();
    }

    /// Block until `sub` reports at least one ingest (writes commit async).
    fn wait_commit(ndb: &Ndb, sub: Subscription) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while ndb.poll_for_notes(sub, 64).is_empty() {
            assert!(Instant::now() < deadline, "note never committed");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A fresh subscription reports only future ingests, so the cache seeds the
    /// history once and folds every later arrival as a delta — never re-seeding.
    #[test]
    fn seeds_once_then_folds_deltas() {
        reset_counters();
        let (ndb, _dir) = test_ndb();
        let kp = enostr::FullKeypair::generate();
        let mut cache: RealtimeCache<ToyReducer> = RealtimeCache::default();

        // First touch on an empty db seeds an empty reducer and opens the sub.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.poll(&ndb, &txn, &kp.pubkey);
        }
        assert_eq!(cache.stats().full_reloads, 1, "first touch seeds once");

        // Two notes arrive after the subscription exists; each folds as a delta.
        let det = ndb.subscribe(&[ToyReducer::filter(&kp.pubkey)]).unwrap();
        write_note(&ndb, &kp, "one");
        wait_commit(&ndb, det);
        write_note(&ndb, &kp, "two");
        wait_commit(&ndb, det);

        let views = loop {
            let txn = Transaction::new(&ndb).unwrap();
            let v = cache
                .with_views(&ndb, &txn, &kp.pubkey, |views| views.to_vec())
                .unwrap();
            if v.len() == 2 {
                break v;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(views.len(), 2, "both deltas folded in");
        assert_eq!(
            cache.stats().full_reloads,
            1,
            "cache re-seeded instead of folding deltas"
        );
    }

    /// Many reads with no intervening fold share a single finalize; a fold (here
    /// forced via `invalidate`, standing in for a freshly ingested note) makes the
    /// next batch of reads share exactly one more.
    #[test]
    fn finalizes_once_per_fold() {
        reset_counters();
        let (ndb, _dir) = test_ndb();
        let kp = enostr::FullKeypair::generate();
        let mut cache: RealtimeCache<ToyReducer> = RealtimeCache::default();

        let txn = Transaction::new(&ndb).unwrap();
        // Seed, then read 20 times with no new notes.
        for _ in 0..20 {
            cache.with_views(&ndb, &txn, &kp.pubkey, |_| ());
        }
        assert_eq!(
            cache.stats().finalizes,
            1,
            "steady reads re-finalized instead of reusing the memo"
        );

        // A fold invalidates the memo; the next 20 reads share one finalize.
        cache.invalidate(&kp.pubkey);
        for _ in 0..20 {
            cache.with_views(&ndb, &txn, &kp.pubkey, |_| ());
        }
        assert_eq!(
            cache.stats().finalizes,
            2,
            "reads after a fold didn't share a single finalize"
        );
    }

    /// A note committed *after* the read txn a delta was polled with must be
    /// retained and retried, not dropped — the cross-device-edit-lands-mid-frame
    /// case. Recovers by folding a delta, not by re-seeding.
    #[test]
    fn retries_deltas_committed_after_the_read_txn() {
        reset_counters();
        let (ndb, _dir) = test_ndb();
        let kp = enostr::FullKeypair::generate();
        let mut cache: RealtimeCache<ToyReducer> = RealtimeCache::default();
        let det = ndb.subscribe(&[ToyReducer::filter(&kp.pubkey)]).unwrap();

        // Seed the cache (opens its own subscription) before any notes exist.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.poll(&ndb, &txn, &kp.pubkey);
        }

        // Open a stale snapshot, *then* commit a note after it: the note enters
        // the cache's subscription inbox but is invisible to this older txn.
        let stale = Transaction::new(&ndb).unwrap();
        write_note(&ndb, &kp, "beta");
        wait_commit(&ndb, det);

        let changed = cache.poll(&ndb, &stale, &kp.pubkey).changed;
        drop(stale);
        assert!(!changed, "a deferred-only advance reads as a no-op");
        assert_eq!(
            cache.pending_len(&kp.pubkey),
            1,
            "the undrained key is retained for retry, not dropped"
        );

        // A fresh snapshot resolves the retained key as a delta.
        let txn = Transaction::new(&ndb).unwrap();
        let views = cache
            .with_views(&ndb, &txn, &kp.pubkey, |views| views.to_vec())
            .unwrap();
        assert_eq!(views.len(), 1, "the retained delta folded in");
        assert_eq!(cache.pending_len(&kp.pubkey), 0);
        assert_eq!(
            cache.stats().full_reloads,
            1,
            "recovered by folding a delta, not by re-seeding"
        );
    }

    /// A *read* (`with_views`) and the fan-out [`poll`](RealtimeCache::poll) share
    /// one destructive subscription, so whichever runs first takes the note off it.
    /// When a read wins — the production render-then-next-update frame shape, or an
    /// inline chip rendered off-foreground — the note must still surface as
    /// `fresh` on the next poll, else a CLI edit folds locally but never fans out to
    /// the user's other devices. Regression guard for the outbox
    /// ([`CachedAuthor::unsynced`]); before it, this key was folded then silently
    /// dropped from every `poll().fresh`.
    #[test]
    fn a_read_does_not_swallow_fresh_from_the_next_poll() {
        reset_counters();
        let (ndb, _dir) = test_ndb();
        let kp = enostr::FullKeypair::generate();
        let mut cache: RealtimeCache<ToyReducer> = RealtimeCache::default();
        let det = ndb.subscribe(&[ToyReducer::filter(&kp.pubkey)]).unwrap();

        // Seed the cache (opens its own subscription) before any notes exist.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.poll(&ndb, &txn, &kp.pubkey);
        }

        // A note arrives, then a *read* — not the fan-out poll — drains it off the
        // shared subscription and folds it, exactly as an inline chip render does a
        // frame before the next fan-out poll.
        write_note(&ndb, &kp, "one");
        wait_commit(&ndb, det);
        let views = loop {
            let txn = Transaction::new(&ndb).unwrap();
            let v = cache
                .with_views(&ndb, &txn, &kp.pubkey, |views| views.to_vec())
                .unwrap();
            if v.len() == 1 {
                break v;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(views.len(), 1, "the read folded the note in");

        // The read drained the subscription, so the poll's own advance folds
        // nothing new — yet it must still report the read-folded note as `fresh`
        // (and as `changed`, to keep waking) by draining the outbox.
        let txn = Transaction::new(&ndb).unwrap();
        let resp = cache.poll(&ndb, &txn, &kp.pubkey);
        assert_eq!(
            resp.fresh, views,
            "poll must surface the read-folded note as fresh for fan-out"
        );
        assert!(resp.changed, "a non-empty outbox drain reads as changed");

        // Drained exactly once: a second poll with nothing new is a clean no-op.
        let resp = cache.poll(&ndb, &txn, &kp.pubkey);
        assert!(
            resp.fresh.is_empty(),
            "the outbox reports each key only once"
        );
        assert!(!resp.changed);
    }
}
