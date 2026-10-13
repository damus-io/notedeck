//! Switching boards from the keyboard: `]b`/`[b` step to the next or
//! previous board in the switcher's order ([`step_board`]), and `gb` opens
//! the board picker ([`board_picker_ui`]), every board under a filter field
//! that narrows them as you type.
//!
//! Both raise [`BoardNav::Switch`] for the app to apply, as the header's
//! switcher does, so a board switched to by key is persisted the same way.

use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_LG, SPACING_SM, SPACING_XS};

use super::detail::{detail_scrim_ui, detail_sheet_frame};
use super::{BoardEffect, BoardNav, BoardUiState};
use crate::BoardSummary;
use crate::event::{self, BoardView};

/// Which way `]b`/`[b` step through the boards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardStep {
    /// To the next board (`]b`).
    Next,
    /// To the previous board (`[b`).
    Prev,
}

/// Whether `board` is the one `view` shows. Matched by coordinate (owner +
/// slug), as the switcher marks it, so a joined board sharing the open one's
/// slug isn't taken for it.
fn is_current(view: &BoardView, board: &BoardSummary) -> bool {
    board.id == view.id && board.owner == view.author
}

/// Whether `boards` holds a board other than the one `view` shows: something
/// for `]b`/`[b` to step to.
pub(crate) fn has_other_board(view: &BoardView, boards: &[BoardSummary]) -> bool {
    boards.iter().any(|b| !is_current(view, b))
}

/// The board `step` lands on from the one `view` shows, in `boards`' order
/// (the switcher's: own boards, then the ones shared with you), wrapping
/// around at either end. A board missing from the list steps from just
/// before its start. `None` when there is no other board.
pub(crate) fn step_board(
    view: &BoardView,
    boards: &[BoardSummary],
    step: BoardStep,
) -> Option<event::BoardCoord> {
    let len = boards.len();
    let at = boards.iter().position(|b| is_current(view, b));
    let to = match (step, at) {
        (BoardStep::Next, Some(at)) => (at + 1) % len,
        (BoardStep::Prev, Some(at)) => (at + len - 1) % len,
        (BoardStep::Next, None) => 0,
        (BoardStep::Prev, None) => len.checked_sub(1)?,
    };
    let board = boards.get(to).filter(|b| !is_current(view, b))?;
    Some(event::BoardCoord::new(board.owner, board.id.clone()))
}

/// The `gb` board picker's state: whether it's open, its query, its cursor,
/// and the boards the query matches, cached so a frame that changed nothing
/// matches nothing (see [`refresh`](Self::refresh)).
#[derive(Default)]
pub(crate) struct BoardPicker {
    /// Whether the picker shows.
    open: bool,
    /// The filter field's text.
    query: String,
    /// Focus the filter field once, as the picker opens.
    focus: bool,
    /// The highlighted row, an index into [`matches`](Self::matches).
    cursor: usize,
    /// The boards the query matches, as indices into the switcher's list,
    /// in its order.
    matches: Vec<usize>,
    /// The query `matches` was computed for.
    matched_query: String,
    /// How many boards the switcher listed when `matches` was computed.
    matched_boards: usize,
    /// Set as the picker opens, so it matches afresh whatever the query.
    stale: bool,
}

impl BoardPicker {
    /// Open the picker with an empty query, its field focused.
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.focus = true;
        self.cursor = 0;
        self.stale = true;
    }

    /// Whether the picker shows.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Close the picker.
    fn close(&mut self) {
        self.open = false;
    }

    /// Re-match the query against `boards`, but only when it or the board
    /// list changed since the last match, or the picker just opened. The
    /// query matches a board's title or slug as a substring, ignoring case.
    fn refresh(&mut self, boards: &[BoardSummary]) {
        if !self.stale && self.query == self.matched_query && boards.len() == self.matched_boards {
            return;
        }
        self.stale = false;
        self.matched_query.clone_from(&self.query);
        self.matched_boards = boards.len();
        let needle = self.query.trim().to_lowercase();
        self.matches.clear();
        self.matches.extend(
            boards
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    needle.is_empty()
                        || b.title.to_lowercase().contains(&needle)
                        || b.id.to_lowercase().contains(&needle)
                })
                .map(|(i, _)| i),
        );
        self.cursor = self.cursor.min(self.matches.len().saturating_sub(1));
    }

    /// Move the highlight by `by` rows, stopping at either end.
    fn move_cursor(&mut self, by: isize) {
        let last = self.matches.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
    }
}

