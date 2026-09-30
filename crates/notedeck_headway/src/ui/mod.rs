//! egui rendering for the Headway board.
//!
//! Split out of `lib.rs` to keep that file focused on the data/reducer layer
//! (the [`crate::Headway`] app, its [`crate::BoardSync`], the inline-render
//! cache and the `KindRenderer` impls). Everything here is pure egui rendering
//! plus the transient per-board UI state it threads through; none of it touches
//! the nostrdb subscription/fold machinery.
//!
//! This module owns the board's top-level [`board_ui`] and its [`BoardUiState`];
//! each surface it draws lives in a submodule: the [`header`], the kanban
//! [`grid`], the card [`detail`] pane, the dependency [`graph`], the commit
//! [`review`] pane, the [`archived`] sheet, the board [`filter`], the [`inline`] widgets the
//! `KindRenderer`s use, and the small shared [`widgets`].

use std::collections::{HashMap, HashSet};

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{SPACING_LG, SPACING_MD, SPACING_SM};
use notedeck_ui::chord::ChordState;

use crate::BoardSummary;
use crate::event::{self, BoardView, CardView};
use crate::keys::{self, BoardPending};
use crate::nav::{NavPos, ReviewTarget};
use crate::store::BoardAction;

mod archived;
mod detail;
mod filter;
mod graph;
mod grid;
mod header;
mod inline;
mod review;
mod widgets;

pub use graph::{GRAPH_NODE_SIZE, GraphNodeView, graph_node_ui};
pub use header::SyncStatus;
pub use inline::{board_inline_ui, card_chip_ui, card_inline_ui, issue_inline_ui};

pub(crate) use filter::{CardFilter, ViewFilter, filter_field_id};

use archived::archived_sheet_ui;
use detail::card_detail_pane_ui;
use filter::filter_ref_jump;
use graph::graph_view_ui;
use grid::{add_column_ui, column_ui, start_move_anims};
use header::{board_switcher, filtered_badge, sync_indicator, view_options_menu};
use review::{
    NOTICE_SECS, ReviewQueue, ReviewSection, ReviewUi, in_review_cards, review_pane_ui,
    review_queue_ui,
};

pub(crate) use review::{QueueNotice, QueuePending, QueueStep, SessionOpen, reason_field_id};

