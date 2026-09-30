//! Headway's **bare-key keymaps**, one per view: the board grid
//! ([`board_keys`]), the review pane in the queue or opened from a card
//! ([`review_pane_keys`]), and the card detail ([`detail_keys`]).
//!
//! Keys come in two classes.
//!
//! - **Card actions** act on *the current card* — the grid's cursor card, the
//!   queue's card, a review pane's card, the open detail's card — and mean the
//!   same thing in every view: `Enter`/`o` open it, `e` its explainer, `s`/`S`
//!   its agentium session (`S` asking for a code review), `r` its review, `a`
//!   archive, `D` done, `X` send back with a reason, `n`/`p` next/previous
//!   card. One table ([`CARD_ACTION_HINTS`]), one mapping ([`card_action`]) and
//!   one dispatcher ([`apply_card_action`]), which each view's keymap tries
//!   before its own navigation.
//! - **Navigation** keeps its meaning while its target is whatever the view
//!   shows: `j`/`k` down/up (the grid's cursor; the diff or the detail
//!   scrolls), `gg`/`G` first/last, `Ctrl-d`/`Ctrl-u` half a page, `]`/`[` the
//!   next/previous file of a diff, `q`/`Esc` back out, `?` the which-key strip.
//!   The grid adds `h`/`l` across columns, `H`/`L`/`J`/`K` to move the cursor
//!   card, `c` to create a card, `/` to filter and `R` for the review queue.
//!
//! The chord mechanics (reading the press, timing out a pending `g`, swallowing
//! handled keys) are [`notedeck_ui::chord`]'s; the grid math is
//! [`crate::cursor`]'s. This module is only the mapping between them, plus the
//! which-key strips ([`key_hints_ui`]) that document it. The keymaps run once
//! per frame from [`crate::ui::board_ui`], before anything lays out, so they
//! allocate nothing of their own (bar what a key sends: the comment an `X`
//! posts, a session open).

use egui::{Key, Modifiers};
use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::SPACING_MD;
use notedeck_ui::chord::{self, KeyPress};
use notedeck_ui::diff::PatchScroll;
use notedeck_ui::keybind_hint::KeybindHint;

use crate::cursor::{self, CursorMove, Side, Vertical};
use crate::event::BoardView;
use crate::store::BoardAction;
use crate::ui::{
    BoardUiState, CardStep, SessionOpen, ViewFilter, filter_field_id, find_card, reason_field_id,
};

/// Chord steps the board grid can be waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardPending {
    /// `g` was pressed; a second `g` jumps to the top of the column.
    G,
}

/// Chord steps the review pane and the card detail can be waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanePending {
    /// `g` was pressed; a second `g` scrolls to the top.
    G,
}

/// An action on the current card, the same in every view that has one (see
/// the module docs). Read off a key by [`card_action`], applied by
/// [`apply_card_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardAction {
    /// `Enter`/`o`: open the card's detail. The detail itself has nothing to
    /// open; a plain review pane backs out to it.
    Open,
    /// `e`: open the explainer of the card's record (the one a review pane
    /// shows, else the newest).
    Explainer,
    /// `s`/`S`: open the record's agentium session, `S` asking it for a
    /// `/code-review` of its work.
    Session(SessionOpen),
    /// `r`: open the review pane on the card's newest record.
    Review,
    /// `a`: archive the card.
    Archive,
    /// `D`: move the card to the end of Done.
    Done,
    /// `X`: ask for a reason, post it as a `review:` comment and send the card
    /// back to In Progress.
    SendBack,
    /// `n`/`p`: the next/previous card — the grid's cursor, the queue's card,
    /// or the neighbouring card in the column of a detail or review pane.
    Step(CardStep),
}

