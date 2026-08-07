use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use enostr::{NoteId, Pubkey, RelayId, RelayStatus};
use nostrdb::{Ndb, NoteKey, Subscription, Transaction};
use notedeck::{
    App, AppContext, AppResponse, ColorTheme, DataPath, DataPathType, PrivateRelaySync,
    fan_out_unseen_notes,
};

pub use headway::{event, store};

mod tools;
mod ui;

use ui::{BoardNav, CardBoardOp, board_ui, empty_state};
pub use ui::{BoardUiState, board_inline_ui, card_chip_ui, card_inline_ui, issue_inline_ui};

use event::{BoardReducer, BoardView};

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
    /// The active board's slug. Switched via the header switcher and restored
    /// per-account from disk; defaults to [`store::BOARD_ID`].
    board_id: String,
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
            board_id: store::BOARD_ID.to_string(),
            board_account: None,
            state: BoardUiState::default(),
            private_sync: PrivateRelaySync::new("headway"),
            seeded: false,
            repaint_frames: 0,
            pending_open: None,
            board_cache: Rc::new(RefCell::new(BoardCache::default())),
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
    /// Driven from [`update`](App::update) (which runs every frame for all opened
    /// apps), so the poll/fan-out loop keeps ticking even off-foreground.
    fn pump_repaint(&mut self, ctx: &egui::Context) {
        if self.repaint_frames > 0 {
            self.repaint_frames -= 1;
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
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

    /// Act on a pending [`open`](Self::open): resolve which board the entity lives
    /// on and switch there, then — for a card — open its detail once that board's
    /// view has folded in. A board just needs the switch. Runs early each render.
    ///
    /// Switching boards is one frame ahead of the fold, so a cross-board card jump
    /// lands on the board first and pops the detail on the following frame once the
    /// view catches up; `open`'s repaint burst keeps us ticking until it does.
    fn process_pending_open(&mut self, ctx: &mut AppContext, author: &Pubkey) {
        let Some(note_id) = self.pending_open else {
            return;
        };
        let Some(target) = resolve_open_target(ctx.ndb, note_id) else {
            // Not a headway entity we can route to (unresolved / unexpected kind).
            self.pending_open = None;
            return;
        };

        // A card lives on whichever board it's *placed* on, which a cross-board
        // move makes differ from the origin board its `a` tag records (what
        // `resolve_open_target` reads). Route to the board it's actually on so the
        // detail can open; fall back to the origin board when it isn't folded.
        let board_id = match &target.card {
            Some(card) => Transaction::new(ctx.ndb)
                .ok()
                .and_then(|txn| {
                    self.board_cache
                        .borrow_mut()
                        .with_boards(ctx.ndb, &txn, author, |boards| {
                            event::locate_card_in_boards(boards, author, card.bytes())
                        })
                        .flatten()
                        .map(|located| located.board_id)
                })
                .unwrap_or(target.board_id),
            None => target.board_id,
        };

        // Switch to the owning board first; the fold lands on a later frame.
        if self.board_id != board_id {
            self.board_id = board_id;
            save_board_pref(ctx.path, author, &self.board_id);
            self.wake();
            return;
        }

        let Some(card) = target.card else {
            // A board: switching to it was the whole job.
            self.pending_open = None;
            return;
        };

        // On the right board: open the card's detail once its view has folded in.
        // Until then keep the request pending and retry on the next repaint.
        let folded = Transaction::new(ctx.ndb).ok().is_some_and(|txn| {
            self.board_cache
                .borrow_mut()
                .board(ctx.ndb, &txn, author, &self.board_id)
                .is_some_and(|v| v.id == self.board_id)
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
    /// The board to switch to.
    board_id: String,
    /// The card whose detail to open once the board has folded in, or `None` when
    /// the target is a board itself.
    card: Option<NoteId>,
}

/// Resolve a headway board/issue note into the [`OpenTarget`] for
/// [`Headway::open`]. An issue opens its board *and* its own detail; a board just
/// opens itself. `None` for anything that isn't one of those.
fn resolve_open_target(ndb: &Ndb, note_id: NoteId) -> Option<OpenTarget> {
    let txn = Transaction::new(ndb).ok()?;
    let note = ndb.get_note_by_id(&txn, note_id.bytes()).ok()?;
    match event::parse(&note)? {
        event::HeadwayEvent::Issue(issue) => Some(OpenTarget {
            board_id: issue.board_id,
            card: Some(NoteId::new(issue.id)),
        }),
        event::HeadwayEvent::Board(board) => Some(OpenTarget {
            board_id: board.id,
            card: None,
        }),
        _ => None,
    }
}

/// Whether `kind` is a headway entity the [`Headway`] app can open inline — a
/// board or an issue. Used by the shell to route a clicked inline widget here.
pub fn is_headway_kind(kind: u32) -> bool {
    matches!(kind, event::KIND_BOARD | event::KIND_ISSUE)
}

/// One entry in the board switcher: a board's stable `id` (slug) and its display
/// `title`. Folded from the account's events by [`BoardCache::boards`].
pub struct BoardSummary {
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
    fn update(&mut self, ctx: &mut AppContext<'_>, egui_ctx: &egui::Context) {
        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`/`remote`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // On first update and after an account switch, restore that account's
        // last-selected board from disk (falling back to the default). Re-arm the
        // auto-seed so a fresh account still gets its default board seeded.
        if self.board_account != Some(author) {
            self.board_id =
                load_board_pref(ctx.path, &author).unwrap_or_else(|| store::BOARD_ID.to_string());
            self.board_account = Some(author);
            self.seeded = false;
        }

        // Declare the inbound cross-device subscription (catch-up + realtime)
        // against the account's private relays, and resolve the same set as our
        // outbound publish targets. Empty => local-only.
        let private_relays = self
            .private_sync
            .update(ctx, vec![event::headway_filter(&author)]);

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

        // No board yet: auto-seed one for an account that can sign. Guarded by
        // `seeded` so the board-existence check (a finalize) only runs until we've
        // confirmed or created one — not every frame. The seeded events fan out via
        // the same poll path on a following frame. (The UI feedback for this state
        // is drawn in `render`.)
        if !self.seeded
            && let Some(secret) = &signer
        {
            let has_board = Transaction::new(ctx.ndb).ok().is_some_and(|txn| {
                self.board_cache
                    .borrow_mut()
                    .board(ctx.ndb, &txn, &author, &self.board_id)
                    .is_some()
            });
            if has_board {
                // Already have a board (e.g. synced from another device): nothing
                // to seed, and no need to keep checking.
                self.seeded = true;
            } else {
                store::seed_default_board(
                    ctx.ndb,
                    &author,
                    secret,
                    &self.board_id,
                    &mut store::NoPublish,
                );
                self.seeded = true;
                self.wake();
            }
        }

        self.pump_repaint(egui_ctx);
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

        // Navigate to an entity a click elsewhere asked us to open (see `open`).
        self.process_pending_open(ctx, &author);

        // Sync (subscription poll, private-relay fan-out, auto-seed) already ran
        // in `update` this frame; read the folded boards off the same shared cache
        // (a cold, Headway-never-opened session seeds it lazily on this read). One
        // finalize backs both the active board and the switcher list.
        let all_boards = Transaction::new(ctx.ndb)
            .ok()
            .map(|txn| {
                self.board_cache
                    .borrow_mut()
                    .all_boards(ctx.ndb, &txn, &author)
            })
            .unwrap_or_default();
        let Some(view) = all_boards.iter().find(|v| v.id == self.board_id).cloned() else {
            // No board yet. `update` auto-seeds one for a signing account; a
            // watch-only account can't create one.
            let msg = if signer.is_some() {
                "Setting up your board…"
            } else {
                "Sign in with a key to create your Headway board."
            };
            empty_state(ui, &theme, msg);
            return AppResponse::default();
        };

        // Render against the folded `view` (owned, so the cache borrow above is
        // already dropped — which matters here: a description that references
        // another card resolves through `self.board_cache` *during* `board_ui`,
        // so holding a borrow across the render would panic the `RefCell`).
        let boards = board_summaries(&all_boards);
        // Header sync indicator: are we reaching a private relay right now?
        let sync = sync_status(ctx);
        let action = board_ui(ui, &theme, ctx, &view, &boards, sync, &mut self.state);

        // A switcher request (raised in the UI state): switch the active board or
        // seed a new one. Both persist the selection so it survives a restart.
        if let Some(nav) = self.state.take_nav() {
            match nav {
                BoardNav::Switch(board_id) => {
                    self.board_id = board_id;
                    save_board_pref(ctx.path, &author, &self.board_id);
                    self.wake();
                }
                BoardNav::Create(title) => {
                    if let Some(secret) = &signer {
                        let slug = store::board_slug(&title, |s| boards.iter().any(|b| b.id == s));
                        // Ingest locally only; `update`'s poll fans the new events
                        // out to the private relays next frame (see `wake`).
                        store::seed_board(
                            ctx.ndb,
                            &author,
                            secret,
                            &slug,
                            &title,
                            &mut store::NoPublish,
                        );
                        self.board_id = slug;
                        save_board_pref(ctx.path, &author, &self.board_id);
                        self.wake();
                    }
                }
            }
        }

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
            let source = store::BoardRef {
                id: &self.board_id,
                view: &view,
            };
            let target = store::BoardRef {
                id: &mv.to_board,
                view: &target_view,
            };
            // Ingest locally only; `update`'s poll fans the new events out to the
            // private relays next frame (see `wake`).
            match mv.op {
                CardBoardOp::Move => {
                    store::move_card_between_boards(
                        ctx.ndb,
                        source,
                        target,
                        &author,
                        secret,
                        mv.card,
                        None,
                        &mut store::NoPublish,
                    );
                }
                CardBoardOp::Link => {
                    store::link_card(
                        ctx.ndb,
                        source,
                        target,
                        &author,
                        secret,
                        mv.card,
                        None,
                        &mut store::NoPublish,
                    );
                }
            }
            self.wake();
        }

        // Apply the collected action by ingesting events locally. Mutations need
        // a signing key; a watch-only account simply can't edit. `update`'s poll
        // fans the ingested events out to the private relays next frame.
        if let (Some(action), Some(secret)) = (action, &signer) {
            store::apply(
                ctx.ndb,
                &self.board_id,
                &view,
                &author,
                secret,
                action,
                None,
                &mut store::NoPublish,
            );
            self.wake();
        }

        AppResponse::default()
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
    let infos = inspect.relay_infos();
    let connected = private_relays.iter().any(|relay| {
        let RelayId::Websocket(url) = relay else {
            return false;
        };
        infos
            .iter()
            .any(|info| info.relay_url == url && info.status == RelayStatus::Connected)
    });
    if connected {
        ui::SyncStatus::Syncing
    } else {
        ui::SyncStatus::Offline
    }
}

// ---------------------------------------------------------------------------
// Per-account board selection, persisted across restarts
// ---------------------------------------------------------------------------

/// The file holding each account's last-selected board: a JSON map of pubkey-hex
/// → board slug, under the app's settings dir. One file for all accounts so a
/// single read/write keeps every account's choice.
fn board_pref_path(path: &DataPath) -> std::path::PathBuf {
    path.path(DataPathType::Setting).join("headway-boards.json")
}

/// The board slug `author` last selected, if one was saved.
fn load_board_pref(path: &DataPath, author: &Pubkey) -> Option<String> {
    let data = std::fs::read_to_string(board_pref_path(path)).ok()?;
    let map: HashMap<String, String> = serde_json::from_str(&data).ok()?;
    map.get(&author.hex()).cloned()
}

/// Persist `author`'s selected board, merging into the existing map so other
/// accounts' choices are preserved. Best-effort: a write failure is non-fatal.
fn save_board_pref(path: &DataPath, author: &Pubkey, board_id: &str) {
    let file = board_pref_path(path);
    let mut map: HashMap<String, String> = std::fs::read_to_string(&file)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    map.insert(author.hex(), board_id.to_string());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(&file, json);
    }
}

// ---------------------------------------------------------------------------
// Inline renderers — drawing a single headway entity referenced from elsewhere
// (e.g. a `nostr:` link in a notebook note), via notedeck's `KindRenderer`
// registry. These are read-only and self-contained, unlike the editable board.
// ---------------------------------------------------------------------------

/// One account's cached headway state within [`BoardCache`]: a live subscription
/// to that author's events plus the long-lived [`BoardReducer`] they fold into.
/// The reducer holds *all* of the author's boards ([`event::fold_board`] folds an
/// account's whole history), so one entry backs the foreground board and every
/// inline reference to any of that author's boards.
#[derive(Default)]
struct CachedAuthor {
    reducer: Option<BoardReducer>,
    sub: Option<Subscription>,
    /// Memoized [`BoardReducer::finalize`] of `reducer`, rebuilt lazily on the
    /// next read after a fold changed the reducer (see [`BoardCache::advance`]).
    /// `finalize` walks every placement into an owned `Vec<BoardView>` (cloning
    /// each card); without this every inline reference re-walked it *twice* a
    /// frame — once to resolve the ref, once to render the chip — so N on-screen
    /// references cost 2·N finalizes per frame. Reads borrow this and extract just
    /// the owned data they need ([`BoardCache::with_boards`]).
    finalized: Option<Vec<BoardView>>,
    /// Keys the subscription drained but that weren't yet visible under the
    /// read txn they were polled with (their note committed after that txn's
    /// snapshot — see [`event::reduce_delta`]). Retried on the next advance with
    /// a fresher snapshot; without this they'd be lost until a full re-seed,
    /// which is why a cross-device edit could go missing until an app restart.
    pending: Vec<NoteKey>,
}

/// The single board-data engine for the Headway app: one subscription + folded
/// [`BoardReducer`] per account, shared (behind `Rc<RefCell<…>>`) between the app
/// and everything it registers for inline display.
///
/// The foreground board and every inline widget — the
/// [`KindRenderer`](notedeck::KindRenderer)s and the
/// [`ReferenceParser`](notedeck::ReferenceParser) — read the *same* reducer, so a
/// realtime edit (a CLI move, a remote sync) that [`update`](App::update)'s
/// per-frame [`poll`](Self::poll) folds in is immediately visible to an inline
/// chip drawn in another app, not just to the open board.
///
/// Keyed by author, not by board: [`event::fold_board`] folds an account's whole
/// board history into one reducer, so a single entry serves all of that author's
/// boards with no per-board refold. Driven by `&Ndb` (the
/// [`KindRenderer`](notedeck::KindRenderer) render path has no `&mut Ndb`), so a
/// chip seeds and folds on render even when Headway isn't the foreground surface.
/// First touch folds the history once to seed; later touches fold in only the
/// freshly-arrived notes ([`event::reduce_delta`]) — an incremental step, sound
/// because the fold is commutative and idempotent. Subscriptions live for the
/// app's lifetime; the touched-account set is tiny, so there's no eviction.
#[derive(Default)]
struct BoardCache {
    authors: HashMap<Pubkey, CachedAuthor>,
    /// Test-only count of full-history folds, to assert later frames fold deltas
    /// rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
    /// Test-only count of [`BoardReducer::finalize`] calls, to assert repeated
    /// reads within a frame reuse the memoized boards rather than re-finalizing.
    #[cfg(test)]
    finalizes: u32,
}

/// The outcome of advancing an author's reducer one step (see
/// [`BoardCache::poll`]).
#[derive(Default)]
struct PollResponse {
    /// The reducer was seeded or had new notes folded in this call, so the caller
    /// schedules follow-up repaints (keep waking while edits stream in).
    changed: bool,
    /// Note keys folded in *incrementally* this call — empty on a seed (those are
    /// historical, not new ingests) and on a no-op. The caller fans these out to
    /// the account's private relays (see [`notedeck::fan_out_unseen_notes`]).
    fresh: Vec<NoteKey>,
}

impl BoardCache {
    /// Ensure a live subscription + seeded reducer for `author` and fold in any
    /// freshly-arrived notes, reporting what changed. The shared primitive behind
    /// [`poll`](Self::poll) and [`reducer`](Self::reducer): the app's per-frame
    /// pump and a chip's lazy fold-on-render both flow through it, so a board is
    /// seeded once and thereafter only ever folds deltas.
    #[profiling::function]
    fn advance(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> PollResponse {
        let entry = self.authors.entry(*author).or_default();
        if entry.sub.is_none() {
            entry.sub = ndb.subscribe(&[event::headway_filter(author)]).ok();
        }
        let (changed, fresh) = match entry.sub {
            // No subscription: fold the whole history each frame so edits still show.
            None => {
                entry.reducer = event::fold_board(ndb, txn, author);
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
                    entry.reducer = event::fold_board(ndb, txn, author);
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
                    let deferred = event::reduce_delta(reducer, ndb, txn, &batch);
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
        // it and let the next read rebuild it once (see `with_boards`).
        if changed {
            entry.finalized = None;
        }
        #[cfg(test)]
        if changed && fresh.is_empty() {
            // A full seed fold, as opposed to an incremental delta (non-empty
            // `fresh`) or a no-op (`!changed`).
            self.full_reloads += 1;
        }
        PollResponse { changed, fresh }
    }

    /// Advance `author`'s reducer and report the change — the per-frame pump
    /// called from [`update`](App::update). Fan out [`fresh`](PollResponse::fresh)
    /// and wake on [`changed`](PollResponse::changed).
    fn poll(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> PollResponse {
        self.advance(ndb, txn, author)
    }

    /// Advance `author`'s reducer, (re)finalize it *at most once per fold*, and run
    /// `read` against the memoized [`BoardView`]s — the single place a finalize is
    /// spent. Returns `None` only when the reducer hasn't seeded yet.
    ///
    /// Every per-frame inline read (resolve a ref, render a chip) flows through
    /// here, so on a steady-state frame the first read finalizes and the rest reuse
    /// the cached [`finalized`](CachedAuthor::finalized) vec. `read` extracts owned
    /// data from the borrowed slice so the caller's cache borrow can drop before it
    /// draws.
    #[profiling::function]
    fn with_boards<R>(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        read: impl FnOnce(&[BoardView]) -> R,
    ) -> Option<R> {
        self.advance(ndb, txn, author);
        let entry = self.authors.get_mut(author)?;
        if entry.finalized.is_none() {
            entry.finalized = Some(entry.reducer.as_ref()?.finalize());
            #[cfg(test)]
            {
                self.finalizes += 1;
            }
        }
        let entry = self.authors.get(author)?;
        Some(read(entry.finalized.as_ref()?))
    }

    /// Fold and pick a single board (`board_id`) authored by `author`, seeding the
    /// reducer on first touch. `None` before the first fold or when no such board
    /// exists. The foreground board, a cross-board move target and an inline board
    /// widget all resolve through this.
    #[profiling::function]
    fn board(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        board_id: &str,
    ) -> Option<BoardView> {
        self.with_boards(ndb, txn, author, |boards| {
            event::find_board(boards, author, board_id).cloned()
        })
        .flatten()
    }

    /// Every board `author` currently holds, seeding on first touch. The foreground
    /// render derives *both* the active board and the switcher list from this
    /// single fold (memoized, see [`with_boards`](Self::with_boards)) rather than
    /// finalizing per read. Clones the memoized vec once (the foreground reads it
    /// once per frame); inline widgets take [`board`](Self::board) instead, which
    /// extracts a single board without cloning the whole set.
    #[profiling::function]
    fn all_boards(&mut self, ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Vec<BoardView> {
        self.with_boards(ndb, txn, author, |boards| boards.to_vec())
            .unwrap_or_default()
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
            id: v.id.clone(),
            title: v.title.clone(),
        })
        .collect();
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

/// Wrap an inline widget's response so clicking it opens the entity in the
/// Headway app: sense clicks over the drawn area, show a pointer cursor on hover,
/// and on click emit a `NoteAction::Note` for `note`. The shell routes headway
/// kinds to [`Headway::open`] (see [`is_headway_kind`]) rather than the timeline.
fn open_on_click(
    ui: &egui::Ui,
    response: egui::Response,
    note: &nostrdb::Note,
) -> notedeck::KindRenderResponse {
    let response = response.interact(egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let action = response
        .clicked()
        .then(|| notedeck::AppAction::Note(notedeck::NoteAction::note(NoteId::new(*note.id()))));
    notedeck::KindRenderResponse::with_action(response, action)
}

/// Renders a headway issue (kind 1621) referenced inline, e.g. from a notebook
/// note. Registered into [`notedeck::KindRendererRegistry`] at app startup.
///
/// The kind-1621 note is only the card's *creation-time* snapshot; its current
/// title/labels/description come from folding the owning board's later edits. So
/// we fold the board (cached, see [`BoardCache`]) and render the resolved
/// [`event::CardView`], falling back to the raw snapshot if the board isn't local.
pub struct HeadwayIssueRenderer {
    cache: Rc<RefCell<BoardCache>>,
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
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        profiling::scope!("HeadwayIssueRenderer::render");
        let note = req.note;
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return notedeck::KindRenderResponse::new(ui.weak("invalid headway issue"));
        };
        let author = Pubkey::new(issue.board_author);
        // Resolve the card off the (cached) folded board and draw the shape the
        // context asks for: a compact chip inline in prose, the full card as a
        // block embed. Resolve across *all* the author's boards, not the card's
        // `a`-tag board: a cross-board move leaves the `a` tag on the origin board
        // while the card lives on the destination (see [`event::locate_card`]).
        // `.and_then` resolves owned data so the cache borrow drops before drawing.
        // Locate the card across *all* the author's boards off the memoized
        // finalize (see [`with_boards`]), not the card's `a`-tag board: a
        // cross-board move leaves the `a` tag on the origin while the card lives
        // on the destination (see [`event::locate_card`]). Resolve to owned data
        // so the cache borrow drops before drawing.
        let located = self
            .cache
            .borrow_mut()
            .with_boards(note_context.ndb, req.txn, &author, |boards| {
                event::locate_card_in_boards(boards, &author, &issue.id)
            })
            .flatten();
        let response = match req.context {
            notedeck::RenderContext::Inline => match located {
                Some(located) => card_chip_ui(ui, &theme, &located.card.title, located.column),
                // Card on no folded board: chip from the creation-time snapshot.
                None => card_chip_ui(ui, &theme, &issue.subject, None),
            },
            _ => match located {
                Some(located) => card_inline_ui(ui, &theme, &located.card),
                // Card on no folded board: show the creation-time snapshot.
                None => issue_inline_ui(ui, &theme, &issue),
            },
        };
        open_on_click(ui, response, note)
    }
}

/// Renders a headway board (kind 30619) referenced inline. The note is the
/// addressable board event; we recover its `(author, board_id)` and fold the
/// full board (cached, see [`BoardCache`]) off the local db to summarise it.
pub struct HeadwayBoardRenderer {
    cache: Rc<RefCell<BoardCache>>,
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
        req: &notedeck::KindRenderRequest,
    ) -> notedeck::KindRenderResponse {
        profiling::scope!("HeadwayBoardRenderer::render");
        let note = req.note;
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Board(board)) = event::parse(note) else {
            return notedeck::KindRenderResponse::new(ui.weak("invalid headway board"));
        };
        let author = Pubkey::new(board.author);
        let view = self
            .cache
            .borrow_mut()
            .board(note_context.ndb, req.txn, &author, &board.id);
        let response = match view {
            Some(view) => board_inline_ui(ui, &theme, &view),
            None => ui.weak("headway board not found"),
        };
        open_on_click(ui, response, note)
    }
}

/// The inline reference parser for a bare headway card id —
/// `<board-slug>#<word>-<word>-<word>` (e.g. `commerce#purse-metal-toilet`) — the
/// same canonical form a human or the CLI writes. Registered via
/// [`App::reference_parsers`]; the browser's markdown scanner recognises the ref
/// in a run of text ([`find`](notedeck::ReferenceParser::find)),
/// [`resolve`](notedeck::ReferenceParser::resolve)s it to the card's kind-1621
/// issue note, and draws it with the already-registered [`HeadwayIssueRenderer`].
///
/// There is no `scheme:` prefix — a headway ref is bare — so [`find`] recognises
/// the shape directly and cheaply (slug, `#`, three dash-joined words) while
/// [`resolve`] is the authority: a candidate that doesn't fold to a real card is
/// re-rendered as plain text, so a loose match is invisible, not a broken chip.
///
/// **Author gap.** A headway ref carries no pubkey, but folding a board needs
/// one. We resolve against [`ReferenceResolveCtx::selected_account`], so a ref
/// only resolves to a board the *current* account owns. A later grammar
/// extension can carry an explicit author (e.g. an `naddr`-style coordinate).
pub struct HeadwayRefParser {
    /// Shared with the app's kind renderers (see [`Headway::board_cache`]), so a
    /// card referenced by word id folds the same cached board a directly-embedded
    /// card/board does — the fold happens once per board, not once per surface.
    cache: Rc<RefCell<BoardCache>>,
}

impl HeadwayRefParser {
    /// A byte is part of a board slug: lowercase ascii, a digit, or `-` (see
    /// `store::board_slug`). Uppercase is intentionally excluded — slugs are
    /// lowercase — but `resolve` lowercases anyway, so a stray capital only costs
    /// a slug boundary, never a wrong resolution.
    fn is_slug_byte(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
    }

    /// End index of a `word-word-word` run of *exactly three* lowercase-letter
    /// (BIP-39) words starting at `i`, or `None`. Words are `[a-z]+` joined by a
    /// single `-`; a fourth dash-joined word is rejected so a longer hyphenated
    /// run isn't mistaken for a word id.
    fn three_words_end(bytes: &[u8], i: usize) -> Option<usize> {
        let mut pos = i;
        for word in 0..3 {
            let start = pos;
            while pos < bytes.len() && bytes[pos].is_ascii_lowercase() {
                pos += 1;
            }
            if pos == start {
                return None; // empty word (leading/double/trailing dash)
            }
            if word < 2 {
                // Require a single `-` before each of the next two words.
                if pos < bytes.len() && bytes[pos] == b'-' {
                    pos += 1;
                } else {
                    return None; // fewer than three words
                }
            }
        }
        // A `-` right after the third word means a fourth is coming: not a bare
        // three-word id.
        if pos < bytes.len() && bytes[pos] == b'-' {
            return None;
        }
        Some(pos)
    }
}

impl notedeck::ReferenceParser for HeadwayRefParser {
    fn id(&self) -> &'static str {
        "headway"
    }

    #[profiling::function]
    fn find(&self, text: &str) -> Option<std::ops::Range<usize>> {
        let bytes = text.as_bytes();
        let mut search = 0;
        while let Some(rel) = text[search..].find('#') {
            let hash = search + rel;
            // Continue past this `#` on the next iteration regardless of outcome.
            search = hash + 1;

            // Slug: the run of slug bytes ending immediately before the `#`.
            let mut slug_start = hash;
            while slug_start > 0 && Self::is_slug_byte(bytes[slug_start - 1]) {
                slug_start -= 1;
            }
            if slug_start == hash {
                continue; // no slug (e.g. a markdown heading `# ...`)
            }

            // Three dash-joined words right after the `#`.
            if let Some(end) = Self::three_words_end(bytes, hash + 1) {
                return Some(slug_start..end);
            }
        }
        None
    }

    #[profiling::function]
    fn resolve(
        &self,
        matched: &str,
        ctx: &notedeck::ReferenceResolveCtx,
    ) -> Option<notedeck::ResolvedRef> {
        let (slug, words) = matched.split_once('#')?;
        // Author gap: no pubkey in the ref, so fold the current account's boards.
        let author = ctx.selected_account?;
        let board_id = slug.to_lowercase();

        // Fold (memoized) the board, then re-encode its cards to match the word id.
        // The closure yields the owned `NoteId` so the cache borrow drops here, and
        // resolving through the shared finalize means a chip drawn right after
        // reuses this frame's fold instead of walking the reducer again.
        let note_id = self
            .cache
            .borrow_mut()
            .with_boards(ctx.ndb, ctx.txn, &author, |boards| {
                event::find_board(boards, &author, &board_id)
                    .and_then(|board| event::resolve_card_by_wordid(board, words))
            })
            .flatten()?;
        Some(notedeck::ResolvedRef::note(note_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use futures_util::StreamExt;
    use nostrdb::{Config, Filter, Ndb, SubscriptionStream};
    use std::time::{Duration, Instant};

    /// A headless harness driving a [`BoardCache`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors the
    /// `store::tests::TestNdb` poll-loop pattern (ingest is async).
    struct TestSync {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        cache: BoardCache,
    }

    impl TestSync {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
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
        fn poll(&mut self) -> bool {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.poll(&self.ndb, &txn, &self.kp.pubkey).changed
        }

        fn seed(&mut self) {
            seed_demo(&self.ndb, &self.kp);
        }

        /// Fold and pick the default board out of the cache, if present.
        fn view(&mut self) -> Option<BoardView> {
            self.board(store::BOARD_ID)
        }

        /// Fold and pick an arbitrary board — exercises reading a board other than
        /// the default out of the one per-account reducer.
        fn board(&mut self, board_id: &str) -> Option<BoardView> {
            let txn = Transaction::new(&self.ndb).unwrap();
            self.cache.board(&self.ndb, &txn, &self.kp.pubkey, board_id)
        }

        /// Every board folded for this account, summarized for the switcher.
        fn boards(&mut self) -> Vec<BoardSummary> {
            let txn = Transaction::new(&self.ndb).unwrap();
            board_summaries(&self.cache.all_boards(&self.ndb, &txn, &self.kp.pubkey))
        }

        /// Fold until the default board satisfies `pred` (ingest is async).
        async fn wait<F: Fn(&BoardView) -> bool>(&mut self, pred: F) {
            self.wait_until(|t| t.view().is_some_and(|v| pred(&v)))
                .await;
        }

        /// Like [`wait`](Self::wait) but keyed on the switcher's board list —
        /// used when the quiescence signal is a *second* board appearing rather
        /// than a change to the default board.
        async fn wait_boards<F: Fn(&[BoardSummary]) -> bool>(&mut self, pred: F) {
            self.wait_until(|t| pred(&t.boards())).await;
        }

        /// Shared loop for [`wait`](Self::wait) / [`wait_boards`](Self::wait_boards):
        /// pump the reducer until `done` holds, awaiting the writer's own ingest
        /// notification (see [`await_ingest`]) between folds rather than polling
        /// against a wall-clock deadline — the only race-free way to wait on an
        /// async ingest. Replaces the old fixed deadline and `drain()` quiescence
        /// guess (see `demo_seed_complete`).
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

    /// Await the next batch of ingested notes on `stream`. Panics if the
    /// subscription closes first, so a predicate that never holds surfaces as a
    /// test-timeout hang rather than a silent spin.
    async fn await_ingest(stream: &mut SubscriptionStream) {
        stream
            .next()
            .await
            .expect("subscription closed before predicate held");
    }

    fn total_cards(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    /// The demo seed's terminal state: [`seed_demo`] moves the drag card into
    /// In Progress with the *last* event it ingests, so the drag card sitting in
    /// column 2 means every earlier seed event has folded and the async writer is
    /// quiescent. A deterministic "seed fully materialised" signal that — unlike a
    /// card-count check plus a `drain()` — can't be satisfied mid-materialisation.
    fn demo_seed_complete(view: &BoardView) -> bool {
        view.columns.get(2).is_some_and(|col| {
            col.cards
                .iter()
                .any(|c| c.title == "Drag-and-drop between columns")
        })
    }

    /// Seed the populated demo board for the sync tests to fold and act on. The
    /// production seed is card-less; the fixture lives in [`store::seed_demo_board`].
    /// Seeded in the past so follow-up edits (stamped with the wall clock)
    /// always sort after it.
    fn seed_demo(ndb: &Ndb, kp: &FullKeypair) {
        store::seed_demo_board(
            ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            1_700_000_000,
            &mut store::NoPublish,
        );
    }

    /// Subscribing before seeding, then polling, materialises the whole board
    /// from events already in ndb.
    #[tokio::test]
    async fn poll_materialises_the_board() {
        let mut t = TestSync::new();
        // Subscribe first so the seed's ingests are reported as new notes.
        t.poll();
        t.seed();

        // Wait on the seed's *terminal* event (the drag card landing in In
        // Progress), not a card count: `total_cards == 7` can hold mid-fold
        // before the drag move settles, asserting a half-materialised layout.
        t.wait(demo_seed_complete).await;
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

        // A board opens itself, with no card detail to pop.
        let board_target = resolve_open_target(&t.ndb, board_id).expect("board resolves");
        assert_eq!(board_target.board_id, store::BOARD_ID);
        assert_eq!(board_target.card, None);

        // An issue opens its board and its own card detail.
        let issue_target = resolve_open_target(&t.ndb, issue_id).expect("issue resolves");
        assert_eq!(issue_target.board_id, issue_board);
        assert_eq!(issue_target.card, Some(issue_id));
    }

    /// An edit ingested after the initial load is picked up on a later poll —
    /// the cache reflects the change, not a stale snapshot.
    #[tokio::test]
    async fn poll_reloads_on_change() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        // Fully materialise the seed first (Todo settles at 2 once the drag card
        // moves out), so the edit below folds against a stable board.
        t.wait(demo_seed_complete).await;

        // Apply against the cached pre-edit view (as render does).
        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Fresh card".to_string(),
                    labels: vec![],
                    parent: None,
                },
                None,
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
        t.seed();
        store::seed_board(
            &t.ndb,
            &t.kp.pubkey,
            &t.secret(),
            "work",
            "Work",
            &mut store::NoPublish,
        );

        // The 'work' board is seeded after the whole demo board, so its event is
        // the last one ingested: waiting for it to appear means every demo event
        // has folded in too, with no quiescence guess.
        t.wait_boards(|bs| bs.iter().any(|b| b.id == "work")).await;
        let folds = t.cache.full_reloads;

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
            t.cache.full_reloads, folds,
            "reading another board must not trigger a full re-fold"
        );
    }

    /// Once quiescent, polling with nothing new must NOT re-fold — this is the
    /// whole point of the cache (no per-frame walk of the event history).
    #[tokio::test]
    async fn poll_does_not_refold_when_idle() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(demo_seed_complete).await;

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
        t.seed();
        t.wait(demo_seed_complete).await;

        // Seeding does exactly one full fold; everything since is incremental.
        assert_eq!(
            t.cache.full_reloads, 1,
            "seeding should fold the history once"
        );

        {
            let view = t.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                &view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Delta card".to_string(),
                    labels: vec![],
                    parent: None,
                },
                None,
                &mut store::NoPublish,
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3).await;

        assert_eq!(
            t.cache.full_reloads, 1,
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
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = BoardCache::default();

        // One read cycle (what a renderer does each frame): bring the cached
        // reducer up to date and fold out the board.
        let fold = |cache: &mut BoardCache, ndb: &Ndb| -> Option<BoardView> {
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
            cache.full_reloads, 1,
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
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = BoardCache::default();

        // Prime the cache's own (internal) subscription and open a read stream to
        // await the writer, both before seeding so every seeded event is reported.
        {
            let txn = Transaction::new(&ndb).unwrap();
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
        }
        let mut stream = ingest_stream(&ndb, &kp.pubkey);
        seed_demo(&ndb, &kp);

        // Fold in the seed as the writer delivers it, until the board has fully
        // materialised *and quiesced*. `seed_demo` keeps ingesting after the 7th
        // card lands (parent relations, card amendments, and finally the drag-card
        // move into In Progress), so stopping at `total_cards == 7` would leave
        // those trailing notes to arrive mid-frame below — each folds and
        // re-finalizes, breaking the "no fold ⇒ no finalize" invariant this test
        // measures. The drag move is the *last* seed event, so `demo_seed_complete`
        // (drag card in In Progress) means every prior event has folded too.
        while {
            let txn = Transaction::new(&ndb).unwrap();
            let ready = cache
                .board(&ndb, &txn, &kp.pubkey, store::BOARD_ID)
                .is_some_and(|v| demo_seed_complete(&v));
            drop(txn);
            !ready
        } {
            await_ingest(&mut stream).await;
        }

        // A steady frame with no new notes: many reads (what N inline references
        // cost — resolve then render each) reuse the memo built while materialising,
        // finalizing zero more times.
        let steady = cache.finalizes;
        let txn = Transaction::new(&ndb).unwrap();
        for _ in 0..20 {
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
            cache.all_boards(&ndb, &txn, &kp.pubkey);
        }
        drop(txn);
        assert_eq!(
            cache.finalizes - steady,
            0,
            "reads with no intervening fold re-finalized instead of reusing the memo"
        );

        // A fold invalidates the memo (see `advance`); the next frame's many reads
        // then share *exactly one* finalize. Simulate the invalidation a freshly
        // ingested note performs, then read many times.
        cache.authors.get_mut(&kp.pubkey).unwrap().finalized = None;
        let after_fold = cache.finalizes;
        let txn = Transaction::new(&ndb).unwrap();
        for _ in 0..20 {
            cache.board(&ndb, &txn, &kp.pubkey, store::BOARD_ID);
            cache.all_boards(&ndb, &txn, &kp.pubkey);
        }
        drop(txn);
        assert_eq!(
            cache.finalizes - after_fold,
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
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
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
                .get(&kp.pubkey)
                .and_then(|a| a.reducer.as_ref())
                .is_some_and(|r| r.finalize().iter().any(|b| b.id == id))
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
        {
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
            !cache.authors[&kp.pubkey].pending.is_empty(),
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
        assert!(cache.authors[&kp.pubkey].pending.is_empty());
        assert_eq!(
            cache.full_reloads, 1,
            "recovered by folding a delta, not by re-seeding the whole history"
        );
    }

    fn ref_parser() -> HeadwayRefParser {
        HeadwayRefParser {
            cache: Rc::new(RefCell::new(BoardCache::default())),
        }
    }

    /// [`HeadwayRefParser::find`] is a purely syntactic filter: it recognises the
    /// bare `slug#word-word-word` shape (no scheme prefix) and rejects near-misses
    /// — markdown headings, non-three-word runs — leaving the rest to `resolve`.
    #[test]
    fn ref_parser_find_recognises_bare_word_ids() {
        use notedeck::ReferenceParser;
        let p = ref_parser();
        let hit = |s: &str| p.find(s).map(|r| s[r].to_string());

        // A bare canonical ref, standalone and embedded in prose.
        assert_eq!(
            hit("commerce#purse-metal-toilet").as_deref(),
            Some("commerce#purse-metal-toilet")
        );
        assert_eq!(
            hit("see commerce#purse-metal-toilet here").as_deref(),
            Some("commerce#purse-metal-toilet")
        );
        // Slugs may carry digits and dashes (see `store::board_slug`).
        assert_eq!(
            hit("2024-goals#maple-river-canyon").as_deref(),
            Some("2024-goals#maple-river-canyon")
        );
        // A trailing period (or any non-letter) ends the third word.
        assert_eq!(
            hit("done: commerce#purse-metal-toilet.").as_deref(),
            Some("commerce#purse-metal-toilet")
        );

        // Markdown headings have no slug before the `#`.
        assert!(hit("# Heading here").is_none());
        assert!(hit("## purse-metal-toilet").is_none());
        // Fewer or more than three words is not a word id.
        assert!(hit("board#one-two").is_none());
        assert!(hit("board#one-two-three-four").is_none());
        // Uppercase words aren't BIP-39 words.
        assert!(hit("board#One-Two-Three").is_none());
        // Plain prose yields nothing.
        assert!(hit("just a sentence, nothing here").is_none());

        // A rejected candidate doesn't swallow a later valid ref.
        assert_eq!(
            hit("board#one-two then b#red-green-blue").as_deref(),
            Some("b#red-green-blue")
        );
    }

    /// [`HeadwayRefParser::resolve`] folds the referenced board (via the shared
    /// [`BoardCache`]) and re-encodes its cards to match the word id,
    /// yielding the card's kind-1621 issue note. It resolves relative to the
    /// selected account (the author gap), so no account means no resolution.
    #[tokio::test]
    async fn ref_parser_resolves_word_id_to_card() {
        use notedeck::{ReferenceParser, ReferenceResolveCtx};
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(demo_seed_complete).await;

        // Take a real card and its word id off the folded board.
        let (card_id, words) = {
            let txn = Transaction::new(&t.ndb).unwrap();
            let board = event::load_board(&t.ndb, &txn, &t.kp.pubkey, store::BOARD_ID).unwrap();
            let card = board.columns.iter().flat_map(|c| &c.cards).next().unwrap();
            (card.id, headway::wordid::encode(card.id.bytes()))
        };
        let matched = format!("{}#{}", store::BOARD_ID, words);

        let p = ref_parser();
        let txn = Transaction::new(&t.ndb).unwrap();
        let ctx = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: Some(t.kp.pubkey),
        };
        // The canonical ref resolves to that card's issue note.
        assert_eq!(p.resolve(&matched, &ctx).unwrap().note_id, card_id);
        // A well-formed word id that encodes no card doesn't resolve.
        let bogus = format!("{}#zoo-zoo-zoo", store::BOARD_ID);
        assert!(p.resolve(&bogus, &ctx).is_none());
        // Without a selected account the author gap can't be filled.
        let ctx_no_acct = ReferenceResolveCtx {
            ndb: &t.ndb,
            txn: &txn,
            selected_account: None,
        };
        assert!(p.resolve(&matched, &ctx_no_acct).is_none());
    }
}
