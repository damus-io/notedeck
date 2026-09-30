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
use crate::keys::{self, BoardPending, PanePending};
use crate::nav::{NavPos, ReviewTarget};
use crate::store::BoardAction;

mod archived;
mod card_actions;
mod detail;
mod filter;
mod graph;
mod grid;
mod header;
mod inline;
mod review;
mod review_comments;
mod widgets;

pub use graph::{GRAPH_NODE_SIZE, GraphNodeView, graph_node_ui};
pub use header::SyncStatus;
pub use inline::{board_inline_ui, card_chip_ui, card_inline_ui, issue_inline_ui};

pub(crate) use card_actions::{CardStep, DetailScroll, reason_field_id};
pub(crate) use filter::{CardFilter, ViewFilter, filter_field_id};
#[cfg(test)]
pub(crate) use review_comments::DraftComment;

use archived::archived_sheet_ui;
use card_actions::{ReasonComposer, reason_bar_ui};
use detail::card_detail_pane_ui;
use filter::filter_ref_jump;
use graph::graph_view_ui;
use grid::{add_column_ui, column_ui, start_move_anims};
use header::{board_switcher, filtered_badge, sync_indicator, view_options_menu};
use review::{NOTICE_SECS, ReviewQueue, ReviewSection, ReviewUi, review_pane_ui, review_queue_ui};
use widgets::KeyedText;

pub(crate) use review::{Notice, QueueNotice, QueueScope, SessionOpen};

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
    /// The `gg` chord of the review pane (queue or not) or the detail, ticked
    /// by [`crate::keys::review_pane_keys`] and [`crate::keys::detail_keys`].
    /// Only one of them shows at a time, so they share it.
    pub(crate) pane_chord: ChordState<PanePending>,
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
    /// A card known to have left the board whose detail entry a back is about
    /// to land on: the one a review pane's `a` archived, which the pane backs
    /// out to the detail of, or an epic that left under its open review queue
    /// ([`close_queue`](Self::close_queue)). The selection drops once the card
    /// is off the board, as it does for a card
    /// [`detail_for`](Self::detail_for) names. That one alone can't say so
    /// here: the chrome's back slides, redrawing the pane's entry until it
    /// lands, and if the archive folds in meanwhile the pane's frame drops the
    /// selection and clears `detail_for`, so the detail entry the slide lands
    /// on would hold a card that never comes back. Cleared when the detail
    /// draws another card.
    archived: Option<NoteId>,
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
    /// What this frame asked of the app: a board switch, a cross-board card
    /// move, a session open. The keys and widgets that raise them run without
    /// an [`AppContext`](notedeck::AppContext), so they queue here and the app
    /// drains them once a frame ([`take_effects`](Self::take_effects)). Empty
    /// on almost every frame, which costs nothing: an empty `Vec` holds no
    /// allocation.
    effects: Vec<BoardEffect>,
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
    /// The review queue (`R`): its snapshot of the In Review column (or an
    /// epic's In Review descendants) and which card the review pane shows.
    /// While it's open the pane shows its card.
    queue: ReviewQueue,
    /// The detail Sub-issues header's `"done/total"` count, keyed by
    /// `(done, total)`.
    subissue_count: KeyedText<(usize, usize)>,
    /// The detail Sub-issues header's "Review N" label, keyed by N.
    subtree_review: KeyedText<usize>,
    /// A short-lived message, when it went up and in which view: an `R` that
    /// found nothing in review, a verdict that finished the queue, a key in
    /// any view with nothing to act on. Drawn in its view's header for
    /// [`NOTICE_SECS`] seconds, or until its view is left.
    notice: Option<Notice>,
    /// A board edit left for the next frame, because a frame applies one: the
    /// move behind an `X` verdict's comment.
    follow_up: Option<BoardAction>,
    /// The `X` composer, while it's open, in the view `X` was pressed in; it
    /// closes when that view goes. Every keymap stands down while it's open;
    /// it takes Enter and Esc unless another widget has the keyboard.
    reason: Option<ReasonComposer>,
    /// A scroll the detail's keys asked of it, applied on its next pass.
    detail_scroll: Option<DetailScroll>,
    /// The egui pass at whose end a widget in the detail held the keyboard,
    /// if the last detail pass ended that way: what tells the detail's Esc
    /// that it only unfocused a field ([`BoardUiState::esc_left_a_field`]).
    detail_focus_pass: Option<u64>,
    /// The last card action a keymap applied, and to which card: what the
    /// `card_actions_mean_the_same_in_every_view` test compares across views.
    #[cfg(test)]
    pub(crate) last_card_action: Option<(keys::CardAction, NoteId)>,
}