/// The picker's filter field's pinned id.
pub(crate) fn board_picker_field_id() -> egui::Id {
    egui::Id::unique("headway-board-picker-field")
}

/// What the picker's keys asked for this frame.
#[derive(Default)]
struct PickerKeys {
    /// Esc: close.
    close: bool,
    /// Enter: switch to the highlighted board.
    pick: bool,
    /// The rows to move the highlight by: ↓ and Ctrl-N / Ctrl-J down, ↑ and
    /// Ctrl-P / Ctrl-K up.
    by: isize,
}

/// Take the picker's keys out of this frame's input before its field lays
/// out, so the field never sees them: a single-line field gives up focus on
/// Enter, and reads Ctrl-K as "delete to the end".
fn picker_keys(ctx: &egui::Context) -> PickerKeys {
    use egui::{Key, Modifiers};
    ctx.input_mut(|i| {
        let mut keys = PickerKeys {
            close: i.consume_key(Modifiers::NONE, Key::Escape),
            pick: i.consume_key(Modifiers::NONE, Key::Enter),
            by: 0,
        };
        for (mods, key, by) in [
            (Modifiers::NONE, Key::ArrowDown, 1),
            (Modifiers::CTRL, Key::N, 1),
            (Modifiers::CTRL, Key::J, 1),
            (Modifiers::NONE, Key::ArrowUp, -1),
            (Modifiers::CTRL, Key::P, -1),
            (Modifiers::CTRL, Key::K, -1),
        ] {
            while i.consume_key(mods, key) {
                keys.by += by;
            }
        }
        keys
    })
}

