//! The board grid's vim-style **bare-key keymap**: `j`/`k`/`h`/`l` walk the
//! card cursor, `gg`/`G` jump to the ends of its column, `Enter`/`o` open the
//! cursor card, `a` archives it, `n` opens the add-card composer, `/` focuses
//! the filter, `?` toggles the which-key strip and `Esc` drops the cursor.
//! Shifted, `H`/`L`
//! move the cursor card to the neighbouring column and `J`/`K` reorder it
//! within its own, and `R` opens the review queue over the In Review column.
//!
//! The review queue has its own keymap, [`queue_keys`]: `n`/`]` and `p`/`[`
//! step cards; `j`/`k`, `Ctrl-d`/`Ctrl-u`, `gg`/`G` and `J`/`K` scroll the
//! diff by a line, half a page, to its ends and by file; `o` opens the
//! explainer and `Enter` the card; `a` opens the record's agentium session and
//! `A` opens it asking for a `/code-review` of its work (both also in a plain
//! review pane, [`review_keys`]); `D` moves the card to Done and `X` asks for
//! a reason and sends it back to In Progress; `?` toggles its which-key strip
//! ([`QUEUE_HINTS`]) and `q`/`Esc` leave it for the grid.
//!
//! The chord mechanics (reading the press, timing out a pending `g`, swallowing
//! handled keys) are [`notedeck_ui::chord`]'s; the grid math is
//! [`crate::cursor`]'s. This module is only the mapping between them, plus the
//! which-key strips ([`BOARD_HINTS`], [`QUEUE_HINTS`], [`key_hints_ui`]) that
//! document it. Both run once per frame from [`crate::ui::board_ui`], so they
//! allocate nothing of their own (bar the comment an `X` posts).

use egui::{Key, Modifiers};
use notedeck::ColorTheme;
use notedeck::tokens::SPACING_MD;
use notedeck_ui::chord::{self, KeyPress};
use notedeck_ui::diff::PatchScroll;
use notedeck_ui::keybind_hint::KeybindHint;

use crate::cursor::{self, CursorMove, Side, Vertical};
use crate::event::BoardView;
use crate::store::BoardAction;
use crate::ui::{
    BoardUiState, QueuePending, QueueStep, SessionOpen, ViewFilter, filter_field_id,
    reason_field_id,
};

/// Chord steps the board grid can be waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardPending {
    /// `g` was pressed; a second `g` jumps to the top of the column.
    G,
}

/// One keycap group in the which-key strip: keys that share a job, and the job.
#[derive(Debug)]
pub(crate) struct KeyHint {
    /// The group's keycaps, as drawn. A chord (`gg`) is one keycap; a capital
    /// is the shifted key.
    pub keys: &'static [&'static str],
    /// What the group does, drawn muted after its keycaps.
    pub label: &'static str,
}

/// The full which-key strip, shown while `?` has it pinned open. Every key here
/// is replayed through [`board_keys`] by the `every_hint_does_what_it_says`
/// test, so the strip can't drift from the keymap.
pub(crate) const BOARD_HINTS: &[KeyHint] = &[
    KeyHint {
        keys: &["j", "k"],
        label: "up/down",
    },
    KeyHint {
        keys: &["h", "l"],
        label: "columns",
    },
    KeyHint {
        keys: &["gg", "G"],
        label: "first/last",
    },
    KeyHint {
        keys: &["\u{21B5}", "o"],
        label: "open",
    },
    KeyHint {
        keys: &["H", "L"],
        label: "move card",
    },
    KeyHint {
        keys: &["J", "K"],
        label: "reorder",
    },
    KeyHint {
        keys: &["n"],
        label: "new",
    },
    KeyHint {
        keys: &["a"],
        label: "archive",
    },
    KeyHint {
        keys: &["R"],
        label: "review queue",
    },
    KeyHint {
        keys: &["/"],
        label: "filter",
    },
    KeyHint {
        keys: &["esc"],
        label: "clear",
    },
];

/// The strip while a `g` is pending: only the key that completes the chord.
pub(crate) const G_HINTS: &[KeyHint] = &[KeyHint {
    keys: &["g"],
    label: "first card",
}];

/// The review queue's which-key strip, shown while `?` has it pinned open.
/// Replayed through [`queue_keys`] by the `every_queue_hint_does_what_it_says`
/// test, as [`BOARD_HINTS`] is through [`board_keys`]. `^d` is Ctrl+D.
pub(crate) const QUEUE_HINTS: &[KeyHint] = &[
    KeyHint {
        keys: &["n", "p"],
        label: "next/prev card",
    },
    KeyHint {
        keys: &["j", "k"],
        label: "scroll",
    },
    KeyHint {
        keys: &["^d", "^u"],
        label: "half page",
    },
    KeyHint {
        keys: &["gg", "G"],
        label: "top/bottom",
    },
    KeyHint {
        keys: &["J", "K"],
        label: "next/prev file",
    },
    KeyHint {
        keys: &["o"],
        label: "explainer",
    },
    KeyHint {
        keys: &["\u{21B5}"],
        label: "open card",
    },
    KeyHint {
        keys: &["D"],
        label: "done",
    },
    KeyHint {
        keys: &["X"],
        label: "send back",
    },
    KeyHint {
        keys: &["a", "A"],
        label: "session/review",
    },
    KeyHint {
        keys: &["q", "esc"],
        label: "leave",
    },
];

