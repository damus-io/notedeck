//! The board grid's vim-style **bare-key keymap**: `j`/`k`/`h`/`l` walk the
//! card cursor, `gg`/`G` jump to the ends of its column, `Enter`/`o` open the
//! cursor card, `a` opens the add-card composer, `/` focuses the filter and
//! `Esc` drops the cursor.
//!
//! The chord mechanics (reading the press, timing out a pending `g`, swallowing
//! handled keys) are [`notedeck_ui::chord`]'s; the grid math is
//! [`crate::cursor`]'s. This module is only the mapping between them, and runs
//! once per grid frame from [`crate::ui::board_ui`], so it allocates nothing.

use egui::{Key, Modifiers};
use notedeck_ui::chord::{self, KeyPress};

use crate::cursor::{self, CursorMove};
use crate::event::BoardView;
use crate::store::BoardAction;
use crate::ui::{BoardUiState, ViewFilter, filter_field_id};

/// Chord steps the board grid can be waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardPending {
    /// `g` was pressed; a second `g` jumps to the top of the column.
    G,
}

/// Read this frame's bare key press and apply it to the grid. Runs before the
/// grid lays out. Returns a board edit for the app to apply (card moves,
/// headway:headway/fold-fetch-stone); navigation mutates `state` directly.
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
        _ => return None,
    }

    // Load-bearing for `/`: the filter field lays out focused later this frame
    // and would otherwise type the slash. (`a`'s composer only grabs focus after
    // its first layout, so it happens to be safe, but shouldn't depend on it.)
    chord::swallow_key_events(ctx);
    None
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

/// Esc drops the cursor and any pending chord. It's consumed only when there
/// was one to drop, so a bare Esc still reaches chrome (which toggles the side
/// menu).
fn escape(ctx: &egui::Context, state: &mut BoardUiState, pending: Option<BoardPending>) {
    if state.cursor().is_none() && pending.is_none() {
        return;
    }
    state.clear_cursor();
    state.chord.clear();
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::tests::{grid, id};
    use crate::ui::CardFilter;
    use egui_kittest::Harness;

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
                board_keys(ui.ctx(), &h.view, &filter, &mut h.state);
                h.esc_left |= ui.input(|i| i.key_pressed(Key::Escape));
                if let Some(field) = h.field {
                    ui.add(egui::TextEdit::singleline(&mut h.text).id(field));
                }
            },
            KeysHarness {
                view: grid(),
                state: BoardUiState::default(),
                field,
                text: String::new(),
                esc_left: false,
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
}