impl BoardUiState {
    /// Take what this frame asked of the app, in the order it was asked, so
    /// each fires once.
    pub fn take_effects(&mut self) -> Vec<BoardEffect> {
        std::mem::take(&mut self.effects)
    }

    /// Ask the app for `effect`; it's drained after the frame's render.
    pub(crate) fn raise(&mut self, effect: BoardEffect) {
        self.effects.push(effect);
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
            self.queue.is_open().then(|| self.queue.scope()),
            self.review.card(),
        )
    }

    /// Seed whether the review queue shows, and over what, from the chrome
    /// global-history route this frame renders, the queue
    /// counterpart to [`set_graph_epic`](Self::set_graph_epic). The queue's
    /// snapshot and position are left alone when the scope is the one it was
    /// taken for, so back/forward onto its entry reopens it where it was left;
    /// another scope's entry retakes the snapshot (see
    /// [`ReviewQueue::set_open`]).
    pub(crate) fn set_queue_open(&mut self, scope: Option<QueueScope>) {
        self.queue.set_open(scope);
    }

    /// The review the open queue shows (its current card, newest record), or
    /// `None` when it's closed: what the review pane is seeded with under a
    /// queue route, which names no card itself.
    pub fn queue_review(&self) -> Option<ReviewTarget> {
        self.queue.target()
    }

    /// Open the review queue over `scope` — the board's In Review column, or
    /// an epic's In Review descendants — snapshotted now. With nothing in
    /// review the queue stays shut and the header says so for a few seconds
    /// instead (`now` is egui time); an epic's empty queue says so of the
    /// epic, and doesn't fall back to the board's.
    pub(crate) fn open_review_queue(&mut self, view: &BoardView, scope: QueueScope, now: f64) {
        if !self.snapshot_queue(view, scope) {
            self.set_notice(scope.empty_notice(), now);
        }
    }

    /// Open the queue over `scope`'s In Review cards as they are now, pointing
    /// the pane at the first. `false`, changing nothing, when there are none.
    fn snapshot_queue(&mut self, view: &BoardView, scope: QueueScope) -> bool {
        if !self.queue.start(view, scope, scope.snapshot(view)) {
            return false;
        }
        self.notice = None;
        self.review.seed(self.queue.target());
        true
    }

    /// Retake the queue's snapshot when a back/forward landed on a queue entry
    /// of another scope than the one it holds (see
    /// [`ReviewQueue::set_open`]). With nothing left in review there the queue
    /// closes, which the app's nav diff turns into a back off the entry, and
    /// says why. The notice goes up after the close, so it belongs to the view
    /// the back lands on rather than the entry it leaves (which
    /// [`retire_stale_notice`](Self::retire_stale_notice) would take it down
    /// with), and it's worded for that view: an epic that has left the board
    /// lands its queue on the grid, where "Nothing in review under this card"
    /// would be about a card that isn't there. Runs before the frame's keys.
    pub(crate) fn refresh_queue(&mut self, view: &BoardView, now: f64) {
        if !self.queue.needs_snapshot() {
            return;
        }
        let scope = self.queue.scope();
        if self.snapshot_queue(view, scope) {
            return;
        }
        let landed = self.close_queue(view);
        self.set_notice(landed.empty_notice(), now);
    }

    /// Step the review queue one card `step`'s way, pointing the pane at it.
    pub(crate) fn step_queue(&mut self, step: CardStep) {
        self.queue.step(step);
        self.review.seed(self.queue.target());
    }

    /// Leave the review queue: the board's for the grid, with the grid's
    /// cursor on the card the queue last showed; an epic's for the epic's
    /// detail it was opened from. A notice about the queue's card (no
    /// explainer) and an open `X` composer go with it.
    ///
    /// An epic that left the board while its queue was open (archived, moved
    /// to another board) has no detail to land on, so its queue leaves as the
    /// board's does. The back still lands on the epic's detail entry first;
    /// [`archived`](Self::archived) marks the epic gone there, so that entry
    /// backs on to the grid rather than holding the selection as a card not
    /// folded in yet.
    ///
    /// Returns the scope of the view it lands on: the epic's, or the board's
    /// for the board's queue and for an epic's whose epic has gone.
    pub(crate) fn close_queue(&mut self, view: &BoardView) -> QueueScope {
        let card = self.queue.close();
        let epic = self.queue.scope().epic();
        let landed = match epic.filter(|&epic| find_card(view, epic).is_some()) {
            // The epic is what the queue was opened from, and stays selected
            // underneath it.
            Some(epic) => {
                self.selected = Some(epic);
                QueueScope::Epic(epic)
            }
            // The board's queue, or an epic's whose epic has gone.
            None => {
                if let Some(gone) = epic {
                    self.selected = None;
                    self.archived = Some(gone);
                }
                if let Some(card) = card {
                    self.set_cursor(card);
                }
                QueueScope::Board
            }
        };
        self.review.close();
        self.pane_chord.clear();
        self.reason = None;
        self.notice = None;
        landed
    }

    /// Whether the review queue is showing.
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
    /// focused. Shared by the column's "+ Add card" button and the `c` key.
    pub(crate) fn open_add_card(&mut self, col: usize) {
        self.edit = InlineEdit::AddCard(col);
        self.edit_text.clear();
        self.focus_edit = true;
    }

    /// Whether a board-level overlay owns the keyboard: an inline editor (card
    /// composer, column rename, new column/board), the archived sheet, the `X`
    /// composer, or the review queue (whose own keys are
    /// [`crate::keys::review_pane_keys`]).
    pub(crate) fn keys_blocked(&self) -> bool {
        self.edit != InlineEdit::None
            || self.showing_archived
            || self.reason.is_some()
            || self.queue.is_open()
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

/// Something the board UI asks of the app, which only the app can do: it
/// needs the signing key, the other boards' folds, or another app. Raised into
/// [`BoardUiState::effects`] and drained by the app after the frame's render.
///
/// A board edit is not one of these: [`board_ui`] returns that as its
/// [`BoardAction`], and an edit left for the next frame (an `X`'s move) waits in
/// `follow_up`, since it goes out the same way the next frame's edit does.
pub enum BoardEffect {
    /// Switch to another board, or create one.
    Nav(BoardNav),
    /// Move a card to another board, or link it onto one.
    CardMove(CardBoardMove),
    /// Open a reference — a card's agentium session, from `s`/`S` or the
    /// review header's button — in the app that owns it, as an
    /// [`AppAction::Open`](notedeck::AppAction::Open).
    Open(notedeck::OpenUri),
}

/// A request from the board switcher, raised as a [`BoardEffect::Nav`] for the
/// app to act on. Switching and creating are mutually exclusive, so one enum
/// models the request rather than a clutch of `Option`s.
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

/// A cross-board card request raised from a card's context menu, as a
/// [`BoardEffect::CardMove`] for the app to act on. The `card` moves to (or is
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
    // The keys of the queue, a review pane, the detail or the `X` composer
    // run before anything lays out, so one that leaves its view (`q`, a
    // verdict on the queue's last card, Enter onto the card) has the view it
    // lands on draw this same frame rather than a blank one. (The grid's run
    // as it lays out, in `board_pane_ui`.)
    state.refresh_queue(view, ui.ctx().input(|i| i.time));
    // A route reseed or last frame's click may have left the `X` composer's
    // view; it closes before its keys could take this frame's Enter.
    state.retire_stale_reason();
    let keyed = keys::pane_keys(ui.ctx(), view, state);
    state.retire_stale_notice(ui.ctx().cumulative_pass_nr());
    if keyed.is_some() {
        // The next card, and an `X`'s follow-up move, want a frame.
        ui.ctx().request_repaint();
    }
    let action = board_pane_ui(ui, theme, app_ctx, view, boards, sync, state);
    // A key swallowed the frame's keys, so the pane's edit could only be a
    // drop landing in the same frame; the key wins, as it does over a drop in
    // the grid.
    keyed.or(action).or_else(|| state.take_follow_up())
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

    // Each view draws an open `X` composer's bar across its top: here, after
    // the pane keys that may have opened it; the grid's after its own keys.
    // (A composer only stays open in the view it was asked in.)

    // The review queue draws the review pane over its current card (its keys
    // ran in `board_ui`, as the plain pane's and the detail's did).
    if let Some(card) = state.queue.current() {
        reason_bar_ui(ui, theme, state);
        return review_queue_ui(ui, theme, app_ctx, view, card, state);
    }

    // A card's review pane takes over the pane the same way, entered from (and
    // drawn over) that card's detail. A card that has left the board drops back
    // to its detail branch below, which drops it in turn.
    if let Some(card) = state.review.card() {
        if let Some((col, card)) = find_card(view, card) {
            reason_bar_ui(ui, theme, state);
            pane_hints_ui(ui, theme, state);
            return review_pane_ui(ui, theme, app_ctx, view, col, card, state);
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
        reason_bar_ui(ui, theme, state);
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
    //
    // A card a review pane's `a` archived has left too, whatever `detail_for`
    // says: the pane's back slides, and a drop during the slide clears
    // `detail_for` before the detail's entry lands (see `archived`).
    let gone = state.detail_for == state.selected || state.archived == state.selected;
    if state.selected.is_some() && gone {
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

    // Board keys (j/k/h/l, gg/G, c, /, ?, Esc, the card actions; H/J/K/L move
    // the cursor card, returned as the action a drop would raise). Only the grid
    // reaches here — the graph, the review panes and the detail returned above
    // and have their own keys —
    // and it runs before any grid widget lays out, so a key it handles is
    // swallowed before a field that `c` or `/` focuses could type it. The keys
    // stand down during a drag, but should a key action and a drop below ever
    // land in one frame, the drop overwrites it.
    let mut action: Option<BoardAction> = keys::board_keys(ui.ctx(), view, &view_filter, state);
    // Drawn after the keys, so a grid `X` shows its composer this frame.
    reason_bar_ui(ui, theme, state);
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
            let here = state.nav_pos();
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
                notice_ui(ui, theme, &mut state.notice, here);
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
                egui::Panel::bottom("headway-key-hints")
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(egui::Frame::new().inner_margin(egui::Margin {
                        top: SPACING_MD as i8,
                        ..Default::default()
                    }))
                    .show(ui, |ui| keys::key_hints_ui(ui, theme, hints));
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

/// The [`QueueNotice`] showing, for [`NOTICE_SECS`] seconds after it went up,
/// when it's about `here`, the view drawing. Another view leaves it alone: a
/// nav slide draws two views in one pass, and the notice belongs to one of
/// them. Schedules the frame that takes it down.
///
/// Drawing it counts its pass as one its view was seen in
/// ([`BoardUiState::retire_stale_notice`]). The retire's own look runs before
/// the pane, which can still leave the view it saw: a gone epic's queue
/// closes onto the grid, but the back lands on the epic's detail entry first,
/// and only the pane drops that selection. The grid is the notice's view, and
/// without this that pass wouldn't count, so the next would take it down.
fn notice_ui(ui: &mut egui::Ui, theme: &ColorTheme, notice: &mut Option<Notice>, here: NavPos) {
    let Some(up) = notice else {
        return;
    };
    if up.pos != here {
        return;
    }
    let left = NOTICE_SECS - (ui.input(|i| i.time) - up.at);
    if left <= 0.0 {
        *notice = None;
        return;
    }
    up.seen = Some(ui.ctx().cumulative_pass_nr());
    let shown = up.what;
    ui.add_space(SPACING_SM);
    ui.label(egui::RichText::new(shown.text()).color(theme.warning));
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(left));
}

/// The which-key strip of the pane showing over the grid ([`keys::pane_key_hints`]:
/// the queue's, a plain review pane's or the detail's), while `?` pins it or a
/// `g` is pending. Reserved from the bottom before the pane lays out, since the
/// pane's scroll area takes every point of height left (as the grid's strip).
fn pane_hints_ui(ui: &mut egui::Ui, theme: &ColorTheme, state: &BoardUiState) {
    let Some(strip) = keys::pane_key_hints(state) else {
        return;
    };
    egui::Panel::bottom("headway-pane-key-hints")
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().inner_margin(egui::Margin {
            left: SPACING_LG as i8,
            right: SPACING_LG as i8,
            top: SPACING_MD as i8,
            bottom: SPACING_MD as i8,
        }))
        .show(ui, |ui| keys::key_hints_ui(ui, theme, strip));
}

/// Find a card anywhere on the board, returning its column index and view.
pub(crate) fn find_card(view: &BoardView, card: NoteId) -> Option<(usize, &CardView)> {
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

    /// Make `child` a subissue of `parent` on `view`, last in its work-order.
    /// Shared with [`crate::keys`]'s and the review queue's tests.
    pub(crate) fn link(view: &mut BoardView, parent: NoteId, child: NoteId) {
        for card in view.columns.iter_mut().flat_map(|c| c.cards.iter_mut()) {
            if card.id == child {
                card.parent = Some(parent);
            }
            if card.id == parent {
                card.subissues.push(headway::event::SubissueView {
                    id: child,
                    title: String::new(),
                    column: None,
                    done: false,
                    archived: false,
                    seq: None,
                });
            }
        }
    }

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