/// The queue's strip while a `g` is pending.
pub(crate) const QUEUE_G_HINTS: &[KeyHint] = &[KeyHint {
    keys: &["g"],
    label: "top of diff",
}];

/// Height of a keycap in the strip.
const KEYCAP: f32 = 18.0;

/// Extra keycap width per character past the first, so `gg` and `esc` fit.
const KEYCAP_PER_CHAR: f32 = 8.0;

/// The hints the strip shows this frame, or `None` to leave it out: the `g`
/// continuation while a `g` is pending, else the full strip if `?` pinned it.
/// Read after [`board_keys`], so it reflects this frame's key.
pub(crate) fn key_hints(state: &BoardUiState) -> Option<&'static [KeyHint]> {
    if state.chord.pending() == Some(BoardPending::G) {
        return Some(G_HINTS);
    }
    state.key_hints_shown().then_some(BOARD_HINTS)
}

/// The review queue's counterpart to [`key_hints`]: its `g` continuation
/// while one is pending, else [`QUEUE_HINTS`] if `?` pinned the strip (the pin
/// is shared with the grid's).
pub(crate) fn queue_key_hints(state: &BoardUiState) -> Option<&'static [KeyHint]> {
    if state.queue_chord.pending() == Some(QueuePending::G) {
        return Some(QUEUE_G_HINTS);
    }
    state.key_hints_shown().then_some(QUEUE_HINTS)
}

/// Width of the keycap for `key`: [`KEYCAP`] square, widened by
/// [`KEYCAP_PER_CHAR`] for each character past the first.
fn keycap_width(key: &str) -> f32 {
    let extra_chars = key.chars().count().saturating_sub(1) as f32;
    KEYCAP + extra_chars * KEYCAP_PER_CHAR
}

/// Width `hint`'s group takes in a row: its keycaps and a label `label_width`
/// wide, with an `item_gap` after each keycap. The measure [`key_hints_ui`]
/// wraps on; it lays the group out from [`keycap_width`] too, so the two agree.
fn hint_group_width(hint: &KeyHint, label_width: f32, item_gap: f32) -> f32 {
    let caps: f32 = hint.keys.iter().map(|key| keycap_width(key)).sum();
    caps + item_gap * hint.keys.len() as f32 + label_width
}

/// Draw `hints` as rows of keycap groups, each keycap run followed by its muted
/// label, [`SPACING_MD`] apart. A group is measured before it is placed and
/// starts a new row if it won't fit what's left of this one, so a narrow pane
/// wraps whole groups and never splits a keycap from its label. (egui can't do
/// this by itself: it places a nested `horizontal` before knowing its width,
/// so the group would run off the right edge instead.) Walks the static table
/// and lays each label out once.
pub(crate) fn key_hints_ui(ui: &mut egui::Ui, theme: &ColorTheme, hints: &'static [KeyHint]) {
    ui.horizontal_wrapped(|ui| {
        let item_gap = ui.spacing().item_spacing.x;
        for (i, hint) in hints.iter().enumerate() {
            let label =
                egui::WidgetText::from(egui::RichText::new(hint.label).color(theme.text_muted))
                    .into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        egui::TextStyle::Body,
                    );
            let width = hint_group_width(hint, label.size().x, item_gap);
            if i > 0 {
                if SPACING_MD + item_gap + width > ui.available_size_before_wrap().x {
                    ui.end_row();
                } else {
                    ui.add_space(SPACING_MD);
                }
            }
            ui.horizontal(|ui| {
                for &key in hint.keys {
                    KeybindHint::new(key)
                        .size(KEYCAP)
                        .width(keycap_width(key))
                        .show(ui);
                }
                ui.label(label);
            });
        }
    });
}