/// Transient, per-board UI state that must persist across frames but isn't part
/// of the data model (e.g. which column has an open "add card" composer).
#[derive(Default)]
pub struct BoardUiState {
    /// Which inline text editor is open on the board, if any.
    edit: InlineEdit,
    /// Shared buffer backing whichever inline editor ([`InlineEdit`]) is open;
    /// only one can be active at a time.
    edit_text: String,
    /// Set when the active inline editor should grab focus once — on open, and
    /// after each card is added, so rapid keyboard entry keeps working.
    focus_edit: bool,
    /// The card whose detail view is open, if any.
    selected: Option<NoteId>,
    /// The board grid's keyboard cursor: the card wearing the accent ring.
    /// Deliberately separate from [`selected`](Self::selected) (the open
    /// detail), and never reseeded from the nav route, so backing out of a
    /// card's detail leaves the cursor where you were. Its grid position is
    /// re-derived from the folded view each time it's needed (see
    /// [`crate::cursor`]), so reorders and remote edits can't strand it.
    cursor: Option<NoteId>,
    /// Set when the cursor moves by keyboard and its card should be scrolled
    /// into view. Consumed into [`scroll_this_frame`](Self::scroll_this_frame)
    /// once per grid render.
    scroll_to_cursor: bool,
    /// This render's latched [`scroll_to_cursor`](Self::scroll_to_cursor). Taken
    /// once per grid pass, so a cursor card that isn't drawn this frame (filtered
    /// out, or the board swapped under it) drops the request instead of leaving
    /// it stuck to fire on some later, unrelated frame.
    scroll_this_frame: bool,
    /// How far into a board-key chord (`gg`) the grid is. Ticked and advanced
    /// by [`crate::keys::board_keys`].
    pub(crate) chord: ChordState<BoardPending>,
    /// The review queue's own `gg` chord, ticked by [`crate::keys::queue_keys`].
    pub(crate) queue_chord: ChordState<QueuePending>,
    /// Set by the grid's own drop-down menus (board switcher, View, column ⋯)
    /// on each frame they're open, and taken by the next frame's
    /// [`crate::keys::board_keys`]. egui 0.31's `menu_button` keeps its open
    /// state in a private per-bar slot, not the popup memory
    /// (`Memory::any_popup_open`), so this latch is how the keymap knows to
    /// leave keys (Esc, Enter, `a`) to an open menu.
    grid_menu_open: bool,
    /// Whether the which-key strip of board keys is pinned open (toggled with
    /// `?`). It also shows, narrowed, while a `g` chord is pending regardless;
    /// see [`crate::keys::key_hints`].
    show_key_hints: bool,
    /// Which card the detail edit buffers below were seeded from. When this
    /// differs from `selected`, the buffers are refreshed from the board.
    detail_for: Option<NoteId>,
    /// Edit buffer for the selected card's title.
    detail_title: String,
    /// The board's title value the title buffer was last synced to. While the
    /// title is shown rendered (not being typed into), a change here versus the
    /// live board drives a refresh so remote edits appear immediately. Keying on
    /// this last-synced value — rather than comparing the buffer to the board —
    /// keeps a just-committed local edit (whose buffer leads the board until its
    /// async ingest lands) from being clobbered back to the old text.
    detail_title_seen: String,
    /// Whether the title is shown rendered or in its raw editor.
    detail_title_mode: EditMode,
    /// Edit buffer for the selected card's description.
    detail_desc: String,
    /// As [`detail_title_seen`](Self::detail_title_seen), for the description
    /// buffer.
    detail_desc_seen: String,
    /// Whether the description is shown rendered or in its raw editor.
    detail_desc_mode: EditMode,
    /// Buffer backing the "add label" field in the detail sheet.
    new_label: String,
    /// Whether the detail sheet's "add label" composer is open (it collapses to
    /// a "+ Add label" affordance, Linear-style).
    label_composer: bool,
    /// Buffer backing the "add subissue" field in the detail sheet.
    new_subissue: String,
    /// Whether the detail sheet's "add sub-issue" composer is open.
    subissue_composer: bool,
    /// Buffer backing the "write a comment" composer in the detail pane.
    comment_draft: String,
    /// Whether the archived-cards sheet is open.
    showing_archived: bool,
    /// A board-switcher request raised this frame (switch or create). The app
    /// reads and clears it to act on it; the new-board *composer* is just another
    /// [`InlineEdit`], sharing `edit_text`.
    nav: Option<BoardNav>,
    /// A cross-board card request raised this frame from a card's context menu
    /// (move to / link onto another board). The app reads and clears it to act.
    card_move: Option<CardBoardMove>,
    /// Free-text board filter. Empty means no filtering. Plain words match a
    /// card's title/description/labels/word-id (all must match,
    /// case-insensitive); a `label:foo` token narrows to cards carrying a
    /// label containing `foo`. Pasting a full card reference filters to that
    /// card, switching boards first if it lives on another known board (see
    /// [`CardFilter`] and [`filter_ref_jump`]).
    filter: String,
    /// Hide cards that are someone's sub-issue (they still show as checklist
    /// rows inside their parent's detail). A Linear-style view option, toggled
    /// from the header's "View" menu; folded into [`ViewFilter`] alongside the
    /// text [`filter`](Self::filter) so every card-visibility check asks one
    /// question.
    hide_subissues: bool,
    /// Where each card was drawn last frame (screen rect + column), so a card
    /// that has jumped to a new column since can be animated sliding in from its
    /// previous slot rather than teleporting.
    card_pos: HashMap<NoteId, CardPos>,
    /// Cards mid-slide, mapped to the screen rect they're sliding *from*. The
    /// 0→1 progress itself lives in egui's animation manager, keyed by card id
    /// (see `grid::move_progress_id`); this only remembers the origin slot.
    moves: HashMap<NoteId, egui::Rect>,
    /// Cards whose next cross-column landing should *not* slide, because the user
    /// just dragged them there by hand — the pointer already carried the card
    /// across, so a slide is redundant. Only relay/CLI moves (which the user
    /// didn't physically perform) animate. A *set*, not a single id, because the
    /// flag must outlive the async ingest between the drop and the folded view
    /// reporting the new column: a second drag can be dropped before the first
    /// move lands, leaving two cards awaiting their move at once. Each entry is
    /// consumed when its move is observed (see [`start_move_anims`]).
    suppress_anim: HashSet<NoteId>,
    /// The epic whose dependency-graph view is open, taking over the whole pane
    /// like the card detail. `None` outside graph mode; set by
    /// [`open_graph`](Self::open_graph) and checked before the detail branch in
    /// [`board_ui`], so it wins the pane while it's set.
    graph_epic: Option<NoteId>,
    /// Persisted pan/zoom of the graph's [`egui::Scene`] (the scene-space region
    /// shown in the pane), mirroring notebook's canvas `scene_rect`. `None` until
    /// the graph first draws, when it is seeded to frame the whole laid-out graph;
    /// thereafter it tracks the user's panning/zooming. Reset to `None` on each
    /// [`open_graph`](Self::open_graph) so a freshly opened epic re-frames.
    graph_scene_rect: Option<egui::Rect>,
    /// The node a connect-drag is currently dragging a new blocking edge *from*,
    /// kept across frames so its side handles are re-created (and the drag keeps
    /// reporting) after the pointer leaves the source node — egui only promotes a
    /// press to a `dragged()` once the threshold is crossed, by which point the
    /// pointer has usually left the handle. Mirrors notebook's `connecting`.
    /// Transient: cleared whenever the graph view is left.
    graph_connecting: Option<NoteId>,
    /// The commit review pane: which card's review is open (seeded from the nav
    /// route like [`graph_epic`](Self::graph_epic)), the picked record, and the
    /// off-thread loader its diffs arrive through.
    review: ReviewUi,
    /// The card detail's Review section: its rows' elided locations and
    /// whether every record shows.
    review_section: ReviewSection,
    /// The review queue (`R`): its snapshot of the In Review column and which
    /// card the review pane shows. While it's open the pane shows its card.
    queue: ReviewQueue,
    /// A short-lived message and when (egui time) it went up: an `R` that
    /// found nothing in review, a verdict that finished the queue, a queue key
    /// with nothing to act on. Drawn in the header, or the queue's bar while
    /// it's open, for [`NOTICE_SECS`] seconds.
    notice: Option<(QueueNotice, f64)>,
    /// A board edit left for the next frame, because a frame applies one: the
    /// move behind an `X` verdict's comment.
    follow_up: Option<BoardAction>,
    /// An agentium session a review key (`a`/`A`) asked to open this frame.
    /// The keys run without an [`AppContext`](notedeck::AppContext), so
    /// [`board_ui`] takes it ([`take_open`](Self::take_open)) and raises it as
    /// an [`AppAction::Open`](notedeck::AppAction::Open).
    open: Option<notedeck::OpenUri>,
}

