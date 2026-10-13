//! What Headway's **key actions** do, one `apply_*` per view: the board grid
//! ([`apply_grid_tool`]), the review pane in the queue or opened from a card
//! ([`apply_pane_tool`]), the card detail ([`apply_detail_tool`]), and the
//! cross-board issue list ([`apply_list_tool`]).
//!
//! Headway reads no keys of its own. Chrome's keymap (`default_keymap.toml` in
//! `notedeck_chrome`) binds each key, in the [key scopes](crate::ui_tools::key_scopes)
//! of the view in front, to one of Headway's UI tools ([`crate::ui_tools`]);
//! the tool is queued on [`BoardUiState`] and run here as the board renders,
//! by [`pane_tools`] for whatever shows over the grid and [`grid_tools`] for
//! the grid, so a key and the same tool called by name have one effect.
//!
//! The actions come in two classes.
//!
//! - **Card actions** act on *the current card* — the grid's cursor card, the
//!   queue's card, a review pane's card, the open detail's card — and mean the
//!   same thing in every view: open it, its explainer, its agentium session
//!   (optionally asking for a code review), its review, send back with a
//!   reason, comment, next/previous card. One enum ([`CardAction`]) and
//!   one dispatcher ([`apply_card_action`]), which every view shares.
//! - **Navigation** keeps its meaning while its target is whatever the view
//!   shows: the grid's cursor and its card moves, a pane's or the detail's
//!   scroll ([`PaneScroll`]), backing out.
//!
//! The board edits `a` (archive), `D` (done) and `H`/`L`/`J`/`K` (move the
//! cursor card) aren't here: chrome's keymap binds them to Headway's data
//! tools (`crate::tools`) on the [current card](notedeck::App::current_ref),
//! and the view follows the edit as it folds in
//! (`BoardUiState::follow_cursor`, `BoardUiState::follow_queue`).
//!
//! What stays here as keys is the text fields': the `X`/`C` composer's Enter and
//! Esc ([`reason_keys`]), which run while its field has the keyboard, where
//! chrome's keymap stands down. (The detail's Esc out of a field is an
//! availability rule instead: [`BoardUiState::detail_field_focused`].)

use egui::{Key, Modifiers};
use nostrdb_net::NoteId;
use notedeck_ui::diff::PatchScroll;

use crate::cache::BoardRef;
use crate::cursor::{self, CursorMove};
use crate::event::BoardView;
use crate::store::BoardAction;
use crate::ui::{
    BoardUiState, CardStep, DetailScroll, QueueNotice, QueueScope, SessionOpen, ViewFilter,
    filter_field_id, find_card, find_detail_card, list_field_id, reason_field_id,
};
use crate::ui_tools::HeadwayUiTool;

/// An action on the current card, the same in every view that has one (see
/// the module docs), applied by [`apply_card_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardAction {
    /// `Enter`/`o`: open the card's detail. The detail itself has nothing to
    /// open, so the tool isn't available there; a plain review pane backs out
    /// to it.
    Open,
    /// `e`: open the explainer of the card's record. `None` (the key) is the
    /// record a review pane shows, else the newest; a click on one record's
    /// link in the detail's Review section names it.
    Explainer(Option<NoteId>),
    /// Open the deployed build of the card's record, where its commit can be
    /// opened and tried. A click only, with no key: `None` is the record a
    /// review pane shows, else the newest; a click on one record's link in
    /// the detail's Review section names it.
    Deploy(Option<NoteId>),
    /// `s`/`S`: open the agentium session of the card's record, `S` asking
    /// it for a `/code-review` of its work. `None` (the key) is the record a
    /// review pane shows, else the newest; a click on one record's session
    /// chip in the detail's Review section names it.
    Session(SessionOpen, Option<NoteId>),
    /// `r`: open the review pane on the card's newest record (`None`, the
    /// key), or on the record a click in the detail's Review section names.
    Review(Option<NoteId>),
    /// `X`: ask for a reason, post it as a `review:` comment, send it to the
    /// record's agentium session, and send the card back to In Progress.
    SendBack,
    /// `C`: send the line comments drafted on the card's record, if any;
    /// else ask for a one-line comment, post it, and send it to the record's
    /// agentium session. The card stays where it is.
    Comment,
    /// `n`/`p`: the next/previous card — the grid's cursor, the queue's card,
    /// or the neighbouring card in the column of a detail or review pane.
    Step(CardStep),
}

/// The view a [`CardAction`] was pressed in, for the few actions whose effect
/// depends on it (open, archive, step).
#[derive(Clone, Copy)]
pub(crate) enum ActionView<'a> {
    /// The board grid, drawn through this filter: the cursor steps over the
    /// cards it shows.
    Grid(&'a ViewFilter<'a>),
    /// The review queue.
    Queue,
    /// A review pane opened from a card, not the queue.
    Pane,
    /// The card detail.
    Detail,
}

/// Apply `action` to `card`, the current card of the view `at`. Returns the
/// board edit it makes, if any; everything else lands in `state`.
///
/// Most actions do the same thing everywhere. Where the view matters: `Open`
/// leaves the queue or a plain review pane for the card's detail; `Review`
/// in the queue only goes back to the newest record, since the queue already
/// shows the card; `Step` walks the grid's cursor, the queue, or the card's
/// column.
pub(crate) fn apply_card_action(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
    card: NoteId,
    action: CardAction,
    at: ActionView<'_>,
) -> Option<BoardAction> {
    #[cfg(test)]
    {
        state.last_card_action = Some((action, card));
    }
    let now = ctx.input(|i| i.time);
    match action {
        CardAction::Open => match at {
            ActionView::Grid(_) => state.open_card(card),
            ActionView::Queue => state.open_queue_card(),
            ActionView::Pane => state.back_to_detail(card),
            ActionView::Detail => {}
        },
        CardAction::Explainer(record) => state.open_explainer(ctx, view, card, record),
        CardAction::Deploy(record) => state.open_deploy(ctx, view, card, record),
        CardAction::Session(how, record) => state.open_card_session(view, card, how, record, now),
        CardAction::Review(record) => match at {
            ActionView::Queue => state.newest_record(card),
            ActionView::Grid(_) | ActionView::Pane | ActionView::Detail => {
                state.open_review(card, record)
            }
        },
        CardAction::SendBack => state.start_reject(view, card, now),
        CardAction::Comment => return state.start_comment(view, card, now),
        CardAction::Step(step) => match at {
            ActionView::Grid(filter) => move_cursor(view, filter, state, step_move(step)),
            ActionView::Queue => state.step_queue(step),
            ActionView::Pane => state.step_review(view, card, step),
            ActionView::Detail => state.step_detail(view, card, step),
        },
    }
    None
}

/// The grid cursor move an `n`/`p` makes: `j`'s and `k`'s.
fn step_move(step: CardStep) -> CursorMove {
    match step {
        CardStep::Next => CursorMove::Down,
        CardStep::Prev => CursorMove::Up,
    }
}

/// Apply `tool` to the grid: what its key does there. Returns the board edit
/// it makes, if any. A tool the grid has no use for (a pane's scroll, `Back`)
/// does nothing.
///
/// A card tool acts on the cursor card. One naming a card moves the cursor
/// to it first, so it acts as the key would with the cursor there; a named
/// card the grid doesn't show is logged and dropped.
pub(crate) fn apply_grid_tool(
    ctx: &egui::Context,
    view: &BoardView,
    filter: &ViewFilter,
    state: &mut BoardUiState,
    tool: &HeadwayUiTool,
) -> Option<BoardAction> {
    match tool {
        HeadwayUiTool::Card { action, card } => {
            match named_card(view, card.as_deref()) {
                None => {}
                Some(Some(named)) if cursor::locate(view, filter, named).is_some() => {
                    state.set_cursor(named)
                }
                Some(_) => {
                    tracing::warn!("headway: UI tool card isn't showing on the grid; dropped");
                    return None;
                }
            }
            let card = state
                .cursor()
                .filter(|&c| cursor::locate(view, filter, c).is_some());
            match (card, *action) {
                (Some(card), action) => {
                    return apply_card_action(
                        ctx,
                        view,
                        state,
                        card,
                        action,
                        ActionView::Grid(filter),
                    );
                }
                // With no cursor, `n`/`p` land it on the first card as `j`/`k`
                // do; the other actions have no card to act on.
                (None, CardAction::Step(step)) => move_cursor(view, filter, state, step_move(step)),
                (None, _) => {}
            }
        }
        HeadwayUiTool::Cursor(mv) => move_cursor(view, filter, state, *mv),
        HeadwayUiTool::AddCard => add_card_at_cursor(view, filter, state),
        HeadwayUiTool::FocusFilter => ctx.memory_mut(|m| m.request_focus(filter_field_id())),
        HeadwayUiTool::ReviewQueue => {
            state.open_review_queue(view, QueueScope::Board, ctx.input(|i| i.time))
        }
        HeadwayUiTool::OpenList => state.open_list(),
        // The grid's `Esc`. Chrome's keymap owns the pending chord and the
        // which-key strip, so all that's left to drop is the cursor.
        HeadwayUiTool::Clear => state.clear_cursor(),
        HeadwayUiTool::Scroll(_)
        | HeadwayUiTool::Comment
        | HeadwayUiTool::SendComments
        | HeadwayUiTool::Back => {}
    }
    None
}

/// Apply one UI tool in the cross-board issue list: `j`/`k`/`gg`/`G` move its
/// cursor over the rows, `Enter`/`o` open the cursor's card (switching boards
/// when it's on another), `/` focuses the query, `q`/`Esc` leave for the grid.
/// `sources` are the boards the list's rows were read from. Nothing else
/// means anything here: the card actions act on a card of the board showing,
/// and a list row may be on another, so they wait for its detail.
pub(crate) fn apply_list_tool(
    ctx: &egui::Context,
    view: &BoardView,
    sources: &[BoardRef],
    state: &mut BoardUiState,
    tool: &HeadwayUiTool,
) {
    match tool {
        HeadwayUiTool::Cursor(mv) => state.move_list_cursor(*mv),
        HeadwayUiTool::Card {
            action: CardAction::Open,
            card: None,
        } => state.open_list_cursor(view, sources),
        HeadwayUiTool::FocusFilter => ctx.memory_mut(|m| m.request_focus(list_field_id())),
        HeadwayUiTool::Back => state.close_list(),
        _ => tracing::debug!("headway: {tool:?} means nothing in the issue list; dropped"),
    }
}

/// The card a card tool names: `None` for a tool on the current card, else
/// `Some` of the card on `view` its `headway:<board>/<word-id>` reference
/// resolves to, `Some(None)` when it names no card on this board.
fn named_card(view: &BoardView, card: Option<&str>) -> Option<Option<NoteId>> {
    let card = card?;
    Some(
        headway::wordid::parse_ref(card)
            .filter(|r| r.board == view.id)
            .and_then(|r| headway::event::resolve_card_by_wordid(view, r.words)),
    )
}

/// Run the UI tools queued on `state` against the grid
/// ([`apply_grid_tool`]) as the grid starts to lay out, so a tool that
/// focuses a field (`/`, `c`) has it laid out focused this frame. None run
/// while an inline editor, the archived sheet or the `X` composer has the
/// keyboard, or the graph shows. Stops at a tool that opens
/// a pane over the grid; what's left is dropped as the frame ends
/// ([`BoardUiState::drop_queued_tools`]). Returns the first board edit.
pub(crate) fn grid_tools(
    ctx: &egui::Context,
    view: &BoardView,
    filter: &ViewFilter,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let mut action = None;
    while !state.keys_blocked() && state.selected().is_none() && state.graph_epic().is_none() {
        let Some(tool) = state.next_queued_tool() else {
            break;
        };
        let edit = apply_grid_tool(ctx, view, filter, state, &tool);
        action = first_edit(action, edit);
    }
    action
}

/// Keep the first board edit of a frame's tools: a frame applies one, as a
/// frame of keys makes at most one. A later one is logged and dropped.
fn first_edit(kept: Option<BoardAction>, edit: Option<BoardAction>) -> Option<BoardAction> {
    if kept.is_some() && edit.is_some() {
        tracing::warn!("headway: a second board edit from UI tools in one frame was dropped");
    }
    kept.or(edit)
}

/// Which review pane [`apply_pane_tool`] drives: they differ only in what
/// `n`/`p` step through and where `q`/`Esc` go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMode {
    /// The review queue: `n`/`p` step the queue, `q` leaves it: the board's
    /// for the grid, an epic's for the epic's detail.
    Queue,
    /// A review pane opened from a card: `n`/`p` step to the neighbouring
    /// card's review in its column, `q` backs out to the card's detail.
    Plain,
}