/// Read this frame's bare key press and apply it to the grid. Runs before the
/// grid lays out. Returns a board edit for the app to apply (a keyboard card
/// move or archive); navigation mutates `state` directly.
///
/// Keys are left alone — and any pending chord dropped — while something else
/// owns the keyboard: a focused text field, an open menu or popup, an inline
/// editor or the archived sheet, or a card drag. Presses with Ctrl/Alt/Cmd fall
/// through to app and chrome shortcuts.
pub(crate) fn board_keys(
    ctx: &egui::Context,
    view: &BoardView,
    filter: &ViewFilter,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let pending = state.chord.tick(ctx);
    // Taken unconditionally so the latch always reflects just last frame.
    let menu_open = state.take_grid_menu_open();

    if keyboard_taken(ctx, state, menu_open) {
        state.chord.clear();
        return None;
    }

    let press = ctx.input(chord::first_key_press)?;
    if !press.is_bare() {
        state.chord.clear();
        return None;
    }

    if press.key == Key::Escape {
        escape(ctx, state, pending);
        return None;
    }

    // With a `g` pending, only a second `g` means anything; every other key
    // just cancels the chord.
    if pending.is_some() {
        state.chord.clear();
        if is_key(press, Key::G) {
            move_cursor(view, filter, state, CursorMove::First);
        }
        chord::swallow_key_events(ctx);
        return None;
    }

    let mut action = None;
    match (press.key, press.modifiers.shift) {
        (Key::J, false) => move_cursor(view, filter, state, CursorMove::Down),
        (Key::K, false) => move_cursor(view, filter, state, CursorMove::Up),
        (Key::H, false) => move_cursor(view, filter, state, CursorMove::Left),
        (Key::L, false) => move_cursor(view, filter, state, CursorMove::Right),
        (Key::G, false) => {
            let now = ctx.input(|i| i.time);
            state.chord.begin(BoardPending::G, now);
        }
        (Key::G, true) => move_cursor(view, filter, state, CursorMove::Last),
        (Key::Enter, _) | (Key::O, false) => open_cursor_card(view, filter, state),
        (Key::N, false) => add_card_at_cursor(view, filter, state),
        (Key::A, false) => action = archive_cursor_card(view, filter, state),
        (Key::Slash, false) => ctx.memory_mut(|m| m.request_focus(filter_field_id())),
        // `?` is Shift+/: a logical `Questionmark` from most layouts, or the
        // physical slash with Shift from the rest.
        (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
        (Key::H, true) => action = move_card(view, filter, state, CardMove::Across(Side::Left)),
        (Key::L, true) => action = move_card(view, filter, state, CardMove::Across(Side::Right)),
        (Key::J, true) => action = move_card(view, filter, state, CardMove::Within(Vertical::Down)),
        (Key::K, true) => action = move_card(view, filter, state, CardMove::Within(Vertical::Up)),
        (Key::R, true) => state.open_review_queue(view, ctx.input(|i| i.time)),
        _ => return None,
    }

    // Load-bearing for `/`: the filter field lays out focused later this frame
    // and would otherwise type the slash. (`n`'s composer only grabs focus after
    // its first layout, so it happens to be safe, but shouldn't depend on it.)
    chord::swallow_key_events(ctx);
    action
}

/// Read this frame's key press and apply it to the open review queue (see the
/// module docs for the map). Runs before anything lays out, so a handled key
/// is swallowed before the review pane (whose own Esc would only close the
/// review) or the reason composer sees it. Returns a verdict's board edit:
/// `D`'s move, or the comment an `X` posts once its reason is entered (its
/// move follows next frame, see [`BoardUiState::take_follow_up`]).
///
/// While the `X` composer is open only its Enter and Esc are read. Otherwise
/// keys are left alone under the same rules as the grid's, bar the grid's own
/// overlays: a focused widget, an open popup or menu, or a drag.
pub(crate) fn queue_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let pending = state.queue_chord.tick(ctx);
    let now = ctx.input(|i| i.time);
    if state.rejecting() {
        state.queue_chord.clear();
        return reason_keys(ctx, view, state, now);
    }
    if focus_taken(ctx) {
        state.queue_chord.clear();
        return None;
    }
    let press = ctx.input(chord::first_key_press)?;

    // Ctrl-d/Ctrl-u, vi's half-page scroll. Safe to take here: chrome's only
    // Ctrl binding is Ctrl+Tab.
    let m = press.modifiers;
    if m.ctrl && !m.alt && !m.shift {
        let pages = match press.key {
            Key::D => 0.5,
            Key::U => -0.5,
            _ => return None,
        };
        state.queue_chord.clear();
        state.scroll_review(PatchScroll::Pages(pages));
        chord::swallow_key_events(ctx);
        return None;
    }
    if !press.is_bare() {
        state.queue_chord.clear();
        return None;
    }

    // With a `g` pending, only a second `g` means anything.
    if pending.is_some() {
        state.queue_chord.clear();
        if is_key(press, Key::G) {
            state.scroll_review(PatchScroll::Top);
        }
        chord::swallow_key_events(ctx);
        return None;
    }

    let mut action = None;
    match (press.key, press.modifiers.shift) {
        (Key::N | Key::CloseBracket, false) => state.step_queue(QueueStep::Next),
        (Key::P | Key::OpenBracket, false) => state.step_queue(QueueStep::Prev),
        (Key::J, false) => state.scroll_review(PatchScroll::Rows(1)),
        (Key::K, false) => state.scroll_review(PatchScroll::Rows(-1)),
        (Key::G, false) => state.queue_chord.begin(QueuePending::G, now),
        (Key::G, true) => state.scroll_review(PatchScroll::Bottom),
        (Key::J, true) => state.scroll_review(PatchScroll::NextFile),
        (Key::K, true) => state.scroll_review(PatchScroll::PrevFile),
        (Key::O, false) => state.open_explainer(ctx, view),
        (Key::Enter, _) => state.open_queue_card(),
        (Key::A, shift) => state.open_record_session(view, session_open_kind(shift), now),
        (Key::D, true) => action = state.accept_queue_card(view, now),
        (Key::X, true) => state.start_reject(view, now),
        (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
        (Key::Q, false) | (Key::Escape, _) => state.close_queue(),
        _ => return None,
    }
    // Load-bearing for `X`: the composer takes focus as it lays out this
    // frame and would otherwise type the X.
    chord::swallow_key_events(ctx);
    action
}

/// A plain review pane's keys (one opened from a card's detail, not the
/// queue): `a` and `A`, as in [`queue_keys`]. Runs before the pane lays out,
/// under the same focus rules; the pane's Esc is its own.
pub(crate) fn review_keys(ctx: &egui::Context, view: &BoardView, state: &mut BoardUiState) {
    if focus_taken(ctx) {
        return;
    }
    let Some(press) = ctx.input(chord::first_key_press) else {
        return;
    };
    if press.key != Key::A || !press.is_bare() {
        return;
    }
    let now = ctx.input(|i| i.time);
    state.open_record_session(view, session_open_kind(press.modifiers.shift), now);
    chord::swallow_key_events(ctx);
}

/// What an `a` asks of the record's session: `A` (Shift) a code review.
fn session_open_kind(shift: bool) -> SessionOpen {
    if shift {
        SessionOpen::CodeReview
    } else {
        SessionOpen::Plain
    }
}

/// The `X` composer's keys: Enter posts the reason (an empty one does
/// nothing), Esc cancels. Both are consumed before the field sees them, and
/// the field's focus goes with the composer.
fn reason_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
    now: f64,
) -> Option<BoardAction> {
    let action = if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        state.cancel_reject();
        None
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        state.submit_reject(view, now)
    } else {
        return None;
    };
    if !state.rejecting() {
        ctx.memory_mut(|m| m.surrender_focus(reason_field_id()));
    }
    action
}

