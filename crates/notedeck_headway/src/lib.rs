use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use enostr::{Pubkey, RelayId};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{App, AppContext, AppResponse, ColorTheme, ExplicitPublishApi};

pub use headway::{event, store};

mod ui;

pub use ui::{BoardUiState, board_inline_ui, card_inline_ui, issue_inline_ui};
use ui::{board_ui, empty_state};

use event::{BoardReducer, BoardView};

/// A [`store::Publisher`] that fans every locally-ingested board event out to
/// the account's "private" relays (NIP-65 4th-entry marker) so the board syncs
/// across the user's own devices. With no private relay marked the relay set is
/// empty and this behaves exactly like [`store::NoPublish`] (local-only).
///
/// We publish plaintext board events, so they can only safely reach a private
/// (AUTH/wireguard) relay. TODO: PNS-encrypt these events (as dave does for its
/// session state via `wrap_pns`) and then we could also fan them out to the
/// user's *public* write relays without leaking board contents.
struct PrivateRelayPublisher<'o, 'a> {
    api: ExplicitPublishApi<'o, 'a>,
    relays: Vec<RelayId>,
}

impl store::Publisher for PrivateRelayPublisher<'_, '_> {
    fn publish(&mut self, event_frame: &str) {
        if self.relays.is_empty() {
            return;
        }
        // `ingest` hands us a ["EVENT", {…}] frame; `publish_event_json` wants
        // the bare event object, which the outbox re-frames per relay.
        match serde_json::from_str::<serde_json::Value>(event_frame)
            .ok()
            .and_then(|frame| frame.get(1).cloned())
        {
            Some(event) => self
                .api
                .publish_event_json(event.to_string(), self.relays.clone()),
            None => {} // malformed frame; local ingest already happened
        }
    }
}

/// A Linear/Trello-style issue & todo tracker app for notedeck.
///
/// The board is backed by nostr events in the local nostrdb: [`BoardSync`] keeps
/// a long-lived reducer over the account's events and the [`BoardView`] folded
/// from them, folding only freshly-arrived notes in as an ndb subscription
/// reports them — not re-walking the history every frame. Every edit is turned
/// into a signed event that is ingested locally (see [`store`]). There is
/// deliberately no relay publishing yet.
pub struct Headway {
    /// Which board this instance manages (single board for now).
    board_id: String,
    /// Transient, per-board UI state.
    state: BoardUiState,
    /// Subscription-backed cache of the reduced board (egui-free, so it's
    /// unit-testable against a bare `Ndb`).
    sync: BoardSync,
    /// Whether we've already auto-seeded a board this session, so we don't try
    /// to seed twice while the first seed is still materialising.
    seeded: bool,
    /// Countdown of follow-up repaints after an async ingest, so we keep waking
    /// up to poll the subscription until the writer thread goes quiet.
    repaint_frames: u8,
}

impl Default for Headway {
    fn default() -> Self {
        Self {
            board_id: store::BOARD_ID.to_string(),
            state: BoardUiState::default(),
            sync: BoardSync::default(),
            seeded: false,
            repaint_frames: 0,
        }
    }
}