/// Apply `tool` to the review pane `mode` shows: what its key does there.
/// Returns the board edit it makes, if any. A tool the pane has no use for
/// (a grid move, the review queue) does nothing.
///
/// A card tool acts on the pane's card. One naming another card is logged
/// and dropped: the pane shows one card.
pub(crate) fn apply_pane_tool(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
    mode: PaneMode,
    tool: &HeadwayUiTool,
) -> Option<BoardAction> {
    let card = match mode {
        PaneMode::Queue => state.queue_card(),
        PaneMode::Plain => state.review_card(),
    };
    match tool {
        HeadwayUiTool::Card {
            action,
            card: named,
        } => {
            if !names_current(view, named.as_deref(), card) {
                return None;
            }
            let at = match mode {
                PaneMode::Queue => ActionView::Queue,
                PaneMode::Plain => ActionView::Pane,
            };
            // The queue's cards are a snapshot, so its card can have left the
            // board since (archived or moved away elsewhere). Stepping off it
            // still works; acting on it only says it's gone, rather than raising
            // an edit the store would drop and stepping on as if it had landed.
            let on_board = card.filter(|&c| find_card(view, c).is_some());
            match (on_board, *action, mode) {
                (Some(card), action, _) => {
                    return apply_card_action(ctx, view, state, card, action, at);
                }
                (None, CardAction::Step(step), PaneMode::Queue) => state.step_queue(step),
                (None, _, _) if card.is_some() => {
                    state.set_notice(QueueNotice::CardGone, ctx.input(|i| i.time));
                }
                (None, _, _) => {}
            }
        }
        HeadwayUiTool::Scroll(scroll) => state.scroll_review(scroll.patch()),
        HeadwayUiTool::Comment | HeadwayUiTool::SendComments => {
            let card = card.filter(|&c| find_card(view, c).is_some())?;
            let now = ctx.input(|i| i.time);
            if *tool == HeadwayUiTool::SendComments {
                return state.send_review_comments(view, card, now);
            }
            state.focus_review_composer(view, card, now);
        }
        HeadwayUiTool::Back => match (mode, card) {
            (PaneMode::Queue, _) => {
                state.close_queue(view);
            }
            (PaneMode::Plain, Some(card)) => state.back_to_detail(card),
            (PaneMode::Plain, None) => {}
        },
        HeadwayUiTool::Cursor(_)
        | HeadwayUiTool::AddCard
        | HeadwayUiTool::OpenList
        | HeadwayUiTool::FocusFilter
        | HeadwayUiTool::ReviewQueue
        | HeadwayUiTool::Clear => {}
    }
    None
}

/// Whether a card tool over a pane or the detail may act: it names no card,
/// or names `current`, the one showing. Logs the refusal.
fn names_current(view: &BoardView, named: Option<&str>, current: Option<NoteId>) -> bool {
    match named_card(view, named) {
        None => true,
        Some(named) if named.is_some() && named == current => true,
        Some(_) => {
            tracing::warn!("headway: UI tool card isn't the one showing; dropped");
            false
        }
    }
}

/// Apply `tool` to the detail of `card`: what its key does there. Returns
/// the board edit it makes, if any. A tool the detail has no use for (a grid
/// move, a diff's file step, a review comment) does nothing.
///
/// A card tool acts on `card`. One naming another card is logged and
/// dropped: the detail shows one card.
pub(crate) fn apply_detail_tool(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
    card: NoteId,
    tool: &HeadwayUiTool,
) -> Option<BoardAction> {
    match tool {
        HeadwayUiTool::Card {
            action,
            card: named,
        } => {
            if names_current(view, named.as_deref(), Some(card)) {
                return apply_card_action(ctx, view, state, card, *action, ActionView::Detail);
            }
        }
        HeadwayUiTool::Scroll(scroll) => {
            if let Some(scroll) = scroll.detail() {
                state.scroll_detail(scroll);
            }
        }
        HeadwayUiTool::ReviewQueue => {
            state.open_review_queue(view, QueueScope::Epic(card), ctx.input(|i| i.time))
        }
        HeadwayUiTool::Back => state.leave_card(),
        HeadwayUiTool::Cursor(_)
        | HeadwayUiTool::AddCard
        | HeadwayUiTool::OpenList
        | HeadwayUiTool::FocusFilter
        | HeadwayUiTool::Clear
        | HeadwayUiTool::Comment
        | HeadwayUiTool::SendComments => {}
    }
    None
}

/// What shows over the grid, for [`pane_tools`]: the queue, a plain review
/// pane, or the detail of a card on the board.
#[derive(Clone, Copy)]
enum ToolPane {
    Review(PaneMode),
    Detail(NoteId),
}

/// The pane a queued tool runs in, if one shows over the grid: nothing while
/// the `X` composer is open or the graph shows (see
/// [`crate::ui_tools::key_view`]).
fn tool_pane(view: &BoardView, state: &BoardUiState) -> Option<ToolPane> {
    if state.rejecting() || state.graph_epic().is_some() {
        return None;
    }
    if state.queue_open() {
        return Some(ToolPane::Review(PaneMode::Queue));
    }
    if state.review_card().is_some() {
        return Some(ToolPane::Review(PaneMode::Plain));
    }
    state
        .selected()
        .filter(|&c| find_detail_card(view, c).is_some())
        .map(ToolPane::Detail)
}

/// Run the UI tools queued on `state` against whatever shows over the grid
/// ([`apply_pane_tool`], [`apply_detail_tool`]), before anything lays out, so
/// a tool that leaves its view (`q`, a verdict on the queue's last card,
/// `Enter` onto a card) has the view it lands on draw this same frame.
/// Each runs in the pane showing as it's taken, so after one that backs out
/// to the grid the rest are left for [`grid_tools`]. Returns the first board
/// edit.
pub(crate) fn pane_tools(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let mut action = None;
    while let Some(pane) = tool_pane(view, state) {
        let Some(tool) = state.next_queued_tool() else {
            break;
        };
        let edit = match pane {
            ToolPane::Review(mode) => apply_pane_tool(ctx, view, state, mode, &tool),
            ToolPane::Detail(card) => apply_detail_tool(ctx, view, state, card, &tool),
        };
        action = first_edit(action, edit);
    }
    action
}

/// A scroll of a review pane's diff or the card detail, as a key or
/// `headway_scroll`/`headway_step_file` asks for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PaneScroll {
    /// By this many lines (negative scrolls up): a diff's rows, the
    /// detail's `j`/`k` steps.
    Rows(i32),
    /// By this many pages (negative scrolls up).
    Pages(f32),
    /// To the top (`gg`).
    Top,
    /// To the bottom (`G`).
    Bottom,
    /// A diff's next file (`]`); nothing in the detail.
    NextFile,
    /// A diff's previous file (`[`); nothing in the detail.
    PrevFile,
}

impl PaneScroll {
    /// The diff scroll this is.
    fn patch(self) -> PatchScroll {
        match self {
            Self::Rows(rows) => PatchScroll::Rows(rows as isize),
            Self::Pages(pages) => PatchScroll::Pages(pages),
            Self::Top => PatchScroll::Top,
            Self::Bottom => PatchScroll::Bottom,
            Self::NextFile => PatchScroll::NextFile,
            Self::PrevFile => PatchScroll::PrevFile,
        }
    }

    /// The detail scroll this is, `None` for a file step.
    fn detail(self) -> Option<DetailScroll> {
        Some(match self {
            Self::Rows(rows) => DetailScroll::Rows(rows),
            Self::Pages(pages) => DetailScroll::Pages(pages),
            Self::Top => DetailScroll::Top,
            Self::Bottom => DetailScroll::Bottom,
            Self::NextFile | Self::PrevFile => return None,
        })
    }
}

/// The `X`/`C` composer's keys: Enter sends the line (an empty one sends
/// nothing), Esc cancels. Both are consumed before the field sees them, and
/// the field's focus goes with the composer. While another widget has the
/// keyboard they're left to it ([`BoardUiState::reason_keys_live`]); the
/// view's own keys stay down either way (its [key scopes](crate::ui_tools::key_scopes)
/// are empty while the composer is open). Runs before anything lays out.
pub(crate) fn reason_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    if !state.reason_keys_live(ctx.memory(|m| m.focused())) {
        return None;
    }
    let action = if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        state.cancel_reject();
        None
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        state.submit_reject(view, ctx.input(|i| i.time))
    } else {
        return None;
    };
    if !state.rejecting() {
        ctx.memory_mut(|m| m.surrender_focus(reason_field_id()));
    }
    action
}

/// Step the cursor, scrolling its new card into view.
fn move_cursor(view: &BoardView, filter: &ViewFilter, state: &mut BoardUiState, mv: CursorMove) {
    if let Some(id) = cursor::step(view, filter, state.cursor(), mv) {
        state.set_cursor(id);
    }
}