/// Whether something other than the grid owns the keyboard this frame.
fn keyboard_taken(ctx: &egui::Context, state: &BoardUiState, menu_open: bool) -> bool {
    // The grid's `menu_button`s report through `menu_open`, which egui's popup
    // memory can't see (see `focus_taken`).
    focus_taken(ctx) || menu_open || state.keys_blocked()
}

/// Whether a widget, popup, menu or drag holds the keyboard this frame: the
/// part of [`keyboard_taken`] that isn't the grid's own state, shared with
/// [`queue_keys`].
fn focus_taken(ctx: &egui::Context) -> bool {
    // Read before any widget runs this frame, so this is the focus the key
    // press was typed into.
    ctx.memory(|m| m.focused().is_some() || m.any_popup_open())
        // `Memory::any_popup_open` sees combo boxes but not egui 0.31's menus:
        // right-click context menus have their own check.
        || ctx.is_context_menu_open()
        || egui::DragAndDrop::has_any_payload(ctx)
        || ctx.dragged_id().is_some()
}

/// A bare, unshifted `key`.
fn is_key(press: KeyPress, key: Key) -> bool {
    press.key == key && !press.modifiers.shift
}

/// Step the cursor, scrolling its new card into view.
fn move_cursor(view: &BoardView, filter: &ViewFilter, state: &mut BoardUiState, mv: CursorMove) {
    if let Some(id) = cursor::step(view, filter, state.cursor(), mv) {
        state.set_cursor(id);
    }
}

/// A keyboard card move: Shift+H/L across columns, Shift+J/K within one.
#[derive(Clone, Copy, Debug)]
enum CardMove {
    /// To the neighbouring column ([`cursor::move_across`]).
    Across(Side),
    /// Past the neighbouring visible card ([`cursor::move_within`]).
    Within(Vertical),
}

/// A [`BoardAction::MoveCard`] taking the cursor card where `mv` drops it, or
/// `None` without a visible cursor card or at an edge.
///
/// The cursor keeps following the card by id, so once the async ingest folds
/// the move in, its ring (and the scroll this requests) land on the card's new
/// slot. The move isn't flagged in `suppress_anim` the way a drop is: nothing
/// carried the card there, so the slide shows where it went.
///
/// A second press before that fold lands recomputes from the stale view and
/// re-emits the same target. That's harmless — the repeat move ranks the card
/// into the same neighbourhood — so there's no pending-move tracking.
fn move_card(
    view: &BoardView,
    filter: &ViewFilter,
    state: &mut BoardUiState,
    mv: CardMove,
) -> Option<BoardAction> {
    let card = state.cursor()?;
    let (to_col, to_row) = match mv {
        CardMove::Across(side) => cursor::move_across(view, filter, card, side),
        CardMove::Within(dir) => cursor::move_within(view, filter, card, dir),
    }?;
    // Re-request the scroll, so the card stays in view as it lands.
    state.set_cursor(card);
    Some(BoardAction::MoveCard {
        card,
        to_col,
        to_row,
    })
}

