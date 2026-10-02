//! The board-data engine: [`BoardCache`], a per-account
//! [`notedeck::RealtimeCache`] over the [`HeadwayReducer`] plus the multi-writer
//! shared-board leg. The foreground board and every inline widget read it.

use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::Pubkey;

use crate::event::{self, BoardReducer, BoardView};
use crate::teams;

/// notedeck_headway's [`notedeck::Reducer`] adapter over the pure-data
/// [`BoardReducer`], so a generic [`notedeck::RealtimeCache`] can drive the board
/// fold without the `headway` data crate depending on the `notedeck` app
/// framework (the orphan rule forbids impl'ing a notedeck trait for a headway
/// type here, and the layering shouldn't invert anyway). A newtype whose trait
/// methods forward straight to the existing free functions ([`event::fold_board`]
/// / [`event::reduce_delta`]) and [`BoardReducer::finalize`].
///
/// The reducer holds *all* of the author's boards ([`event::fold_board`] folds an
/// account's whole history), so one cache entry backs the foreground board and
/// every inline reference to any of that author's boards.
struct HeadwayReducer(BoardReducer);

impl notedeck::Reducer for HeadwayReducer {
    type View = BoardView;

    fn filter(author: &Pubkey) -> Filter {
        event::headway_filter(author)
    }

    fn fold(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Self> {
        event::fold_board(ndb, txn, author).map(HeadwayReducer)
    }

    fn reduce_delta(&mut self, ndb: &Ndb, txn: &Transaction, keys: &[NoteKey]) -> Vec<NoteKey> {
        event::reduce_delta(&mut self.0, ndb, txn, keys)
    }

    fn finalize(&self) -> Vec<BoardView> {
        self.0.finalize()
    }
}

/// The single board-data engine for the Headway app: a per-account
/// [`notedeck::RealtimeCache`] over the [`HeadwayReducer`], plus the
/// headway-specific shared-board leg. Shared (behind `Rc<RefCell<…>>`) between the
/// app and everything it registers for inline display.
///
/// The foreground board and every inline widget — the
/// [`KindRenderer`](notedeck::KindRenderer)s and the
/// [`ReferenceParser`](notedeck::ReferenceParser) — read the *same* reducer, so a
/// realtime edit (a CLI move, a remote sync) that [`update`](notedeck::App::update)'s
/// per-frame [`poll`](Self::poll) folds in is immediately visible to an inline
/// chip drawn in another app, not just to the open board.
///
/// The author-keyed leg ([`authors`](Self::authors)) is the reusable
/// subscribe/seed/delta/memoize skeleton, generic in notedeck; the shared-board
/// leg ([`shared`](Self::shared)) stays here because it folds by board
/// *coordinate* (multi-writer), not by author.
#[derive(Default)]
pub(crate) struct BoardCache {
    /// Per-account board fold: [`event::fold_board`] folds an account's whole
    /// board history into one reducer, so a single entry serves all of that
    /// author's boards with no per-board refold. Seed-once + delta, the memoized
    /// finalize, and the post-snapshot deferral are all owned by the generic
    /// cache; [`HeadwayReducer`] supplies only the fold itself.
    authors: notedeck::RealtimeCache<HeadwayReducer>,
    /// Joined shared boards, keyed by board coordinate (`30619:owner:slug`). Unlike
    /// [`authors`](Self::authors) these are multi-writer, folded by *coordinate*
    /// via [`event::fold_shared_board`] so every member's events are gathered, and
    /// re-folded whenever a new kind-1081 envelope for the channel arrives (every
    /// shared edit is one, so the envelope stream is the universal change signal).
    shared: HashMap<String, SharedBoard>,
}

/// One joined shared board within [`BoardCache`]: the multi-writer reducer folded
/// by board coordinate, plus a subscription to the channel's kind-1081 envelopes
/// that signals when to re-fold.
#[derive(Default)]
struct SharedBoard {
    reducer: Option<BoardReducer>,
    /// Memoized finalize of `reducer` — the shared-board analogue of the memo the
    /// generic [`RealtimeCache`](notedeck::RealtimeCache) keeps for the per-author
    /// leg, rebuilt lazily after a re-fold. Shared so [`BoardRef`] can hand it
    /// out without copying.
    finalized: Option<Rc<[BoardView]>>,
    /// Subscription to `{kinds:[1081], authors:[team_pubkey]}` — every edit to the
    /// board is published as one such envelope, so a poll reporting new notes means
    /// the board changed (a member's edit unwrapped) and we re-fold.
    sub: Option<Subscription>,
}

/// A zero-copy handle to one folded board: the memoized board set it lives in
/// plus its position there. Derefs to the [`BoardView`]; cloning bumps a
/// refcount rather than copying cards.
///
/// The foreground render resolves the active board through [`BoardCache`] and
/// then has to drop the cache borrow before drawing, because a description that
/// references another card resolves through the same cache mid-draw. A plain
/// `&BoardView` can't outlive that borrow, and an owned copy deep-clones every
/// card, comment and activity row each frame; this handle keeps the memo alive
/// instead.
#[derive(Clone)]
pub(crate) struct BoardRef {
    boards: Rc<[BoardView]>,
    index: usize,
}

impl BoardRef {
    /// The board `board_id` authored by `author` within `boards`, if present.
    pub(crate) fn find(boards: Rc<[BoardView]>, author: &[u8; 32], board_id: &str) -> Option<Self> {
        let index = boards
            .iter()
            .position(|v| v.id == board_id && &v.author == author)?;
        Some(Self { boards, index })
    }