impl BoardUiState {
    /// Take this frame's board-switcher request (switch or create), if any, for
    /// the app to act on. Clears it so it fires once.
    pub fn take_nav(&mut self) -> Option<BoardNav> {
        self.nav.take()
    }

    /// Take this frame's cross-board card request (move or link), if any, for the
    /// app to act on. Clears it so it fires once.
    pub fn take_card_move(&mut self) -> Option<CardBoardMove> {
        self.card_move.take()
    }

    /// Open a card's full-pane detail view, e.g. when navigating in from a click
    /// on the card's inline widget elsewhere in the app (see [`crate::Headway::open`]).
    /// The detail edit buffers reseed from the board on the next render because
    /// `detail_for` now differs from `selected`.
    pub fn open_card(&mut self, card: NoteId) {
        self.selected = Some(card);
    }

    /// Open the dependency-graph view for `epic`, taking over the whole pane
    /// until it's dismissed. Resets the persisted scene rect so the newly opened
    /// epic re-frames its graph rather than inheriting the last one's pan/zoom.
    ///
    /// The card the graph was entered from stays [`selected`](Self::selected), so
    /// closing the graph returns to that card's detail rather than the board.
    pub fn open_graph(&mut self, epic: NoteId) {
        self.graph_epic = Some(epic);
        self.graph_scene_rect = None;
        self.graph_connecting = None;
    }

    /// The epic whose dependency-graph view is open, if any.
    pub fn graph_epic(&self) -> Option<NoteId> {
        self.graph_epic
    }

    /// Seed whether the graph view is open from the chrome global-history route
    /// this frame renders, the graph counterpart to [`set_selected`](Self::set_selected).
    ///
    /// Unlike [`open_graph`](Self::open_graph) this leaves
    /// [`graph_scene_rect`](Self::graph_scene_rect) untouched, so re-visiting a
    /// graph entry via back/forward keeps its pan/zoom — only a fresh open (the
    /// detail entry-point button) reframes. Called at the top of each render pass
    /// so a global back/forward/jump onto (or off) a graph entry is reflected
    /// before the board draws; the UI may then close the graph, and the app diffs
    /// the result back into a nav request.
    pub fn set_graph_epic(&mut self, epic: Option<NoteId>) {
        self.graph_epic = epic;
    }