impl Headway {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule a short burst of repaints so a just-ingested event (ingest is
    /// async, on a writer thread) gets polled and surfaced promptly.
    fn wake(&mut self) {
        self.repaint_frames = 8;
    }

    /// Burn down the repaint countdown, requesting a delayed repaint each step.
    fn pump_repaint(&mut self, ui: &egui::Ui) {
        if self.repaint_frames > 0 {
            self.repaint_frames -= 1;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}

/// Subscription-backed, *online* reducer for one account's board.
///
/// Holds a live nostrdb subscription to the account's headway events **and a
/// long-lived [`BoardReducer`]** that persists across frames. The first poll
/// folds the whole history once to seed the reducer; every later poll feeds it
/// only the freshly-arrived notes ([`event::reduce_delta`]) — an incremental
/// step, not a re-walk of the history. The reducer is rebuilt from scratch only
/// on a first load or an account switch.
///
/// This is correct because the fold is commutative and idempotent: applying a
/// delta to an up-to-date reducer lands in the same state as a full re-fold.
///
/// Deliberately free of any egui dependency so it can be unit-tested against a
/// bare `Ndb`.
#[derive(Default)]
struct BoardSync {
    /// The last reduced board. `None` means "no such board" (or not loaded yet).
    view: Option<BoardView>,
    /// The accumulator, kept alive across polls so new notes fold in
    /// incrementally. `None` until the first full fold (and again after an
    /// account switch), which is the signal to re-fold from scratch.
    reducer: Option<BoardReducer>,
    /// Live subscription to `sub_author`'s headway events; polling it is how we
    /// learn the board changed (including our own async ingests landing).
    sub: Option<Subscription>,
    /// The account `sub`/`reducer`/`view` belong to, so we resubscribe and
    /// re-fold on an account switch.
    sub_author: Option<Pubkey>,
    /// Test-only count of full-history re-folds, used to assert that an ordinary
    /// change folds in as a delta rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

impl BoardSync {
    /// Ensure a live subscription to `author`, drain it, and update the cached
    /// board. Returns `true` if the board was (re)reduced this call — a first
    /// load, an account switch, or new notes folded in — so the caller can
    /// schedule follow-up repaints. The cached board is read via
    /// [`view`](Self::view).
    fn poll(&mut self, ndb: &mut Ndb, author: &Pubkey, board_id: &str) -> bool {
        self.sync_subscription(ndb, author);

        let Some(sub) = self.sub else {
            // Subscribe failed: degrade to a full reload each frame so edits show.
            self.reload(ndb, author, board_id);
            return true;
        };

        let keys = ndb.poll_for_notes(sub, 64);

        // First load (or just resubscribed): fold the whole history once to seed
        // the long-lived reducer.
        if self.reducer.is_none() {
            self.reload(ndb, author, board_id);
            return true;
        }

        // Nothing new since the last poll: the cached view stands, no re-fold.
        if keys.is_empty() {
            return false;
        }

        // Incremental: fold only the freshly-arrived notes into the live reducer
        // and re-finalize (O(cards)). Commutative/idempotent, so this matches a
        // full re-fold without walking the whole history.
        if let Ok(txn) = Transaction::new(ndb) {
            let reducer = self.reducer.as_mut().expect("reducer present");
            event::reduce_delta(reducer, ndb, &txn, &keys);
            self.view = event::pick_board(reducer, author, board_id);
        }
        true
    }

    /// The cached board, if one has been folded.
    fn view(&self) -> Option<&BoardView> {
        self.view.as_ref()
    }

    /// Re-fold the whole event history into a fresh reducer (seeding or after an
    /// account switch) and pick out our board.
    fn reload(&mut self, ndb: &Ndb, author: &Pubkey, board_id: &str) {
        let reducer = Transaction::new(ndb)
            .ok()
            .and_then(|txn| event::fold_board(ndb, &txn, author));
        self.view = reducer
            .as_ref()
            .and_then(|r| event::pick_board(r, author, board_id));
        self.reducer = reducer;
        #[cfg(test)]
        {
            self.full_reloads += 1;
        }
    }

    /// Ensure we hold a live subscription to `author`'s headway events,
    /// resubscribing (and dropping the cached reducer) when the selected account
    /// changes. A fresh subscription only reports *future* ingests, so the next
    /// poll does a one-off full fold to pick up what's already there.
    fn sync_subscription(&mut self, ndb: &mut Ndb, author: &Pubkey) {
        if self.sub.is_some() && self.sub_author.as_ref() == Some(author) {
            return;
        }
        if let Some(old) = self.sub.take() {
            let _ = ndb.unsubscribe(old);
        }
        self.sub = ndb.subscribe(&[event::headway_filter(author)]).ok();
        self.sub_author = Some(*author);
        // New account (or first run): drop the cache so the next poll re-folds.
        self.view = None;
        self.reducer = None;
    }
}

impl App for Headway {
    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        // One cache shared by both renderers so an issue and its board, when both
        // are referenced, fold off a single subscription + reducer per board.
        let cache = Rc::new(RefCell::new(InlineBoardCache::default()));
        vec![
            Box::new(HeadwayIssueRenderer {
                cache: cache.clone(),
            }),
            Box::new(HeadwayBoardRenderer { cache }),
        ]
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let theme = ColorTheme::current(ui.ctx());

        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Keep a live subscription to this account's events and re-fold the
        // cached board only when something changed (first load, account switch,
        // or our own async ingests landing); keep waking while it streams in.
        if self.sync.poll(ctx.ndb, &author, &self.board_id) {
            self.wake();
        }

        if self.sync.view().is_none() {
            // No board yet: auto-seed one for an account that can sign.
            match &signer {
                Some(secret) => {
                    if !self.seeded {
                        let relays = ctx.accounts.selected_account_private_relays();
                        let mut publisher = PrivateRelayPublisher {
                            api: ctx.remote.publisher_explicit(),
                            relays,
                        };
                        store::seed_default_board(
                            ctx.ndb,
                            &author,
                            secret,
                            &self.board_id,
                            &mut publisher,
                        );
                        self.seeded = true;
                        self.wake();
                    }
                    empty_state(ui, &theme, "Setting up your board…");
                }
                None => empty_state(
                    ui,
                    &theme,
                    "Sign in with a key to create your Headway board.",
                ),
            }
            self.pump_repaint(ui);
            return AppResponse::default();
        }

        // Render against the cached view; `sync` and `state` are disjoint fields.
        // The detail pane draws comments with notedeck_ui's note renderer, so it
        // needs a `NoteContext` borrowed off `ctx` — dropped before we touch `ctx`
        // again to ingest the action below.
        let action = {
            let mut note_context = ctx.note_context();
            board_ui(
                ui,
                &theme,
                &mut note_context,
                self.sync.view().expect("view present"),
                &mut self.state,
            )
        };

        // Apply the collected action by ingesting events locally. Mutations need
        // a signing key; a watch-only account simply can't edit.
        if let (Some(action), Some(secret)) = (action, &signer) {
            let view = self.sync.view().expect("view present");
            let relays = ctx.accounts.selected_account_private_relays();
            let mut publisher = PrivateRelayPublisher {
                api: ctx.remote.publisher_explicit(),
                relays,
            };
            store::apply(
                ctx.ndb,
                &self.board_id,
                view,
                &author,
                secret,
                action,
                &mut publisher,
            );
            self.wake();
        }

        self.pump_repaint(ui);
        AppResponse::default()
    }
}

// ---------------------------------------------------------------------------
// Inline renderers — drawing a single headway entity referenced from elsewhere
// (e.g. a `nostr:` link in a notebook note), via notedeck's `KindRenderer`
// registry. These are read-only and self-contained, unlike the editable board.
// ---------------------------------------------------------------------------

/// Per-board fold cache shared by the inline renderers, so referenced headway
/// entities resolve to their *current* state without re-folding the whole event
/// history every frame.
///
/// Mirrors [`BoardSync`] but keyed by `(board author, board_id)` for arbitrarily
/// many boards (an inline reference can point at any board, not just the open
/// one), driven by a `&Ndb` (the [`notedeck::KindRenderer`] render path has no
/// `&mut Ndb`). Each board holds a live subscription + long-lived reducer; the
/// first touch folds the history once to seed it and every later frame folds in
/// only the freshly-arrived notes ([`event::reduce_delta`]). Subscriptions are
/// kept for the app's lifetime — there's no eviction, since the set of referenced
/// boards is small and bounded by what the user actually views.
#[derive(Default)]
struct InlineBoardCache {
    boards: HashMap<(Pubkey, String), InlineBoard>,
    /// Test-only count of full-history folds, to assert later frames fold deltas
    /// rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

/// One board's cached subscription + reducer within [`InlineBoardCache`].
#[derive(Default)]
struct InlineBoard {
    reducer: Option<BoardReducer>,
    sub: Option<Subscription>,
}

impl InlineBoardCache {
    /// Bring the cached reducer for `(author, board_id)` up to date with the
    /// local db and return it. Seeds with a one-off full fold on first touch
    /// (and folds every frame if the subscription couldn't be created), then
    /// folds only freshly-arrived notes in on later frames.
    fn reducer(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        board_id: &str,
    ) -> Option<&BoardReducer> {
        let key = (*author, board_id.to_owned());
        let mut seeded = false;
        {
            let board = self.boards.entry(key.clone()).or_default();
            if board.sub.is_none() {
                board.sub = ndb.subscribe(&[event::headway_filter(author)]).ok();
            }
            match board.sub {
                // No subscription: fold the whole history each frame so edits show.
                None => {
                    board.reducer = event::fold_board(ndb, txn, author);
                    seeded = true;
                }
                Some(sub) => {
                    let keys = ndb.poll_for_notes(sub, 64);
                    if board.reducer.is_none() {
                        // First touch (a fresh subscription only reports *future*
                        // ingests): fold the existing history once to seed.
                        board.reducer = event::fold_board(ndb, txn, author);
                        seeded = true;
                    } else if !keys.is_empty() {
                        // Incremental: fold only the new notes into the live
                        // reducer. Commutative/idempotent, so it matches a re-fold.
                        if let Some(reducer) = board.reducer.as_mut() {
                            event::reduce_delta(reducer, ndb, txn, &keys);
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        if seeded {
            self.full_reloads += 1;
        }
        let _ = seeded;
        self.boards.get(&key).and_then(|b| b.reducer.as_ref())
    }
}

/// Renders a headway issue (kind 1621) referenced inline, e.g. from a notebook
/// note. Registered into [`notedeck::KindRendererRegistry`] at app startup.
///
/// The kind-1621 note is only the card's *creation-time* snapshot; its current
/// title/labels/description come from folding the owning board's later edits. So
/// we fold the board (cached, see [`InlineBoardCache`]) and render the resolved
/// [`event::CardView`], falling back to the raw snapshot if the board isn't local.
pub struct HeadwayIssueRenderer {
    cache: Rc<RefCell<InlineBoardCache>>,
}

impl notedeck::KindRenderer for HeadwayIssueRenderer {
    fn id(&self) -> &'static str {
        "headway.issue"
    }
    fn name(&self) -> &'static str {
        "Headway issue"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_ISSUE]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        txn: &Transaction,
        note: &nostrdb::Note,
    ) -> egui::Response {
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return ui.weak("invalid headway issue");
        };
        let author = Pubkey::new(issue.board_author);
        // Resolve the card's current state off the (cached) folded board.
        let card = self
            .cache
            .borrow_mut()
            .reducer(note_context.ndb, txn, &author, &issue.board_id)
            .and_then(|reducer| event::pick_card(reducer, &author, &issue.board_id, &issue.id));
        match card {
            Some(card) => card_inline_ui(ui, &theme, &card),
            // Board not local to fold: show the creation-time snapshot.
            None => issue_inline_ui(ui, &theme, &issue),
        }
    }
}

/// Renders a headway board (kind 30619) referenced inline. The note is the
/// addressable board event; we recover its `(author, board_id)` and fold the
/// full board (cached, see [`InlineBoardCache`]) off the local db to summarise it.
pub struct HeadwayBoardRenderer {
    cache: Rc<RefCell<InlineBoardCache>>,
}

impl notedeck::KindRenderer for HeadwayBoardRenderer {
    fn id(&self) -> &'static str {
        "headway.board"
    }
    fn name(&self) -> &'static str {
        "Headway board"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_BOARD]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        txn: &Transaction,
        note: &nostrdb::Note,
    ) -> egui::Response {
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Board(board)) = event::parse(note) else {
            return ui.weak("invalid headway board");
        };
        let author = Pubkey::new(board.author);
        let view = self
            .cache
            .borrow_mut()
            .reducer(note_context.ndb, txn, &author, &board.id)
            .and_then(|reducer| event::pick_board(reducer, &author, &board.id));
        match view {
            Some(view) => board_inline_ui(ui, &theme, &view),
            None => ui.weak("headway board not found"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use nostrdb::{Config, Ndb};
    use std::time::{Duration, Instant};

    /// A headless harness driving a [`BoardSync`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors the
    /// `store::tests::TestNdb` poll-loop pattern (ingest is async).
    struct TestSync {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        sync: BoardSync,
    }

    impl TestSync {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
                sync: BoardSync::default(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// One poll cycle against this account's board. Returns whether the
        /// board was re-folded this call.
        fn poll(&mut self) -> bool {
            self.sync
                .poll(&mut self.ndb, &self.kp.pubkey, store::BOARD_ID)
        }

        fn seed(&mut self) {
            seed_demo(&self.ndb, &self.kp);
        }

        /// Poll until the cached view satisfies `pred` (ingest is async). Fails
        /// the test if it never holds.
        fn wait<F: Fn(&BoardView) -> bool>(&mut self, pred: F) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.poll();
                if self.sync.view().is_some_and(&pred) {
                    return;
                }
                assert!(Instant::now() < deadline, "sync predicate never held");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// Poll until the subscription stops reporting new notes, so the cache
        /// is quiescent (the async writer has drained).
        fn drain(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.poll() {
                assert!(Instant::now() < deadline, "sync never quiesced");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn total_cards(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    /// Seed the populated demo board for the sync tests to fold and act on. The
    /// production seed is card-less; the fixture lives in [`store::seed_demo_board`].
    fn seed_demo(ndb: &Ndb, kp: &FullKeypair) {
        store::seed_demo_board(
            ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            &mut store::NoPublish,
        );
    }

    /// Subscribing before seeding, then polling, materialises the whole board
    /// from events already in ndb.
    #[test]
    fn poll_materialises_the_board() {
        let mut t = TestSync::new();
        // Subscribe first so the seed's ingests are reported as new notes.
        t.poll();
        t.seed();

        t.wait(|v| total_cards(v) == 7);
        let view = t.sync.view().expect("board loaded");
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
    #[test]
    fn poll_reloads_on_change() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| v.columns[1].cards.len() == 2);

        // Apply against the cached pre-edit view (as render does).
        {
            let view = t.sync.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Fresh card".to_string(),
                    labels: vec![],
                },
                &mut store::NoPublish,
            );
        }

        // The new card only appears if a later poll re-folded the board.
        t.wait(|v| v.columns[1].cards.len() == 3);
        let view = t.sync.view().expect("board loaded");
        assert_eq!(view.columns[1].cards.last().unwrap().title, "Fresh card");
    }

    /// Once quiescent, polling with nothing new must NOT re-fold — this is the
    /// whole point of the cache (no per-frame walk of the event history).
    #[test]
    fn poll_does_not_refold_when_idle() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| total_cards(v) == 7);
        t.drain();

        assert!(
            !t.poll(),
            "cache re-folded with no new events — the per-frame fold is back"
        );
    }

    /// A change after the initial load is absorbed incrementally: the live
    /// reducer folds the delta, with no additional full-history re-fold. Guards
    /// against a regression to reload-on-every-change.
    #[test]
    fn poll_folds_changes_as_a_delta() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| v.columns[1].cards.len() == 2);
        t.drain();

        // Seeding does exactly one full fold; everything since is incremental.
        assert_eq!(
            t.sync.full_reloads, 1,
            "seeding should fold the history once"
        );

        {
            let view = t.sync.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Delta card".to_string(),
                    labels: vec![],
                },
                &mut store::NoPublish,
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3);

        assert_eq!(
            t.sync.full_reloads, 1,
            "the edit triggered a full re-fold instead of a delta"
        );
    }

    /// The inline-renderer cache ([`InlineBoardCache`]) folds the history once on
    /// first touch and then absorbs later edits as deltas via its subscription —
    /// never re-walking the history per frame. The render-path counterpart to
    /// [`poll_folds_changes_as_a_delta`], driven by `&Ndb` like the renderers.
    #[test]
    fn inline_cache_folds_once_then_deltas() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = InlineBoardCache::default();

        // One cache cycle (what a renderer does each frame): bring the cached
        // reducer up to date and fold out the board.
        let fold = |cache: &mut InlineBoardCache, ndb: &Ndb| -> Option<BoardView> {
            let txn = Transaction::new(ndb).unwrap();
            cache
                .reducer(ndb, &txn, &kp.pubkey, store::BOARD_ID)
                .and_then(|r| event::pick_board(r, &kp.pubkey, store::BOARD_ID))
        };

        // Subscribe (seeding an empty reducer) before the board exists, so the
        // seed's ingests arrive as subscription deltas rather than a re-fold.
        fold(&mut cache, &ndb);
        seed_demo(&ndb, &kp);

        // Poll until the board materialises (ingest is async on a writer thread).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fold(&mut cache, &ndb).is_some_and(|v| total_cards(&v) == 7) {
                break;
            }
            assert!(Instant::now() < deadline, "inline board never materialised");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Exactly one full fold — the initial empty seed; every event since
        // (the whole seeded board) folded in incrementally as deltas.
        assert_eq!(
            cache.full_reloads, 1,
            "inline cache re-walked the history instead of folding deltas"
        );
    }
}