    /// The first board in `boards` — a shared fold's only board — if it has one.
    fn first(boards: Rc<[BoardView]>) -> Option<Self> {
        (!boards.is_empty()).then_some(Self { boards, index: 0 })
    }
}

impl Deref for BoardRef {
    type Target = BoardView;

    fn deref(&self) -> &BoardView {
        &self.boards[self.index]
    }
}

/// What one [`BoardCache::poll_shared`] pass found.
#[derive(Default)]
pub(crate) struct SharedPoll {
    /// Freshly-arrived envelope keys. The envelope, not the unwrapped rumor, is
    /// the sync unit (the rumor is skipped by
    /// [`notedeck::fan_out_unseen_notes`]'s `is_rumor` guard).
    pub(crate) fresh: Vec<NoteKey>,
    /// A channel subscribed on this pass and so deliberately didn't fold yet (see
    /// [`BoardCache::poll_shared`]). The caller has to schedule another frame, or
    /// the fold waits for whatever repaints next.
    pub(crate) subscribed: bool,
}

impl BoardCache {
    /// Advance `author`'s reducer and report the change — the per-frame pump
    /// called from [`update`](notedeck::App::update). Fan out
    /// [`fresh`](notedeck::PollResponse::fresh) and wake on
    /// [`changed`](notedeck::PollResponse::changed). Thin delegate to the generic
    /// [`RealtimeCache`](notedeck::RealtimeCache), which owns the
    /// subscribe/seed/delta/pending discipline; a chip's lazy fold-on-render goes
    /// through [`with_boards`](Self::with_boards).
    pub(crate) fn poll(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
    ) -> notedeck::PollResponse {
        self.authors.poll(ndb, txn, author)
    }