    /// The card whose review pane is open, if any.
    pub fn review_card(&self) -> Option<NoteId> {
        self.review.card()
    }

    /// The record the open review pane shows, by note id (`None` = newest): what
    /// a [`Review`](crate::HeadwayRoute::Review) push snapshots.
    pub fn review_record(&self) -> Option<NoteId> {
        self.review.record()
    }

    /// Seed the review pane — which card, and which of its records — from the
    /// chrome global-history route this frame renders, the review counterpart to
    /// [`set_graph_epic`](Self::set_graph_epic). The record is applied only when
    /// the route differs from the last one seeded, so a pick made in the open
    /// pane holds while back/forward onto another entry reopens that entry's
    /// record. The loaded diffs are left alone.
    pub fn set_review(&mut self, target: Option<ReviewTarget>) {
        self.review.seed(target);
    }

    /// Which view depth this state shows (board, a card, a graph, a review or
    /// the review queue), the value the app diffs across a render into a nav
    /// request.
    pub(crate) fn nav_pos(&self) -> NavPos {
        NavPos::of(
            self.selected,
            self.graph_epic,
            self.queue.is_open(),
            self.review.card(),
        )
    }

    /// Seed whether the review queue shows from the chrome global-history route
    /// this frame renders, the queue counterpart to
    /// [`set_graph_epic`](Self::set_graph_epic). The queue's snapshot and
    /// position are left alone, so back/forward onto its entry reopens it where
    /// it was left.
    pub fn set_queue_open(&mut self, open: bool) {
        self.queue.set_open(open);
    }

    /// The review the open queue shows (its current card, newest record), or
    /// `None` when it's closed: what the review pane is seeded with under a
    /// queue route, which names no card itself.
    pub fn queue_review(&self) -> Option<ReviewTarget> {
        self.queue.target()
    }

    /// Open the review queue over the board's In Review column, snapshotted
    /// now. With nothing in review the queue stays shut and the header says so
    /// for a few seconds instead (`now` is egui time).
    pub(crate) fn open_review_queue(&mut self, view: &BoardView, now: f64) {
        if self.queue.start(in_review_cards(view)) {
            self.notice = None;
            self.review.seed(self.queue.target());
        } else {
            self.set_notice(QueueNotice::NothingInReview, now);
        }
    }

    /// Step the review queue one card `step`'s way, pointing the pane at it.
    pub(crate) fn step_queue(&mut self, step: QueueStep) {
        self.queue.step(step);
        self.review.seed(self.queue.target());
    }

    /// Leave the review queue for the board grid, with the grid's cursor on the
    /// card the queue last showed. A notice about the queue's card (no
    /// explainer) goes with it.
    pub(crate) fn close_queue(&mut self) {
        if let Some(card) = self.queue.close() {
            self.set_cursor(card);
        }
        self.review.close();
        self.queue_chord.clear();
        self.notice = None;
    }

    /// Whether the review queue is showing.
    #[cfg(test)]
    pub(crate) fn queue_open(&self) -> bool {
        self.queue.is_open()
    }

    /// The card whose detail is currently open, if any.
    ///
    /// The app diffs this against the [`selection it seeded from the nav
    /// route`](Self::set_selected) after a render pass to turn a board↔card
    /// transition the UI made (a card click, a detail close, a subissue swap) into
    /// a chrome global-history request — see [`crate::Headway`].
    pub fn selected(&self) -> Option<NoteId> {
        self.selected
    }

    /// Seed which card's detail is open from the chrome global-history route this
    /// frame renders, making the nav stack — not this transient view-state — the
    /// source of truth for board-vs-card. Called at the top of each render pass so
    /// a global back/forward/jump is reflected before the board draws; the UI may
    /// then mutate the selection (a click, a close), and the app diffs the result
    /// back into a nav request.
    pub fn set_selected(&mut self, selected: Option<NoteId>) {
        self.selected = selected;
    }

    /// The card holding the board grid's keyboard cursor, if any.
    pub fn cursor(&self) -> Option<NoteId> {
        self.cursor
    }

    /// Put the keyboard cursor on `id` and scroll its card into view on the next
    /// grid render.
    pub(crate) fn set_cursor(&mut self, id: NoteId) {
        self.cursor = Some(id);
        self.scroll_to_cursor = true;
    }

