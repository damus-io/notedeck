//! The board grid's vim-style **bare-key keymap**: `j`/`k`/`h`/`l` walk the
//! card cursor, `gg`/`G` jump to the ends of its column, `Enter`/`o` open the
//! cursor card, `a` opens the add-card composer, `/` focuses the filter, `?`
//! toggles the which-key strip and `Esc` drops the cursor. Shifted, `H`/`L`
//! move the cursor card to the neighbouring column and `J`/`K` reorder it
//! within its own.
//!
//! The chord mechanics (reading the press, timing out a pending `g`, swallowing
//! handled keys) are [`notedeck_ui::chord`]'s; the grid math is
//! [`crate::cursor`]'s. This module is only the mapping between them, plus the
//! which-key strip ([`BOARD_HINTS`], [`key_hints_ui`]) that documents it. Both
//! run once per grid frame from [`crate::ui::board_ui`], so they allocate
//! nothing of their own.

use egui::{Key, Modifiers};
use notedeck::ColorTheme;
use notedeck::tokens::SPACING_MD;
use notedeck_ui::chord::{self, KeyPress};
use notedeck_ui::keybind_hint::KeybindHint;

use crate::cursor::{self, CursorMove, Side, Vertical};
use crate::event::BoardView;
use crate::store::BoardAction;
use crate::ui::{BoardUiState, ViewFilter, filter_field_id};

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
        keys: &["a"],
        label: "add",
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

/// Draw `hints` as a row of keycap groups, each followed by its muted label.
/// Wraps a whole group at a time on a narrow pane. Walks the static table only.
pub(crate) fn key_hints_ui(ui: &mut egui::Ui, theme: &ColorTheme, hints: &'static [KeyHint]) {
    ui.horizontal_wrapped(|ui| {
        for (i, hint) in hints.iter().enumerate() {
            if i > 0 {
                ui.add_space(SPACING_MD);
            }
            ui.horizontal(|ui| {
                for &key in hint.keys {
                    let extra_chars = key.chars().count().saturating_sub(1) as f32;
                    KeybindHint::new(key)
                        .size(KEYCAP)
                        .width(KEYCAP + extra_chars * KEYCAP_PER_CHAR)
                        .show(ui);
                }
                ui.label(egui::RichText::new(hint.label).color(theme.text_muted));
            });
        }
    });
}

/// Read this frame's bare key press and apply it to the grid. Runs before the
/// grid lays out. Returns a board edit for the app to apply (a keyboard card
/// move); navigation mutates `state` directly.
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
        (Key::A, false) => add_card_at_cursor(view, filter, state),
        (Key::Slash, false) => ctx.memory_mut(|m| m.request_focus(filter_field_id())),
        // `?` is Shift+/: a logical `Questionmark` from most layouts, or the
        // physical slash with Shift from the rest.
        (Key::Questionmark, _) | (Key::Slash, true) => state.toggle_key_hints(),
        (Key::H, true) => action = move_card(view, filter, state, CardMove::Across(Side::Left)),
        (Key::L, true) => action = move_card(view, filter, state, CardMove::Across(Side::Right)),
        (Key::J, true) => action = move_card(view, filter, state, CardMove::Within(Vertical::Down)),
        (Key::K, true) => action = move_card(view, filter, state, CardMove::Within(Vertical::Up)),
        _ => return None,
    }

    // Load-bearing for `/`: the filter field lays out focused later this frame
    // and would otherwise type the slash. (`a`'s composer only grabs focus after
    // its first layout, so it happens to be safe, but shouldn't depend on it.)
    chord::swallow_key_events(ctx);
    action
}

/// Whether something other than the grid owns the keyboard this frame.
fn keyboard_taken(ctx: &egui::Context, state: &BoardUiState, menu_open: bool) -> bool {
    // Read before any widget runs this frame, so this is the focus the key
    // press was typed into.
    ctx.memory(|m| m.focused().is_some() || m.any_popup_open())
        // `Memory::any_popup_open` sees combo boxes but not egui 0.31's menus:
        // right-click context menus have their own check, and the grid's
        // `menu_button`s report through `menu_open`.
        || ctx.is_context_menu_open()
        || menu_open
        || state.keys_blocked()
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
    use crate::ui::CardFilter;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
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
                if let Some(BoardAction::MoveCard {
                    card,
                    to_col,
                    to_row,
                }) = board_keys(ui.ctx(), &h.view, &filter, &mut h.state)
                {
                    h.moved = Some((card, to_col, to_row));
                }
                h.esc_left |= ui.input(|i| i.key_pressed(Key::Escape));
                if let Some(field) = h.field {
                    ui.add(egui::TextEdit::singleline(&mut h.text).id(field));
                }
                if let Some(hints) = key_hints(&h.state) {
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
        /// An inline editor (the `a` composer) is open.
        editing: bool,
        focused: Option<egui::Id>,
        moved: Option<(NoteId, usize, usize)>,
    }

    fn effects(harness: &Harness<'static, KeysHarness>) -> Effects {
        let h = harness.state();
        Effects {
            cursor: h.state.cursor(),
            selected: h.state.selected(),
            editing: h.state.keys_blocked(),
            focused: harness.ctx.memory(|m| m.focused()),
            moved: h.moved,
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
    fn bare_l_still_just_moves_the_cursor() {
        let mut harness = keys_harness(None);
        harness.state_mut().state.set_cursor(id(2));
        press(&mut harness, Key::L);
        assert_eq!(harness.state().moved, None);
        assert_eq!(harness.state().state.cursor(), Some(id(4)));
    }
}