    /// Advance `author`'s reducer, (re)finalize it *at most once per fold*, and run
    /// `read` against the memoized [`BoardView`]s. `None` only when the reducer
    /// hasn't seeded yet. Delegates to
    /// [`RealtimeCache::with_views`](notedeck::RealtimeCache::with_views): the
    /// foreground board and every inline widget resolve through it, so on a steady
    /// frame the first read finalizes and the rest reuse the memo. `read` runs
    /// under the cache borrow, so it should pull out what it needs (an id, a
    /// title) rather than draw; a reader that must draw from the boards takes
    /// [`board`](Self::board) or [`all_boards`](Self::all_boards) instead.
    pub(crate) fn with_boards<R>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&[BoardView]) -> R,
    ) -> Option<R> {
        self.authors.with_views(ndb, txn, author, read)
    }

    /// Fold and pick a single board (`board_id`) authored by `author`, seeding the
    /// reducer on first touch. `None` before the first fold or when no such board
    /// exists. The foreground board, a cross-board move target and an inline board
    /// widget all resolve through this. A [`BoardRef`] into the memo, so it copies
    /// nothing.
    #[profiling::function]
    pub(crate) fn board(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        board_id: &str,
    ) -> Option<BoardRef> {
        BoardRef::find(self.all_boards(ndb, txn, author)?, author.bytes(), board_id)
    }

    /// Every board `author` currently holds, seeding on first touch; `None` until
    /// the reducer seeds. The foreground render derives *both* the active board
    /// and the switcher list from this single fold (memoized, see
    /// [`with_boards`](Self::with_boards)) rather than finalizing per read. A
    /// shared handle to the memo, so a frame's read is a refcount bump, not a copy
    /// of every board.
    #[profiling::function]
    pub(crate) fn all_boards(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
    ) -> Option<Rc<[BoardView]>> {
        self.authors.views(ndb, txn, author)
    }

    /// Advance every joined shared board: ensure a kind-1081 envelope subscription
    /// per channel and re-fold (by coordinate, gathering all members) any whose
    /// subscription reports new envelopes or that hasn't folded yet.
    ///
    /// Re-fold is full each time rather than incremental: shared boards are few and
    /// only re-fold when a member actually edits (every edit is one 1081 envelope).
    pub(crate) fn poll_shared(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        teams: &[teams::Team],
    ) -> SharedPoll {
        let mut poll = SharedPoll::default();
        // One pass per *board*, not per key-share: a board with several channels
        // (a rotation epoch, or a re-seal that ran under a fresh root) has its
        // content split across them irrecoverably, so it folds — and watches —
        // the union. Two key-shares naming one coordinate must not become two
        // competing entries for the same cache slot.
        let mut done: Vec<&str> = Vec::new();
        for team in teams {
            if done.contains(&team.board_addr.as_str()) {
                continue;
            }
            done.push(&team.board_addr);
            let channels = teams::board_channel_pubkeys(teams, &team.board_addr);
            if channels.is_empty() {
                continue;
            }
            let entry = self.shared.entry(team.board_addr.clone()).or_default();
            if entry.sub.is_none() {
                entry.sub = ndb.subscribe(&[teams::envelope_filter(&channels)]).ok();
                if entry.sub.is_some() {
                    // Don't fold on the pass that subscribes — `txn` was opened
                    // before this subscription existed, and an envelope that
                    // committed in between is in neither: too new for the snapshot,
                    // too old for a subscription that only reports later ingests.
                    // Since this leg only re-folds when the subscription reports
                    // something, a board whose *only* envelope landed in that
                    // window would never fold at all. Same hazard, and the same
                    // shape of fix, as the author leg's seed
                    // (`notedeck::RealtimeCache::advance`).
                    //
                    // Leaving `reducer` unset defers the fold to the next toucher —
                    // the next pass here, or `shared_board` during this frame's
                    // render — each of which opens its own transaction after this
                    // one is dropped, so its snapshot postdates the subscribe.
                    poll.subscribed = true;
                    continue;
                }
            }
            let polled = match entry.sub {
                Some(sub) => ndb.poll_for_notes(sub, 64),
                None => Vec::new(),
            };
            if !polled.is_empty() || entry.reducer.is_none() {
                entry.reducer = event::fold_shared_board(ndb, txn, &team.board_addr, &channels);
                entry.finalized = None;
            }
            poll.fresh.extend(polled);
        }
        poll
    }

    /// Fold (memoized) a joined shared board by coordinate and return its view.
    /// `None` until its definition has folded in. Seeds the fold lazily so a render
    /// before the next [`poll_shared`] still resolves.
    pub(crate) fn shared_board(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        board_addr: &str,
        team_pubkeys: &[Pubkey],
    ) -> Option<BoardRef> {
        let entry = self.shared.entry(board_addr.to_string()).or_default();
        if entry.reducer.is_none() {
            entry.reducer = event::fold_shared_board(ndb, txn, board_addr, team_pubkeys);
        }
        if entry.finalized.is_none() {
            entry.finalized = Some(entry.reducer.as_ref()?.finalize().into());
        }
        // fold_shared_board folds a single coordinate, so its finalize yields the
        // one board (empty until the board definition has arrived).
        BoardRef::first(Rc::clone(entry.finalized.as_ref()?))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{BoardSummary, board_summaries, store};
    use nostrdb::{Ndb, SubscriptionStream};
    use nostrdb_net::FullKeypair;
    use notedeck_testing::fixtures::test_config;
    use std::time::{Duration, Instant};

    /// A headless harness driving a [`BoardCache`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors the
    /// `store::tests::TestNdb` poll-loop pattern (ingest is async).
    pub(crate) struct TestSync {
        pub(crate) ndb: Ndb,
        _dir: tempfile::TempDir,
        pub(crate) kp: FullKeypair,
        cache: BoardCache,
    }

    impl TestSync {
        pub(crate) fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
                cache: BoardCache::default(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// One poll cycle: advance this account's reducer. Returns whether it
        /// changed (a seed or a folded-in delta) this call.
        pub(crate) fn poll(&mut self) -> bool {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.poll(&self.ndb, &txn, &self.kp.pubkey).changed
        }

        /// Seed the demo board, returning how many events it wrote (for
        /// [`seed_and_settle`](Self::seed_and_settle) to await).
        pub(crate) fn seed(&mut self) -> usize {
            seed_demo(&self.ndb, &self.kp)
        }

        /// Seed the demo board and fold until the *whole* seed has committed.
        ///
        /// Ingest is async, so quiescence is proven by count, not by inspecting
        /// the board: [`seed`](Self::seed) reports how many events it wrote and
        /// this returns only once the writer has delivered them all. That is what
        /// lets the idle / finalize tests assert "no further folds" without racing
        /// a straggler, and it holds no matter which seed event lands last.
        ///
        /// Subscribe *before* seeding so every ingest wakes the stream; a prior
        /// [`poll`](Self::poll) must have primed the cache's own subscription so
        /// the seed folds in as deltas (one full reload, not a per-event walk).
        pub(crate) async fn seed_and_settle(&mut self) {
            let mut stream = ingest_stream(&self.ndb, &self.kp.pubkey);
            let n = self.seed();
            await_ingested(&mut stream, n).await;
            self.poll();
        }

        /// Fold and pick the default board out of the cache, if present.
        fn view(&mut self) -> Option<BoardRef> {
            self.board(store::BOARD_ID)
        }

        /// Fold and pick an arbitrary board — exercises reading a board other than
        /// the default out of the one per-account reducer.
        fn board(&mut self, board_id: &str) -> Option<BoardRef> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.board(&self.ndb, &txn, &self.kp.pubkey, board_id)
        }

        /// Every board folded for this account, summarized for the switcher.
        fn boards(&mut self) -> Vec<BoardSummary> {
            let txn = Transaction::new(&self.ndb).unwrap();
            let boards = self.cache.all_boards(&self.ndb, &txn, &self.kp.pubkey);
            board_summaries(boards.as_deref().unwrap_or(&[]))
        }

        /// Fold until the default board satisfies `pred` (ingest is async).
        pub(crate) async fn wait<F: Fn(&BoardView) -> bool>(&mut self, pred: F) {
            self.wait_until(|t| t.view().is_some_and(|v| pred(&v)))
                .await;
        }

        /// Shared loop for [`wait`](Self::wait): pump the reducer until `done`
        /// holds, awaiting the writer's own ingest
        /// notification (see [`await_ingest`]) between folds rather than polling
        /// against a wall-clock deadline — the only race-free way to wait on an
        /// async ingest. Used when the test waits for a *specific* change to
        /// appear; [`seed_and_settle`](Self::seed_and_settle) instead waits for the
        /// whole seed to commit by count.
        async fn wait_until(&mut self, done: impl Fn(&mut Self) -> bool) {
            let mut stream = ingest_stream(&self.ndb, &self.kp.pubkey);
            while !done(self) {
                await_ingest(&mut stream).await;
            }
        }
    }

    /// Open a live await-handle on `author`'s headway events. Subscribing before
    /// a wait means every note the async writer ingests *after* this point wakes
    /// [`await_ingest`], so the fold loops advance on the writer's own
    /// notification instead of a wall-clock sleep.
    fn ingest_stream(ndb: &Ndb, author: &Pubkey) -> SubscriptionStream {
        let sub = ndb.subscribe(&[event::headway_filter(author)]).unwrap();
        SubscriptionStream::new(ndb.clone(), sub)
    }

    /// Await the next batch of ingested notes on `stream`, returning their keys.
    async fn await_ingest(stream: &mut SubscriptionStream) -> Vec<NoteKey> {
        notedeck_testing::await_batch(stream).await
    }

    /// Drain `stream` until the async writer has delivered `n` ingested notes in
    /// total (summing each batch). `n` is the exact number of events the seed
    /// wrote (see [`seed_demo`] / [`store::seed_demo_board`]), so this returns the
    /// instant the whole seed has committed — a quiescence signal that, unlike a
    /// board-state predicate, can't be satisfied while trailing events are still
    /// in flight and doesn't depend on which event the seed writes last.
    ///
    /// Bounded: a seed count that can never be reached fails the test with the
    /// running total instead of parking the thread until CI's own timeout.
    async fn await_ingested(stream: &mut SubscriptionStream, n: usize) {
        notedeck_testing::await_notes_async(stream, n).await
    }

    pub(crate) fn total_cards(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    /// Seed the populated demo board for the sync tests to fold and act on. The
    /// production seed is card-less; the fixture lives in [`store::seed_demo_board`].
    /// Seeded in the past so follow-up edits (stamped with the wall clock)
    /// always sort after it.
    fn seed_demo(ndb: &Ndb, kp: &FullKeypair) -> usize {
        store::seed_demo_board(
            ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        )
    }

    /// Subscribing before seeding, then polling, materialises the whole board
    /// from events already in ndb.
    #[tokio::test]
    async fn poll_materialises_the_board() {
        let mut t = TestSync::new();
        // Subscribe (via poll) first so the seed's ingests are reported as new
        // notes, then seed and fold until the writer has delivered every event —
        // a card count can hold mid-fold, asserting a half-materialised layout.
        t.poll();
        t.seed_and_settle().await;
        let view = t.view().expect("board loaded");
        assert_eq!(
            view.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Backlog", "Todo", "In Progress", "In Review", "Done"]
        );
        assert_eq!(view.columns[0].cards.len(), 3);
    }

    /// An edit ingested after the initial load is picked up on a later poll —
    /// the cache reflects the change, not a stale snapshot.
    #[tokio::test]
    async fn poll_reloads_on_change() {
        let mut t = TestSync::new();
        t.poll();
        // Fully materialise the seed first (Todo settles at 2 once the drag card
        // moves out), so the edit below folds against a stable board.
        t.seed_and_settle().await;

        // Apply against the cached pre-edit view (as render does).
        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.secret(), None),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Fresh card".to_string(),
                    description: String::new(),
                    labels: vec![],
                    parent: None,
                },
                &mut store::NoPublish,
            );
        }

        // The new card only appears if a later poll re-folded the board.
        t.wait(|v| {
            v.columns[1]
                .cards
                .last()
                .is_some_and(|c| c.title == "Fresh card")
        })
        .await;
        let view = t.view().expect("board loaded");
        assert_eq!(view.columns[1].cards.last().unwrap().title, "Fresh card");
    }

    /// Two boards under one account are both discoverable, and switching the
    /// active board re-picks from the existing reducer — no full re-fold, since
    /// the target board's events are already folded in.
    #[tokio::test]
    async fn switching_board_repicks_without_refold() {
        let mut t = TestSync::new();
        // Subscribe first so the seeds' ingests arrive as subscription deltas.
        t.poll();
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        let n = t.seed()
            + store::seed_board(
                &t.ndb,
                &t.kp.pubkey,
                &t.secret(),
                "work",
                "Work",
                &mut store::NoPublish,
            );

        // Both seeds counted together, because the writer is free to commit them
        // out of order: "the 'work' board has appeared" would hold while demo
        // events were still in flight, and folding those stragglers after the
        // snapshot below is exactly what this test must not race.
        await_ingested(&mut stream, n).await;
        t.poll();
        let folds = t.cache.authors.stats().full_reloads;

        // Both boards are discoverable from the one reducer.
        let boards = t.boards();
        assert!(
            boards
                .iter()
                .any(|b| b.id == store::BOARD_ID && b.title == "Headway")
        );
        assert!(boards.iter().any(|b| b.id == "work" && b.title == "Work"));

        // Reading the 'work' board picks it out of the same reducer — a different
        // board, no full re-fold.
        let view = t.board("work").expect("work board");
        assert_eq!(view.id, "work");
        assert_eq!(view.title, "Work");
        assert_eq!(total_cards(&view), 0, "fresh board has no cards");
        assert_eq!(
            t.cache.authors.stats().full_reloads,
            folds,
            "reading another board must not trigger a full re-fold"
        );
    }

    /// Once quiescent, polling with nothing new must NOT re-fold — this is the
    /// whole point of the cache (no per-frame walk of the event history).
    #[tokio::test]
    async fn poll_does_not_refold_when_idle() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        assert!(
            !t.poll(),
            "cache re-folded with no new events — the per-frame fold is back"
        );
    }

    /// A change after the initial load is absorbed incrementally: the live
    /// reducer folds the delta, with no additional full-history re-fold. Guards
    /// against a regression to reload-on-every-change.
    #[tokio::test]
    async fn poll_folds_changes_as_a_delta() {
        let mut t = TestSync::new();
        t.poll();
        t.seed_and_settle().await;

        // Seeding does exactly one full fold; everything since is incremental.
        assert_eq!(
            t.cache.authors.stats().full_reloads,
            1,
            "seeding should fold the history once"
        );

        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &store::Signer::new(&t.secret(), None),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Delta card".to_string(),
                    description: String::new(),
                    labels: vec![],
                    parent: None,
                },
                &mut store::NoPublish,
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3).await;

        assert_eq!(
            t.cache.authors.stats().full_reloads,
            1,
            "the edit triggered a full re-fold instead of a delta"
        );
    }

    /// [`BoardCache`] folds the history once on first touch and then absorbs later
    /// edits as deltas via its subscription — never re-walking the history per
    /// frame. The render-path counterpart to [`poll_folds_changes_as_a_delta`],
    /// exercising the `&Ndb` fold-on-read the inline widgets use.
    #[tokio::test]
    async fn board_cache_folds_once_then_deltas() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = BoardCache::default();

        // One read cycle (what a renderer does each frame): bring the cached
        // reducer up to date and fold out the board.
        let fold = |cache: &mut BoardCache, ndb: &Ndb| -> Option<BoardRef> {
            let txn = Transaction::new(ndb).unwrap();
            cache.board(ndb, &txn, &kp.pubkey, store::BOARD_ID)
        };

        // Subscribe (seeding an empty reducer) before the board exists, so the
        // seed's ingests arrive as subscription deltas rather than a re-fold.
        fold(&mut cache, &ndb);
        let mut stream = ingest_stream(&ndb, &kp.pubkey);
        seed_demo(&ndb, &kp);

        // Fold each ingest in as the writer delivers it (ingest is async on a
        // writer thread), until the whole board has materialised.
        while fold(&mut cache, &ndb).is_none_or(|v| total_cards(&v) != 7) {
            await_ingest(&mut stream).await;
        }

        // Exactly one full fold — the initial empty seed; every event since
        // (the whole seeded board) folded in incrementally as deltas.
        assert_eq!(
            cache.authors.stats().full_reloads,
            1,
            "board cache re-walked the history instead of folding deltas"
        );
    }

    /// A frame with several inline references resolves and renders many cards, but
    /// [`BoardCache`] must [`finalize`](event::BoardReducer::finalize) the reducer
    /// only *once* per frame — the first read builds the boards, every later read
    /// reuses them. Only a fresh fold (a new note) invalidates the memo.
    #[tokio::test]
    async fn board_cache_finalizes_once_per_fold() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = BoardCache::default();

        // Prime the cache's own (internal) subscription and open a read stream to
        // await the writer, both before seeding so every seeded event is reported.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
        }
        let mut stream = ingest_stream(&ndb, &kp.pubkey);
        let n = seed_demo(&ndb, &kp);

        // Wait for the seed to fully commit *and quiesce* before measuring. Any
        // straggler arriving mid-frame below would fold and re-finalize, breaking
        // the "no fold ⇒ no finalize" invariant this test measures — and a board
        // predicate (e.g. `total_cards == 7`) can hold while trailing notes are
        // still in flight. Counting the seed's own events is the race-free signal:
        // once the writer has delivered all `n`, nothing else is coming.
        await_ingested(&mut stream, n).await;
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
        }

        // A steady frame with no new notes: many reads (what N inline references
        // cost — resolve then render each) reuse the memo built while materialising,
        // finalizing zero more times.
        let steady = cache.authors.stats().finalizes;
        let txn = Transaction::new(&ndb).unwrap();
        for _ in 0..20 {
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
            cache.all_boards(&ndb, &txn, &kp.pubkey);
        }
        drop(txn);
        assert_eq!(
            cache.authors.stats().finalizes - steady,
            0,
            "reads with no intervening fold re-finalized instead of reusing the memo"
        );

        // A fold invalidates the memo (see `RealtimeCache::advance`); the next
        // frame's many reads then share *exactly one* finalize. Simulate the
        // invalidation a freshly ingested note performs, then read many times.
        cache.authors.invalidate(&kp.pubkey);
        let after_fold = cache.authors.stats().finalizes;
        let txn = Transaction::new(&ndb).unwrap();
        for _ in 0..20 {
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
            cache.all_boards(&ndb, &txn, &kp.pubkey);
        }
        drop(txn);
        assert_eq!(
            cache.authors.stats().finalizes - after_fold,
            1,
            "reads after a fold didn't share a single finalize"
        );
    }

    /// A note committed *after* the read txn the delta was polled with — the
    /// shape of a cross-device edit landing mid-frame — must still fold in, not
    /// vanish until an app restart. The subscription drains the key immediately,
    /// but a stale snapshot can't see the note yet; the cache has to retain the
    /// key and retry it, rather than drop it (the bug this guards against).
    #[test]
    fn board_cache_retries_deltas_committed_after_the_read_txn() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &test_config()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = BoardCache::default();

        // Block until `sub` reports an ingest (the writer commits asynchronously).
        // Reads the subscription inbox, not the db, so it needs no transaction —
        // which lets us wait while holding a stale read txn open below.
        let wait_commit = |sub| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while ndb.poll_for_notes(sub, 64).is_empty() {
                assert!(Instant::now() < deadline, "note never committed");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let has_board = |cache: &BoardCache, id: &str| {
            cache
                .authors
                .reducer(&kp.pubkey)
                .is_some_and(|r| r.0.finalize().iter().any(|b| b.id == id))
        };
        let seed = |id: &str, title: &str| {
            store::seed_board(
                &ndb,
                &kp.pubkey,
                &kp.secret_key.secret_bytes(),
                id,
                title,
                &mut store::NoPublish,
            );
        };

        // Board "alpha" exists before the cache subscribes, so it lands in the
        // seed fold (a detector sub tells us when the async write has committed).
        let det = ndb.subscribe(&[event::headway_filter(&kp.pubkey)]).unwrap();
        seed("alpha", "Alpha");
        wait_commit(det);
        // Two advances: the first subscribes, the next seeds from a snapshot
        // taken after it (see `RealtimeCache::advance`).
        for _ in 0..2 {
            let txn = Transaction::new(&ndb).unwrap();
            cache.poll(&ndb, &txn, &kp.pubkey);
        }
        assert!(has_board(&cache, "alpha"), "seed fold picked up alpha");

        // Open a snapshot, *then* commit board "beta" after it: beta's note is now
        // in the subscription inbox but invisible to this older transaction.
        let stale = Transaction::new(&ndb).unwrap();
        seed("beta", "Beta");
        wait_commit(det);

        // Advancing under the stale snapshot drains beta's key but can't read the
        // note — it must be retained, not folded and not lost.
        let changed = cache.poll(&ndb, &stale, &kp.pubkey).changed;
        drop(stale);
        assert!(!changed, "a deferred-only advance reads as a no-op");
        assert!(
            !has_board(&cache, "beta"),
            "beta isn't visible under the stale snapshot yet"
        );
        assert!(
            cache.authors.pending_len(&kp.pubkey) > 0,
            "the undrained key is retained for retry, not dropped"
        );

        // The next advance opens a fresh snapshot; the retained key now resolves.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.poll(&ndb, &txn, &kp.pubkey);
        }
        assert!(
            has_board(&cache, "beta"),
            "the retained delta folds in on the next, fresher advance"
        );
        assert_eq!(cache.authors.pending_len(&kp.pubkey), 0);
        assert_eq!(
            cache.authors.stats().full_reloads,
            1,
            "recovered by folding a delta, not by re-seeding the whole history"
        );
    }

    /// An envelope that commits between the caller's transaction and the shared
    /// leg's *first* subscribe must still fold. This leg only re-folds when its
    /// subscription reports something, so a board whose only envelope landed in
    /// that window would never fold at all — the board stays blank for the life of
    /// the process, not just for a frame. The author-leg twin of this is
    /// `notedeck::realtime_cache`'s
    /// `seeds_a_note_committed_between_the_callers_txn_and_the_first_subscribe`.
    #[tokio::test]
    async fn shared_board_folds_an_envelope_that_landed_before_the_first_subscribe() {
        let mut t = TestSync::new();
        let mut root = [0u8; 32];
        root[0] = 0x33;
        root[31] = 0x44;
        assert!(t.ndb.add_team_root(&root));
        let channel = store::SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("keys"),
        };
        let team = teams::Team {
            team_root: hex::encode(root),
            board_addr: event::board_address(&t.kp.pubkey, store::BOARD_ID),
            epoch: None,
            shared_at: 0,
        };
        let teams = vec![team.clone()];
        let team_pubkey = channel.keys.team_keypair.pubkey;

        // The frame opens its read transaction first, as every caller does...
        let txn = Transaction::new(&t.ndb).unwrap();

        // ...and the board's one and only envelope commits after it, while the
        // cache has still never been touched for this coordinate.
        let mut stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
        ];
        store::ingest_signed(
            &t.ndb,
            event::build_board(store::BOARD_ID, "Shared", "", &cols),
            &store::Signer::shared(&t.secret(), &channel),
            &mut store::NoPublish,
        );
        await_ingest(&mut stream).await;

        // First touch: subscribes, from a frame whose snapshot predates the commit.
        t.cache.poll_shared(&t.ndb, &txn, &teams);
        drop(txn);

        // No further envelope is ever published, so nothing re-arms the fold; the
        // board has to come from a later pass seeding off a fresher snapshot.
        let txn = Transaction::new(&t.ndb).unwrap();
        t.cache.poll_shared(&t.ndb, &txn, &teams);
        let view = t
            .cache
            .shared_board(
                &t.ndb,
                &txn,
                &team.board_addr,
                std::slice::from_ref(&team_pubkey),
            )
            .expect("the definition that landed before the subscribe must fold in");
        assert_eq!(view.title, "Shared");
    }

    /// The shared-board read path: an edit applied over an SNS channel is folded
    /// by the cache's multi-writer `poll_shared`/`shared_board` (by coordinate),
    /// and the stored issue is an unwrapped rumor — the same envelope the 1081
    /// subscription reports for fan-out. Exercises the read half of the shared
    /// wiring against a bare ndb.
    #[tokio::test]
    async fn shared_board_folds_via_cache() {
        let mut t = TestSync::new();
        let mut root = [0u8; 32];
        root[0] = 0x11;
        root[31] = 0x22;
        assert!(t.ndb.add_team_root(&root));
        let channel = store::SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(&root).expect("keys"),
        };
        let team = teams::Team {
            team_root: hex::encode(root),
            board_addr: event::board_address(&t.kp.pubkey, store::BOARD_ID),
            epoch: None,
            shared_at: 0,
        };
        let teams = vec![team.clone()];

        // Seal the board definition into the channel (a shared board has no
        // plaintext leg, and the shared fold gathers only team-sealed rumors), then
        // establish the shared 1081 subscription *before* editing so it reports our
        // own envelope when it lands.
        let cols = vec![
            event::ColumnDef::new("backlog", "Backlog"),
            event::ColumnDef::new("todo", "Todo"),
            event::ColumnDef::new("done", "Done"),
        ];
        store::ingest_signed(
            &t.ndb,
            event::build_board(store::BOARD_ID, "Headway", "", &cols),
            &store::Signer::shared(&t.secret(), &channel),
            &mut store::NoPublish,
        );
        t.poll();
        t.wait(|v| v.id == store::BOARD_ID).await;
        {
            let txn = Transaction::new(&t.ndb).unwrap();
            t.cache.poll_shared(&t.ndb, &txn, &teams);
        }

        // Add a card over the SNS channel — wrapped into a 1081 envelope, ingested
        // (and unwrapped) locally.
        let view = t.view().expect("board");
        let mut author_stream = ingest_stream(&t.ndb, &t.kp.pubkey);
        store::apply(
            &t.ndb,
            store::BOARD_ID,
            &view,
            &t.kp.pubkey,
            &store::Signer::new(&t.secret(), Some(&channel)),
            store::BoardAction::AddCard {
                col: 0,
                title: "Sealed".to_string(),
                description: String::new(),
                labels: vec![],
                parent: None,
            },
            &mut store::NoPublish,
        );

        // Re-fold the shared board (multi-writer) until the sealed card surfaces.
        let card = loop {
            let found = {
                let txn = Transaction::new(&t.ndb).unwrap();
                t.cache.poll_shared(&t.ndb, &txn, &teams);
                t.cache
                    .shared_board(
                        &t.ndb,
                        &txn,
                        &team.board_addr,
                        std::slice::from_ref(&channel.keys.team_keypair.pubkey),
                    )
                    .and_then(|v| {
                        v.columns
                            .iter()
                            .flat_map(|c| c.cards.iter())
                            .find(|c| c.title == "Sealed")
                            .map(|c| c.id)
                    })
            };
            if let Some(id) = found {
                break id;
            }
            await_ingest(&mut author_stream).await;
        };

        let txn = Transaction::new(&t.ndb).unwrap();
        assert!(
            t.ndb.get_note_by_id(&txn, card.bytes()).unwrap().is_rumor(),
            "a shared-board edit must be stored as an unwrapped rumor"
        );
    }
}