    /// Take the keyboard cursor off the board.
    pub(crate) fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    /// Open the "add card" composer at the foot of column `col`, empty and
    /// focused. Shared by the column's "+ Add card" button and the `n` key.
    pub(crate) fn open_add_card(&mut self, col: usize) {
        self.edit = InlineEdit::AddCard(col);
        self.edit_text.clear();
        self.focus_edit = true;
    }

    /// Whether a board-level overlay owns the keyboard: an inline editor (card
    /// composer, column rename, new column/board), the archived sheet, or the
    /// review queue (whose own keys are [`crate::keys::queue_keys`]).
    pub(crate) fn keys_blocked(&self) -> bool {
        self.edit != InlineEdit::None || self.showing_archived || self.queue.is_open()
    }

    /// Take the [`grid_menu_open`](Self::grid_menu_open) latch: whether one of
    /// the grid's drop-down menus was open last frame.
    pub(crate) fn take_grid_menu_open(&mut self) -> bool {
        std::mem::take(&mut self.grid_menu_open)
    }

    /// Whether the which-key strip is pinned open by `?`.
    pub(crate) fn key_hints_shown(&self) -> bool {
        self.show_key_hints
    }

    /// Show the which-key strip if it's hidden, hide it if it's showing.
    pub(crate) fn toggle_key_hints(&mut self) {
        self.show_key_hints = !self.show_key_hints;
    }

    /// Hide the which-key strip (Esc).
    pub(crate) fn hide_key_hints(&mut self) {
        self.show_key_hints = false;
    }
}

/// A card's on-screen placement last frame: its screen rect and which column it
/// sat in. Used to detect cross-column jumps (drags, detail-sheet moves, or a
/// `headway move` arriving over the relay) and seed the slide animation.
///
/// The column is identified by a hash of its stable id, not its index: indices
/// shift when columns are reordered or removed, which would otherwise read as
/// every card in the board jumping at once.
#[derive(Clone, Copy)]
struct CardPos {
    rect: egui::Rect,
    col: egui::Id,
}

/// The board's inline text editors are mutually exclusive — you can only be
/// composing a card, renaming a column, adding a column, or naming a new board at
/// any one moment — so they share [`BoardUiState::edit_text`] and
/// [`BoardUiState::focus_edit`] and this enum tracks which (if any) is live.
#[derive(Default, PartialEq, Eq)]
enum InlineEdit {
    /// No inline editor open.
    #[default]
    None,
    /// Composing a new card in the column at this index.
    AddCard(usize),
    /// Renaming the column at this index.
    RenameColumn(usize),
    /// Composing a new column.
    AddColumn,
    /// Naming a new board in the switcher.
    NewBoard,
}

/// A request from the board switcher, raised in [`BoardUiState::nav`] for the app
/// to act on. Switching and creating are mutually exclusive, so one enum models
/// the frame's intent rather than a clutch of `Option`s.
pub enum BoardNav {
    /// Switch the active board to this coordinate (owner + slug), so a joined
    /// board owned by a co-member selects distinctly from one of yours.
    Switch(event::BoardCoord),
    /// Create (seed) a new board with this display title, then switch to it.
    Create(String),
}

/// Whether a cross-board card request relocates the card or shares it.
#[derive(Clone, Copy, Debug)]
pub enum CardBoardOp {
    /// Relocate the card to the target board (remove it from the current one).
    Move,
    /// Also place the card on the target board, keeping it on the current one —
    /// membership is placement-driven, so the same issue lives on both.
    Link,
}

/// A cross-board card request raised from a card's context menu, in
/// [`BoardUiState::card_move`] for the app to act on. The `card` moves to (or is
/// linked onto) the board with slug `to_board`.
pub struct CardBoardMove {
    pub card: NoteId,
    pub to_board: String,
    pub op: CardBoardOp,
}

/// How the detail sheet shows an editable field (title or description): the
/// finished, read-only render or its raw text editor. The states are mutually
/// exclusive and the one-shot focus grab only has meaning while editing, so an
/// enum models it more honestly than a pair of bools.
#[derive(Default, PartialEq, Eq)]
enum EditMode {
    /// The read-only render — a heading for the title, markdown for the
    /// description — with an affordance to switch into the editor.
    #[default]
    Rendered,
    /// The raw text editor. `focus` requests a one-shot keyboard-focus grab on
    /// the frame the editor opens.
    Editing { focus: bool },
}

