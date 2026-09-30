use std::cell::RefCell;
use std::rc::Rc;

use enostr::{RelayId, RelayStatus};
use nostrdb::{Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use notedeck::{App, AppContext, AppResponse, ColorTheme, PrivateRelaySync, fan_out_unseen_notes};

pub use headway::{event, store, teams};

mod cache;
mod cursor;
mod keys;
mod nav;
mod renderers;
mod review;
mod tools;
mod ui;

use cache::BoardCache;
pub use nav::{HeadwayRoute, ReviewTarget};
use nav::{NavReconcile, reconcile_nav};
pub use renderers::{HeadwayBoardRenderer, HeadwayIssueRenderer, HeadwayRefParser};
use ui::{BoardNav, CardBoardOp, board_ui, card_title, empty_state};
pub use ui::{
    BoardUiState, GRAPH_NODE_SIZE, GraphNodeView, board_inline_ui, card_chip_ui, card_inline_ui,
    graph_node_ui, issue_inline_ui,
};

use event::BoardView;

/// A Linear/Trello-style issue & todo tracker app for notedeck.
///
/// The board is backed by nostr events in the local nostrdb: [`BoardCache`] keeps
/// a long-lived reducer over the account's events and folds a [`BoardView`] out
/// of it, folding only freshly-arrived notes in as an ndb subscription reports
/// them — not re-walking the history every frame. Every edit is turned
/// into a signed event that is ingested locally (see [`store`]); the sync
/// coordination — polling that subscription and fanning each freshly-ingested
/// event out to the account's private relays — runs in [`update`](App::update)
/// so it stays live even when Headway isn't the foreground app, which is what
/// lets edits ingested by the `headway` CLI reach the user's other devices.
/// [`PrivateRelaySync`] feeds remote edits back in.
pub struct Headway {
    /// The active board, identified by its full coordinate (owner + slug) — not a
    /// bare slug, so a board you own and a joined shared board of the same slug
    /// stay distinct, and your own shared board routes through the multi-writer
    /// fold. Switched via the header switcher and restored per-account from the
    /// saved preference. `None` until the first [`update`](App::update) after an
    /// account switch resolves it (defaulting to [`store::BOARD_ID`]); always
    /// `Some` by the time `render` or any sync logic reads it (see [`active`]).
    active: Option<event::BoardCoord>,
    /// The account `board_id` was loaded for, so switching accounts reloads that
    /// account's own saved board selection.
    board_account: Option<Pubkey>,
    /// Transient, per-board UI state.
    state: BoardUiState,
    /// Inbound cross-device sync: declares a live + full-history subscription to
    /// the account's private relays each frame, and resolves the outbound
    /// publish targets.
    private_sync: PrivateRelaySync,
    /// Whether we've already auto-seeded a board this session, so we don't try
    /// to seed twice while the first seed is still materialising.
    seeded: bool,
    /// Countdown of follow-up repaints after an async ingest, so we keep waking
    /// up to poll the subscription until the writer thread goes quiet.
    repaint_frames: u8,
    /// A headway entity to navigate to, set by [`open`](Headway::open) when an
    /// inline widget is clicked elsewhere in the app. Resolved by
    /// [`process_pending_open`](Headway::process_pending_open) on the next render:
    /// switch to the owning board and, for a card, open its detail.
    pending_open: Option<NoteId>,
    /// The shared boards the selected account has joined (SNS `team_root`s),
    /// loaded from disk and re-registered with nostrdb on each account switch (see
    /// [`teams`]). Grown live as incoming kind-1082 key-shares are auto-accepted.
    teams: Vec<teams::Team>,
    /// Team-of-one boards we just created this session, kept until
    /// [`teams::teams_from_ndb`] independently returns them (their self-share
    /// key-share has folded back through nostrdb, which is async). Merged into
    /// [`teams`](Self::teams) on every roster rebuild so a freshly-created board
    /// keeps sealing edits — and stays in the roster — without a window where an
    /// edit would be written plaintext and then excluded from the shared fold.
    pending_teams: Vec<teams::Team>,
    /// The team roots already handed to nostrdb this session. [`set_roster`] runs on
    /// every arriving key-share and re-registers the *whole* roster, and nostrdb's
    /// root table is fixed-size and does not dedup, so those repeats have to be
    /// filtered here or a board joined late in a long session stops peeling (see
    /// [`teams::RootRegistry`]).
    ///
    /// [`set_roster`]: Headway::set_roster
    root_registry: teams::RootRegistry,
    /// Subscription to unwrapped kind-1082 key-share rumors, so a share that
    /// arrives while Headway is open is detected and accepted without a restart.
    keyshare_sub: Option<Subscription>,
    /// Self-share gift-wrap frames from boards created this session, captured at
    /// [`create_board`](Self::create_board) and awaiting fan-out to the account's
    /// private relays in [`update`](App::update).
    ///
    /// A board's kind-1059 self-share (the key-share that lets another device's
    /// roster discover the board) is authored by an ephemeral NIP-59 key and
    /// addressed to us, so it matches neither the plaintext author poll
    /// ([`fan_out_unseen_notes`]) nor the host's kind-1081 envelope leg
    /// ([`notedeck::HostPrivateSync`]) — nothing else fans it out. Without this
    /// leg a board created in the app syncs its *content* (its kind-1081 envelopes
    /// do ride the host leg) but never its key, so every other device sees the
    /// sealed envelopes with no root registered and folds nothing: the board is
    /// invisible off the device that made it. Buffered rather than sent inline
    /// because the outbound relay set isn't resolved until [`update`](App::update).
    pending_selfshares: Vec<String>,
    /// The one board-data engine (see [`BoardCache`]): the account's folded board
    /// reducer, pumped in [`update`](App::update) and read by both the foreground
    /// UI here and everything this app registers for inline display — the
    /// [`KindRenderer`](notedeck::KindRenderer)s and the
    /// [`ReferenceParser`](notedeck::ReferenceParser). Handed to each at
    /// registration by cloning the `Rc`, so a realtime edit folded in by the pump
    /// is immediately visible to an inline chip drawn in another app.
    board_cache: Rc<RefCell<BoardCache>>,
}

impl Default for Headway {
    fn default() -> Self {
        Self {
            active: None,
            board_account: None,
            state: BoardUiState::default(),
            private_sync: PrivateRelaySync::new("headway"),
            seeded: false,
            repaint_frames: 0,
            pending_open: None,
            teams: Vec::new(),
            pending_teams: Vec::new(),
            root_registry: teams::RootRegistry::default(),
            keyshare_sub: None,
            pending_selfshares: Vec::new(),
            board_cache: Rc::new(RefCell::new(BoardCache::default())),
        }
    }
}

/// A [`store::Publisher`] that keeps only the kind-1059 self-share gift-wrap
/// frames [`store::create_shared_board`] emits, dropping the board's kind-1081
/// definition envelope (the host's SNS envelope leg already fans that out). See
/// [`Headway::pending_selfshares`].
#[derive(Default)]
struct SelfShareSink(Vec<String>);

impl store::Publisher for SelfShareSink {
    fn publish(&mut self, frame: &str) {
        if frame_kind(frame) == Some(1059) {
            self.0.push(frame.to_string());
        }
    }
}

/// The `kind` of the event in a NIP-01 `["EVENT", {…}]` frame, or `None` if the
/// frame doesn't parse as one.
fn frame_kind(frame: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(frame)
        .ok()?
        .get(1)?
        .get("kind")?
        .as_u64()
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

    /// The active board's coordinate. Resolved in [`update`](App::update) on the
    /// first frame after an account switch (`board_account` starts `None`), which
    /// always runs before `render` and the sync logic — so this is set by the time
    /// anything reads it. Panics only if that invariant is ever broken.
    fn active(&self) -> &event::BoardCoord {
        self.active
            .as_ref()
            .expect("active board is resolved in update() before it is read")
    }

    /// Burn down the repaint countdown, requesting a delayed repaint each step.
    /// Driven from [`update`](App::update) (which runs every frame for all opened
    /// apps), so the poll/fan-out loop keeps ticking even off-foreground.
    /// Spend one of the follow-up frames [`wake`](Self::wake) queued, asking
    /// egui to come back in 60ms.
    ///
    /// A *delayed* repaint rather than `ctx.wake()`: this paces a poll of the
    /// writer thread, so an immediate wake would spin (the wake's own `update`
    /// asks for the next one). It therefore needs a window, and does nothing
    /// without one — a headless host has no animation clock, and its run loop
    /// already drops delayed requests in favour of its own idle cap.
    fn pump_repaint(&mut self, egui: Option<&egui::Context>) {
        if self.repaint_frames == 0 {
            return;
        }
        let Some(ctx) = egui else {
            return;
        };
        self.repaint_frames -= 1;
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    /// Poll the live key-share subscription (creating it on first use), returning
    /// the note keys of any kind-1082 rumors that arrived since the last poll — the
    /// signal to re-derive the roster (see [`teams::teams_from_ndb`]). Keyed on the
    /// rumor kind alone: nostrdb only unwraps a `1059` gift-wrap to its `1082` when
    /// it was addressed to one of our registered account keys, and the roster
    /// derivation filters those to the selected account by receiver pubkey.
    fn poll_keyshare_sub(&mut self, ctx: &mut AppContext) -> Vec<NoteKey> {
        if self.keyshare_sub.is_none() {
            self.keyshare_sub = ctx.ndb.subscribe(&[teams::keyshare_filter()]).ok();
        }
        match self.keyshare_sub {
            Some(sub) => ctx.ndb.poll_for_notes(sub, 32),
            None => Vec::new(),
        }
    }

    /// The switcher list: the account's own boards plus every joined shared board,
    /// each keyed by coordinate. A board you own *and* shared is already in `own`
    /// (same coordinate), so it's listed once; two joined boards that share a slug
    /// but differ in owner are distinct coordinates and both appear. A shared
    /// board's title comes from its folded view, falling back to its slug until the
    /// definition arrives.
    fn board_summaries_with_shared(
        &self,
        ctx: &AppContext,
        own: &[BoardView],
    ) -> Vec<BoardSummary> {
        let mut boards = board_summaries(own);
        // Resolve each joined board into a summary (its title needs the cache),
        // then merge by coordinate. Collected first so the transient cache borrow
        // is released before the merge.
        let shared: Vec<BoardSummary> = self
            .teams
            .iter()
            .filter_map(|team| {
                let coord = event::BoardCoord::parse(&team.board_addr)?;
                let channels = teams::board_channel_pubkeys(&self.teams, &team.board_addr);
                let title = Transaction::new(ctx.ndb)
                    .ok()
                    .and_then(|txn| {
                        self.board_cache.borrow_mut().shared_board(
                            ctx.ndb,
                            &txn,
                            &team.board_addr,
                            &channels,
                        )
                    })
                    .map(|v| v.title)
                    .unwrap_or_else(|| coord.slug.clone());
                Some(BoardSummary {
                    owner: coord.owner,
                    id: coord.slug,
                    title,
                })
            })
            .collect();
        merge_shared_boards(&mut boards, shared.into_iter());
        boards
    }

    /// Rebuild the joined-boards roster from nostrdb, preserving boards created
    /// this session whose self-share hasn't folded back yet.
    ///
    /// [`teams::teams_from_ndb`] is the source of truth, but a board we just
    /// created ([`create_board`](Self::create_board)) self-shares asynchronously —
    /// its kind-1082 rumor isn't queryable until nostrdb ingests it. Until then we
    /// keep it in [`pending_teams`](Self::pending_teams) and merge it in, so the
    /// board keeps resolving as a sealed channel (edits seal, not leak plaintext)
    /// even if some *other* keyshare triggers a rebuild first. Each pending team is
    /// dropped once `teams_from_ndb` returns it independently.
    fn set_roster(&mut self, ndb: &Ndb, author: &Pubkey) {
        self.teams = teams::teams_from_ndb(ndb, author);
        self.pending_teams.retain(|p| {
            !self
                .teams
                .iter()
                .any(|t| t.team_root == p.team_root && t.board_addr == p.board_addr)
        });
        self.teams.extend(self.pending_teams.iter().cloned());
        self.root_registry.register(ndb, &self.teams);
    }

    /// Create a new team-of-one board (sealed + self-shared via
    /// [`store::create_shared_board`]) and register it in the roster immediately, so
    /// it resolves as its own sealed channel from the very next frame — edits seal
    /// under the board's `team_root` rather than leaking plaintext while the
    /// self-share folds back in. Returns whether the board was created. The path for
    /// both an explicit "New board" and the auto-seeded default.
    ///
    /// The `team_root` is *derived* from the account secret and the board slug
    /// ([`nostrdb_net::sns::derive_board_root`]), never randomly minted: the same board
    /// created independently on another device — same secret, same slug — lands the
    /// same root and converges on one channel instead of forking a divergent one.
    /// Per-board content isolation is unchanged: a different slug derives an
    /// unrelated root, and the derivation is one-way.
    fn create_board(
        &mut self,
        ndb: &Ndb,
        author: &Pubkey,
        secret: &[u8; 32],
        board_id: &str,
        title: &str,
    ) -> bool {
        let root = nostrdb_net::sns::derive_board_root(secret, board_id);
        // Capture the kind-1059 self-share so `update` can fan it out to the
        // private relays (see `pending_selfshares`). `create_shared_board` also
        // emits the board's kind-1081 definition envelope, but the host's SNS
        // envelope leg already fans that, so the sink keeps only the self-share to
        // avoid a redundant double-send.
        let mut sink = SelfShareSink::default();
        if !store::create_shared_board(ndb, author, secret, board_id, title, &root, &mut sink) {
            return false;
        }
        self.pending_selfshares.extend(sink.0);
        let team = teams::Team {
            team_root: hex::encode(root),
            board_addr: event::board_address(author, board_id),
            epoch: None,
            shared_at: 0,
        };
        if !self
            .pending_teams
            .iter()
            .any(|t| t.team_root == team.team_root && t.board_addr == team.board_addr)
        {
            self.pending_teams.push(team.clone());
        }
        if !self
            .teams
            .iter()
            .any(|t| t.team_root == team.team_root && t.board_addr == team.board_addr)
        {
            self.teams.push(team);
        }
        true
    }

    /// Navigate to a headway entity referenced from elsewhere in the app — raised
    /// when an inline board/issue widget (drawn by our [`KindRenderer`]) is clicked
    /// in another app like the notebook. `note` is the board or issue event; the
    /// switch happens on the next render (see [`process_pending_open`](Self::process_pending_open)).
    pub fn open(&mut self, note: NoteId) {
        self.pending_open = Some(note);
        // Wake so the switch is processed even if nothing else is repainting.
        self.wake();
    }

    /// The card holding the board grid's keyboard cursor, if any (see
    /// [`BoardUiState::cursor`]). Lets integration tests assert where keyboard
    /// navigation left the cursor without reading pixels.
    pub fn cursor(&self) -> Option<NoteId> {
        self.state.cursor()
    }

    /// Resolve a headway note into its [`OpenTarget`] and make that target's board
    /// the active one. The shared half of opening an entity from outside the board
    /// grid, so the retry path below and a chrome deep link agree on *which* board
    /// a note lands you on — and on persisting that switch.
    ///
    /// `None` when `note_id` isn't a headway entity we can route to (unresolved, or
    /// an unexpected kind). The returned target's [`board`](OpenTarget::board) is
    /// the board now active, which for a card is where it is actually *placed*
    /// rather than the origin coordinate its `a` tag records.
    ///
    /// Only the switch happens here; a card's detail needs the new board's view to
    /// have folded in, which is a later frame's job (see
    /// [`process_pending_open`](Self::process_pending_open)).
    fn activate_open_target(
        &mut self,
        ctx: &mut AppContext,
        author: &Pubkey,
        note_id: NoteId,
    ) -> Option<OpenTarget> {
        let mut target = resolve_open_target(ctx.ndb, note_id)?;

        // A card lives on whichever board it's *placed* on, which a cross-board
        // move makes differ from the origin board its `a` tag records (what
        // `resolve_open_target` reads). Route to the board it's actually on so the
        // detail can open; fall back to the origin board when it isn't folded.
        // `locate_card_in_boards` is author-scoped, so a card found there is on one
        // of our own boards (owner = author); otherwise keep the target coordinate.
        if let Some(card) = target.card {
            let placed = Transaction::new(ctx.ndb).ok().and_then(|txn| {
                self.board_cache
                    .borrow_mut()
                    .with_boards(ctx.ndb, &txn, author, |boards| {
                        event::locate_card_in_boards(boards, author, card.bytes())
                    })
                    .flatten()
                    .map(|located| event::BoardCoord::new(*author.bytes(), located.board_id))
            });
            if let Some(placed) = placed {
                target.board = placed;
            }
        }

        // Switch to the owning board. Guarded on an actual change so a caller that
        // retries every frame until the fold lands doesn't republish the board
        // preference on each one.
        if self.active.as_ref() != Some(&target.board) {
            self.active = Some(target.board.clone());
            // Persist the switch as a PNS note when we can sign; a watch-only
            // account has no secret to encrypt with, so its selection just isn't
            // remembered (it never was persistable).
            if let Some(secret) = ctx
                .accounts
                .selected_filled()
                .map(|f| f.secret_key.secret_bytes())
            {
                store::save_board_pref(
                    ctx.ndb,
                    author,
                    &secret,
                    self.active(),
                    &mut store::NoPublish,
                );
            }
            self.wake();
        }

        Some(target)
    }

    /// Act on a pending [`open`](Self::open): resolve which board the entity lives
    /// on and switch there, then — for a card — open its detail once that board's
    /// view has folded in. A board just needs the switch. Runs early each render.
    ///
    /// The switch can land a frame ahead of the fold, so a cross-board card jump
    /// lands on the board first and pops the detail on a following frame once the
    /// view catches up; `open`'s repaint burst keeps us ticking until it does.
    fn process_pending_open(&mut self, ctx: &mut AppContext, author: &Pubkey) {
        let Some(note_id) = self.pending_open else {
            return;
        };
        let Some(target) = self.activate_open_target(ctx, author, note_id) else {
            // Not a headway entity we can route to (unresolved / unexpected kind).
            self.pending_open = None;
            return;
        };

        let Some(card) = target.card else {
            // A board: switching to it was the whole job.
            self.pending_open = None;
            return;
        };

        // On the right board: open the card's detail once its view has folded in.
        // Until then keep the request pending and retry on the next repaint. The
        // fold check is author-scoped (own boards); a card on a shared board owned
        // by a co-member opens once that board is active and folded via `render`.
        let slug = self.active().slug.clone();
        let folded = Transaction::new(ctx.ndb).ok().is_some_and(|txn| {
            self.board_cache
                .borrow_mut()
                .board(ctx.ndb, &txn, author, &slug)
                .is_some_and(|v| v.id == slug)
        });
        if folded {
            self.state.open_card(card);
            self.pending_open = None;
        }
    }
}

/// Where an inline headway widget click should land, resolved from the clicked
/// entity by [`resolve_open_target`].
struct OpenTarget {
    /// The board to switch to, by its full coordinate (owner + slug).
    board: event::BoardCoord,
    /// The card whose detail to open once the board has folded in, or `None` when
    /// the target is a board itself.
    card: Option<NoteId>,
    /// The target's name, taken straight off the resolved event — a card's
    /// `subject` or a board's `title` — so a history entry can be labelled
    /// *before* the board folds. [`card_title`] can't: it reads an already-folded
    /// `BoardView`, and a board we only just switched to has none yet. Like every
    /// other [`HeadwayRoute`](nav::HeadwayRoute) title it is therefore a snapshot
    /// of the original subject and can lag a later rename, which is fine for a
    /// back/forward label. `None` when the event carries no name.
    ///
    /// Only the cross-app deep-link path ([`open_note_route`](App::open_note_route))
    /// needs it (it mints its route token from the note itself); the in-app board
    /// grid goes through [`card_title`] against the view it is already drawing.
    title: Option<String>,
}

/// Resolve a headway board/issue note into the [`OpenTarget`]
/// [`activate_open_target`](Headway::activate_open_target) acts on. An issue opens
/// its board *and* its own detail; a board just opens itself. `None` for anything
/// that isn't one of those. The board's owner comes straight off the parsed event
/// (an issue's `a`-tag coordinate, a board's author), so the switch targets the
/// exact coordinate — not a bare slug; the title likewise, so it needs no fold.
fn resolve_open_target(ndb: &Ndb, note_id: NoteId) -> Option<OpenTarget> {
    let txn = Transaction::new(ndb).ok()?;
    let note = ndb.get_note_by_id(&txn, note_id.bytes()).ok()?;
    match event::parse(&note)? {
        event::HeadwayEvent::Issue(issue) => Some(OpenTarget {
            board: event::BoardCoord::new(issue.board_author, issue.board_id),
            card: Some(NoteId::new(issue.id)),
            title: (!issue.subject.is_empty()).then_some(issue.subject),
        }),
        event::HeadwayEvent::Board(board) => Some(OpenTarget {
            title: (!board.title.is_empty()).then_some(board.title),
            card: None,
            board: event::BoardCoord::new(board.author, board.id),
        }),
        _ => None,
    }
}

/// Whether `kind` is a headway entity the [`Headway`] app can open inline — a
/// board or an issue. Used by the shell to route a clicked inline widget here.
pub fn is_headway_kind(kind: u32) -> bool {
    matches!(kind, event::KIND_BOARD | event::KIND_ISSUE)
}

/// One entry in the board switcher: a board's `owner` + `id` (slug) — together its
/// coordinate — and its display `title`. Carrying the owner keeps two boards that
/// share a slug (yours and a joined one, or two joined ones) distinct entries that
/// each select the right coordinate. Folded from events by [`BoardCache::boards`].
pub struct BoardSummary {
    pub owner: [u8; 32],
    pub id: String,
    pub title: String,
}

impl App for Headway {
    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        // Clone the app's one board cache (see `board_cache`) so an issue and its
        // board read the same account reducer the foreground UI and `update`'s
        // realtime pump do — a realtime edit shows on the chip, not just the board.
        let cache = self.board_cache.clone();
        vec![
            Box::new(HeadwayIssueRenderer {
                cache: cache.clone(),
            }),
            Box::new(HeadwayBoardRenderer { cache }),
        ]
    }

    fn reference_parsers(&self) -> Vec<Box<dyn notedeck::ReferenceParser>> {
        // Same board cache as the kind renderers and the foreground UI: a card
        // referenced by word id resolves off the one realtime-pumped reducer.
        vec![Box::new(HeadwayRefParser {
            cache: self.board_cache.clone(),
        })]
    }

    fn tools(&self) -> Vec<notedeck::RegisteredTool> {
        tools::tools()
    }

    /// Background sync, run every frame for all *opened* apps (not just the
    /// foreground one) — which is what lets edits ingested by the `headway` CLI
    /// while the user is on another tab still sync out. Polls the account's board
    /// subscription, fans freshly-ingested events out to its private relays, and
    /// auto-seeds a default board. Rendering happens separately in [`render`].
    fn update(&mut self, ctx: &mut AppContext<'_>) {
        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`/`remote`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // On first update and after an account switch, restore that account's
        // last-selected board from nostrdb — a PNS-wrapped kind-30623 note
        // ([`store::save_board_pref`]) that replaced the old `headway-boards.json`
        // (falling back to the default). Re-arm the auto-seed so a fresh account
        // still gets its default board seeded.
        if self.board_account != Some(author) {
            self.active = Some(
                event::load_board_pref(ctx.ndb, &author)
                    .unwrap_or_else(|| event::BoardCoord::new(*author.bytes(), store::BOARD_ID)),
            );
            self.board_account = Some(author);
            self.seeded = false;

            // Reconstruct this account's joined shared boards from nostrdb — the
            // roster is the set of kind-1082 key-shares gift-wrapped to us, which
            // nostrdb has already unwrapped and stored durably (no disk cache; see
            // [`teams::teams_from_ndb`]). Re-register their team_roots so nostrdb
            // unwraps the boards' envelopes (registered keys don't survive a restart
            // — the same reason `add_key` re-runs on boot).
            self.set_roster(ctx.ndb, &author);
        }

        // Declare the inbound cross-device subscription (catch-up + realtime)
        // against the account's private relays, and resolve the same set as our
        // outbound publish targets. Empty => local-only. This is headway's own
        // *plaintext* board leg only: the sealed shared-board kind-1081/1082 streams
        // are now pulled centrally by the notedeck host's private `Session` (see
        // `notedeck::HostPrivateSync`), which registers the roots and auto-unwraps
        // the envelopes off-foreground — so headway no longer declares them here.
        let inbound = vec![event::headway_filter(&author)];
        let private_relays = self.private_sync.update(ctx, inbound);

        // Flush any self-share gift-wraps captured when a board was created this
        // session, now that the outbound relay set is resolved (see
        // `pending_selfshares`). Held until a private relay is reachable so an
        // account created offline still shares its key once one appears; drained
        // only once actually forwarded, so a local-only account keeps buffering.
        if !self.pending_selfshares.is_empty() && !private_relays.is_empty() {
            let mut api = ctx.remote.publisher_explicit();
            for frame in self.pending_selfshares.drain(..) {
                notedeck::fan_out_event_frame(&mut api, &frame, &private_relays);
            }
        }

        // Pump the shared board cache: advance this account's reducer, folding in
        // any freshly-arrived notes — our own async ingests, CLI moves into the
        // embedded relay, remote sync. Inline widgets read this same cache, so a
        // chip drawn in another app stays as live as the open board. Keep waking
        // while edits stream in.
        let poll = Transaction::new(ctx.ndb)
            .ok()
            .map(|txn| self.board_cache.borrow_mut().poll(ctx.ndb, &txn, &author))
            .unwrap_or_default();
        if poll.changed {
            self.wake();
        }

        // Notice any SNS key-share (kind-1082) that arrived this frame — a member
        // sharing a board with us. Its unwrapped rumor is now in the db, so
        // re-derive the roster from nostrdb and re-register roots (see [`teams`]);
        // wake when it grew so the newly-joined board folds in.
        if !self.poll_keyshare_sub(ctx).is_empty() {
            let joined = self.teams.len();
            self.set_roster(ctx.ndb, &author);
            if self.teams.len() != joined {
                self.wake();
            }
        }

        // Fan every freshly-ingested board event out to the private relays it
        // hasn't reached yet. This is the outbound half of cross-device sync: it
        // covers our own edits *and* events written straight into nostrdb by the
        // `headway` CLI, which never pass through the app's edit path.
        if !poll.fresh.is_empty()
            && !private_relays.is_empty()
            && let Ok(txn) = Transaction::new(ctx.ndb)
        {
            let mut api = ctx.remote.publisher_explicit();
            fan_out_unseen_notes(&mut api, ctx.ndb, &txn, &poll.fresh, &private_relays);
        }

        // Advance joined shared boards (multi-writer fold, driven by each channel's
        // kind-1081 envelope stream) and wake on a fresh sealed edit so the open
        // board re-folds. The sealed envelopes are *not* fanned out here anymore:
        // the notedeck host now owns the SNS 1081 outbound leg (it fans every
        // roster channel's locally-authored envelopes — see
        // `notedeck::HostPrivateSync`), so publishing them here too would just
        // double-send. The plaintext-board fan above stays app-owned (the host
        // only carries private envelopes, not plaintext board events).
        if !self.teams.is_empty()
            && let Ok(txn) = Transaction::new(ctx.ndb)
        {
            let shared = self
                .board_cache
                .borrow_mut()
                .poll_shared(ctx.ndb, &txn, &self.teams);
            if !shared.fresh.is_empty() || shared.subscribed {
                self.wake();
            }
        }

        // No board yet: auto-seed one for an account that can sign. Guarded by
        // `seeded` so the board-existence check (a finalize) only runs until we've
        // confirmed or created one — not every frame. The seeded events fan out via
        // the same poll path on a following frame. (The UI feedback for this state
        // is drawn in `render`.)
        //
        // Never auto-seed when the active board is a *shared* one (its coordinate
        // is in the roster): it may be owned by a co-member, so seeding would create
        // a conflicting own board instead of waiting for the shared one to fold in.
        let active_is_shared = active_shared_team(&self.teams, self.active()).is_some();
        if !self.seeded
            && !active_is_shared
            && let Some(secret) = &signer
        {
            let slug = self.active().slug.clone();
            let has_board = Transaction::new(ctx.ndb).ok().is_some_and(|txn| {
                self.board_cache
                    .borrow_mut()
                    .board(ctx.ndb, &txn, &author, &slug)
                    .is_some()
            });
            if has_board {
                // Already have a board (e.g. synced from another device): nothing
                // to seed, and no need to keep checking.
                self.seeded = true;
            } else if slug == store::BOARD_ID {
                // Auto-seed the default board as its own team-of-one SNS channel,
                // sealed + self-shared from note #1 like an explicit "New board" — so
                // the default is shareable too and every board flows through one
                // sealed path. Its `team_root` is derived from the account secret and
                // slug (see [`create_board`]), so every device auto-seeding it
                // independently at the same coordinate converges on one channel. An
                // existing plaintext default is untouched — `has_board` is true for it
                // above, so we never reach here to re-seal it.
                self.create_board(ctx.ndb, &author, secret, store::BOARD_ID, "Headway");
                self.seeded = true;
                self.wake();
            } else {
                // A non-default active board is never auto-created — we wait for it
                // to sync in. Every non-default board originates from an explicit
                // create (the GUI's "New board" or the CLI's `headway seed`, both
                // born team-of-one SNS and in the roster from that frame). Reaching
                // here means such a board is named by the restored board pref but
                // hasn't folded in from its origin yet. We can't auto-create it: we
                // don't know its title, and writing our own definition would clobber
                // the real one once it arrives (both seal under the same derived root
                // now, so the later `created_at` wins). So we sit tight — the board
                // and its kind-1082 self-share sync in and are adopted via the roster
                // (`active_is_shared` above), at which point it folds the shared way.
                // Mark seeded so we stop re-checking; the roster path takes over.
                self.seeded = true;
            }
        }

        self.pump_repaint(ctx.egui);
    }

    /// Render Headway's currently-open board or card detail (whichever
    /// [`state.selected`](BoardUiState::selected) names) and reconcile the chrome
    /// global-history stack with any board↔card move the frame makes.
    ///
    /// This path does **not** reseed the selection from a nav route: in production
    /// [`render_nav`](Self::render_nav) always seeds it first (the chrome reaches
    /// every app through `render_nav`), and this is also the standalone entrypoint a
    /// chrome-less embedding (tests) drives, where the selection must persist across
    /// frames because no nav entry is pushed to carry it.
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        self.render_board(ctx, ui)
    }

    /// Draw one chrome global-history entry. Seeds the view mode from the entry's
    /// route token — [`HeadwayRoute::Card`] ⇒ that card's full-pane detail,
    /// [`Graph`](HeadwayRoute::Graph) ⇒ that epic's dependency graph (with the epic
    /// also seeded as the selected card underneath, so a back off the graph lands
    /// on its detail), [`Review`](HeadwayRoute::Review) ⇒ that card's review pane on
    /// the record the entry carries, [`ReviewQueue`](HeadwayRoute::ReviewQueue) ⇒
    /// the review queue where it was left, [`Board`](HeadwayRoute::Board) or any unrecognized token (the
    /// `()` a plain app-switch entry carries) ⇒ the board grid — so the nav stack,
    /// not stale view-state, decides which screen shows. A global back/forward/jump
    /// is honored here before the board draws.
    fn render_nav(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        token: &Rc<dyn std::any::Any>,
    ) -> AppResponse {
        let route = token.downcast_ref::<HeadwayRoute>();
        // Seed both dimensions from the route: the selected card (a `Graph`
        // route seeds its epic here too) and, separately, whether the graph is
        // open. Seeding graph mode via `set_graph_epic` rather than `open_graph`
        // leaves the persisted scene rect untouched, so re-visiting a graph entry
        // (back/forward) keeps its pan/zoom.
        self.state
            .set_selected(route.and_then(|r| r.selected_card()));
        self.state
            .set_graph_epic(route.and_then(|r| r.graph_epic()));
        // The queue route names no card: while it's open the review pane shows
        // the queue's current card, so seed that rather than closing the pane
        // (which would read as a re-open every frame).
        self.state
            .set_queue_open(route.is_some_and(HeadwayRoute::is_review_queue));
        let review = route
            .and_then(|r| r.review_target())
            .or_else(|| self.state.queue_review());
        self.state.set_review(review);
        self.render_board(ctx, ui)
    }

    /// A short title for one global-history entry, shown in the chrome's history
    /// dropdown: a [`Card`](HeadwayRoute::Card)'s snapshotted title, or `None` for
    /// the board so the chrome falls back to the "Headway" app label. `nav_title`
    /// is handed only the token — no [`AppContext`] to re-resolve through — so the
    /// title comes from the snapshot the card route carries (see [`HeadwayRoute`]).
    fn nav_title(&self, token: &Rc<dyn std::any::Any>) -> Option<String> {
        token
            .downcast_ref::<HeadwayRoute>()
            .and_then(|r| r.title())
            .map(str::to_owned)
    }

    /// Mint the route for a board or card opened from *another* app (an inline
    /// widget in a Dave message, a notebook node, a timeline note), so the chrome
    /// can land the whole open as a single global-history entry.
    ///
    /// The board switch happens here, eagerly: `active` is app state the token
    /// doesn't carry, and [`render_nav`](Self::render_nav) resolves a `Card` token
    /// against whichever board is active. A board note yields
    /// [`HeadwayRoute::Board`] — switching to it was the whole job. A card note
    /// yields its [`Card`](HeadwayRoute::Card) route, titled straight off the issue
    /// event so it needs no folded view, and also leaves an [`open`](Self::open)
    /// pending: that retry only corrects `active` (a cross-board-moved card whose
    /// placement hasn't folded yet) and selects the card the token already names.
    ///
    /// Why that costs exactly one history entry: `render_nav` seeds the pre-render
    /// [`NavPos`](nav::NavPos) from the token as `Card(card)`, and
    /// [`process_pending_open`](Self::process_pending_open)'s `open_card(card)`
    /// selects that *same* card, so the post-render diff is `Card(card) →
    /// Card(card)` and [`reconcile_nav`] enqueues nothing. The chrome's push is the
    /// only one. (A card that hasn't folded in yet keeps its selection rather than
    /// backing out — see `board_ui` — so the entry isn't popped either.)
    ///
    /// `None` when the note isn't a headway entity we can route to; the chrome then
    /// falls back to a plain app switch.
    fn open_note_route(
        &mut self,
        ctx: &mut AppContext<'_>,
        note_id: NoteId,
    ) -> Option<Rc<dyn std::any::Any>> {
        let author = *ctx.accounts.selected_account_pubkey();
        let target = self.activate_open_target(ctx, &author, note_id)?;
        let Some(card) = target.card else {
            return Some(Rc::new(HeadwayRoute::Board));
        };
        // Keep the retry pending; it corrects `active`, not the route token, so it
        // costs no extra history entry (see above).
        self.open(note_id);
        Some(Rc::new(HeadwayRoute::card(card, target.title)))
    }
}

impl Headway {
    /// Render one board↔card↔graph view and reconcile the chrome global-history
    /// stack.
    ///
    /// The open screen is whatever [`state.selected`](BoardUiState::selected) and
    /// [`state.graph_epic`](BoardUiState::graph_epic) already name — seeded from the
    /// nav route by [`render_nav`](Self::render_nav) in production, or persisted
    /// across frames in a chrome-less embedding. The UI may then move (a card click,
    /// a detail close, a subissue swap, opening or closing an epic's graph); we diff
    /// the resulting [`NavPos`](nav::NavPos) against where it started into a nav request
    /// afterward — a drill (board→card, card→card, card→graph) pushes a walkable
    /// entry, and stepping shallower (close a card, close the graph) is a single
    /// global-back. Card→card pushes rather than replaces so the back trail stays
    /// walkable (see [`NavReconcile`]). The chrome fills in Headway's own
    /// [`AppId`](notedeck::AppId) on drain, since `render_nav` never tells the app
    /// its own slot.
    fn render_board(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // The chrome hands every app a zero horizontal item gap
        // (`notedeck_chrome/src/chrome/frame.rs`, `Chrome::show`), which glues
        // adjacent labels together; Headway's layout assumes a real gap, so it
        // owns one rather than inheriting the chrome's.
        ui.spacing_mut().item_spacing.x = notedeck::tokens::SPACING_SM;
        let theme = ColorTheme::current(ui.ctx());

        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Snapshot the view position before the UI runs so we can diff the frame's
        // board↔card↔graph move into a nav request afterward. In production this is
        // the route `render_nav` just seeded; a cross-app `open` may override it
        // below.
        let before = self.state.nav_pos();

        // Navigate to an entity a click elsewhere asked us to open (see `open`).
        self.process_pending_open(ctx, &author);

        // Sync (subscription poll, private-relay fan-out, auto-seed) already ran
        // in `update` this frame; read the folded boards off the same shared cache
        // (a cold, Headway-never-opened session seeds it lazily on this read). One
        // finalize backs both the active board and the switcher list.
        let own_boards = Transaction::new(ctx.ndb)
            .ok()
            .map(|txn| {
                self.board_cache
                    .borrow_mut()
                    .all_boards(ctx.ndb, &txn, &author)
            })
            .unwrap_or_default();

        // Is the active board shared? Matched by coordinate against the roster (see
        // `active_shared_team`) — which is what fixes owner-blindness: a board you
        // *own* and shared is in the roster too (self-shared team-of-one, including
        // one you just created via `create_board`), so it routes through the
        // multi-writer fold below rather than the author-scoped fold that would hide
        // co-members' cards. Only a legacy plaintext board (never self-shared, not in
        // the roster) falls through to the plaintext own path.
        let active_team: Option<teams::Team> =
            active_shared_team(&self.teams, self.active()).cloned();
        // The SNS channel to seal edits into when the active board is shared.
        let channel: Option<store::SnsChannel> = active_team.as_ref().and_then(team_channel);

        // Resolve the active board's view: a shared board folds by coordinate
        // (multi-writer, every member's events), an own board off the per-account
        // reducer keyed on its owner + slug.
        let view = match &active_team {
            // Fold the union of the board's channels — see
            // `event::fold_shared_board`. No usable channel means we can't fold;
            // falls through to the "loading" message below.
            Some(team) => {
                let channels = teams::board_channel_pubkeys(&self.teams, &team.board_addr);
                Transaction::new(ctx.ndb).ok().and_then(|txn| {
                    self.board_cache.borrow_mut().shared_board(
                        ctx.ndb,
                        &txn,
                        &team.board_addr,
                        &channels,
                    )
                })
            }
            None => {
                let active = self.active();
                own_boards
                    .iter()
                    .find(|v| v.id == active.slug && v.author == active.owner)
                    .cloned()
            }
        };
        // The switcher list, needed before the board renders because an unfolded
        // board still draws the switcher (below) — that's the escape hatch off a
        // board that won't fold. Owned, so the cache borrow it takes is dropped
        // before we render: a description that references another card resolves
        // through `self.board_cache` *during* `board_ui`, so holding a borrow
        // across the render would panic the `RefCell`.
        let boards = self.board_summaries_with_shared(ctx, &own_boards);

        let Some(view) = view else {
            // No board folded yet. `update` auto-seeds one for a signing account;
            // a watch-only account can't create one; a joined shared board is
            // still folding in from its co-members.
            //
            // A watch-only account has nothing to switch between and can't create
            // a board, so it gets a plain prompt. Everyone else keeps the board
            // chrome around a placeholder: dead-ending on a full-pane message
            // takes the switcher with it, which strands the app for as long as the
            // fold stays empty — and a shared board whose definition never arrives
            // strands it for good (the bug this replaces).
            let Some(secret) = &signer else {
                empty_state(
                    ui,
                    &theme,
                    "Sign in with a key to create your Headway board.",
                );
                return AppResponse::default();
            };
            let msg = if active_team.is_some() {
                "Loading shared board…"
            } else {
                "Setting up your board…"
            };
            let placeholder = placeholder_board(self.active(), &boards);
            ui::unfolded_board_ui(ui, &theme, &placeholder, &boards, &mut self.state, msg);
            // Only the switcher is live here, so that's the only request to drain
            // — no `board_ui` action exists to apply, and applying one against a
            // placeholder would republish a board definition over the real one.
            self.drain_board_nav(ctx, &author, Some(secret), &boards);
            return AppResponse::default();
        };

        // Header sync indicator: are we reaching a private relay right now?
        let sync = sync_status(ctx);
        let action = board_ui(ui, &theme, ctx, &view, &boards, sync, &mut self.state);

        // Reconcile the chrome global-history stack with the view position the board
        // UI left, comparing it against `before` (seeded from this entry's route).
        // The card/epic title is snapshotted from the freshly-folded `view` for the
        // entry's history-dropdown label.
        let after = self.state.nav_pos();
        match reconcile_nav(before, after) {
            // Opening a card — from the board, or drilling from one card into a
            // subissue/parent/blocker — pushes a new detail entry (the chrome tags
            // Headway's own slot on drain, since `render_nav` never told us our
            // AppId). A card→card drill pushes rather than replaces on purpose: the
            // `replace` primitive collapses the *whole* history to the new top
            // (`ReplacementType::All`), which would drop the board root and every
            // prior app so a global-back had nothing to return to. Pushing keeps a
            // browser-style trail — back walks from a subissue up to its parent and
            // on to the board.
            Some(NavReconcile::PushCard(card)) => ctx
                .navigator
                .push_active_route(HeadwayRoute::card(card, card_title(&view, card))),
            // Opening an epic's dependency graph from its detail pushes a graph
            // entry on top of the card — a sibling one level deeper — so a single
            // global-back returns to the epic's detail.
            Some(NavReconcile::PushGraph(epic)) => ctx
                .navigator
                .push_active_route(HeadwayRoute::graph(epic, card_title(&view, epic))),
            // Opening a card's review pane from its detail pushes a review entry
            // one level deeper, a sibling of the graph's, so a global-back
            // returns to the card. The entry carries the record the pane opened
            // on, so back/forward onto it reopens that record.
            Some(NavReconcile::PushReview(card)) => ctx.navigator.push_active_route(
                HeadwayRoute::review(card, self.state.review_record(), card_title(&view, card)),
            ),
            // Opening the review queue from the grid pushes its one entry; the
            // steps through it are view state under that entry, so a single
            // global-back leaves the whole queue.
            Some(NavReconcile::PushQueue) => {
                ctx.navigator.push_active_route(HeadwayRoute::ReviewQueue)
            }
            // Leaving a card (close, delete, or a card that vanished), closing the
            // graph or leaving the queue steps one entry back in the global history.
            Some(NavReconcile::Back) => ctx.navigator.back(),
            // Steady frame — nothing moved, so enqueue nothing (this doesn't spin).
            None => {}
        }

        self.drain_board_nav(ctx, &author, signer.as_ref(), &boards);

        // A cross-board card request (move or link, raised from a card's context
        // menu): resolve the target board's view out of the same reducer and
        // link/relocate the card. Needs a signing key, and silently no-ops if the
        // target board can't be folded (e.g. it was just deleted).
        if let (Some(mv), Some(secret)) = (self.state.take_card_move(), &signer)
            && let Some(target_view) = Transaction::new(ctx.ndb).ok().and_then(|txn| {
                self.board_cache
                    .borrow_mut()
                    .board(ctx.ndb, &txn, &author, &mv.to_board)
            })
        {
            // Each board seals with its own channel: the source tombstone under
            // the active board's, the target placement under the target's. Sealing
            // both with the source's — what this did before boards carried
            // per-board channels — handed the target a placement its own fold
            // refuses to trust, so the card left one board without arriving at the
            // other (headway:headway/series-high-praise).
            let target_channel =
                board_channel(&self.teams, &event::board_address(&author, &mv.to_board));
            let source = store::BoardRef {
                id: &self.active().slug,
                view: &view,
                channel: channel.as_ref(),
            };
            let target = store::BoardRef {
                id: &mv.to_board,
                view: &target_view,
                channel: target_channel.as_ref(),
            };
            // Ingest locally only; `update`'s poll fans the new events out to the
            // private relays next frame (see `wake`).
            let placed = match mv.op {
                CardBoardOp::Move => store::move_card_between_boards(
                    ctx.ndb,
                    source,
                    target,
                    secret,
                    mv.card,
                    &mut store::NoPublish,
                ),
                CardBoardOp::Link => store::link_card(
                    ctx.ndb,
                    source,
                    target,
                    secret,
                    mv.card,
                    &mut store::NoPublish,
                ),
            };
            // A refusal writes nothing, so the card simply stays where it is. The
            // app has no way to say so yet — it needs a transient-message surface
            // it doesn't have — so log it and leave the board unchanged rather
            // than move a card into a board that can't show it.
            if let Err(err) = placed {
                tracing::warn!("headway: cross-board {:?} refused: {err}", mv.op);
            }
            self.wake();
        }

        // Apply the collected action by ingesting events locally. Mutations need
        // a signing key; a watch-only account simply can't edit. `update`'s poll
        // fans the ingested events out to the private relays next frame.
        if let (Some(action), Some(secret)) = (action, &signer) {
            store::apply(
                ctx.ndb,
                &self.active().slug,
                &view,
                &author,
                &store::Signer::new(secret, channel.as_ref()),
                action,
                &mut store::NoPublish,
            );
            self.wake();
        }

        AppResponse::default()
    }

    /// Drain a switcher request (raised in the UI state): switch the active board
    /// or seed a new one. Both persist the selection so it survives a restart,
    /// which needs `signer` — a watch-only account can still switch boards for the
    /// session, it just can't record the choice or create a board.
    ///
    /// Split out of [`render_board`](Self::render_board) because a board that
    /// hasn't folded still draws its switcher (see [`ui::unfolded_board_ui`]) and
    /// so still has this one request to drain, even though no board edit exists to
    /// apply there.
    fn drain_board_nav(
        &mut self,
        ctx: &mut AppContext<'_>,
        author: &Pubkey,
        signer: Option<&[u8; 32]>,
        boards: &[BoardSummary],
    ) {
        let Some(nav) = self.state.take_nav() else {
            return;
        };
        match nav {
            // The switcher entry carries the board's full coordinate, so selecting
            // a joined board (owned by a co-member) keeps its owner rather than
            // collapsing onto one of ours with the same slug.
            BoardNav::Switch(coord) => {
                self.active = Some(coord);
                if let Some(secret) = signer {
                    store::save_board_pref(
                        ctx.ndb,
                        author,
                        secret,
                        self.active(),
                        &mut store::NoPublish,
                    );
                }
                self.wake();
            }
            BoardNav::Create(title) => {
                let Some(secret) = signer else {
                    return;
                };
                let slug = store::board_slug(&title, |s| boards.iter().any(|b| b.id == s));
                // Born a team-of-one SNS board (sealed + self-shared), so it can be
                // shared later with no history re-seal. Ingest locally only;
                // `update`'s poll fans the new events out to the private relays
                // next frame (see `wake`).
                self.create_board(ctx.ndb, author, secret, &slug, &title);
                // A new board is ours: coordinate owner = the account.
                self.active = Some(event::BoardCoord::new(*author.bytes(), slug));
                store::save_board_pref(
                    ctx.ndb,
                    author,
                    secret,
                    self.active(),
                    &mut store::NoPublish,
                );
                self.wake();
            }
        }
    }
}

/// An empty stand-in for a board that hasn't folded yet, so the board chrome (the
/// switcher, which is the escape hatch off a board that won't fold) can render
/// against something. Its title comes from the switcher roster when that knows it
/// — a joined shared board is listed by slug until its definition arrives — so the
/// board reads by name rather than as a blank while it's still syncing.
///
/// Never hand this to [`store::apply`]: a board-level edit republishes the board
/// definition derived from the view it was given, so an edit against a placeholder
/// would overwrite the real definition with an empty one.
fn placeholder_board(active: &event::BoardCoord, boards: &[BoardSummary]) -> BoardView {
    let title = boards
        .iter()
        .find(|b| b.id == active.slug && b.owner == active.owner)
        .map_or_else(|| active.slug.clone(), |b| b.title.clone());
    BoardView {
        id: active.slug.clone(),
        author: active.owner,
        title,
        description: String::new(),
        created_at: 0,
        columns: Vec::new(),
        archived: Vec::new(),
    }
}

/// Derive the header sync indicator from the account's private relay set (owned
/// by [`notedeck::Accounts`]) and the relay pool's live connection status.
/// Mirrors the diagnostic in [`notedeck::PrivateRelaySync`]'s change logging: an
/// empty set (or one with no websocket relay) is local-only; a set with at least
/// one *connected* relay is syncing; otherwise a relay is configured but we're
/// not reaching it yet.
fn sync_status(ctx: &AppContext) -> ui::SyncStatus {
    let private_relays = ctx.accounts.selected_account_private_relays();
    let has_private = private_relays
        .iter()
        .any(|relay| matches!(relay, RelayId::Websocket(_)));
    if !has_private {
        return ui::SyncStatus::LocalOnly;
    }
    let inspect = ctx.remote.relay_inspect();
    let connected = private_relays.iter().any(|relay| {
        let RelayId::Websocket(url) = relay else {
            return false;
        };
        inspect
            .relay_infos()
            .any(|info| info.relay_url == url && info.status == RelayStatus::Connected)
    });
    if connected {
        ui::SyncStatus::Syncing
    } else {
        ui::SyncStatus::Offline
    }
}

/// The switcher's [`BoardSummary`] list for an already-finalized set of boards,
/// sorted by slug. A free function over [`BoardCache::all_boards`]'s output so the
/// foreground render and the test harness summarize one finalize identically,
/// without a second finalize.
fn board_summaries(boards: &[BoardView]) -> Vec<BoardSummary> {
    let mut summaries: Vec<BoardSummary> = boards
        .iter()
        .map(|v| BoardSummary {
            owner: v.author,
            id: v.id.clone(),
            title: v.title.clone(),
        })
        .collect();
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

/// The roster entry for the active board, if it is shared. A board is shared iff
/// its coordinate is in the roster — matched on the full coordinate (owner +
/// slug), not the slug — so a board you *own* and shared still routes through the
/// multi-writer fold (author-scoped folding would hide co-members' cards). Own
/// *private* boards aren't in the roster and return `None`.
fn active_shared_team<'a>(
    teams: &'a [teams::Team],
    active: &event::BoardCoord,
) -> Option<&'a teams::Team> {
    let coord = active.coordinate();
    teams.iter().find(|t| t.board_addr == coord)
}

/// The SNS channel a roster entry's edits seal into, if its keys are usable.
fn team_channel(team: &teams::Team) -> Option<store::SnsChannel> {
    team.sns_keys().map(|keys| store::SnsChannel { keys })
}

/// The SNS channel the board at `addr` seals into — `None` for a plaintext board
/// (one that was never self-shared, so isn't in the roster). Keyed on the full
/// coordinate, like [`active_shared_team`]: two boards that merely share a slug
/// are different boards with different keys.
fn board_channel(teams: &[teams::Team], addr: &str) -> Option<store::SnsChannel> {
    teams
        .iter()
        .find(|t| t.board_addr == addr)
        .and_then(team_channel)
}

/// Append joined shared boards to the switcher list, deduped by coordinate: a
/// board already present with the same owner + slug (one you own and shared) is
/// skipped, but two boards that merely share a slug with different owners both
/// remain. Sorts by slug then owner for a stable order.
fn merge_shared_boards(boards: &mut Vec<BoardSummary>, shared: impl Iterator<Item = BoardSummary>) {
    for entry in shared {
        if boards
            .iter()
            .any(|b| b.owner == entry.owner && b.id == entry.id)
        {
            continue;
        }
        boards.push(entry);
    }
    boards.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.owner.cmp(&b.owner)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostrdb::Filter;
    use nostrdb_net::FullKeypair;

    use crate::cache::tests::{TestSync, total_cards};

    /// A click on an inline widget resolves to the app's navigation target
    /// (see [`resolve_open_target`]): a board opens itself with no card detail,
    /// while an issue opens its owning board *and* its own card detail.
    #[tokio::test]
    async fn resolve_open_target_board_and_issue() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| total_cards(v) == 7).await;

        // Pull a board (kind 30619) and an issue (kind 1621) note id out of the db.
        let (board_id, issue_id, issue_board) = {
            let txn = Transaction::new(&t.ndb).unwrap();
            let board = t
                .ndb
                .query(
                    &txn,
                    &[Filter::new().kinds([event::KIND_BOARD as u64]).build()],
                    1,
                )
                .unwrap()
                .into_iter()
                .next()
                .expect("seeded board")
                .note;
            let issue = t
                .ndb
                .query(
                    &txn,
                    &[Filter::new().kinds([event::KIND_ISSUE as u64]).build()],
                    1,
                )
                .unwrap()
                .into_iter()
                .next()
                .expect("seeded issue")
                .note;
            let issue_board = match event::parse(&issue).expect("issue parses") {
                event::HeadwayEvent::Issue(i) => i.board_id,
                _ => unreachable!("queried kind 1621"),
            };
            (
                NoteId::new(*board.id()),
                NoteId::new(*issue.id()),
                issue_board,
            )
        };

        // A board opens itself, with no card detail to pop. The target carries the
        // board's full coordinate (owner + slug), not a bare slug.
        let board_target = resolve_open_target(&t.ndb, board_id).expect("board resolves");
        assert_eq!(board_target.board.slug, store::BOARD_ID);
        assert_eq!(board_target.board.owner, *t.kp.pubkey.bytes());
        assert_eq!(board_target.card, None);

        // An issue opens its board and its own card detail.
        let issue_target = resolve_open_target(&t.ndb, issue_id).expect("issue resolves");
        assert_eq!(issue_target.board.slug, issue_board);
        assert_eq!(issue_target.board.owner, *t.kp.pubkey.bytes());
        assert_eq!(issue_target.card, Some(issue_id));
    }

    /// Breakage #1 (owner-blindness): a board you *own* and shared must select as
    /// shared — routed through the multi-writer fold that gathers co-members'
    /// cards — even though you own a board of that slug. Its coordinate is in the
    /// roster (self-shared team-of-one), so `active_shared_team` matches it; a
    /// private board of yours (not in the roster) still selects as own. Paired with
    /// `shared_board_folds_via_cache`, which proves the shared fold then surfaces a
    /// co-member's coordinate-anchored card.
    #[test]
    fn own_shared_board_selects_as_shared() {
        let owner = FullKeypair::generate();
        let team = teams::Team {
            team_root: hex::encode([0x11u8; 32]),
            board_addr: event::board_address(&owner.pubkey, "roadmap"),
            epoch: None,
            shared_at: 0,
        };
        let teams = vec![team.clone()];

        // Your own "roadmap" — same slug you own — selects as shared because its
        // coordinate is in the roster (the old "own wins" rule would have hidden
        // co-members' cards here).
        let shared = event::BoardCoord::new(*owner.pubkey.bytes(), "roadmap");
        assert_eq!(active_shared_team(&teams, &shared), Some(&team));

        // A private board you own is absent from the roster, so it selects as own.
        let private = event::BoardCoord::new(*owner.pubkey.bytes(), "notes");
        assert_eq!(active_shared_team(&teams, &private), None);

        // A same-slug board owned by someone else is a different coordinate: not
        // this team.
        let foreign = event::BoardCoord::new([0x99u8; 32], "roadmap");
        assert_eq!(active_shared_team(&teams, &foreign), None);
    }

    /// Breakage #3: two joined boards that share a slug but differ in owner are
    /// distinct coordinates, so both stay in the switcher; a board you own and also
    /// shared (same coordinate) is not duplicated.
    #[test]
    fn switcher_dedups_by_coordinate_not_slug() {
        let alice = [0xAAu8; 32];
        let bob = [0xBBu8; 32];

        // No own boards: Alice's "notes" and Bob's "notes" both survive.
        let mut boards = Vec::new();
        merge_shared_boards(
            &mut boards,
            [
                BoardSummary {
                    owner: alice,
                    id: "notes".into(),
                    title: "Alice notes".into(),
                },
                BoardSummary {
                    owner: bob,
                    id: "notes".into(),
                    title: "Bob notes".into(),
                },
            ]
            .into_iter(),
        );
        assert_eq!(boards.len(), 2);
        assert!(boards.iter().any(|b| b.owner == alice && b.id == "notes"));
        assert!(boards.iter().any(|b| b.owner == bob && b.id == "notes"));

        // An own board you also shared (same coordinate) is listed once.
        let mut with_own = vec![BoardSummary {
            owner: alice,
            id: "roadmap".into(),
            title: "Roadmap".into(),
        }];
        merge_shared_boards(
            &mut with_own,
            [BoardSummary {
                owner: alice,
                id: "roadmap".into(),
                title: "Roadmap".into(),
            }]
            .into_iter(),
        );
        assert_eq!(with_own.iter().filter(|b| b.id == "roadmap").count(), 1);
    }
}