/// The `gb` board picker, while it's open: a sheet over the board listing
/// every board, the open one marked, under a focused filter field that
/// narrows them by title or slug. ↑/↓ (or Ctrl-N/P, Ctrl-J/K) move the
/// highlight, Enter or a click switches to a board, Esc or a click outside
/// closes it. The field holds the keyboard while it's open, so chrome's
/// keymap stands down and these keys are the picker's own.
pub(super) fn board_picker_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
) {
    if !state.picker.is_open() {
        return;
    }
    let picker = &mut state.picker;
    picker.refresh(boards);

    let keys = picker_keys(ui.ctx());
    picker.move_cursor(keys.by);
    let mut close = keys.close;
    let mut picked = keys
        .pick
        .then(|| picker.matches.get(picker.cursor).copied())
        .flatten();

    let screen = ui.ctx().content_rect();
    let pad = SPACING_LG;
    let width = if notedeck::ui::is_narrow(ui.ctx()) {
        screen.width() - 2.0 * pad
    } else {
        420.0
    };
    if detail_scrim_ui(ui, screen) {
        close = true;
    }

    egui::Area::new(egui::Id::unique("headway-board-picker"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::CENTER_TOP,
            egui::vec2(0.0, screen.height() * 0.15),
        )
        .show(ui.ctx(), |ui| {
            detail_sheet_frame(theme, pad).show(ui, |ui| {
                ui.set_width(width);
                let field = egui::TextEdit::singleline(&mut picker.query)
                    .id(board_picker_field_id())
                    .desired_width(f32::INFINITY)
                    .hint_text("Switch to board…");
                let field = ui.add(field);
                if std::mem::take(&mut picker.focus) {
                    field.request_focus();
                }
                ui.add_space(SPACING_SM);
                if picker.matches.is_empty() {
                    ui.label(egui::RichText::new("No matching boards.").color(theme.text_muted));
                }
                for (row, &index) in picker.matches.iter().enumerate() {
                    let Some(board) = boards.get(index) else {
                        continue;
                    };
                    let highlighted = row == picker.cursor;
                    if board_row_ui(ui, theme, board, is_current(view, board), highlighted)
                        .clicked()
                    {
                        picked = Some(index);
                    }
                }
            });
        });

    if let Some(board) = picked.and_then(|i| boards.get(i)) {
        if !is_current(view, board) {
            state.raise(BoardEffect::Nav(BoardNav::Switch(event::BoardCoord::new(
                board.owner,
                board.id.clone(),
            ))));
        }
        close = true;
    }
    if close {
        state.picker.close();
        ui.ctx()
            .memory_mut(|m| m.surrender_focus(board_picker_field_id()));
    }
}

/// One board in the picker: its title, its slug muted beside it, and a ✓ on
/// the open board; filled while highlighted. Returns its click response.
fn board_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    board: &BoardSummary,
    current: bool,
    highlighted: bool,
) -> egui::Response {
    let fill = if highlighted {
        theme.surface_secondary
    } else {
        egui::Color32::TRANSPARENT
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, SPACING_XS as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&board.title).color(theme.text_primary));
                ui.label(egui::RichText::new(&board.id).color(theme.text_muted));
                if current {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("✓").color(theme.text_muted));
                    });
                }
            });
        })
        .response
        .interact(egui::Sense::click())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(owner: u8, id: &str, title: &str) -> BoardSummary {
        BoardSummary {
            owner: [owner; 32],
            id: id.to_owned(),
            title: title.to_owned(),
        }
    }

    fn view_of(owner: u8, id: &str) -> BoardView {
        let mut view = crate::cursor::tests::grid();
        view.author = [owner; 32];
        view.id = id.to_owned();
        view
    }

    fn boards() -> Vec<BoardSummary> {
        vec![
            summary(1, "main", "Main"),
            summary(1, "side", "Side"),
            // A joined board sharing the first one's slug.
            summary(2, "main", "Their main"),
        ]
    }

    /// A step's landing as `owner/slug`, the owner by its first byte.
    fn slug_of(coord: Option<event::BoardCoord>) -> Option<String> {
        coord.map(|c| format!("{}/{}", c.owner[0], c.slug))
    }

    #[test]
    fn steps_wrap_in_switcher_order_by_coordinate() {
        let boards = boards();
        let at = |owner, id, step| slug_of(step_board(&view_of(owner, id), &boards, step));
        assert_eq!(at(1, "main", BoardStep::Next), Some("1/side".into()));
        assert_eq!(at(1, "side", BoardStep::Next), Some("2/main".into()));
        assert_eq!(at(2, "main", BoardStep::Next), Some("1/main".into()));
        assert_eq!(at(1, "main", BoardStep::Prev), Some("2/main".into()));
        assert_eq!(at(2, "main", BoardStep::Prev), Some("1/side".into()));
    }

    #[test]
    fn nothing_to_step_to_alone() {
        let boards = vec![summary(1, "main", "Main")];
        let view = view_of(1, "main");
        assert!(!has_other_board(&view, &boards));
        assert_eq!(slug_of(step_board(&view, &boards, BoardStep::Next)), None);
        assert_eq!(slug_of(step_board(&view, &[], BoardStep::Prev)), None);
        // A board the list doesn't have yet steps onto the list.
        let other = view_of(3, "new");
        assert!(has_other_board(&other, &boards));
        assert_eq!(
            slug_of(step_board(&other, &boards, BoardStep::Next)),
            Some("1/main".into())
        );
    }

    #[test]
    fn the_query_matches_title_or_slug_ignoring_case() {
        let boards = boards();
        let mut picker = BoardPicker::default();
        picker.open();
        picker.refresh(&boards);
        assert_eq!(picker.matches, [0, 1, 2]);
        picker.query = "SIDE".into();
        picker.refresh(&boards);
        assert_eq!(picker.matches, [1]);
        picker.query = "their".into();
        picker.refresh(&boards);
        assert_eq!(picker.matches, [2]);
        picker.query = "mai".into();
        picker.refresh(&boards);
        assert_eq!(picker.matches, [0, 2]);
    }

    #[test]
    fn the_cursor_stays_on_a_match() {
        let boards = boards();
        let mut picker = BoardPicker::default();
        picker.open();
        picker.refresh(&boards);
        picker.move_cursor(5);
        assert_eq!(picker.cursor, 2);
        picker.move_cursor(-1);
        assert_eq!(picker.cursor, 1);
        picker.move_cursor(-5);
        assert_eq!(picker.cursor, 0);
        picker.move_cursor(2);
        picker.query = "side".into();
        picker.refresh(&boards);
        assert_eq!(picker.cursor, 0, "clamped onto the one match left");
    }
}