/// Pick the opening mode for an editable field: blank fields drop straight into
/// the editor (nothing to render), populated ones show their render first.
fn seed_edit_mode(text: &str) -> EditMode {
    if text.trim().is_empty() {
        EditMode::Editing { focus: false }
    } else {
        EditMode::Rendered
    }
}

/// Render the board (header, columns, the add-column affordance and the floating
/// card detail sheet) and return the edit the user made this frame, if any —
/// or, on a frame that made none, one an earlier frame left
/// ([`BoardUiState::take_follow_up`]).
pub fn board_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    boards: &[BoardSummary],
    sync: SyncStatus,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    // The review queue's keys run before anything lays out, so one that
    // leaves the queue (`q`, a verdict on its last card, Enter onto the card)
    // has the grid or the detail draw this same frame rather than a blank one.
    let verdict = if state.queue.is_open() {
        keys::queue_keys(ui.ctx(), view, state)
    } else {
        None
    };
    if verdict.is_some() {
        // The next card, and an `X`'s follow-up move, want a frame.
        ui.ctx().request_repaint();
    }
    let action = board_pane_ui(ui, theme, app_ctx, view, boards, sync, state);
    // `a`/`A` in the queue or a review pane leave for the record's session.
    if let Some(open) = state.take_open() {
        app_ctx.app_actions.push(notedeck::AppAction::Open(open));
    }
    // A verdict swallowed the frame's keys, so the pane's edit could only be a
    // drop landing in the same frame; the verdict wins, as a key does over a
    // drop in the grid.
    verdict.or(action).or_else(|| state.take_follow_up())
}