/// The [`CardAction`] a press means, if any. Bare presses only; Shift picks
/// the capital.
pub(crate) fn card_action(press: KeyPress) -> Option<CardAction> {
    if !press.is_bare() {
        return None;
    }
    Some(match (press.key, press.modifiers.shift) {
        (Key::Enter, _) | (Key::O, false) => CardAction::Open,
        (Key::E, false) => CardAction::Explainer,
        (Key::S, false) => CardAction::Session(SessionOpen::Plain),
        (Key::S, true) => CardAction::Session(SessionOpen::CodeReview),
        (Key::R, false) => CardAction::Review,
        (Key::A, false) => CardAction::Archive,
        (Key::D, true) => CardAction::Done,
        (Key::X, true) => CardAction::SendBack,
        (Key::N, false) => CardAction::Step(CardStep::Next),
        (Key::P, false) => CardAction::Step(CardStep::Prev),
        _ => return None,
    })
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
/// leaves the queue or a plain review pane for the card's detail; `Archive`
/// steps the grid's cursor off the card, steps the queue on (as `D` does), or
/// backs a detail or plain pane out to the board; `Step` walks the grid's
/// cursor, the queue, or the card's column.
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
        CardAction::Explainer => state.open_explainer(ctx, view, card),
        CardAction::Session(how) => state.open_card_session(view, card, how, now),
        CardAction::Review => state.open_review(card),
        CardAction::Archive => return archive_card(view, state, card, at, now),
        CardAction::Done => return state.accept_card(view, card, now),
        CardAction::SendBack => state.start_reject(view, card, now),
        CardAction::Step(step) => match at {
            ActionView::Grid(filter) => move_cursor(view, filter, state, step_move(step)),
            ActionView::Queue => state.step_queue(step),
            ActionView::Pane => state.step_review(view, card, step),
            ActionView::Detail => state.step_detail(view, card, step),
        },
    }
    None
}

/// `a`: a [`BoardAction::ArchiveCard`] for `card`, and what leaving it takes
/// in view `at` (see [`apply_card_action`]).
fn archive_card(
    view: &BoardView,
    state: &mut BoardUiState,
    card: NoteId,
    at: ActionView<'_>,
    now: f64,
) -> Option<BoardAction> {
    match at {
        ActionView::Grid(filter) => return archive_cursor_card(view, filter, state),
        ActionView::Queue => state.advance_queue(now),
        ActionView::Pane | ActionView::Detail => state.leave_card(),
    }
    Some(BoardAction::ArchiveCard { card })
}

/// The grid cursor move an `n`/`p` makes: `j`'s and `k`'s.
fn step_move(step: CardStep) -> CursorMove {
    match step {
        CardStep::Next => CursorMove::Down,
        CardStep::Prev => CursorMove::Up,
    }
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

/// A which-key strip: hint tables drawn one after another, as one run.
pub(crate) type HintStrip = &'static [&'static [KeyHint]];

/// The [`CardAction`] keys, in every view's strip. The `card_actions_mean_the
/// _same_in_every_view` test replays each keycap in all four views.
pub(crate) const CARD_ACTION_HINTS: &[KeyHint] = &[
    KeyHint {
        keys: &["\u{21B5}", "o"],
        label: "open",
    },
    KeyHint {
        keys: &["e"],
        label: "explainer",
    },
    KeyHint {
        keys: &["s", "S"],
        label: "session/review",
    },
    KeyHint {
        keys: &["r"],
        label: "review diff",
    },
    KeyHint {
        keys: &["a"],
        label: "archive",
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
        keys: &["n", "p"],
        label: "next/prev card",
    },
];