/// Open the add-card composer in the cursor's column, or the first column when
/// there's no (visible) cursor.
fn add_card_at_cursor(view: &BoardView, filter: &ViewFilter, state: &mut BoardUiState) {
    // A composer on a column that doesn't exist would never render, so it
    // could never close — and would hold the keymap off for good.
    if view.columns.is_empty() {
        return;
    }
    let col = state
        .cursor()
        .and_then(|c| cursor::locate(view, filter, c))
        .map_or(0, |pos| pos.col);
    state.open_add_card(col);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::tests::{grid, id, square_grid};
    use crate::nav::ReviewTarget;
    use crate::ui::{BoardEffect, QueueNotice};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use headway::event::{ReviewFields, ReviewView};
    use headway::query::IssueFilter;
    use notedeck::ColorTheme;
    use notedeck::test_harness::PressKey;
    use notedeck_chrome::keymap::{KeyDispatch, KeyEngine, Scopes};
    use notedeck_chrome::{HIDE_KEY_HINTS, TOGGLE_KEY_HINTS};
    use notedeck_ui::chord::KeyPress;

    use crate::ui_tools::{available, key_scopes};

    /// What a keymap test frame reads and leaves behind.
    struct KeysHarness {
        /// The board the keys walk: [`grid`]'s three columns.
        view: BoardView,
        state: BoardUiState,
        /// When set, a text field with this id is laid out after the keymap
        /// each frame, standing in for the filter or a composer.
        field: Option<egui::Id>,
        /// The field's buffer.
        text: String,
        /// Whether any frame still had an Esc press in its input once the
        /// keymap had run.
        esc_left: bool,
        /// The `(card, to_col, to_row)` of the last `MoveCard` the keymap
        /// returned, if any.
        moved: Option<(NoteId, usize, usize)>,
        /// The card of the last `ArchiveCard` the keymap returned, if any.
        archived: Option<NoteId>,
        /// The `(card, body)` of the last `AddComment` the keymap returned.
        commented: Option<(NoteId, String)>,
        /// The last URL a key asked the platform to open.
        opened: Option<String>,
        /// The last agentium session open a key raised, as the app drains
        /// it ([`BoardUiState::take_effects`]).
        session: Option<notedeck::OpenUri>,
        /// The `(card, record, comments)` of the last `AddReviewComments`
        /// the keymap returned.
        review_comments: Option<(NoteId, NoteId, Vec<crate::store::NewReviewComment>)>,
        /// Chrome's key engine with the shipped keymap, run ahead of the board
        /// each frame as chrome runs it ahead of the app ([`chrome_keys`]).
        keys: KeyEngine,
        /// Whether `?` pinned chrome's which-key strip.
        hints_pinned: bool,
    }

    /// Headway's data tools the shipped keymap binds its board edits to
    /// (`a`, `D`, `H L J K`), on the current card.
    const BOARD_EDITS: &[&str] = &["headway_archive_card", "headway_move_card"];

    /// Whether the chrome's own tool, Headway's UI tool, or Headway's data
    /// tool `name` is available, as chrome asks its keymap: `?` only while
    /// Headway reports scopes, Esc's strip-hide only while it's pinned, a
    /// data tool always (the keymap holds it to the current card).
    fn tool_available(state: &BoardUiState, pinned: bool, name: &str) -> bool {
        match name {
            TOGGLE_KEY_HINTS => !key_scopes(state).is_empty(),
            HIDE_KEY_HINTS => pinned,
            name if BOARD_EDITS.contains(&name) => true,
            name => available(state, name),
        }
    }

    /// The scopes chrome resolves Headway's keys in, with its current card.
    fn chrome_scopes(state: &BoardUiState) -> Scopes<'_> {
        Scopes::new(key_scopes(state)).with_current(state.current_ref())
    }

    /// Chrome's part of a frame, ahead of Headway's render: the key engine
    /// resolving this frame's press against Headway's scopes and current
    /// card, then the tool it's bound to called: a UI tool queued, as
    /// `Headway::call_ui_tool` does; a board edit's data tool run, its edit
    /// recorded and folded into the board ([`fold`]); or the strip toggled.
    fn chrome_keys(ctx: &egui::Context, h: &mut KeysHarness) {
        let pinned = h.hints_pinned;
        let state = &h.state;
        let dispatch = h.keys.resolve(ctx, chrome_scopes(state), |name| {
            tool_available(state, pinned, name)
        });
        let Some(KeyDispatch::Tool(call)) = dispatch else {
            return;
        };
        match call.name.as_str() {
            TOGGLE_KEY_HINTS => h.hints_pinned = !pinned,
            HIDE_KEY_HINTS => h.hints_pinned = false,
            name if BOARD_EDITS.contains(&name) => {
                // An edit the tool refuses (a move off the board's edge) is
                // only logged by chrome.
                if let Ok(edit) = crate::tools::board_edit(&h.view, name, &call.args) {
                    fold(h, edit);
                }
            }
            name => {
                let tool = HeadwayUiTool::parse(name, &call.args)
                    .unwrap_or_else(|e| panic!("keymap binds {name} {}: {e}", call.args));
                h.state.queue_tool(tool);
            }
        }
    }

    /// Record a key's board edit and fold it into the harness's board, as
    /// the app's async ingest does a frame or more later: here before the
    /// board draws, so the frame of the key press sees it. A card that left
    /// is reported as `nav::SeenCards` reports it to the app.
    fn fold(h: &mut KeysHarness, edit: BoardAction) {
        match edit {
            BoardAction::ArchiveCard { card } => {
                h.archived = Some(card);
                take_card(&mut h.view, card);
                h.state.note_departed(&[card]);
            }
            BoardAction::MoveCard {
                card,
                to_col,
                to_row,
            } => {
                h.moved = Some((card, to_col, to_row));
                let Some(taken) = take_card(&mut h.view, card) else {
                    return;
                };
                // `to_row` counts the moving card where it was.
                let row = if taken.col == to_col && to_row > taken.row {
                    to_row - 1
                } else {
                    to_row
                };
                let cards = &mut h.view.columns[to_col].cards;
                cards.insert(row.min(cards.len()), taken.card);
            }
            _ => panic!("not a key's board edit"),
        }
    }

    /// A card [`take_card`] took out of the board, with where it was.
    struct TakenCard {
        /// The column it sat in.
        col: usize,
        /// Its row in that column.
        row: usize,
        /// The card.
        card: headway::event::CardView,
    }

    /// Take `card` out of its column.
    fn take_card(view: &mut BoardView, card: NoteId) -> Option<TakenCard> {
        view.columns
            .iter_mut()
            .enumerate()
            .find_map(|(col, column)| {
                let row = column.cards.iter().position(|c| c.id == card)?;
                Some(TakenCard {
                    col,
                    row,
                    card: column.cards.remove(row),
                })
            })
    }

    /// Chrome's which-key strip, as it floats it over the bottom of the app:
    /// what finishes a pending chord, else, pinned, every key Headway's
    /// scopes bind to an available tool.
    fn chrome_strip(ui: &egui::Ui, h: &KeysHarness) {
        let pending = h.keys.pending();
        let (state, pinned) = (&h.state, h.hints_pinned);
        let hints = (pending.is_some() || pinned).then(|| {
            h.keys
                .keymap()
                .hints(chrome_scopes(state), pending, move |name| {
                    tool_available(state, pinned, name)
                })
                .map(|hint| (hint.keycap_iter(), hint.label))
        });
        let muted = ColorTheme::current(ui.ctx()).text_muted;
        notedeck_ui::keybind_hint::key_hints_overlay(
            ui.ctx(),
            egui::Id::unique("chrome-key-hints"),
            ui.max_rect(),
            muted,
            hints,
        );
    }

    /// A harness that runs Headway's keys over [`grid`] each frame,
    /// unfiltered: chrome's keymap first ([`chrome_keys`]), then as
    /// [`crate::ui::board_ui`] does, a queue route's re-snapshot, the queue
    /// following a verdict that folded in, [`reason_keys`] and
    /// [`pane_tools`], then, when no pane shows over the grid, the cursor
    /// following an edit and [`grid_tools`]; the current card latched as the
    /// pass ends, and chrome's strip last.
    fn keys_harness(field: Option<egui::Id>) -> Harness<'static, KeysHarness> {
        let mut harness = Harness::new_ui_state(
            |ui, h: &mut KeysHarness| {
                let everything = IssueFilter::default();
                let pane = |s: &BoardUiState| {
                    s.queue_open() || s.review_card().is_some() || s.selected().is_some()
                };
                chrome_keys(ui.ctx(), h);
                h.state.reset_grid_menu_open();
                let now = ui.ctx().input(|i| i.time);
                h.state.refresh_queue(&h.view, now);
                h.state.follow_queue(&h.view, now);
                let showed_pane = pane(&h.state);
                h.state.retire_stale_reason();
                let keyed = reason_keys(ui.ctx(), &h.view, &mut h.state);
                let keyed = keyed.or(pane_tools(ui.ctx(), &h.view, &mut h.state));
                h.state.retire_stale_notice(ui.ctx().cumulative_pass_nr());
                let action = if showed_pane || keyed.is_some() {
                    keyed
                } else {
                    let filter = ViewFilter {
                        view: &h.view,
                        filter: &everything,
                    };
                    h.state.follow_cursor(&h.view, &filter);
                    let action = grid_tools(ui.ctx(), &h.view, &filter, &mut h.state);
                    h.state.latch_cursor_slot(&h.view, &filter);
                    action
                };
                h.state.drop_queued_tools();
                h.state.latch_current_ref(&h.view);
                for effect in h.state.take_effects() {
                    if let BoardEffect::Open(open) = effect {
                        h.session = Some(open);
                    }
                }
                match action.or_else(|| h.state.take_follow_up()) {
                    Some(BoardAction::MoveCard {
                        card,
                        to_col,
                        to_row,
                    }) => h.moved = Some((card, to_col, to_row)),
                    Some(BoardAction::ArchiveCard { card }) => h.archived = Some(card),
                    Some(BoardAction::AddComment { card, body, .. }) => {
                        h.commented = Some((card, body))
                    }
                    Some(BoardAction::AddReviewComments {
                        card,
                        record,
                        comments,
                    }) => h.review_comments = Some((card, record, comments)),
                    _ => {}
                }
                h.state.reason_test_ui(ui);
                if let Some(url) = ui.ctx().output(|o| {
                    o.commands.iter().find_map(|c| match c {
                        egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                        _ => None,
                    })
                }) {
                    h.opened = Some(url);
                }
                h.esc_left |= ui.input(|i| i.key_pressed(Key::Escape));
                if let Some(field) = h.field {
                    ui.add(egui::TextEdit::singleline(&mut h.text).id(field))
                        .accessible_name("test field");
                }
                // As the detail's pass ends, after its fields laid out.
                h.state.latch_detail_focus(ui.ctx());
                chrome_strip(ui, h);
            },
            KeysHarness {
                view: grid(),
                state: BoardUiState::default(),
                field,
                text: String::new(),
                esc_left: false,
                moved: None,
                archived: None,
                commented: None,
                opened: None,
                session: None,
                review_comments: None,
                keys: KeyEngine::with_default_keymap(),
                hints_pinned: false,
            },
        );
        harness.run();
        harness
    }

    /// Press `key` bare, running its down frame and its up frame.
    fn press(harness: &mut Harness<'static, KeysHarness>, key: Key) {
        press_with(harness, Modifiers::NONE, key);
    }

    /// Press `key` with `modifiers`, running its down frame and its up frame.
    fn press_with(harness: &mut Harness<'static, KeysHarness>, modifiers: Modifiers, key: Key) {
        harness.press_key_modifiers(modifiers, key);
        harness.step();
    }

    #[test]
    fn jkhl_walk_the_grid() {
        let mut harness = keys_harness(None);

        // No cursor yet: any motion lands on the first card.
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), Some(id(2)));
        press(&mut harness, Key::K);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        press(&mut harness, Key::L);
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
        press(&mut harness, Key::L);
        assert_eq!(harness.state().state.cursor(), Some(id(5)));
        press(&mut harness, Key::H);
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
    }

    /// `n`/`p` walk the grid as `j`/`k` do, landing a missing cursor on the
    /// first card.
    #[test]
    fn n_and_p_step_the_grid_cursor() {
        let mut harness = keys_harness(None);
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.cursor(), Some(id(2)));
        press(&mut harness, Key::P);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        assert!(!harness.state().state.keys_blocked(), "no composer");
    }

    #[test]
    fn gg_goes_first_and_shift_g_goes_last() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));

        press_with(&mut harness, Modifiers::SHIFT, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)));

        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)), "one g waits");
        assert_eq!(pending(&harness).as_deref(), Some("g"));
        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        assert_eq!(pending(&harness), None);
    }

    /// A `g` that goes nowhere starts over from the key that broke it, so
    /// `g j` still moves down (chrome's chord rule; Headway's own keymap
    /// dropped the `j`).
    #[test]
    fn g_then_another_key_runs_that_key() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(1));

        press(&mut harness, Key::G);
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), Some(id(2)));
        assert_eq!(pending(&harness), None);
    }

    #[test]
    fn a_lone_g_lapses() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(3));

        press(&mut harness, Key::G);
        // Each step is a quarter second; nine outlast the two-second timeout.
        for _ in 0..9 {
            harness.step();
        }
        assert_eq!(pending(&harness), None);

        // So the next `g` starts a fresh chord instead of completing one.
        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)));
        assert_eq!(pending(&harness).as_deref(), Some("g"));
    }

    #[test]
    fn ctrl_j_falls_through() {
        let mut harness = keys_harness(None);
        press_with(&mut harness, Modifiers::CTRL, Key::J);
        assert_eq!(harness.state().state.cursor(), None);
    }

    #[test]
    fn a_focused_text_field_keeps_its_keys() {
        let field = egui::Id::unique("keys_test_field");
        let mut harness = keys_harness(Some(field));
        harness.ctx.memory_mut(|m| m.request_focus(field));
        harness.run();

        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), None);
    }

    #[test]
    fn enter_opens_the_cursor_card() {
        let mut harness = keys_harness(None);
        press(&mut harness, Key::Enter);
        assert_eq!(harness.state().state.selected(), None, "no cursor, no-op");

        harness.state_mut().state.set_cursor(id(5));
        press(&mut harness, Key::Enter);
        assert_eq!(harness.state().state.selected(), Some(id(5)));
    }

    #[test]
    fn esc_clears_a_cursor_and_consumes_itself() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));

        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.cursor(), None);
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    #[test]
    fn esc_with_nothing_to_clear_is_left_for_chrome() {
        let mut harness = keys_harness(None);
        press(&mut harness, Key::Escape);
        assert!(harness.state().esc_left, "Esc left in input");
    }

    #[test]
    fn shift_hjkl_move_the_cursor_card() {
        let mut harness = keys_harness(None);
        press_with(&mut harness, Modifiers::SHIFT, Key::L);
        assert_eq!(harness.state().moved, None, "no cursor, no move");

        put_cursor(&mut harness, id(2));
        // Row 1 of `a` into `b`, which holds one card: its end.
        press_with(&mut harness, Modifiers::SHIFT, Key::L);
        assert_eq!(harness.state().moved, Some((id(2), 1, 1)));
        // Folded in, `b` is 4, 2: the cursor followed the card.
        assert_eq!(harness.state().state.cursor(), Some(id(2)), "cursor kept");
        press_with(&mut harness, Modifiers::SHIFT, Key::K);
        assert_eq!(harness.state().moved, Some((id(2), 1, 0)));
        press_with(&mut harness, Modifiers::SHIFT, Key::J);
        assert_eq!(harness.state().moved, Some((id(2), 1, 2)));
        press_with(&mut harness, Modifiers::SHIFT, Key::H);
        assert_eq!(harness.state().moved, Some((id(2), 0, 1)));
        assert_eq!(harness.state().view.columns[0].cards[1].id, id(2));

        // At an edge the tool refuses: nothing moves.
        press_with(&mut harness, Modifiers::SHIFT, Key::H);
        assert_eq!(harness.state().moved, Some((id(2), 0, 1)), "no new move");
        assert_eq!(harness.state().state.cursor(), Some(id(2)), "cursor kept");
    }

    /// Put the grid's cursor on `card` and let a frame pass, so the current
    /// card chrome's keymap edits is latched, as a cursor key would leave it.
    fn put_cursor(harness: &mut Harness<'static, KeysHarness>, card: NoteId) {
        harness.state_mut().state.set_cursor(card);
        harness.run();
    }

    /// Everything a board key can visibly do, for [`every_hint_does_what_it_says`]
    /// to compare before and after.
    #[derive(Debug, PartialEq)]
    struct Effects {
        cursor: Option<NoteId>,
        /// Chrome's which-key strip is pinned.
        hints: bool,
        selected: Option<NoteId>,
        /// An inline editor (the `c` composer) is open.
        editing: bool,
        focused: Option<egui::Id>,
        moved: Option<(NoteId, usize, usize)>,
        archived: Option<NoteId>,
        /// The review queue is open.
        queue: bool,
        /// The issue list is open.
        list: bool,
        /// The review pane is open, on this card.
        review: Option<NoteId>,
        rejecting: bool,
        notice: Option<QueueNotice>,
        opened: Option<String>,
        session: Option<notedeck::OpenUri>,
    }

    fn effects(harness: &Harness<'static, KeysHarness>) -> Effects {
        let h = harness.state();
        Effects {
            cursor: h.state.cursor(),
            hints: h.hints_pinned,
            selected: h.state.selected(),
            editing: h.state.keys_blocked(),
            focused: harness.ctx.memory(|m| m.focused()),
            moved: h.moved,
            archived: h.archived,
            queue: h.state.queue_open(),
            list: h.state.list_open(),
            review: h.state.review_card(),
            rejecting: h.state.rejecting(),
            notice: h.state.notice(),
            opened: h.opened.clone(),
            session: h.session.clone(),
        }
    }

    /// The presses a which-key keycap stands for: `gg` is two, a capital is
    /// Shift plus the letter, `^d` is Ctrl+D, `S-space` Shift+Space, and the
    /// named keycaps are their keys.
    fn keycap_presses(cap: &str) -> Vec<KeyPress> {
        let bare = |key| KeyPress {
            key,
            modifiers: Modifiers::NONE,
        };
        if let Some(rest) = cap.strip_prefix("S-") {
            let presses = keycap_presses(rest);
            return presses
                .into_iter()
                .map(|press| KeyPress {
                    modifiers: press.modifiers | Modifiers::SHIFT,
                    ..press
                })
                .collect();
        }
        match cap {
            "\u{21B5}" => return vec![bare(Key::Enter)],
            "esc" => return vec![bare(Key::Escape)],
            "space" => return vec![bare(Key::Space)],
            _ => {}
        }
        if let Some(letter) = cap.strip_prefix('^') {
            let key = Key::from_name(letter).unwrap_or_else(|| panic!("keycap {cap:?}"));
            return vec![KeyPress {
                key,
                modifiers: Modifiers::CTRL,
            }];
        }
        cap.chars()
            .map(|c| {
                let key = Key::from_name(&c.to_string())
                    .unwrap_or_else(|| panic!("keycap {cap:?}: no key for {c:?}"));
                let shifted = c.is_ascii_uppercase() || c == '?';
                let modifiers = if shifted {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                KeyPress { key, modifiers }
            })
            .collect()
    }

    /// The chord chrome's engine is holding, as keymap keys (`g`).
    fn pending(harness: &Harness<'static, KeysHarness>) -> Option<String> {
        harness.state().keys.pending().map(|seq| seq.to_string())
    }

    /// One keycap of chrome's strip and the label it shows under.
    #[derive(Debug, PartialEq, Eq)]
    struct StripKey {
        cap: String,
        label: String,
    }

    /// Chrome's strip as it stands, one [`StripKey`] per keycap: what finishes
    /// a pending chord, else everything, as if `?` pinned it.
    fn strip_now(harness: &Harness<'static, KeysHarness>) -> Vec<StripKey> {
        let h = harness.state();
        let state = &h.state;
        h.keys
            .keymap()
            .hints(chrome_scopes(state), h.keys.pending(), move |name| {
                tool_available(state, true, name)
            })
            .flat_map(|hint| {
                hint.keycap_iter().map(move |cap| StripKey {
                    cap: cap.to_owned(),
                    label: hint.label.to_owned(),
                })
            })
            .collect()
    }

    /// The labels of chrome's pinned strip over `harness`'s view, in order,
    /// one per hint.
    fn strip_labels(harness: &Harness<'static, KeysHarness>) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        for StripKey { label, .. } in strip_now(harness) {
            if labels.last() != Some(&label) {
                labels.push(label);
            }
        }
        labels
    }

    /// Replay every keycap of chrome's strip over the grid from the middle of
    /// a 3×3 board and check it did *something*, so the strip can't advertise
    /// a key the keymap sends nowhere.
    #[test]
    fn every_hint_does_what_it_says() {
        // `D` needs a Done column to move the card to.
        let board = || {
            let mut view = square_grid();
            view.columns[2].name = "Done".to_string();
            view
        };
        let mut grid = keys_harness(Some(filter_field_id()));
        grid.state_mut().view = board();
        grid.state_mut().state.set_cursor(id(5));
        grid.run();
        for StripKey { cap, label } in strip_now(&grid) {
            // The filter field is laid out so `/`'s focus request has a
            // widget to land on.
            let mut harness = keys_harness(Some(filter_field_id()));
            harness.state_mut().view = board();
            harness.state_mut().state.set_cursor(id(5));
            harness.run();
            let before = effects(&harness);

            for KeyPress { modifiers, key } in keycap_presses(&cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
    }

    #[test]
    fn question_mark_toggles_the_strip() {
        // With a cursor, so the card actions are available to list.
        let mut harness = grid_harness();
        assert!(!harness.state().hints_pinned);
        assert!(harness.query_by_label("up/down").is_none());

        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        assert!(harness.state().hints_pinned);
        assert!(harness.query_by_label("up/down").is_some());
        assert!(
            harness.query_by_label("session/review").is_some(),
            "card actions"
        );

        // The other way a layout can report `?`.
        press_with(&mut harness, Modifiers::SHIFT, Key::Slash);
        assert!(!harness.state().hints_pinned);
        assert!(harness.query_by_label("up/down").is_none());

        // Esc closes it too, and eats itself doing so: once the grid's own
        // Esc has dropped the cursor (the strip is chrome's, the cursor
        // Headway's, so it takes one each).
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.cursor(), None);
        assert!(harness.state().hints_pinned);
        press(&mut harness, Key::Escape);
        assert!(!harness.state().hints_pinned);
        assert!(!harness.state().esc_left, "Esc consumed");
        press(&mut harness, Key::Escape);
        assert!(
            harness.state().esc_left,
            "nothing left to drop: Esc is chrome's"
        );
    }

    #[test]
    fn a_pending_g_shows_only_its_continuation() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));

        press(&mut harness, Key::G);
        assert_eq!(
            strip_now(&harness),
            [StripKey {
                cap: "g".to_owned(),
                label: "first/last".to_owned(),
            }]
        );
        assert!(harness.query_by_label("first/last").is_some());
        assert!(harness.query_by_label("up/down").is_none());

        // Even over a pinned strip, the g continuation wins while it's pending.
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first/last").is_none());
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first/last").is_some());
        assert!(harness.query_by_label("up/down").is_none());

        // Once the chord completes, the pinned strip is back.
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("up/down").is_some());
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
    }

    #[test]
    fn a_archives_the_cursor_card_and_steps_off_it() {
        let mut harness = keys_harness(None);
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, None, "no cursor, no archive");

        // Mid-column: the cursor moves down to the next card.
        put_cursor(&mut harness, id(1));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(1)));
        assert_eq!(harness.state().state.cursor(), Some(id(2)));

        // Column end: it moves up instead.
        put_cursor(&mut harness, id(3));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(3)));
        assert_eq!(harness.state().state.cursor(), Some(id(2)));

        // A column's only card: the cursor is dropped.
        put_cursor(&mut harness, id(4));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(4)));
        assert_eq!(harness.state().state.cursor(), None);
    }

    #[test]
    fn c_opens_the_composer_without_archiving() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));
        press(&mut harness, Key::C);
        assert!(harness.state().state.keys_blocked(), "composer open");
        assert_eq!(harness.state().archived, None);
    }

    /// `R` opens the queue over the In Review column at its first card; `n`
    /// and `p` step it, stopping at the ends; grid keys stand down while it's
    /// open; and `q` leaves it with the grid cursor on the card it showed.
    #[test]
    fn shift_r_walks_the_in_review_column_and_q_leaves_it() {
        let mut harness = keys_harness(None);
        // `grid` has no In Review column: nothing opens, the header says so.
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        assert!(!harness.state().state.queue_open());
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NothingInReview)
        );

        harness.state_mut().view.columns[2].id = "in-review".to_string();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        let reviewing = |h: &Harness<'static, KeysHarness>| h.state().state.review_card();
        assert!(harness.state().state.queue_open());
        assert_eq!(reviewing(&harness), Some(id(5)));

        press(&mut harness, Key::N);
        assert_eq!(reviewing(&harness), Some(id(6)));
        press(&mut harness, Key::N);
        assert_eq!(reviewing(&harness), Some(id(6)), "stops at the end");
        press(&mut harness, Key::P);
        assert_eq!(reviewing(&harness), Some(id(5)));
        press(&mut harness, Key::P);
        assert_eq!(reviewing(&harness), Some(id(5)), "stops at the start");
        press(&mut harness, Key::N);
        assert_eq!(reviewing(&harness), Some(id(6)));

        // The grid's keys are the queue's to refuse.
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), None);

        press(&mut harness, Key::Q);
        assert!(!harness.state().state.queue_open());
        assert_eq!(reviewing(&harness), None);
        assert_eq!(harness.state().state.cursor(), Some(id(6)));
    }

    /// Esc leaves the queue too, and is eaten doing so rather than reaching
    /// the review pane or chrome.
    #[test]
    fn esc_leaves_the_queue() {
        let mut harness = keys_harness(None);
        harness.state_mut().view.columns[2].id = "in-review".to_string();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        press(&mut harness, Key::Escape);
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.cursor(), Some(id(5)));
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    /// [`review_board`] with card 1 an epic over 6 then 2, and 2 a sub-epic
    /// over 4: its In Review descendants are 6 and 4, in that work-order,
    /// while 5 is In Review outside it.
    fn epic_board() -> BoardView {
        use crate::ui::tests::link;
        let mut view = review_board();
        link(&mut view, id(1), id(6));
        link(&mut view, id(1), id(2));
        link(&mut view, id(2), id(4));
        view
    }

    /// `R` on an epic's detail walks only its In Review descendants, at any
    /// depth, in its work-order (not column order); `n`/`p` stay inside
    /// that set; `q` lands back on the epic's detail, not the grid.
    #[test]
    fn shift_r_on_an_epic_walks_its_in_review_subtree() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = epic_board();
        harness.state_mut().state.set_selected(Some(id(1)));
        harness.run();

        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        let reviewing = |h: &Harness<'static, KeysHarness>| h.state().state.review_card();
        assert!(harness.state().state.queue_open());
        assert_eq!(reviewing(&harness), Some(id(6)));
        press(&mut harness, Key::N);
        assert_eq!(reviewing(&harness), Some(id(4)));
        press(&mut harness, Key::N);
        assert_eq!(reviewing(&harness), Some(id(4)), "5 is outside the epic");
        press(&mut harness, Key::P);
        assert_eq!(reviewing(&harness), Some(id(6)));

        press(&mut harness, Key::Q);
        assert!(!harness.state().state.queue_open());
        assert_eq!(reviewing(&harness), None);
        assert_eq!(harness.state().state.selected(), Some(id(1)));
    }

    /// `q` on an epic's queue selects the epic whatever the selection was
    /// by then (a route reseed can leave it on nothing, or on another card),
    /// so it lands on the epic's detail. Nothing inside a queue moves the
    /// selection, so the tests that open the queue from the epic's detail
    /// can't tell this apart from leaving the selection alone.
    #[test]
    fn q_on_an_epic_queue_selects_the_epic_whatever_was_selected() {
        for reseeded in [None, Some(id(5))] {
            let mut harness = keys_harness(None);
            harness.state_mut().view = epic_board();
            harness.state_mut().state.set_selected(Some(id(1)));
            harness.run();
            press_with(&mut harness, Modifiers::SHIFT, Key::R);
            assert!(harness.state().state.queue_open());

            harness.state_mut().state.set_selected(reseeded);
            press(&mut harness, Key::Q);
            assert!(!harness.state().state.queue_open());
            assert_eq!(
                harness.state().state.selected(),
                Some(id(1)),
                "reseeded to {reseeded:?}"
            );
        }
    }

    /// An epic queue's `D` on its last card closes it back onto the epic's
    /// detail with the done notice, as the board's queue closes onto the grid.
    #[test]
    fn an_epic_queue_drains_back_to_the_epic() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = epic_board();
        harness.state_mut().state.set_selected(Some(id(1)));
        harness.run();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        press(&mut harness, Key::N);

        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved.map(|m| m.0), Some(id(4)));
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), Some(id(1)));
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::QueueDone));
    }

    /// An epic that left the board under its open queue has no detail to
    /// land on, so the queue's `q` leaves for the grid as the board's does,
    /// cursor on the card it last showed, rather than selecting a card that
    /// isn't there.
    #[test]
    fn an_epic_that_left_the_board_leaves_its_queue_for_the_grid() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = epic_board();
        harness.state_mut().state.set_selected(Some(id(1)));
        harness.run();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(4)));

        harness.state_mut().view.columns[0]
            .cards
            .retain(|c| c.id != id(1));
        press(&mut harness, Key::Q);
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), None);
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
    }

    /// A back/forward onto a queue entry of another scope than the snapshot's
    /// retakes it from the board as it is: the board's queue over the whole
    /// In Review column after an epic's, and the reverse. With nothing left
    /// in review there it closes, onto the view the entry sat over, and the
    /// notice saying why stays up there.
    #[test]
    fn a_queue_route_of_another_scope_retakes_the_snapshot() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = epic_board();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        assert_eq!(harness.state().state.review_card(), Some(id(4)));

        // Forward onto the epic's queue, seeded as its route does.
        let state = &mut harness.state_mut().state;
        state.set_selected(Some(id(1)));
        state.set_queue_open(Some(QueueScope::Epic(id(1))));
        harness.run();
        assert!(harness.state().state.queue_open());
        assert_eq!(harness.state().state.review_card(), Some(id(6)));

        // Back onto the board's.
        let state = &mut harness.state_mut().state;
        state.set_selected(None);
        state.set_queue_open(Some(QueueScope::Board));
        harness.run();
        assert!(harness.state().state.queue_open());
        assert_eq!(harness.state().state.review_card(), Some(id(4)));

        // In Review empties; forward onto the epic's queue closes it onto
        // the epic's detail, saying so of the epic.
        harness.state_mut().view.columns[1].cards.clear();
        let state = &mut harness.state_mut().state;
        state.set_selected(Some(id(1)));
        state.set_queue_open(Some(QueueScope::Epic(id(1))));
        harness.run();
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), Some(id(1)));
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NothingInReviewUnder)
        );

        // And back onto the board's closes it onto the grid.
        let state = &mut harness.state_mut().state;
        state.set_selected(None);
        state.set_queue_open(Some(QueueScope::Board));
        harness.run();
        assert!(!harness.state().state.queue_open());
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NothingInReview)
        );
    }

    /// As [`a_queue_route_of_another_scope_retakes_the_snapshot`], for an
    /// epic's queue entry whose epic has left the board: it closes onto the
    /// grid, and says so in the grid's words, not of a card that isn't there.
    #[test]
    fn a_gone_epics_empty_queue_says_so_in_the_grids_words() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = epic_board();
        let view = &mut harness.state_mut().view;
        view.columns[0].cards.retain(|c| c.id != id(1));
        view.columns[1].cards.clear();

        let state = &mut harness.state_mut().state;
        state.set_selected(Some(id(1)));
        state.set_queue_open(Some(QueueScope::Epic(id(1))));
        harness.run();
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), None);
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NothingInReview)
        );
    }

    /// `R` on a card with nothing in review under it opens nothing, says so
    /// of the card, and doesn't fall back to the board's queue (card 5 is
    /// itself In Review, and 4 and 6 beside it).
    #[test]
    fn shift_r_on_a_card_with_nothing_under_it_says_so() {
        let mut harness = detail_harness(None);
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), Some(id(5)));
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NothingInReviewUnder)
        );
    }

    /// A 3×3 board shaped for the review queue: `In Progress` holds 1–3,
    /// `In Review` 4–6 and `Done` 7–9. Card 5 carries a review record with an
    /// explainer, a commit and the agentium session [`SESSION`] that made it.
    fn review_board() -> BoardView {
        let mut view = square_grid();
        for (col, id) in view
            .columns
            .iter_mut()
            .zip(["in-progress", "in-review", "done"])
        {
            col.id = id.to_string();
        }
        view.columns[1].cards[1].reviews = vec![ReviewView {
            id: id(50),
            author: [0; 32],
            created_at: 0,
            fields: ReviewFields {
                explainer: Some("https://example.com/explainer".to_string()),
                commit: Some(COMMIT.to_string()),
                agentium: Some(SESSION.to_string()),
                ..Default::default()
            },
            comments: Vec::new(),
        }];
        view
    }

    /// The agentium session behind [`review_board`]'s record.
    const SESSION: &str = "agentium:power-baby-metal";

    /// The commit [`review_board`]'s record names.
    const COMMIT: &str = "136ceb9d3bfa0123456789abcdef0123456789ab";

    /// A harness on [`review_board`] with the queue open on its second card.
    fn queue_harness() -> Harness<'static, KeysHarness> {
        let mut harness = keys_harness(None);
        harness.state_mut().view = review_board();
        press_with(&mut harness, Modifiers::SHIFT, Key::R);
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
        harness
    }

    /// A harness on [`review_board`] with a plain review pane (one opened from
    /// a card's detail, not the queue) open on card 5.
    fn pane_harness() -> Harness<'static, KeysHarness> {
        let mut harness = keys_harness(None);
        harness.state_mut().view = review_board();
        let state = &mut harness.state_mut().state;
        state.set_selected(Some(id(5)));
        state.set_review(Some(ReviewTarget {
            card: id(5),
            record: None,
        }));
        harness.run();
        harness
    }

    /// A harness on [`review_board`] with card 5's detail open. `field`, as
    /// [`keys_harness`]'s, stands in for the detail's comment composer.
    fn detail_harness(field: Option<egui::Id>) -> Harness<'static, KeysHarness> {
        let mut harness = keys_harness(field);
        harness.state_mut().view = review_board();
        harness.state_mut().state.set_selected(Some(id(5)));
        harness.run();
        harness
    }

    /// One of the four views the card actions are replayed in: its name, for
    /// the failure message, and how to open it with card 5 current.
    struct ActionTestView {
        name: &'static str,
        open: fn() -> Harness<'static, KeysHarness>,
    }

    /// A harness on [`review_board`] with the grid cursor on card 5.
    fn grid_harness() -> Harness<'static, KeysHarness> {
        let mut harness = keys_harness(None);
        harness.state_mut().view = review_board();
        harness.state_mut().state.set_cursor(id(5));
        harness.run();
        harness
    }

    /// **The guard.** Every card action's key, pressed in each of the four
    /// views with card 5 current (the grid's cursor, the queue's card, a
    /// plain review pane's, the detail's), applies the same [`CardAction`]
    /// to card 5, and the board edits `a` and `D`, data tools on chrome's
    /// current card, make the same edit to it. The per-view `every_*_hint`
    /// tests only show that a key does *something*; this shows it means the
    /// same thing.
    #[test]
    fn card_actions_mean_the_same_in_every_view() {
        let views = [
            ActionTestView {
                name: "grid",
                open: grid_harness,
            },
            ActionTestView {
                name: "queue",
                open: queue_harness,
            },
            ActionTestView {
                name: "review pane",
                open: pane_harness,
            },
            ActionTestView {
                name: "detail",
                open: || detail_harness(None),
            },
        ];
        let by_tool = card_action_tools()
            .into_iter()
            .map(|KeyTool { cap, tool, args }| {
                let HeadwayUiTool::Card { action, .. } = HeadwayUiTool::parse(tool, &args).unwrap()
                else {
                    panic!("{tool} is no card tool");
                };
                (cap, tool, action)
            });
        for (cap, tool, action) in by_tool {
            let presses = keycap_presses(cap);
            let [KeyPress { modifiers, key }] = presses[..] else {
                panic!("card action {cap:?} is one press");
            };
            for ActionTestView { name, open } in &views {
                let mut harness = open();
                press_with(&mut harness, modifiers, key);
                // The detail's card is open already: `open` isn't offered there.
                let expected = (*name != "detail" || tool != crate::ui_tools::OPEN_CARD)
                    .then_some((action, id(5)));
                assert_eq!(
                    harness.state().state.last_card_action,
                    expected,
                    "keycap {cap:?} ({tool}) in the {name}"
                );
            }
        }

        // `D` moves the card to the end of Done, the third column.
        for ActionTestView { name, open } in &views {
            let mut harness = open();
            press(&mut harness, Key::A);
            assert_eq!(harness.state().archived, Some(id(5)), "a in the {name}");
            let mut harness = open();
            press_with(&mut harness, Modifiers::SHIFT, Key::D);
            assert_eq!(
                harness.state().moved,
                Some((id(5), 2, 3)),
                "D in the {name}"
            );
        }
    }

    /// Everything a key over the grid (the queue's, a review pane's, the
    /// detail's) can visibly do, for the `every_*_hint_does_what_it_says`
    /// tests to compare before and after.
    #[derive(Debug, PartialEq)]
    struct PaneEffects {
        open: bool,
        card: Option<NoteId>,
        /// The record the pane has picked (`None` is the newest).
        record: Option<NoteId>,
        selected: Option<NoteId>,
        scroll: Option<PatchScroll>,
        detail_scroll: Option<DetailScroll>,
        moved: Option<(NoteId, usize, usize)>,
        archived: Option<NoteId>,
        commented: Option<(NoteId, String)>,
        rejecting: bool,
        notice: Option<QueueNotice>,
        opened: Option<String>,
        session: Option<notedeck::OpenUri>,
        hints: bool,
        /// The record whose commit as a whole the review composer is open on.
        commenting: Option<NoteId>,
    }

    fn pane_effects(harness: &Harness<'static, KeysHarness>) -> PaneEffects {
        let h = harness.state();
        PaneEffects {
            open: h.state.queue_open(),
            card: h.state.review_card(),
            record: h.state.review_record(),
            selected: h.state.selected(),
            scroll: h.state.review_scroll(),
            detail_scroll: h.state.detail_scroll(),
            moved: h.moved,
            archived: h.archived,
            commented: h.commented.clone(),
            rejecting: h.state.rejecting(),
            notice: h.state.notice(),
            opened: h.opened.clone(),
            session: h.session.clone(),
            hints: h.hints_pinned,
            commenting: h.state.commenting_on_commit(),
        }
    }

    /// Replay every keycap of chrome's strip over the queue from the middle of
    /// a three-card queue and check it did *something*, so the strip can't
    /// advertise a key the keymap sends nowhere.
    #[test]
    fn every_queue_hint_does_what_it_says() {
        for StripKey { cap, label } in strip_now(&queue_harness()) {
            let mut harness = queue_harness();
            if cap == "r" {
                // `r` only goes back to the newest record, so start from
                // another pick for it to have somewhere to go.
                harness.state_mut().state.set_review(Some(ReviewTarget {
                    card: id(5),
                    record: Some(id(99)),
                }));
            }
            let before = pane_effects(&harness);
            for KeyPress { modifiers, key } in keycap_presses(&cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                pane_effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
    }

    /// Replay every keycap of the strip in a plain review pane, as
    /// [`every_queue_hint_does_what_it_says`] does in the queue.
    #[test]
    fn every_review_pane_hint_does_what_it_says() {
        for StripKey { cap, label } in strip_now(&pane_harness()) {
            let mut harness = pane_harness();
            if cap == "r" {
                // `r` goes back to the newest record, so start from another
                // pick for it to have somewhere to go.
                harness.state_mut().state.set_review(Some(ReviewTarget {
                    card: id(5),
                    record: Some(id(99)),
                }));
            }
            let before = pane_effects(&harness);
            for KeyPress { modifiers, key } in keycap_presses(&cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                pane_effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
    }

    /// Replay every keycap of the strip on card 5's detail, as
    /// [`every_queue_hint_does_what_it_says`] does in the queue. `R` finds
    /// nothing in review under card 5, and says so.
    #[test]
    fn every_detail_hint_does_what_it_says() {
        for StripKey { cap, label } in strip_now(&detail_harness(None)) {
            let mut harness = detail_harness(None);
            let before = pane_effects(&harness);
            for KeyPress { modifiers, key } in keycap_presses(&cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                pane_effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
    }

    /// The detail's strip leaves out `open`, the one card action it has no
    /// use for, and only that.
    #[test]
    fn the_detail_strip_drops_only_open() {
        let detail = strip_now(&detail_harness(None));
        for KeyTool { cap, tool, .. } in card_action_tools() {
            let shown = detail.iter().any(|shown| shown.cap == cap);
            assert_eq!(
                shown,
                tool != crate::ui_tools::OPEN_CARD,
                "{cap:?} ({tool})"
            );
        }
    }

    /// Chrome's strip reads as Headway's own did: each view's labels in the
    /// order its strip listed them (the card actions after the grid's own
    /// keys, before a pane's), then chrome's `?`. The board edits (`a`, `D`,
    /// `H L J K`) are listed with them, as data tools on the current card.
    #[test]
    fn strips_read_as_before() {
        const CARD: &[&str] = &[
            "open",
            "explainer",
            "session/review",
            "review diff",
            "archive",
            "done",
            "send back",
            "comment",
            "next/prev card",
        ];
        const SCROLL: &[&str] = &["scroll", "page", "half page", "top/bottom"];
        const DIFF: &[&str] = &["next/prev file", "comment (picked lines or commit)"];
        let grid: &[&str] = &[
            "up/down",
            "columns",
            "first/last",
            "move card",
            "reorder",
            "new",
            "review queue",
            "filter",
            "issue list",
            "clear",
        ];
        let views = [
            ("grid", grid_harness(), [grid, CARD].concat()),
            (
                "queue",
                queue_harness(),
                [CARD, SCROLL, DIFF, &["leave"]].concat(),
            ),
            (
                "review pane",
                pane_harness(),
                [CARD, SCROLL, DIFF, &["back to card"]].concat(),
            ),
            (
                "detail",
                detail_harness(None),
                [&CARD[1..], SCROLL, &["review queue here", "back"]].concat(),
            ),
        ];
        for (name, harness, mut expected) in views {
            expected.push("keys");
            assert_eq!(strip_labels(&harness), expected, "the {name}'s strip");
        }
    }

    /// Esc backs the detail out to the grid, eaten on the way so chrome
    /// doesn't see it, as `q` does.
    #[test]
    fn esc_leaves_the_detail() {
        let mut harness = detail_harness(None);
        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.selected(), None);
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    /// An Esc in one of the detail's fields only leaves the field, so the
    /// field is still there to commit its edit; the next Esc leaves the card.
    #[test]
    fn esc_in_a_detail_field_only_leaves_the_field() {
        let field = egui::Id::unique("keys_test_detail_field");
        let mut harness = detail_harness(Some(field));
        harness.ctx.memory_mut(|m| m.request_focus(field));
        harness.run();
        assert_eq!(harness.ctx.memory(|m| m.focused()), Some(field));

        // The keymap leaves that Esc to the detail, whose own pass eats it as
        // it ends (not drawn here; `esc_in_the_title_editor_commits_and_keeps
        // _the_detail` runs the real one).
        press(&mut harness, Key::Escape);
        assert_eq!(harness.ctx.memory(|m| m.focused()), None, "field left");
        assert_eq!(harness.state().state.selected(), Some(id(5)), "card kept");
        assert!(harness.state().esc_left, "left to the detail");

        harness.state_mut().esc_left = false;
        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.selected(), None);
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    /// A two-file commit for the review-comment tests: `src/a.rs` changes a
    /// line, `b.txt` loses one.
    const TWO_FILES: &str = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,2 @@
 fn a() {
-    old();
+    new();
diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,1 @@
-gone
 kept
";

    /// Two drafts, on the new side of `src/a.rs` and the old side of
    /// `b.txt`, in that order.
    fn two_drafts() -> [crate::ui::DraftComment; 2] {
        let patch = notedeck_ui::diff::GitPatch::parse(TWO_FILES);
        let pick = |file, lines| notedeck_ui::diff::PatchSelection { file, lines };
        let draft = |file, lines, body: &str| {
            crate::ui::DraftComment::new(&patch, &pick(file, lines), COMMIT, body.to_string())
                .expect("the pick has numbered lines")
        };
        [
            draft(0, 0..3, "rename it"),
            draft(1, 0..1, "why drop this?"),
        ]
    }

    /// `C`, `line` typed into its composer, and Enter, as a keyboard
    /// delivers them.
    fn comment_line(harness: &mut Harness<'static, KeysHarness>, line: &str) {
        press_with(harness, Modifiers::SHIFT, Key::C);
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Text(line.to_string()));
        harness.step();
        press(harness, Key::Enter);
    }

    /// `C` in a review pane with drafts queued posts the shown record's
    /// drafts as review comments on it at once, in order, and sends them all
    /// to the record's session in one message that names each place, quotes
    /// its lines and carries its comment; no composer opens. The drafts go
    /// with the send, so a second `C` has none and asks for a line instead.
    /// A record with no session still gets its comments, and nothing opens.
    #[test]
    fn send_comments_posts_each_and_opens_the_session_once() {
        for session in [true, false] {
            let mut harness = pane_harness();
            if !session {
                harness.state_mut().view.columns[1].cards[1].reviews[0]
                    .fields
                    .agentium = None;
            }
            for draft in two_drafts() {
                harness.state_mut().state.add_review_draft(id(50), draft);
            }
            press_with(&mut harness, Modifiers::SHIFT, Key::C);

            let h = harness.state();
            assert!(!h.state.rejecting(), "drafts queued: no composer");
            let (card, record, comments) = h.review_comments.clone().expect("comments posted");
            assert_eq!((card, record), (id(5), id(50)));
            let posted: Vec<_> = comments
                .iter()
                .map(|c| {
                    let loc = c.location.as_ref().expect("a drafted comment has lines");
                    (
                        loc.path.as_str(),
                        loc.line_value(),
                        loc.side,
                        c.body.as_str(),
                    )
                })
                .collect();
            assert_eq!(
                posted,
                vec![
                    (
                        "src/a.rs",
                        "1-2".to_string(),
                        headway::event::LineSide::New,
                        "rename it"
                    ),
                    (
                        "b.txt",
                        "1".to_string(),
                        headway::event::LineSide::Old,
                        "why drop this?"
                    ),
                ]
            );
            assert!(comments.iter().all(|c| {
                c.reply_to.is_none() && c.location.as_ref().is_some_and(|l| l.commit == COMMIT)
            }));

            if !session {
                assert_eq!(h.session, None, "no session, no open");
                continue;
            }
            let open = h.session.clone().expect("one open");
            assert_eq!(open.reference, SESSION);
            let card_ref = headway::wordid::card_ref(&h.view.id, id(5).bytes());
            assert_eq!(
                open.msg.as_deref(),
                Some(
                    format!(
                        "Review comments on commit 136ceb9d3bfa (card {card_ref}):\n\n\
                         src/a.rs:1-2\n```diff\n fn a() {{\n-    old();\n+    new();\n```\nrename it\n\n\
                         b.txt:old 1\n```diff\n-gone\n```\nwhy drop this?"
                    )
                    .as_str()
                )
            );

            assert_eq!(h.commented, None, "no line comment");

            harness.state_mut().review_comments = None;
            press_with(&mut harness, Modifiers::SHIFT, Key::C);
            assert_eq!(harness.state().review_comments, None);
            assert!(harness.state().state.rejecting(), "nothing queued: a line");
        }
    }

    /// `c` with no lines picked opens the composer on the shown record's
    /// commit as a whole, rather than asking for a pick.
    #[test]
    fn c_with_nothing_picked_comments_on_the_commit() {
        let mut harness = pane_harness();
        press(&mut harness, Key::C);
        assert_eq!(harness.state().state.commenting_on_commit(), Some(id(50)));
        assert_eq!(harness.state().state.notice(), None);
    }

    /// A comment on the commit as a whole posts with no location, and leads
    /// the session message — just what it says, no place or quote — ahead of
    /// the comments on lines, whatever order they were added in. The card
    /// stays where it is.
    #[test]
    fn send_comments_leads_with_the_whole_commit_comments() {
        let mut harness = pane_harness();
        let [lines, _] = two_drafts();
        let state = &mut harness.state_mut().state;
        state.add_review_draft(id(50), lines);
        state.add_review_draft(
            id(50),
            crate::ui::DraftComment::on_commit("split this commit".to_string()),
        );
        press_with(&mut harness, Modifiers::SHIFT, Key::C);

        let h = harness.state();
        let (_, _, comments) = h.review_comments.clone().expect("comments posted");
        let posted: Vec<_> = comments
            .iter()
            .map(|c| (c.location.is_some(), c.body.as_str()))
            .collect();
        assert_eq!(
            posted,
            vec![(true, "rename it"), (false, "split this commit")],
            "published as added"
        );
        let card_ref = headway::wordid::card_ref(&h.view.id, id(5).bytes());
        assert_eq!(
            h.session.as_ref().and_then(|o| o.msg.as_deref()),
            Some(
                format!(
                    "Review comments on commit 136ceb9d3bfa (card {card_ref}):\n\n\
                     split this commit\n\n\
                     src/a.rs:1-2\n```diff\n fn a() {{\n-    old();\n+    new();\n```\nrename it"
                )
                .as_str()
            )
        );
        assert_eq!(h.moved, None, "no verdict");
    }

    /// `C` comments on the current card in every view, the detail too:
    /// Enter posts the line as a plain comment and sends it to the record's
    /// session, and the card stays where it is: no move, no queue step.
    #[test]
    fn c_comments_and_tells_the_session_in_every_view() {
        let views = [
            ActionTestView {
                name: "grid",
                open: grid_harness,
            },
            ActionTestView {
                name: "queue",
                open: queue_harness,
            },
            ActionTestView {
                name: "review pane",
                open: pane_harness,
            },
            ActionTestView {
                name: "detail",
                open: || detail_harness(None),
            },
        ];
        for ActionTestView { name, open } in views {
            let mut harness = open();
            comment_line(&mut harness, "looks good, ship it");
            let h = harness.state();
            assert_eq!(
                h.commented,
                Some((id(5), "looks good, ship it".to_string())),
                "{name}"
            );
            let card_ref = headway::wordid::card_ref(&h.view.id, id(5).bytes());
            assert_eq!(
                h.session,
                Some(notedeck::OpenUri {
                    reference: SESSION.to_string(),
                    msg: Some(format!(
                        "Review comments on commit 136ceb9d3bfa (card {card_ref}):\n\n\
                         looks good, ship it"
                    )),
                }),
                "{name}"
            );
            assert!(!h.state.rejecting(), "{name}: composer closed");
            harness.step();
            assert_eq!(harness.state().moved, None, "{name}: no move");
            if name == "queue" {
                assert_eq!(harness.state().state.review_card(), Some(id(5)), "no step");
            }
        }
    }

    /// `C` with drafts queued sends them, from any view: the detail's `C`
    /// sends what was drafted in the card's review pane. The line composer
    /// only opens with nothing queued.
    #[test]
    fn c_with_drafts_queued_sends_them_from_the_detail() {
        let mut harness = detail_harness(None);
        let [lines, _] = two_drafts();
        harness.state_mut().state.add_review_draft(id(50), lines);
        press_with(&mut harness, Modifiers::SHIFT, Key::C);

        let h = harness.state();
        assert!(!h.state.rejecting(), "no composer");
        let (card, record, comments) = h.review_comments.clone().expect("drafts posted");
        assert_eq!((card, record), (id(5), id(50)));
        let bodies: Vec<_> = comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, vec!["rename it"]);
        assert_eq!(
            h.session.as_ref().map(|o| o.reference.as_str()),
            Some(SESSION)
        );
        assert_eq!(h.commented, None);
        assert_eq!(h.moved, None);
    }

    /// `C` on a card with no review record still posts its comment; there's
    /// no session to tell.
    #[test]
    fn c_on_a_card_without_a_record_only_comments() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = review_board();
        harness.state_mut().state.set_selected(Some(id(6)));
        harness.run();
        comment_line(&mut harness, "picking this up tomorrow");
        assert_eq!(
            harness.state().commented,
            Some((id(6), "picking this up tomorrow".to_string()))
        );
        assert_eq!(harness.state().session, None);
        assert_eq!(harness.state().review_comments, None);
    }

    /// `X` sends the card back from its detail as from any other view.
    #[test]
    fn x_from_the_detail_sends_the_card_back() {
        let mut harness = detail_harness(None);
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("flaky".to_string()));
        harness.step();
        press(&mut harness, Key::Enter);
        assert_eq!(
            harness.state().commented,
            Some((id(5), "review: flaky".to_string()))
        );
        assert!(harness.state().session.is_some(), "its session hears it");
        harness.step();
        assert_eq!(harness.state().moved, Some((id(5), 0, 3)));
        assert_eq!(harness.state().state.selected(), Some(id(5)), "still open");
    }

    /// The scroll keys each ask the diff for their scroll; `]` the next file.
    #[test]
    fn queue_scroll_keys_ask_the_diff_to_scroll() {
        let mut harness = queue_harness();
        let scroll = |h: &Harness<'static, KeysHarness>| h.state().state.review_scroll();
        press(&mut harness, Key::J);
        assert_eq!(scroll(&harness), Some(PatchScroll::Rows(1)));
        press(&mut harness, Key::K);
        assert_eq!(scroll(&harness), Some(PatchScroll::Rows(-1)));
        press_with(&mut harness, Modifiers::CTRL, Key::D);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(0.5)));
        press_with(&mut harness, Modifiers::CTRL, Key::U);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(-0.5)));
        press(&mut harness, Key::Space);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(1.0)));
        press_with(&mut harness, Modifiers::SHIFT, Key::Space);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(-1.0)));
        press_with(&mut harness, Modifiers::CTRL, Key::F);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(1.0)));
        press_with(&mut harness, Modifiers::CTRL, Key::B);
        assert_eq!(scroll(&harness), Some(PatchScroll::Pages(-1.0)));
        press_with(&mut harness, Modifiers::SHIFT, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Bottom));
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("top/bottom").is_some(), "g hint up");
        press(&mut harness, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Top));
        press(&mut harness, Key::CloseBracket);
        assert_eq!(scroll(&harness), Some(PatchScroll::NextFile));
        press(&mut harness, Key::OpenBracket);
        assert_eq!(scroll(&harness), Some(PatchScroll::PrevFile));
        assert_eq!(harness.state().state.review_card(), Some(id(5)), "no step");
    }

    /// `D` moves the card to the end of Done and, as that folds in, the
    /// queue steps on; on the last card it leaves the queue saying it's done.
    #[test]
    fn d_moves_the_card_to_done_and_advances() {
        let mut harness = queue_harness();
        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(5), 2, 3)));
        assert_eq!(harness.state().state.review_card(), Some(id(6)));

        // Done holds 5 by now, so 6 goes after it.
        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(6), 2, 4)));
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::QueueDone));
        assert_eq!(harness.state().state.cursor(), Some(id(6)));
    }

    /// `a` in the queue archives the card and steps on, as a verdict does.
    #[test]
    fn a_in_the_queue_archives_and_advances() {
        let mut harness = queue_harness();
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(5)));
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        assert!(harness.state().state.queue_open());
        assert_eq!(harness.state().session, None, "not a session open");
    }

    /// `r` in the queue goes back to the card's newest record and nothing
    /// else: the queue stays open on the card, and the selection stays what
    /// the queue was opened over (none, from the grid), so leaving the queue
    /// still lands on the grid rather than the card's detail.
    #[test]
    fn r_in_the_queue_only_resets_the_record() {
        let mut harness = queue_harness();
        harness.state_mut().state.set_review(Some(ReviewTarget {
            card: id(5),
            record: Some(id(99)),
        }));
        press(&mut harness, Key::R);
        let state = &harness.state().state;
        assert_eq!(state.review_record(), None, "newest record");
        assert!(state.queue_open());
        assert_eq!(state.review_card(), Some(id(5)));
        assert_eq!(state.selected(), None, "no selection leaks out");

        press(&mut harness, Key::Q);
        assert_eq!(harness.state().state.selected(), None, "back to the grid");
    }

    /// The queue's cards are a snapshot: a card that left the board before
    /// the queue reached it keeps its place, and a card action on it raises
    /// no edit (`a` is unavailable without a current card) and doesn't step
    /// the queue on, `X` only says so; `n` still steps off it.
    #[test]
    fn queue_actions_on_a_card_that_left_the_board_only_say_so() {
        let mut harness = queue_harness();
        press(&mut harness, Key::P);
        assert_eq!(harness.state().state.review_card(), Some(id(4)));
        harness.state_mut().view.columns[1].cards.remove(1);
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));

        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, None);
        assert_eq!(harness.state().state.review_card(), Some(id(5)), "no step");
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::CardGone));
        press(&mut harness, Key::Enter);
        assert!(harness.state().state.queue_open(), "no detail to open");

        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
    }

    /// The queue follows a verdict on its card from anywhere: once the card
    /// it shows leaves In Review (moved on another device, archived), it
    /// steps on as after `D`.
    #[test]
    fn a_card_leaving_in_review_steps_the_queue_on() {
        let mut harness = queue_harness();
        let card = harness.state_mut().view.columns[1].cards.remove(1);
        harness.state_mut().view.columns[0].cards.push(card);
        harness.run();
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        assert!(harness.state().state.queue_open());
    }

    /// `X` opens the reason composer, which holds the queue's keys; Enter
    /// posts the reason as a `review:` comment, the move to In Progress lands
    /// the frame after, and the queue steps on.
    #[test]
    fn x_posts_a_review_comment_and_sends_the_card_back() {
        let mut harness = queue_harness();
        // As a keyboard delivers it: the press and the character it types.
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("X".to_string()));
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        assert!(harness.state().state.rejecting());
        harness.run();
        assert_eq!(
            harness.ctx.memory(|m| m.focused()),
            Some(crate::ui::reason_field_id()),
            "composer focused"
        );
        assert_eq!(harness.state().state.reason_text(), "", "X not typed");

        // The queue's keys stand down while it's open.
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(5)), "no step");
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("no tests".to_string()));
        harness.step();
        assert_eq!(harness.state().state.reason_text(), "no tests");

        press(&mut harness, Key::Enter);
        assert_eq!(
            harness.state().commented,
            Some((id(5), "review: no tests".to_string()))
        );
        let card_ref = headway::wordid::card_ref(&harness.state().view.id, id(5).bytes());
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri {
                reference: SESSION.to_string(),
                msg: Some(format!(
                    "Sent back for changes (commit 136ceb9d3bfa, card {card_ref}): no tests"
                )),
            }),
            "the reason goes to the record's session"
        );
        assert!(!harness.state().state.rejecting());
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        harness.step();
        assert_eq!(harness.state().moved, Some((id(5), 0, 3)));
        assert_eq!(harness.ctx.memory(|m| m.focused()), None, "focus returned");

        // The queue's keys are back.
        press(&mut harness, Key::P);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
    }

    /// `X` on the grid's cursor card asks for the reason too; Enter comments
    /// and sends it back without any queue to step.
    #[test]
    fn x_on_the_grid_sends_the_cursor_card_back() {
        let mut harness = grid_harness();
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        assert!(harness.state().state.rejecting());
        harness.run();
        // Grid keys stand down while the composer has the keyboard.
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), Some(id(5)));
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("flaky".to_string()));
        harness.step();
        press(&mut harness, Key::Enter);
        assert_eq!(
            harness.state().commented,
            Some((id(5), "review: flaky".to_string()))
        );
        harness.step();
        assert_eq!(harness.state().moved, Some((id(5), 0, 3)));
    }

    /// `X` on a card whose record names no agentium session still comments
    /// and sends it back; it just opens no session.
    #[test]
    fn x_without_a_session_still_sends_the_card_back() {
        let mut harness = queue_harness();
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("flaky".to_string()));
        harness.step();
        press(&mut harness, Key::Enter);
        assert_eq!(
            harness.state().commented,
            Some((id(6), "review: flaky".to_string()))
        );
        assert_eq!(harness.state().session, None);
        harness.step();
        assert_eq!(harness.state().moved, Some((id(6), 0, 3)));
    }

    /// Esc in the composer cancels it without a comment or a move, and is
    /// eaten rather than leaving the queue.
    #[test]
    fn esc_in_the_reason_composer_cancels() {
        let mut harness = queue_harness();
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        harness.run();
        press(&mut harness, Key::Enter);
        assert!(
            harness.state().state.rejecting(),
            "empty reason posts nothing"
        );
        assert_eq!(harness.state().commented, None);

        press(&mut harness, Key::Escape);
        harness.step();
        assert!(!harness.state().state.rejecting());
        assert!(harness.state().state.queue_open());
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
        assert_eq!(
            (harness.state().moved, harness.state().commented.clone()),
            (None, None)
        );
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    /// The composer closes with the view `X` was pressed in, however it's
    /// left — a click onto another card, the detail's `q` or ✕, a back the
    /// route seeded — so an Enter there can't send the first card back.
    #[test]
    fn the_reason_composer_closes_with_its_view() {
        /// A way out of the detail, as a click or a seeded route leaves it.
        type Leave = fn(&mut BoardUiState);
        let leaves: [(&str, Leave); 3] = [
            ("another card", |s| s.set_selected(Some(id(6)))),
            ("the grid", |s| s.leave_card()),
            ("the graph", |s| s.set_graph_epic(Some(id(5)))),
        ];
        for (name, leave) in leaves {
            let mut harness = detail_harness(None);
            press_with(&mut harness, Modifiers::SHIFT, Key::X);
            harness.run();
            assert!(harness.state().state.rejecting(), "{name}");
            harness
                .input_mut()
                .events
                .push(egui::Event::Text("flaky".to_string()));
            harness.step();

            leave(&mut harness.state_mut().state);
            press(&mut harness, Key::Enter);
            assert!(!harness.state().state.rejecting(), "{name}");
            assert_eq!(harness.state().commented, None, "{name}");
            harness.step();
            assert_eq!(harness.state().moved, None, "{name}");
        }
    }

    /// While another field has the keyboard, the composer leaves Enter and
    /// Esc to it; once nothing does, they're the composer's again.
    #[test]
    fn the_reason_composer_leaves_enter_to_a_focused_field() {
        let comment = egui::Id::unique("comment");
        let mut harness = detail_harness(Some(comment));
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("flaky".to_string()));
        harness.step();

        harness.ctx.memory_mut(|m| m.request_focus(comment));
        harness.step();
        press(&mut harness, Key::Enter);
        assert_eq!(harness.state().commented, None, "nothing posted");
        assert!(harness.state().state.rejecting());
        // A single-line field gives its focus up on its Enter: it got it.
        assert_eq!(harness.ctx.memory(|m| m.focused()), None, "field's Enter");

        press(&mut harness, Key::Escape);
        assert!(
            !harness.state().state.rejecting(),
            "Esc with nothing focused"
        );
        assert!(
            harness.state().state.selected().is_some(),
            "still in the detail"
        );
    }

    /// `e` opens the shown record's explainer; on a card without one it only
    /// says so.
    #[test]
    fn e_opens_the_explainer_or_says_there_is_none() {
        let mut harness = queue_harness();
        press(&mut harness, Key::E);
        assert_eq!(
            harness.state().opened.as_deref(),
            Some("https://example.com/explainer")
        );

        press(&mut harness, Key::N);
        harness.state_mut().opened = None;
        press(&mut harness, Key::E);
        assert_eq!(harness.state().opened, None);
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NoExplainer)
        );
        assert!(harness.state().state.queue_open());
    }

    /// Enter and `o` leave the queue for its card's detail, keeping the
    /// queue's place for the back that returns to it.
    #[test]
    fn enter_and_o_open_the_queue_card() {
        for key in [Key::Enter, Key::O] {
            let mut harness = queue_harness();
            press(&mut harness, key);
            assert!(!harness.state().state.queue_open(), "{key:?}");
            assert_eq!(harness.state().state.selected(), Some(id(5)));
            assert_eq!(harness.state().state.review_card(), None);

            // Back onto the queue's entry reopens it on the same card.
            harness.state_mut().state.set_selected(None);
            harness
                .state_mut()
                .state
                .set_queue_open(Some(QueueScope::Board));
            assert_eq!(
                harness.state().state.queue_review().map(|t| t.card),
                Some(id(5))
            );
        }
    }

    /// `s` opens the shown record's session; `S` opens it with a
    /// `/code-review` message naming the commit and the card. Neither leaves
    /// the queue, since the open is a cross-app one.
    #[test]
    fn s_opens_the_record_session_and_shift_s_asks_for_a_review() {
        let mut harness = queue_harness();
        press(&mut harness, Key::S);
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri::new(SESSION))
        );

        harness.state_mut().session = None;
        press_with(&mut harness, Modifiers::SHIFT, Key::S);
        let card_ref = headway::wordid::card_ref(&harness.state().view.id, id(5).bytes());
        let msg = format!(
            "launch a /code-review for the work done in this session \
             (commit 136ceb9d3bfa, card {card_ref})"
        );
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri {
                reference: SESSION.to_string(),
                msg: Some(msg),
            })
        );
        assert!(harness.state().state.queue_open());
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
        assert_eq!(harness.state().state.notice(), None);
    }

    /// On a record with no session, `s` and `S` open nothing and say so.
    #[test]
    fn s_without_a_session_only_says_so() {
        let mut harness = queue_harness();
        press(&mut harness, Key::N);
        press(&mut harness, Key::S);
        press_with(&mut harness, Modifiers::SHIFT, Key::S);
        assert_eq!(harness.state().session, None);
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::NoSession));
    }

    /// A notice is about the view it went up in: `s` on a sessionless record
    /// in a plain review pane says so there, and it's gone once Esc has backed
    /// out to the card's detail, rather than following on to the grid — or
    /// coming back with the pane when `r` reopens it the very next frame.
    #[test]
    fn a_pane_notice_comes_down_with_the_pane() {
        let mut harness = pane_harness();
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        press(&mut harness, Key::S);
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::NoSession));

        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.review_card(), None);
        assert_eq!(harness.state().state.selected(), Some(id(6)));
        assert_eq!(harness.state().state.notice(), None);

        press(&mut harness, Key::R);
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        assert_eq!(harness.state().state.notice(), None);
    }

    /// A plain review pane reads like the queue — `j`, `G` and `]` scroll its
    /// diff, `?` shows its strip, `S` asks the session for a review — while
    /// `n` steps to the next card's review in the column and `q` backs out to
    /// the card's detail.
    #[test]
    fn a_plain_review_pane_takes_the_queue_keys() {
        let mut harness = pane_harness();
        let scroll = |h: &Harness<'static, KeysHarness>| h.state().state.review_scroll();
        press(&mut harness, Key::J);
        assert_eq!(scroll(&harness), Some(PatchScroll::Rows(1)));
        press_with(&mut harness, Modifiers::SHIFT, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Bottom));
        press(&mut harness, Key::CloseBracket);
        assert_eq!(scroll(&harness), Some(PatchScroll::NextFile));
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        assert!(harness.query_by_label("back to card").is_some());
        assert!(harness.query_by_label("next/prev file").is_some());

        press_with(&mut harness, Modifiers::SHIFT, Key::S);
        let open = harness.state().session.clone().expect("an open");
        assert_eq!(open.reference, SESSION);
        assert!(open.msg.is_some_and(|m| m.contains("commit 136ceb9d3bfa")));
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().archived, None, "not an archive");

        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        assert_eq!(harness.state().state.selected(), Some(id(6)));
        press(&mut harness, Key::P);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));

        press(&mut harness, Key::Q);
        assert_eq!(harness.state().state.review_card(), None);
        assert_eq!(harness.state().state.selected(), Some(id(5)));
        assert!(!harness.state().esc_left);
    }

    /// Esc backs a plain review pane out to its card's detail, eaten on the
    /// way so chrome doesn't see it.
    #[test]
    fn esc_backs_a_plain_review_pane_out_to_the_card() {
        let mut harness = pane_harness();
        press(&mut harness, Key::Escape);
        assert_eq!(harness.state().state.review_card(), None);
        assert_eq!(harness.state().state.selected(), Some(id(5)));
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    /// The detail takes the card actions — `s` opens its session, `n` steps
    /// to the next card in its column, `D` moves it to Done — and scrolls
    /// with `j`; `q` backs out to the grid.
    #[test]
    fn the_detail_takes_the_card_actions() {
        let mut harness = detail_harness(None);
        press(&mut harness, Key::S);
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri::new(SESSION))
        );
        press(&mut harness, Key::J);
        assert_eq!(
            harness.state().state.detail_scroll(),
            Some(DetailScroll::Rows(1))
        );
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.selected(), Some(id(6)));
        assert_eq!(harness.state().state.cursor(), Some(id(6)));
        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(6), 2, 3)));
        assert_eq!(harness.state().state.selected(), Some(id(6)), "still open");
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        assert!(harness.query_by_label("session/review").is_some());
        press(&mut harness, Key::Q);
        assert_eq!(harness.state().state.selected(), None);
    }

    /// `a` in a plain review pane archives its card and stays put: the app
    /// prunes the pane's entry and its detail's once the archive folds in
    /// (notedeck_chrome's `pane_a_ends_on_the_board`), so a back from here
    /// would only land on a detail about to go.
    #[test]
    fn a_in_a_plain_pane_archives_and_stays() {
        let mut harness = pane_harness();
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(5)));
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
        assert_eq!(harness.state().state.selected(), Some(id(5)));
    }

    /// `a` on the detail archives its card. The detail stays until the app
    /// prunes the card's entries as the archive folds in, as a plain pane
    /// does (notedeck_chrome's `detail_a_ends_on_the_board`).
    #[test]
    fn a_on_the_detail_archives_it() {
        let mut harness = detail_harness(None);
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(5)));
    }

    /// Typing into the detail's comment composer (or any focused field) keeps
    /// its keys: an `s` there opens no session.
    #[test]
    fn typing_in_the_detail_composer_keeps_its_keys() {
        let field = egui::Id::unique("keys_test_comment");
        let mut harness = detail_harness(Some(field));
        harness.ctx.memory_mut(|m| m.request_focus(field));
        harness.run();
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("s".to_string()));
        press(&mut harness, Key::S);
        assert_eq!(harness.state().session, None);
        assert_eq!(harness.state().state.last_card_action, None);
        assert_eq!(harness.state().text, "s");
    }

    /// A verdict key on a board without its column does nothing: `D`'s data
    /// tool refuses (chrome logs it), and `X` says so.
    #[test]
    fn verdicts_need_their_columns() {
        let mut harness = queue_harness();
        harness.state_mut().view.columns[2].id = "shipped".to_string();
        harness.state_mut().view.columns[2].name = "Shipped".to_string();
        harness.state_mut().view.columns[0].id = "doing".to_string();
        harness.state_mut().view.columns[0].name = "Doing".to_string();

        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, None);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
        press_with(&mut harness, Modifiers::SHIFT, Key::X);
        assert!(!harness.state().state.rejecting());
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NoInProgressColumn)
        );
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
    }

    #[test]
    fn bare_l_still_just_moves_the_cursor() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));
        press(&mut harness, Key::L);
        assert_eq!(harness.state().moved, None);
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
    }

    /// Everything a keymap test can see a key or a tool do, in any view.
    #[derive(Debug, PartialEq)]
    struct AllEffects {
        grid: Effects,
        pane: PaneEffects,
    }

    fn all_effects(harness: &Harness<'static, KeysHarness>) -> AllEffects {
        AllEffects {
            grid: effects(harness),
            pane: pane_effects(harness),
        }
    }

    /// Call the UI tool `name` with `args` as chrome would (it lands on
    /// [`BoardUiState::queue_tool`] via `Headway::call_ui_tool`), and run the
    /// frame that drains it.
    fn call_tool(harness: &mut Harness<'static, KeysHarness>, name: &str, args: serde_json::Value) {
        let tool =
            HeadwayUiTool::parse(name, &args).unwrap_or_else(|e| panic!("{name} {args}: {e}"));
        harness.state_mut().state.queue_tool(tool);
        harness.step();
        harness.step();
    }

    /// A keycap and the UI tool call that stands for it.
    #[derive(Clone)]
    struct KeyTool {
        cap: &'static str,
        tool: &'static str,
        args: serde_json::Value,
    }

    /// [`KeyTool`] by position, to keep the tables below one row per key.
    fn key_tool(cap: &'static str, tool: &'static str, args: serde_json::Value) -> KeyTool {
        KeyTool { cap, tool, args }
    }

    /// Each card action's keycap and the UI tool call that stands for it.
    /// `a` and `D` are board edits, which are data tools, not UI tools.
    fn card_action_tools() -> Vec<KeyTool> {
        use crate::ui_tools::*;
        use serde_json::json;
        vec![
            key_tool("\u{21B5}", OPEN_CARD, json!({})),
            key_tool("o", OPEN_CARD, json!({})),
            key_tool("e", OPEN_EXPLAINER, json!({})),
            key_tool("s", OPEN_SESSION, json!({})),
            key_tool("S", OPEN_SESSION, json!({ "code_review": true })),
            key_tool("r", OPEN_REVIEW, json!({})),
            key_tool("X", SEND_BACK, json!({})),
            key_tool("C", QUICK_COMMENT, json!({})),
            key_tool("n", STEP_CARD, json!({ "dir": "next" })),
            key_tool("p", STEP_CARD, json!({ "dir": "prev" })),
        ]
    }

    /// Scrolling a pane: its keycaps and their tool calls.
    fn scroll_tools() -> Vec<KeyTool> {
        use crate::ui_tools::*;
        use serde_json::json;
        vec![
            key_tool("j", SCROLL, json!({ "to": "down" })),
            key_tool("k", SCROLL, json!({ "to": "up" })),
            key_tool("space", SCROLL, json!({ "to": "page_down" })),
            key_tool("^f", SCROLL, json!({ "to": "page_down" })),
            key_tool("^b", SCROLL, json!({ "to": "page_up" })),
            key_tool("^d", SCROLL, json!({ "to": "half_down" })),
            key_tool("^u", SCROLL, json!({ "to": "half_up" })),
            key_tool("gg", SCROLL, json!({ "to": "top" })),
            key_tool("G", SCROLL, json!({ "to": "bottom" })),
            key_tool("q", BACK, json!({})),
            key_tool("esc", BACK, json!({})),
        ]
    }

    /// One view's keycaps and the tool calls that stand for them, and how to
    /// open the view with card 5 current.
    struct ToolTestView {
        name: &'static str,
        open: fn() -> Harness<'static, KeysHarness>,
        keys: Vec<KeyTool>,
    }

    /// [`grid_harness`] with the filter field laid out, so `/` has a field to
    /// focus.
    fn grid_with_filter() -> Harness<'static, KeysHarness> {
        let mut harness = keys_harness(Some(filter_field_id()));
        harness.state_mut().view = review_board();
        harness.state_mut().state.set_cursor(id(5));
        harness.run();
        harness
    }

    fn tool_test_views() -> Vec<ToolTestView> {
        use crate::ui_tools::*;
        use serde_json::json;
        let grid = [
            key_tool("j", CURSOR_MOVE, json!({ "to": "down" })),
            key_tool("k", CURSOR_MOVE, json!({ "to": "up" })),
            key_tool("h", CURSOR_MOVE, json!({ "to": "left" })),
            key_tool("l", CURSOR_MOVE, json!({ "to": "right" })),
            key_tool("gg", CURSOR_MOVE, json!({ "to": "first" })),
            key_tool("G", CURSOR_MOVE, json!({ "to": "last" })),
            key_tool("c", COMPOSE_CARD, json!({})),
            key_tool("/", FOCUS_FILTER, json!({})),
            key_tool("R", OPEN_REVIEW_QUEUE, json!({})),
            key_tool("esc", CLEAR, json!({})),
        ];
        let review = [
            key_tool("]", STEP_FILE, json!({ "dir": "next" })),
            key_tool("[", STEP_FILE, json!({ "dir": "prev" })),
            key_tool("c", COMMENT, json!({})),
        ];
        let detail = [key_tool("R", OPEN_REVIEW_QUEUE, json!({}))];
        let with = |extra: &[KeyTool], scroll: bool| {
            let mut keys = card_action_tools();
            if scroll {
                keys.extend(scroll_tools());
            }
            keys.extend(extra.iter().cloned());
            keys
        };
        vec![
            ToolTestView {
                name: "grid",
                open: grid_with_filter,
                keys: with(&grid, false),
            },
            ToolTestView {
                name: "queue",
                open: queue_harness,
                keys: with(&review, true),
            },
            ToolTestView {
                name: "review pane",
                open: pane_harness,
                keys: with(&review, true),
            },
            ToolTestView {
                name: "detail",
                open: || detail_harness(None),
                keys: with(&detail, true),
            },
        ]
    }

    /// **The tool guard.** In each view, every key that has a UI tool and
    /// that tool, dispatched by name, leave the same state behind: one
    /// `apply_*` path under both, so moving a key into chrome's keymap (bound
    /// to the tool) can't change what it does.
    #[test]
    fn every_tool_does_what_its_key_does() {
        for ToolTestView { name, open, keys } in tool_test_views() {
            for KeyTool { cap, tool, args } in keys {
                let mut by_key = open();
                for KeyPress { modifiers, key } in keycap_presses(cap) {
                    press_with(&mut by_key, modifiers, key);
                }
                by_key.step();
                let mut by_tool = open();
                call_tool(&mut by_tool, tool, args.clone());
                assert_eq!(
                    all_effects(&by_tool),
                    all_effects(&by_key),
                    "{tool} {args} vs keycap {cap:?} in the {name}"
                );
            }
        }
    }

    /// A card tool can name its card. On the grid the cursor moves to it
    /// first; over a pane only the card showing may be named, and a card
    /// that isn't on the board is dropped.
    #[test]
    fn a_card_tool_can_name_its_card() {
        use crate::ui_tools::{OPEN_CARD, OPEN_EXPLAINER};
        let card_ref = |n| headway::wordid::card_ref("headway", id(n).bytes());

        let mut harness = grid_harness();
        call_tool(
            &mut harness,
            OPEN_CARD,
            serde_json::json!({ "card": card_ref(4) }),
        );
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
        assert_eq!(harness.state().state.selected(), Some(id(4)));

        // Another board's card, or one that isn't anywhere: nothing.
        let mut harness = grid_harness();
        let elsewhere = headway::wordid::card_ref("elsewhere", id(4).bytes());
        call_tool(
            &mut harness,
            OPEN_CARD,
            serde_json::json!({ "card": elsewhere }),
        );
        call_tool(
            &mut harness,
            OPEN_CARD,
            serde_json::json!({ "card": card_ref(77) }),
        );
        assert_eq!(harness.state().state.cursor(), Some(id(5)));
        assert_eq!(harness.state().state.selected(), None);

        // The detail shows card 5: naming it acts, naming another doesn't.
        let mut harness = detail_harness(None);
        call_tool(
            &mut harness,
            OPEN_EXPLAINER,
            serde_json::json!({ "card": card_ref(4) }),
        );
        assert_eq!(harness.state().opened, None);
        call_tool(
            &mut harness,
            OPEN_EXPLAINER,
            serde_json::json!({ "card": card_ref(5) }),
        );
        assert_eq!(
            harness.state().opened.as_deref(),
            Some("https://example.com/explainer")
        );
    }

    /// A tool the view in front has no use for does nothing, and doesn't
    /// linger for a later frame.
    #[test]
    fn a_tool_with_nothing_to_act_on_is_dropped() {
        use crate::ui_tools::{CURSOR_MOVE, SCROLL};
        let mut harness = detail_harness(None);
        let before = all_effects(&harness);
        call_tool(
            &mut harness,
            CURSOR_MOVE,
            serde_json::json!({ "to": "down" }),
        );
        assert_eq!(all_effects(&harness), before);

        // The graph takes no tools; one called while it's open is gone by the
        // time it closes.
        let mut harness = grid_harness();
        harness.state_mut().state.set_graph_epic(Some(id(5)));
        call_tool(&mut harness, SCROLL, serde_json::json!({}));
        call_tool(
            &mut harness,
            CURSOR_MOVE,
            serde_json::json!({ "to": "down" }),
        );
        harness.state_mut().state.set_graph_epic(None);
        harness.step();
        assert_eq!(harness.state().state.cursor(), Some(id(5)));
    }

    /// The key scopes, and which tools are available, in each view.
    #[test]
    fn scopes_and_availability_follow_the_view() {
        use crate::ui_tools::*;
        let scopes = |h: &Harness<'static, KeysHarness>| key_scopes(&h.state().state);
        let available = |h: &Harness<'static, KeysHarness>, name| {
            crate::ui_tools::available(&h.state().state, name)
        };

        const GRID: [&str; 3] = ["headway.grid", "headway.card", "headway"];
        // The grid, its cursor on card 5, whose record has a session.
        let grid = grid_harness();
        assert_eq!(scopes(&grid), GRID);
        for name in [
            OPEN_CARD,
            OPEN_SESSION,
            STEP_CARD,
            CURSOR_MOVE,
            COMPOSE_CARD,
            FOCUS_FILTER,
            OPEN_REVIEW_QUEUE,
            CLEAR,
        ] {
            assert!(available(&grid, name), "{name} on the grid");
        }
        for name in [SCROLL, STEP_FILE, COMMENT, BACK, "headway_nope"] {
            assert!(!available(&grid, name), "{name} on the grid");
        }

        // No cursor: no card to act on and nothing to clear, but `n` and the
        // motions still land one.
        let bare = keys_harness(None);
        for name in [OPEN_CARD, OPEN_EXPLAINER, CLEAR] {
            assert!(!available(&bare, name), "{name} with no cursor");
        }
        assert!(available(&bare, STEP_CARD));
        assert!(available(&bare, CURSOR_MOVE));

        // A card whose record has no session: `s` still takes the key, to
        // say so.
        let mut no_session = keys_harness(None);
        no_session.state_mut().state.set_cursor(id(1));
        no_session.run();
        assert!(available(&no_session, OPEN_EXPLAINER));
        assert!(available(&no_session, OPEN_SESSION));

        let queue = queue_harness();
        assert_eq!(
            scopes(&queue),
            [
                "headway.card",
                "headway.scroll",
                "headway.diff",
                "headway.queue",
                "headway"
            ]
        );
        for name in [
            OPEN_CARD,
            OPEN_SESSION,
            SCROLL,
            STEP_FILE,
            COMMENT,
            SEND_COMMENTS,
            BACK,
        ] {
            assert!(available(&queue, name), "{name} in the queue");
        }
        for name in [CURSOR_MOVE, COMPOSE_CARD, OPEN_REVIEW_QUEUE, CLEAR] {
            assert!(!available(&queue, name), "{name} in the queue");
        }

        let pane = pane_harness();
        assert_eq!(
            scopes(&pane),
            [
                "headway.card",
                "headway.scroll",
                "headway.diff",
                "headway.pane",
                "headway"
            ]
        );
        assert!(available(&pane, STEP_FILE));
        assert!(!available(&pane, OPEN_REVIEW_QUEUE));

        let detail = detail_harness(None);
        assert_eq!(
            scopes(&detail),
            [
                "headway.card",
                "headway.scroll",
                "headway.detail",
                "headway"
            ]
        );
        for name in [
            OPEN_EXPLAINER,
            OPEN_SESSION,
            SCROLL,
            OPEN_REVIEW_QUEUE,
            BACK,
        ] {
            assert!(available(&detail, name), "{name} in the detail");
        }
        for name in [OPEN_CARD, STEP_FILE, COMMENT, CURSOR_MOVE, CLEAR] {
            assert!(!available(&detail, name), "{name} in the detail");
        }

        // The `X` composer's keys are its field's; an open editor or the
        // graph takes no keys either.
        let mut rejecting = grid_harness();
        press_with(&mut rejecting, Modifiers::SHIFT, Key::X);
        assert!(rejecting.state().state.rejecting());
        assert!(scopes(&rejecting).is_empty());
        assert!(!available(&rejecting, OPEN_CARD));

        let mut editing = grid_harness();
        press(&mut editing, Key::C);
        assert!(scopes(&editing).is_empty());
        assert!(!available(&editing, CURSOR_MOVE));

        let mut graph = grid_harness();
        graph.state_mut().state.set_graph_epic(Some(id(5)));
        assert!(scopes(&graph).is_empty());
        assert!(!available(&graph, BACK));
    }
}