/// Open the cursor card's detail, if the cursor is on a visible card. The app's
/// post-render nav diff turns the selection into a global-history push, exactly
/// as a click does.
fn open_cursor_card(view: &BoardView, filter: &ViewFilter, state: &mut BoardUiState) {
    let Some(id) = state
        .cursor()
        .filter(|&c| cursor::locate(view, filter, c).is_some())
    else {
        return;
    };
    state.open_card(id);
}

/// A [`BoardAction::ArchiveCard`] for the cursor card, or `None` without a
/// visible cursor card. Archiving is recoverable from the archived sheet, so it
/// takes no confirmation.
///
/// The cursor steps off the card first — to the next card down, else the one
/// above, else nowhere — so repeated `a` presses triage a column top to bottom
/// instead of landing back on the first card once the archive folds in.
fn archive_cursor_card(
    view: &BoardView,
    filter: &ViewFilter,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let card = state
        .cursor()
        .filter(|&c| cursor::locate(view, filter, c).is_some())?;
    // `step` clamps at a column's end by returning the card itself.
    let neighbour = [CursorMove::Down, CursorMove::Up]
        .into_iter()
        .filter_map(|mv| cursor::step(view, filter, Some(card), mv))
        .find(|&id| id != card);
    match neighbour {
        Some(id) => state.set_cursor(id),
        None => state.clear_cursor(),
    }
    Some(BoardAction::ArchiveCard { card })
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

/// Esc drops the cursor, any pending chord and the which-key strip. It's
/// consumed only when there was one to drop, so a bare Esc still reaches chrome
/// (which toggles the side menu).
fn escape(ctx: &egui::Context, state: &mut BoardUiState, pending: Option<BoardPending>) {
    if state.cursor().is_none() && pending.is_none() && !state.key_hints_shown() {
        return;
    }
    state.clear_cursor();
    state.hide_key_hints();
    state.chord.clear();
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::tests::{grid, id, square_grid};
    use crate::ui::{CardFilter, QueueNotice};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use headway::event::{ReviewFields, ReviewView};
    use nostrdb_net::NoteId;

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
        /// The last agentium session open a key raised, as `board_ui` takes
        /// it ([`BoardUiState::take_open`]).
        session: Option<notedeck::OpenUri>,
    }

    /// A harness that runs [`board_keys`] over [`grid`] each frame, unfiltered.
    fn keys_harness(field: Option<egui::Id>) -> Harness<'static, KeysHarness> {
        let mut harness = Harness::new_ui_state(
            |ui, h: &mut KeysHarness| {
                let parsed = CardFilter::parse("", &h.view.id);
                let filter = ViewFilter {
                    filter: &parsed,
                    hide_subissues: false,
                };
                // As `board_ui` does: the queue's keys while it's open, a
                // plain review pane's while one is, the grid's otherwise, then
                // any follow-up an earlier frame left.
                let verdict = if h.state.queue_open() {
                    queue_keys(ui.ctx(), &h.view, &mut h.state)
                } else {
                    None
                };
                let action = if h.state.queue_open() || verdict.is_some() {
                    verdict
                } else if h.state.review_card().is_some() {
                    review_keys(ui.ctx(), &h.view, &mut h.state);
                    None
                } else {
                    board_keys(ui.ctx(), &h.view, &filter, &mut h.state)
                };
                if let Some(open) = h.state.take_open() {
                    h.session = Some(open);
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
                    ui.add(egui::TextEdit::singleline(&mut h.text).id(field));
                }
                let hints = if h.state.queue_open() {
                    queue_key_hints(&h.state)
                } else {
                    key_hints(&h.state)
                };
                if let Some(hints) = hints {
                    key_hints_ui(ui, &ColorTheme::current(ui.ctx()), hints);
                }
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

    #[test]
    fn gg_goes_first_and_shift_g_goes_last() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));

        press_with(&mut harness, Modifiers::SHIFT, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)));

        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)), "one g waits");
        assert_eq!(harness.state().state.chord.pending(), Some(BoardPending::G));
        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        assert_eq!(harness.state().state.chord.pending(), None);
    }

    #[test]
    fn g_then_another_key_cancels_without_moving() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(1));

        press(&mut harness, Key::G);
        press(&mut harness, Key::J);
        assert_eq!(harness.state().state.cursor(), Some(id(1)));
        assert_eq!(harness.state().state.chord.pending(), None);
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
        assert_eq!(harness.state().state.chord.pending(), None);

        // So the next `g` starts a fresh chord instead of completing one.
        press(&mut harness, Key::G);
        assert_eq!(harness.state().state.cursor(), Some(id(3)));
        assert_eq!(harness.state().state.chord.pending(), Some(BoardPending::G));
    }

    #[test]
    fn ctrl_j_falls_through() {
        let mut harness = keys_harness(None);
        press_with(&mut harness, Modifiers::CTRL, Key::J);
        assert_eq!(harness.state().state.cursor(), None);
    }

    #[test]
    fn a_focused_text_field_keeps_its_keys() {
        let field = egui::Id::new("keys_test_field");
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

        harness.state_mut().state.set_cursor(id(2));
        // Row 1 of `a` into `b`, which holds one card: its end.
        press_with(&mut harness, Modifiers::SHIFT, Key::L);
        assert_eq!(harness.state().moved, Some((id(2), 1, 1)));
        press_with(&mut harness, Modifiers::SHIFT, Key::J);
        assert_eq!(harness.state().moved, Some((id(2), 0, 3)));
        press_with(&mut harness, Modifiers::SHIFT, Key::K);
        assert_eq!(harness.state().moved, Some((id(2), 0, 0)));

        // The grid view isn't refolded here, so the cursor card is still in
        // `a`, the leftmost column: Shift+H is a no-op.
        harness.state_mut().moved = None;
        press_with(&mut harness, Modifiers::SHIFT, Key::H);
        assert_eq!(harness.state().moved, None);
        assert_eq!(harness.state().state.cursor(), Some(id(2)), "cursor kept");
    }

    /// Everything a board key can visibly do, for [`every_hint_does_what_it_says`]
    /// to compare before and after.
    #[derive(Debug, PartialEq)]
    struct Effects {
        cursor: Option<NoteId>,
        selected: Option<NoteId>,
        /// An inline editor (the `n` composer) is open.
        editing: bool,
        focused: Option<egui::Id>,
        moved: Option<(NoteId, usize, usize)>,
        archived: Option<NoteId>,
        /// The review queue is open, or said there was nothing to review.
        reviewing: (bool, bool),
    }

    fn effects(harness: &Harness<'static, KeysHarness>) -> Effects {
        let h = harness.state();
        Effects {
            cursor: h.state.cursor(),
            selected: h.state.selected(),
            editing: h.state.keys_blocked(),
            focused: harness.ctx.memory(|m| m.focused()),
            moved: h.moved,
            archived: h.archived,
            reviewing: (h.state.queue_open(), h.state.notice().is_some()),
        }
    }

    /// The presses a keycap stands for: `gg` is two, a capital is Shift plus the
    /// letter, and the named keycaps are their keys.
    fn keycap_presses(cap: &str) -> Vec<(Modifiers, Key)> {
        match cap {
            "\u{21B5}" => return vec![(Modifiers::NONE, Key::Enter)],
            "esc" => return vec![(Modifiers::NONE, Key::Escape)],
            _ => {}
        }
        if let Some(letter) = cap.strip_prefix('^') {
            let key = Key::from_name(letter).unwrap_or_else(|| panic!("keycap {cap:?}"));
            return vec![(Modifiers::CTRL, key)];
        }
        cap.chars()
            .map(|c| {
                let key = Key::from_name(&c.to_string())
                    .unwrap_or_else(|| panic!("keycap {cap:?}: no key for {c:?}"));
                let modifiers = if c.is_ascii_uppercase() {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                (modifiers, key)
            })
            .collect()
    }

    /// Replay every keycap in [`BOARD_HINTS`] from the middle of a 3×3 board
    /// and check it did *something*, so the strip can't advertise a key the
    /// keymap dropped or misspelled.
    #[test]
    fn every_hint_does_what_it_says() {
        for hint in BOARD_HINTS {
            for &cap in hint.keys {
                // The filter field is laid out so `/`'s focus request has a
                // widget to land on.
                let mut harness = keys_harness(Some(filter_field_id()));
                harness.state_mut().view = square_grid();
                harness.state_mut().state.set_cursor(id(5));
                harness.run();
                let before = effects(&harness);

                for (modifiers, key) in keycap_presses(cap) {
                    press_with(&mut harness, modifiers, key);
                }
                assert_ne!(
                    effects(&harness),
                    before,
                    "keycap {cap:?} ({}) did nothing",
                    hint.label
                );
            }
        }
    }

    #[test]
    fn question_mark_toggles_the_strip() {
        let mut harness = keys_harness(None);
        assert!(!harness.state().state.key_hints_shown());
        assert!(harness.query_by_label("up/down").is_none());

        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        assert!(harness.state().state.key_hints_shown());
        assert!(harness.query_by_label("up/down").is_some());

        // The other way a layout can report `?`.
        press_with(&mut harness, Modifiers::SHIFT, Key::Slash);
        assert!(!harness.state().state.key_hints_shown());
        assert!(harness.query_by_label("up/down").is_none());

        // Esc closes it too, and eats itself doing so.
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        press(&mut harness, Key::Escape);
        assert!(!harness.state().state.key_hints_shown());
        assert!(!harness.state().esc_left, "Esc consumed");
    }

    #[test]
    fn a_pending_g_shows_only_its_continuation() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));

        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first card").is_some());
        assert!(harness.query_by_label("up/down").is_none());

        // Even over a pinned strip, the g continuation wins while it's pending.
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first card").is_none());
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first card").is_some());
        assert!(harness.query_by_label("up/down").is_none());

        // Once the chord completes, the pinned strip is back.
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("first card").is_none());
        assert!(harness.query_by_label("up/down").is_some());
    }

    #[test]
    fn a_archives_the_cursor_card_and_steps_off_it() {
        let mut harness = keys_harness(None);
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, None, "no cursor, no archive");

        // Mid-column: the cursor moves down to the next card.
        harness.state_mut().state.set_cursor(id(1));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(1)));
        assert_eq!(harness.state().state.cursor(), Some(id(2)));

        // Column end: it moves up instead.
        harness.state_mut().state.set_cursor(id(3));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(3)));
        assert_eq!(harness.state().state.cursor(), Some(id(2)));

        // A column's only card: the cursor is dropped.
        harness.state_mut().state.set_cursor(id(4));
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(4)));
        assert_eq!(harness.state().state.cursor(), None);
    }

    #[test]
    fn n_opens_the_composer_without_archiving() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));
        press(&mut harness, Key::N);
        assert!(harness.state().state.keys_blocked(), "composer open");
        assert_eq!(harness.state().archived, None);
    }

    /// `R` opens the queue over the In Review column at its first card; `n`/`]`
    /// and `p`/`[` step it, stopping at the ends; grid keys stand down while
    /// it's open; and `q` leaves it with the grid cursor on the card it showed.
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
        press(&mut harness, Key::CloseBracket);
        assert_eq!(reviewing(&harness), Some(id(6)), "stops at the end");
        press(&mut harness, Key::P);
        assert_eq!(reviewing(&harness), Some(id(5)));
        press(&mut harness, Key::OpenBracket);
        assert_eq!(reviewing(&harness), Some(id(5)), "stops at the start");
        press(&mut harness, Key::CloseBracket);
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

    /// Everything a queue key can visibly do, for
    /// [`every_queue_hint_does_what_it_says`] to compare before and after.
    #[derive(Debug, PartialEq)]
    struct QueueEffects {
        open: bool,
        card: Option<NoteId>,
        selected: Option<NoteId>,
        scroll: Option<PatchScroll>,
        moved: Option<(NoteId, usize, usize)>,
        commented: Option<(NoteId, String)>,
        rejecting: bool,
        notice: Option<QueueNotice>,
        opened: Option<String>,
        session: Option<notedeck::OpenUri>,
        hints: bool,
    }

    fn queue_effects(harness: &Harness<'static, KeysHarness>) -> QueueEffects {
        let h = harness.state();
        QueueEffects {
            open: h.state.queue_open(),
            card: h.state.review_card(),
            selected: h.state.selected(),
            scroll: h.state.review_scroll(),
            moved: h.moved,
            commented: h.commented.clone(),
            rejecting: h.state.rejecting(),
            notice: h.state.notice(),
            opened: h.opened.clone(),
            session: h.session.clone(),
            hints: h.state.key_hints_shown(),
        }
    }

    /// Replay every keycap in [`QUEUE_HINTS`] from the middle of a three-card
    /// queue and check it did *something*, so the queue's strip can't
    /// advertise a key its keymap dropped.
    #[test]
    fn every_queue_hint_does_what_it_says() {
        for hint in QUEUE_HINTS.iter().chain(EXTRA_QUEUE_KEYS) {
            for &cap in hint.keys {
                let mut harness = queue_harness();
                let before = queue_effects(&harness);
                for (modifiers, key) in keycap_presses(cap) {
                    press_with(&mut harness, modifiers, key);
                }
                assert_ne!(
                    queue_effects(&harness),
                    before,
                    "keycap {cap:?} ({}) did nothing",
                    hint.label
                );
            }
        }
    }

    /// `?` isn't in the strip it toggles; replay it alongside.
    const EXTRA_QUEUE_KEYS: &[KeyHint] = &[KeyHint {
        keys: &["?"],
        label: "hints",
    }];

    /// The scroll keys each ask the diff for their scroll; `J` the next file.
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
        press_with(&mut harness, Modifiers::SHIFT, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Bottom));
        press(&mut harness, Key::G);
        assert!(harness.query_by_label("top of diff").is_some(), "g hint up");
        press(&mut harness, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Top));
        press_with(&mut harness, Modifiers::SHIFT, Key::J);
        assert_eq!(scroll(&harness), Some(PatchScroll::NextFile));
        press_with(&mut harness, Modifiers::SHIFT, Key::K);
        assert_eq!(scroll(&harness), Some(PatchScroll::PrevFile));
        assert_eq!(harness.state().state.review_card(), Some(id(5)), "no step");
    }

    /// `D` moves the card to the end of Done and steps on; on the last card
    /// it leaves the queue saying it's done.
    #[test]
    fn d_moves_the_card_to_done_and_advances() {
        let mut harness = queue_harness();
        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(5), 2, 3)));
        assert_eq!(harness.state().state.review_card(), Some(id(6)));

        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(6), 2, 3)));
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::QueueDone));
        assert_eq!(harness.state().state.cursor(), Some(id(6)));
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
        assert!(!harness.state().state.rejecting());
        assert_eq!(harness.state().state.review_card(), Some(id(6)));
        harness.step();
        assert_eq!(harness.state().moved, Some((id(5), 0, 3)));
        assert_eq!(harness.ctx.memory(|m| m.focused()), None, "focus returned");

        // The queue's keys are back.
        press(&mut harness, Key::P);
        assert_eq!(harness.state().state.review_card(), Some(id(5)));
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

    /// `o` opens the shown record's explainer; on a card without one it only
    /// says so.
    #[test]
    fn o_opens_the_explainer_or_says_there_is_none() {
        let mut harness = queue_harness();
        press(&mut harness, Key::O);
        assert_eq!(
            harness.state().opened.as_deref(),
            Some("https://example.com/explainer")
        );

        press(&mut harness, Key::N);
        harness.state_mut().opened = None;
        press(&mut harness, Key::O);
        assert_eq!(harness.state().opened, None);
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NoExplainer)
        );
        assert!(harness.state().state.queue_open());
    }

    /// Enter leaves the queue for its card's detail, keeping the queue's place
    /// for the back that returns to it.
    #[test]
    fn enter_opens_the_queue_card() {
        let mut harness = queue_harness();
        press(&mut harness, Key::Enter);
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().state.selected(), Some(id(5)));
        assert_eq!(harness.state().state.review_card(), None);

        // Back onto the queue's entry reopens it on the same card.
        harness.state_mut().state.set_selected(None);
        harness.state_mut().state.set_queue_open(true);
        assert_eq!(
            harness.state().state.queue_review().map(|t| t.card),
            Some(id(5))
        );
    }

    /// `a` opens the shown record's session; `A` opens it with a
    /// `/code-review` message naming the commit and the card. Neither leaves
    /// the queue, since the open is a cross-app one.
    #[test]
    fn a_opens_the_record_session_and_shift_a_asks_for_a_review() {
        let mut harness = queue_harness();
        press(&mut harness, Key::A);
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri::new(SESSION))
        );

        harness.state_mut().session = None;
        press_with(&mut harness, Modifiers::SHIFT, Key::A);
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

    /// On a record with no session, `a` and `A` open nothing and say so.
    #[test]
    fn a_without_a_session_only_says_so() {
        let mut harness = queue_harness();
        press(&mut harness, Key::N);
        press(&mut harness, Key::A);
        press_with(&mut harness, Modifiers::SHIFT, Key::A);
        assert_eq!(harness.state().session, None);
        assert_eq!(harness.state().state.notice(), Some(QueueNotice::NoSession));
    }

    /// A plain review pane (opened from a card's detail, not the queue) takes
    /// `a` and `A` too.
    #[test]
    fn a_works_in_a_plain_review_pane() {
        let mut harness = keys_harness(None);
        harness.state_mut().view = review_board();
        harness
            .state_mut()
            .state
            .set_review(Some(crate::nav::ReviewTarget {
                card: id(5),
                record: None,
            }));
        press_with(&mut harness, Modifiers::SHIFT, Key::A);
        let open = harness.state().session.clone().expect("an open");
        assert_eq!(open.reference, SESSION);
        assert!(open.msg.is_some_and(|m| m.contains("commit 136ceb9d3bfa")));
        assert!(!harness.state().state.queue_open());
        assert_eq!(harness.state().archived, None, "not the grid's archive");
    }

    /// A verdict key on a board without its column does nothing but say so.
    #[test]
    fn verdicts_need_their_columns() {
        let mut harness = queue_harness();
        harness.state_mut().view.columns[2].id = "shipped".to_string();
        harness.state_mut().view.columns[2].name = "Shipped".to_string();
        harness.state_mut().view.columns[0].id = "doing".to_string();
        harness.state_mut().view.columns[0].name = "Doing".to_string();

        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, None);
        assert_eq!(
            harness.state().state.notice(),
            Some(QueueNotice::NoDoneColumn)
        );
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
}