/// The grid's own navigation. Every key here, and in [`CARD_ACTION_HINTS`],
/// is replayed through [`board_keys`] by the `every_hint_does_what_it_says`
/// test, so the strip can't drift from the keymap.
pub(crate) const BOARD_NAV_HINTS: &[KeyHint] = &[
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
        keys: &["H", "L"],
        label: "move card",
    },
    KeyHint {
        keys: &["J", "K"],
        label: "reorder",
    },
    KeyHint {
        keys: &["c"],
        label: "new",
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

/// Scrolling a review pane's diff, in the queue or not. `^f` is Ctrl+F.
pub(crate) const REVIEW_NAV_HINTS: &[KeyHint] = &[
    KeyHint {
        keys: &["j", "k"],
        label: "scroll",
    },
    KeyHint {
        keys: &["space", "^f", "^b"],
        label: "page",
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
        keys: &["]", "["],
        label: "next/prev file",
    },
];

/// Leaving the review queue, for the grid.
pub(crate) const QUEUE_EXIT_HINTS: &[KeyHint] = &[KeyHint {
    keys: &["q", "esc"],
    label: "leave",
}];

/// Leaving a plain review pane, for its card's detail.
pub(crate) const PANE_EXIT_HINTS: &[KeyHint] = &[KeyHint {
    keys: &["q", "esc"],
    label: "back to card",
}];

/// Scrolling the card detail, and leaving it for the grid.
pub(crate) const DETAIL_NAV_HINTS: &[KeyHint] = &[
    KeyHint {
        keys: &["j", "k"],
        label: "scroll",
    },
    KeyHint {
        keys: &["space", "^f", "^b"],
        label: "page",
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
        keys: &["q", "esc"],
        label: "back",
    },
];

/// The grid's strip: its navigation, then the card actions.
pub(crate) const BOARD_STRIP: HintStrip = &[BOARD_NAV_HINTS, CARD_ACTION_HINTS];

/// The review queue's strip.
pub(crate) const QUEUE_STRIP: HintStrip = &[CARD_ACTION_HINTS, REVIEW_NAV_HINTS, QUEUE_EXIT_HINTS];

/// A plain review pane's strip: the queue's, bar how it's left.
pub(crate) const PANE_STRIP: HintStrip = &[CARD_ACTION_HINTS, REVIEW_NAV_HINTS, PANE_EXIT_HINTS];

/// The card detail's strip.
pub(crate) const DETAIL_STRIP: HintStrip = &[CARD_ACTION_HINTS, DETAIL_NAV_HINTS];

/// The grid's strip while a `g` is pending: only the key that completes it.
const G_STRIP: HintStrip = &[&[KeyHint {
    keys: &["g"],
    label: "first card",
}]];

/// A review pane's or the detail's strip while a `g` is pending.
const PANE_G_STRIP: HintStrip = &[&[KeyHint {
    keys: &["g"],
    label: "top",
}]];

/// Height of a keycap in the strip.
const KEYCAP: f32 = 18.0;

/// Extra keycap width per character past the first, so `gg` and `esc` fit.
const KEYCAP_PER_CHAR: f32 = 8.0;

/// The grid's strip this frame, or `None` to leave it out: the `g`
/// continuation while a `g` is pending, else [`BOARD_STRIP`] if `?` pinned
/// it. Read after [`board_keys`], so it reflects this frame's key.
pub(crate) fn key_hints(state: &BoardUiState) -> Option<HintStrip> {
    if state.chord.pending() == Some(BoardPending::G) {
        return Some(G_STRIP);
    }
    state.key_hints_shown().then_some(BOARD_STRIP)
}

/// The strip of whichever pane shows over the grid — the queue, a plain
/// review pane or the detail — as [`key_hints`] is the grid's (the `?` pin is
/// shared).
pub(crate) fn pane_key_hints(state: &BoardUiState) -> Option<HintStrip> {
    if state.pane_chord.pending() == Some(PanePending::G) {
        return Some(PANE_G_STRIP);
    }
    if !state.key_hints_shown() {
        return None;
    }
    Some(if state.queue_open() {
        QUEUE_STRIP
    } else if state.review_card().is_some() {
        PANE_STRIP
    } else {
        DETAIL_STRIP
    })
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

/// Draw `strip` as rows of keycap groups, each keycap run followed by its muted
/// label, [`SPACING_MD`] apart. A group is measured before it is placed and
/// starts a new row if it won't fit what's left of this one, so a narrow pane
/// wraps whole groups and never splits a keycap from its label. (egui can't do
/// this by itself: it places a nested `horizontal` before knowing its width,
/// so the group would run off the right edge instead.) Walks the static tables
/// and lays each label out once.
pub(crate) fn key_hints_ui(ui: &mut egui::Ui, theme: &ColorTheme, strip: HintStrip) {
    ui.horizontal_wrapped(|ui| {
        let item_gap = ui.spacing().item_spacing.x;
        for (i, hint) in strip.iter().flat_map(|table| table.iter()).enumerate() {
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
/// move, an archive, a `D`); navigation mutates `state` directly.
///
/// Keys are left alone — and any pending chord dropped — while something else
/// owns the keyboard: a focused text field, an open menu or popup, an inline
/// editor, the archived sheet or the `X` composer, or a card drag. Presses
/// with Ctrl/Alt/Cmd fall through to app and chrome shortcuts.
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
    if let Some(card_action) = card_action(press) {
        let card = state
            .cursor()
            .filter(|&c| cursor::locate(view, filter, c).is_some());
        match (card, card_action) {
            (Some(card), _) => {
                action = apply_card_action(
                    ctx,
                    view,
                    state,
                    card,
                    card_action,
                    ActionView::Grid(filter),
                )
            }
            // With no cursor, `n`/`p` land it on the first card as `j`/`k`
            // do; the other actions have no card to act on.
            (None, CardAction::Step(step)) => move_cursor(view, filter, state, step_move(step)),
            (None, _) => {}
        }
    } else {
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
            (Key::C, false) => add_card_at_cursor(view, filter, state),
            (Key::Slash, false) => ctx.memory_mut(|m| m.request_focus(filter_field_id())),
            // `?` is Shift+/: a logical `Questionmark` from most layouts, or
            // the physical slash with Shift from the rest.
            (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
            (Key::H, true) => action = move_card(view, filter, state, CardMove::Across(Side::Left)),
            (Key::L, true) => {
                action = move_card(view, filter, state, CardMove::Across(Side::Right))
            }
            (Key::J, true) => {
                action = move_card(view, filter, state, CardMove::Within(Vertical::Down))
            }
            (Key::K, true) => {
                action = move_card(view, filter, state, CardMove::Within(Vertical::Up))
            }
            (Key::R, true) => state.open_review_queue(view, ctx.input(|i| i.time)),
            _ => return None,
        }
    }

    // Load-bearing for `/` and `X`: the filter field and the reason composer
    // lay out focused later this frame and would otherwise type the key. (`c`'s
    // composer only grabs focus after its first layout, so it happens to be
    // safe, but shouldn't depend on it.)
    chord::swallow_key_events(ctx);
    action
}

/// The keys of whatever shows over the grid: the `X` composer's
/// ([`reason_keys`]) while it's open, whatever view it's in; else the review
/// pane's in the queue or opened from a card ([`review_pane_keys`]); else the
/// open detail's ([`detail_keys`]). Nothing for the grid (its keys are
/// [`board_keys`], run as it lays out) or the dependency graph.
///
/// Runs before anything lays out, so a key that leaves a view (`q`, a verdict
/// on the queue's last card, `Enter` onto a card) has the view it lands on
/// draw this same frame rather than a blank one, and a handled key is
/// swallowed before a field could type it. Returns the key's board edit.
pub(crate) fn pane_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    if state.rejecting() {
        state.pane_chord.clear();
        return reason_keys(ctx, view, state);
    }
    if state.graph_epic().is_some() {
        return None;
    }
    if state.queue_open() {
        return review_pane_keys(ctx, view, state, PaneMode::Queue);
    }
    if state.review_card().is_some() {
        return review_pane_keys(ctx, view, state, PaneMode::Plain);
    }
    if state.selected().is_some() {
        return detail_keys(ctx, view, state);
    }
    None
}

/// Which review pane [`review_pane_keys`] drives: they differ only in what
/// `n`/`p` step through and where `q`/`Esc` go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneMode {
    /// The review queue: `n`/`p` step the queue, `q` leaves it for the grid.
    Queue,
    /// A review pane opened from a card: `n`/`p` step to the neighbouring
    /// card's review in its column, `q` backs out to the card's detail.
    Plain,
}

/// A review pane's keys, in the queue or opened from a card: the card
/// actions, then scrolling the diff by a line (`j`/`k`), a page
/// (`Space`/`Shift-Space`, `Ctrl-f`/`Ctrl-b`), half a page
/// (`Ctrl-d`/`Ctrl-u`), to its ends (`gg`/`G`) or by file (`]`/`[`), `?` and
/// `q`/`Esc`. Left alone under the grid's rules, bar its own overlays: a
/// focused widget, an open popup or menu, or a drag.
pub(crate) fn review_pane_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
    mode: PaneMode,
) -> Option<BoardAction> {
    let pending = state.pane_chord.tick(ctx);
    if focus_taken(ctx) {
        state.pane_chord.clear();
        return None;
    }
    let press = ctx.input(chord::first_key_press)?;
    if let Some(pages) = page_scroll(press) {
        state.pane_chord.clear();
        state.scroll_review(PatchScroll::Pages(pages));
        chord::swallow_key_events(ctx);
        return None;
    }
    if !press.is_bare() {
        state.pane_chord.clear();
        return None;
    }
    if pending.is_some() {
        state.pane_chord.clear();
        if is_key(press, Key::G) {
            state.scroll_review(PatchScroll::Top);
        }
        chord::swallow_key_events(ctx);
        return None;
    }

    let card = match mode {
        PaneMode::Queue => state.queue_card(),
        PaneMode::Plain => state.review_card(),
    };
    let mut action = None;
    if let Some(card_action) = card_action(press) {
        let at = match mode {
            PaneMode::Queue => ActionView::Queue,
            PaneMode::Plain => ActionView::Pane,
        };
        if let Some(card) = card {
            action = apply_card_action(ctx, view, state, card, card_action, at);
        }
    } else {
        match (press.key, press.modifiers.shift) {
            (Key::J, false) => state.scroll_review(PatchScroll::Rows(1)),
            (Key::K, false) => state.scroll_review(PatchScroll::Rows(-1)),
            (Key::G, false) => state
                .pane_chord
                .begin(PanePending::G, ctx.input(|i| i.time)),
            (Key::G, true) => state.scroll_review(PatchScroll::Bottom),
            (Key::CloseBracket, false) => state.scroll_review(PatchScroll::NextFile),
            (Key::OpenBracket, false) => state.scroll_review(PatchScroll::PrevFile),
            (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
            (Key::Q, false) | (Key::Escape, _) => match (mode, card) {
                (PaneMode::Queue, _) => state.close_queue(),
                (PaneMode::Plain, Some(card)) => state.back_to_detail(card),
                (PaneMode::Plain, None) => {}
            },
            _ => return None,
        }
    }
    // Load-bearing for `X`: the composer takes focus as it lays out this
    // frame and would otherwise type the X.
    chord::swallow_key_events(ctx);
    action
}

/// The card detail's keys: the card actions, then scrolling it by a line
/// (`j`/`k`), a page (`Space`/`Shift-Space`, `Ctrl-f`/`Ctrl-b`), half a page
/// (`Ctrl-d`/`Ctrl-u`) or to its ends (`gg`/`G`), `?`
/// and `q` back to the grid (the detail's `Esc` is its own). Left alone while
/// a widget has the keyboard — the comment composer, the title, description
/// and label editors — or a popup, menu or drag does.
pub(crate) fn detail_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let pending = state.pane_chord.tick(ctx);
    if focus_taken(ctx) {
        state.pane_chord.clear();
        return None;
    }
    // A selection that hasn't folded in yet draws the grid; leave its keys be.
    let card = state.selected().filter(|&c| find_card(view, c).is_some())?;
    let press = ctx.input(chord::first_key_press)?;
    if let Some(pages) = page_scroll(press) {
        state.pane_chord.clear();
        state.scroll_detail(PatchScroll::Pages(pages));
        chord::swallow_key_events(ctx);
        return None;
    }
    if !press.is_bare() {
        state.pane_chord.clear();
        return None;
    }
    if pending.is_some() {
        state.pane_chord.clear();
        if is_key(press, Key::G) {
            state.scroll_detail(PatchScroll::Top);
        }
        chord::swallow_key_events(ctx);
        return None;
    }

    let mut action = None;
    if let Some(card_action) = card_action(press) {
        action = apply_card_action(ctx, view, state, card, card_action, ActionView::Detail);
    } else {
        match (press.key, press.modifiers.shift) {
            (Key::J, false) => state.scroll_detail(PatchScroll::Rows(1)),
            (Key::K, false) => state.scroll_detail(PatchScroll::Rows(-1)),
            (Key::G, false) => state
                .pane_chord
                .begin(PanePending::G, ctx.input(|i| i.time)),
            (Key::G, true) => state.scroll_detail(PatchScroll::Bottom),
            (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
            (Key::Q, false) => state.leave_card(),
            _ => return None,
        }
    }
    chord::swallow_key_events(ctx);
    action
}

/// The page scroll a press asks for, in pages (negative scrolls up): a whole
/// page for `Space`/`Shift-Space` and vi's `Ctrl-f`/`Ctrl-b`, which lands
/// exactly (see [`PatchScroll::Pages`]), half of one for vi's
/// `Ctrl-d`/`Ctrl-u`. Safe to take: chrome's only Ctrl binding is Ctrl+Tab,
/// and these views put no focus on a button a Space would press.
fn page_scroll(press: KeyPress) -> Option<f32> {
    let m = press.modifiers;
    if press.key == Key::Space && press.is_bare() {
        return Some(if m.shift { -1.0 } else { 1.0 });
    }
    if !m.ctrl || m.alt || m.shift {
        return None;
    }
    match press.key {
        Key::F => Some(1.0),
        Key::B => Some(-1.0),
        Key::D => Some(0.5),
        Key::U => Some(-0.5),
        _ => None,
    }
}

/// The `X` composer's keys: Enter posts the reason (an empty one does
/// nothing), Esc cancels. Both are consumed before the field sees them, and
/// the field's focus goes with the composer.
fn reason_keys(
    ctx: &egui::Context,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
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

/// Whether something other than the grid owns the keyboard this frame.
fn keyboard_taken(ctx: &egui::Context, state: &BoardUiState, menu_open: bool) -> bool {
    // The grid's `menu_button`s report through `menu_open`, which egui's popup
    // memory can't see (see `focus_taken`).
    focus_taken(ctx) || menu_open || state.keys_blocked()
}

/// Whether a widget, popup, menu or drag holds the keyboard this frame: the
/// part of [`keyboard_taken`] that isn't the grid's own state, shared with
/// the pane keymaps.
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
    use crate::nav::ReviewTarget;
    use crate::ui::{CardFilter, QueueNotice};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use headway::event::{ReviewFields, ReviewView};

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

    /// A harness that runs the keymaps over [`grid`] each frame, unfiltered,
    /// as [`crate::ui::board_ui`] does: [`pane_keys`] first, then
    /// [`board_keys`] when no pane shows over the grid.
    fn keys_harness(field: Option<egui::Id>) -> Harness<'static, KeysHarness> {
        let mut harness = Harness::new_ui_state(
            |ui, h: &mut KeysHarness| {
                let parsed = CardFilter::parse("", &h.view.id);
                let filter = ViewFilter {
                    filter: &parsed,
                    hide_subissues: false,
                };
                let pane = |s: &BoardUiState| {
                    s.queue_open() || s.review_card().is_some() || s.selected().is_some()
                };
                let showed_pane = pane(&h.state);
                let keyed = pane_keys(ui.ctx(), &h.view, &mut h.state);
                let action = if showed_pane || keyed.is_some() {
                    keyed
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
                let hints = if pane(&h.state) {
                    pane_key_hints(&h.state)
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
        /// An inline editor (the `c` composer) is open.
        editing: bool,
        focused: Option<egui::Id>,
        moved: Option<(NoteId, usize, usize)>,
        archived: Option<NoteId>,
        /// The review queue is open.
        queue: bool,
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
            selected: h.state.selected(),
            editing: h.state.keys_blocked(),
            focused: harness.ctx.memory(|m| m.focused()),
            moved: h.moved,
            archived: h.archived,
            queue: h.state.queue_open(),
            review: h.state.review_card(),
            rejecting: h.state.rejecting(),
            notice: h.state.notice(),
            opened: h.opened.clone(),
            session: h.session.clone(),
        }
    }

    /// The presses a keycap stands for: `gg` is two, a capital is Shift plus the
    /// letter, `^d` is Ctrl+D, and the named keycaps are their keys.
    fn keycap_presses(cap: &str) -> Vec<(Modifiers, Key)> {
        match cap {
            "\u{21B5}" => return vec![(Modifiers::NONE, Key::Enter)],
            "esc" => return vec![(Modifiers::NONE, Key::Escape)],
            "space" => return vec![(Modifiers::NONE, Key::Space)],
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

    /// Every keycap in `strip`, with the label of its group.
    fn strip_keycaps(strip: HintStrip) -> impl Iterator<Item = (&'static str, &'static str)> {
        strip
            .iter()
            .flat_map(|table| table.iter())
            .flat_map(|hint| hint.keys.iter().map(move |&cap| (cap, hint.label)))
    }

    /// Replay every keycap in [`BOARD_STRIP`] from the middle of a 3×3 board
    /// and check it did *something*, so the strip can't advertise a key the
    /// keymap dropped or misspelled.
    #[test]
    fn every_hint_does_what_it_says() {
        for (cap, label) in strip_keycaps(BOARD_STRIP) {
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
                "keycap {cap:?} ({label}) did nothing"
            );
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
        assert!(
            harness.query_by_label("session/review").is_some(),
            "card actions"
        );

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

    /// **The guard.** Every keycap in [`CARD_ACTION_HINTS`], pressed in each
    /// of the four views with card 5 current (the grid's cursor, the queue's
    /// card, a plain review pane's, the detail's), applies the same
    /// [`CardAction`] to card 5. The per-view `every_*_hint` tests only show
    /// that a key does *something*; this shows it means the same thing.
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
        for (cap, label) in strip_keycaps(&[CARD_ACTION_HINTS]) {
            let presses = keycap_presses(cap);
            let [(modifiers, key)] = presses[..] else {
                panic!("card action {cap:?} is one press");
            };
            let expected = card_action(KeyPress { key, modifiers })
                .unwrap_or_else(|| panic!("keycap {cap:?} ({label}) is no card action"));
            for ActionTestView { name, open } in &views {
                let mut harness = open();
                press_with(&mut harness, modifiers, key);
                assert_eq!(
                    harness.state().state.last_card_action,
                    Some((expected, id(5))),
                    "keycap {cap:?} ({label}) in the {name}"
                );
            }
        }
    }

    /// Everything a queue key can visibly do, for
    /// [`every_queue_hint_does_what_it_says`] to compare before and after.
    #[derive(Debug, PartialEq)]
    struct QueueEffects {
        open: bool,
        card: Option<NoteId>,
        /// The record the pane has picked (`None` is the newest).
        record: Option<NoteId>,
        selected: Option<NoteId>,
        scroll: Option<PatchScroll>,
        moved: Option<(NoteId, usize, usize)>,
        archived: Option<NoteId>,
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
            record: h.state.review_record(),
            selected: h.state.selected(),
            scroll: h.state.review_scroll(),
            moved: h.moved,
            archived: h.archived,
            commented: h.commented.clone(),
            rejecting: h.state.rejecting(),
            notice: h.state.notice(),
            opened: h.opened.clone(),
            session: h.session.clone(),
            hints: h.state.key_hints_shown(),
        }
    }

    /// Replay every keycap in [`QUEUE_STRIP`] from the middle of a three-card
    /// queue and check it did *something*, so the queue's strip can't
    /// advertise a key its keymap dropped.
    #[test]
    fn every_queue_hint_does_what_it_says() {
        for (cap, label) in strip_keycaps(QUEUE_STRIP).chain([("?", "hints")]) {
            let mut harness = queue_harness();
            let before = queue_effects(&harness);
            for (modifiers, key) in keycap_presses(cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                queue_effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
    }

    /// Replay every keycap in [`PANE_STRIP`] in a plain review pane, as
    /// [`every_queue_hint_does_what_it_says`] does in the queue.
    #[test]
    fn every_review_pane_hint_does_what_it_says() {
        for (cap, label) in strip_keycaps(PANE_STRIP).chain([("?", "hints")]) {
            let mut harness = pane_harness();
            if cap == "r" {
                // `r` goes back to the newest record, so start from another
                // pick for it to have somewhere to go.
                harness.state_mut().state.set_review(Some(ReviewTarget {
                    card: id(5),
                    record: Some(id(99)),
                }));
            }
            let before = queue_effects(&harness);
            for (modifiers, key) in keycap_presses(cap) {
                press_with(&mut harness, modifiers, key);
            }
            assert_ne!(
                queue_effects(&harness),
                before,
                "keycap {cap:?} ({label}) did nothing"
            );
        }
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
        assert!(harness.query_by_label("top").is_some(), "g hint up");
        press(&mut harness, Key::G);
        assert_eq!(scroll(&harness), Some(PatchScroll::Top));
        press(&mut harness, Key::CloseBracket);
        assert_eq!(scroll(&harness), Some(PatchScroll::NextFile));
        press(&mut harness, Key::OpenBracket);
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
            harness.state_mut().state.set_queue_open(true);
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

    /// The detail takes the card actions — `s` opens its session, `D` moves
    /// it to Done, `n` steps to the next card in its column — and scrolls with
    /// `j`; `q` backs out to the grid.
    #[test]
    fn the_detail_takes_the_card_actions() {
        let mut harness = detail_harness(None);
        press(&mut harness, Key::S);
        assert_eq!(
            harness.state().session,
            Some(notedeck::OpenUri::new(SESSION))
        );
        press_with(&mut harness, Modifiers::SHIFT, Key::D);
        assert_eq!(harness.state().moved, Some((id(5), 2, 3)));
        press(&mut harness, Key::J);
        assert_eq!(
            harness.state().state.detail_scroll(),
            Some(PatchScroll::Rows(1))
        );
        press(&mut harness, Key::N);
        assert_eq!(harness.state().state.selected(), Some(id(6)));
        assert_eq!(harness.state().state.cursor(), Some(id(6)));
        press_with(&mut harness, Modifiers::SHIFT, Key::Questionmark);
        assert!(harness.query_by_label("session/review").is_some());
        press(&mut harness, Key::Q);
        assert_eq!(harness.state().state.selected(), None);
    }

    /// `a` on the detail archives its card and backs out to the grid.
    #[test]
    fn a_on_the_detail_archives_and_leaves() {
        let mut harness = detail_harness(None);
        press(&mut harness, Key::A);
        assert_eq!(harness.state().archived, Some(id(5)));
        assert_eq!(harness.state().state.selected(), None);
    }

    /// Typing into the detail's comment composer (or any focused field) keeps
    /// its keys: an `s` there opens no session.
    #[test]
    fn typing_in_the_detail_composer_keeps_its_keys() {
        let field = egui::Id::new("keys_test_comment");
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