/// [`board_ui`]'s body: whichever pane the state names, and its edit.
fn board_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    boards: &[BoardSummary],
    sync: SyncStatus,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    // A dependency-graph view takes over the whole pane like the detail screen,
    // and wins over it while open (an epic's graph is entered from that epic's
    // detail, which stays selected underneath). An epic that has left the board
    // drops back to the grid rather than rendering an empty pane.
    if let Some(epic) = state.graph_epic {
        if find_card(view, epic).is_some() {
            return graph_view_ui(ui, theme, view, state);
        }
        // Dropped unconditionally, unlike the card selection below: a graph is only
        // ever entered from an epic already on this board, never minted by a deep
        // link, so there is no not-yet-folded-in case to wait for.
        state.graph_epic = None;
        state.graph_scene_rect = None;
        state.graph_connecting = None;
    }

    // The review queue draws the review pane over its current card (its keys
    // ran in `board_ui`).
    if let Some(card) = state.queue.current() {
        review_queue_ui(ui, theme, app_ctx, view, card, state);
        return None;
    }

    // A card's review pane takes over the pane the same way, entered from (and
    // drawn over) that card's detail. A card that has left the board drops back
    // to its detail branch below, which drops it in turn.
    if let Some(card) = state.review.card() {
        if let Some((_, card)) = find_card(view, card) {
            // The queue's session keys (`a`/`A`) work in a plain pane too.
            keys::review_keys(ui.ctx(), view, state);
            review_pane_ui(ui, theme, app_ctx, view, card, state);
            return None;
        }
        state.review.close();
    }

    // A selected card takes over the whole view as a full-pane detail screen,
    // replacing the board grid until dismissed (back / ✕ / Escape). A selection
    // pointing at a card that no longer exists is dropped so we fall back to the
    // board rather than render an empty pane.
    if state
        .selected
        .is_some_and(|id| find_card(view, id).is_some())
    {
        let mut action: Option<BoardAction> = None;
        card_detail_pane_ui(ui, theme, app_ctx, view, state, &mut action);
        return action;
    }
    // Only drop a selection whose card has actually *left* the board. `detail_for`
    // is set the first time the detail pane renders a card and cleared when it
    // closes, so `detail_for == selected` means we have already seen this card
    // here — it went away. A selection the detail has never rendered is instead a
    // card that hasn't folded in *yet*: a fresh cross-app deep link, or a remote
    // card still in flight. Clearing that one would make the app's post-render nav
    // diff read Card→Board and emit a `Back` that pops a real global-history entry
    // (see `reconcile_nav`), snapping a just-opened deep link back to the board.
    // Holding it costs nothing — the grid draws underneath, and the detail opens
    // the frame the card lands.
    if state.detail_for.is_some() && state.detail_for == state.selected {
        state.selected = None;
        state.detail_for = None;
    }

    // Parsed once per frame from the persisted query; reflects the prior
    // frame's keystroke, which is imperceptible in an immediate-mode UI.
    // Bundled with the view options into the single grid-visibility predicate
    // the keymap, the header and the columns share. Owned, so it doesn't hold
    // `state` borrowed.
    let filter = CardFilter::parse(&state.filter, &view.id);
    let view_filter = ViewFilter {
        filter: &filter,
        hide_subissues: state.hide_subissues,
    };

    // Board keys (j/k/h/l, gg/G, Enter, a, /, ?, Esc; H/J/K/L move the cursor
    // card, returned as the action a drop would raise). Only the grid reaches here —
    // the graph and the detail pane returned above and handle their own keys —
    // and it runs before any grid widget lays out, so a key it handles is
    // swallowed before a field that `a` or `/` focuses could type it. The keys
    // stand down during a drag, but should a key action and a drop below ever
    // land in one frame, the drop overwrites it.
    let mut action: Option<BoardAction> = keys::board_keys(ui.ctx(), view, &view_filter, state);
    // The card a click landed on this frame; opens the detail view next frame.
    let mut clicked: Option<NoteId> = None;

    // Latch this pass's scroll-to-cursor request, so it fires on at most one
    // grid render whether or not the cursor card is drawn.
    state.scroll_this_frame = std::mem::take(&mut state.scroll_to_cursor);

    // Kick off (and retire) slide animations for any card that changed columns
    // since last frame. This reads last frame's placements, so afterwards we can
    // clear them (keeping the map's capacity) and let the card renderers refill
    // `card_pos` with this frame's rects as they go.
    start_move_anims(ui.ctx(), view, state);
    state.card_pos.clear();

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            // Board switcher: the active board's title as a dropdown listing the
            // account's other boards, plus a "+ New board" composer.
            board_switcher(ui, theme, view, boards, state);
            ui.add_space(SPACING_SM);

            // Board header: how many cards it holds, or — when narrowed — how many
            // of them are showing.
            let total: usize = view.columns.iter().map(|c| c.cards.len()).sum();
            let shown = view
                .columns
                .iter()
                .flat_map(|c| &c.cards)
                .filter(|c| view_filter.shows(c))
                .count();
            ui.horizontal(|ui| {
                // Sync affordance: is this board reaching a private relay?
                sync_indicator(ui, theme, sync);
                ui.add_space(SPACING_MD);
                // A narrowed board wears a prominent "Filtered" pill (click to
                // clear); an unnarrowed one just states its size, muted.
                if view_filter.is_active() {
                    if filtered_badge(ui, theme, shown, total).clicked() {
                        state.filter.clear();
                        state.hide_subissues = false;
                    }
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "{total} card{} · {} columns",
                            if total == 1 { "" } else { "s" },
                            view.columns.len()
                        ))
                        .color(theme.text_muted),
                    );
                }
                notice_ui(ui, theme, &mut state.notice);
                // The archived entry point only appears when there's something
                // behind it, so the header stays quiet on a fresh board.
                if !view.archived.is_empty() {
                    ui.add_space(SPACING_SM);
                    let label =
                        egui::RichText::new(format!("View archived ({})", view.archived.len()))
                            .color(theme.text_muted);
                    if ui
                        .add(egui::Button::new(label).fill(egui::Color32::TRANSPARENT))
                        .clicked()
                    {
                        state.showing_archived = true;
                    }
                }
                // View menu + filter field, right-aligned. Packs from the right so
                // the clear affordance trails the input and the View menu leads it.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !state.filter.is_empty()
                        && ui
                            .add(egui::Button::new("✕").frame(false))
                            .on_hover_text("Clear filter")
                            .clicked()
                    {
                        state.filter.clear();
                    }
                    // Pin a stable id: the ✕ button is only laid out when the
                    // filter is non-empty, and in this right_to_left layout it
                    // sits ahead of the field. Without a fixed id, typing the
                    // first character inserts that button and shifts egui's
                    // auto-generated id for the field, so it loses focus.
                    let field = egui::TextEdit::singleline(&mut state.filter)
                        .id(filter_field_id())
                        .desired_width(220.0)
                        .hint_text("Filter… e.g. label:bug perf");
                    // A pasted reference to a card on another board switches
                    // to that board (see [`filter_ref_jump`]).
                    if ui.add(field).changed() {
                        filter_ref_jump(view, boards, state);
                    }
                    ui.add_space(SPACING_SM);
                    view_options_menu(ui, theme, state);
                });
            });
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_MD);

            // The which-key strip (`?`, or a pending `g`). Reserved from the
            // bottom before the columns lay out, since their scroll area takes
            // every point of height left; the columns size off what remains.
            if let Some(hints) = keys::key_hints(state) {
                egui::TopBottomPanel::bottom("headway-key-hints")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(egui::Frame::new().inner_margin(egui::Margin {
                        top: SPACING_MD as i8,
                        ..Default::default()
                    }))
                    .show_inside(ui, |ui| keys::key_hints_ui(ui, theme, hints));
            }

            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = SPACING_MD;
                        for col_idx in 0..view.columns.len() {
                            column_ui(
                                ui,
                                theme,
                                view,
                                boards,
                                state,
                                &view_filter,
                                col_idx,
                                &mut action,
                                &mut clicked,
                            );
                        }
                        add_column_ui(ui, theme, state, &mut action);
                    });
                });
        });

    if let Some(card_id) = clicked {
        state.selected = Some(card_id);
        // The clicked card also takes the cursor (no scroll: it's already on
        // screen), so coming back from its detail shows where you were.
        state.cursor = Some(card_id);
    }

    // Archived-cards sheet floats above the board.
    archived_sheet_ui(ui, theme, view, state, &mut action);

    action
}

/// Render the board chrome around a board whose contents haven't folded yet: the
/// switcher plus `message`, and nothing else.
///
/// This is what a not-yet-folded board shows instead of a full-pane message. The
/// distinction matters: a full-pane message takes the switcher away with it, so a
/// board that never folds (a shared board whose definition hasn't arrived, or
/// won't) strands the app with no way to reach another board or make a new one.
/// Here the switcher — the escape hatch — stays live while the board itself is
/// still empty.
///
/// Deliberately no editing affordances: every board-level edit republishes the
/// board definition derived from the view it was given, so an edit made against
/// the placeholder would clobber the real definition once it folds in.
pub fn unfolded_board_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
    message: &str,
) {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            board_switcher(ui, theme, view, boards, state);
            ui.add_space(SPACING_SM);
            ui.label(egui::RichText::new(message).color(theme.text_muted));
        });
}

/// A centered, muted message shown when there's no board to render yet.
pub fn empty_state(ui: &mut egui::Ui, theme: &ColorTheme, message: &str) {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(SPACING_LG * 2.0);
                ui.heading("Headway");
                ui.add_space(SPACING_SM);
                ui.label(egui::RichText::new(message).color(theme.text_muted));
            });
        });
}

/// The [`QueueNotice`] showing, for [`NOTICE_SECS`] seconds after it went up.
/// Schedules the frame that takes it down.
fn notice_ui(ui: &mut egui::Ui, theme: &ColorTheme, notice: &mut Option<(QueueNotice, f64)>) {
    let Some((shown, at)) = *notice else {
        return;
    };
    let left = NOTICE_SECS - (ui.input(|i| i.time) - at);
    if left <= 0.0 {
        *notice = None;
        return;
    }
    ui.add_space(SPACING_SM);
    ui.label(egui::RichText::new(shown.text()).color(theme.warning));
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(left));
}

/// Find a card anywhere on the board, returning its column index and view.
fn find_card(view: &BoardView, card: NoteId) -> Option<(usize, &CardView)> {
    view.columns
        .iter()
        .enumerate()
        .find_map(|(i, col)| col.cards.iter().find(|c| c.id == card).map(|c| (i, c)))
}

/// The title of a card on this board, snapshotted for a global-history entry's
/// label when the app pushes a [`Card`](crate::nav::HeadwayRoute::Card) route
/// (see [`crate::Headway`]). `None` when the card isn't on the folded view — the
/// dropdown then falls back to the app label.
pub(crate) fn card_title(view: &BoardView, card: NoteId) -> Option<String> {
    find_card(view, card).map(|(_, c)| c.title.clone())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bare card with a zero id and the given text, for tests that only care
    /// about its searchable fields. Shared with [`crate::cursor`]'s tests.
    pub(crate) fn card(title: &str, description: &str, labels: &[&str]) -> CardView {
        CardView {
            id: NoteId::new([0u8; 32]),
            author: [0u8; 32],
            title: title.to_string(),
            description: description.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            priority: headway::event::Priority::None,
            due: None,
            estimate: None,
            rank: String::new(),
            seq: None,
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            reviews: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
            blocked_by: vec![],
            blocks: vec![],
            related: vec![],
        }
    }

    /// Board slug used by tests that don't care about reference parsing.
    pub(crate) const BOARD: &str = "headway";
}
