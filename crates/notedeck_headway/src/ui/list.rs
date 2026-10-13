//! The issue list: every board's cards as one filterable list, one row per
//! card — the Linear-style lens across issues that the board grid, being one
//! board at a time, can't be (headway:headway/acoustic-canvas-diesel).
//!
//! It reads the same query language as the grid's filter and `headway list`
//! ([`headway::query::IssueFilter`], unscoped, so a pasted ref names exactly
//! one card), with an Active/All toggle standing in for `is:open`. The rows
//! are recomputed only when a board re-folds or the query changes
//! ([`ListState::refresh`]); a steady frame draws the cached rows, and only
//! the ones on screen.

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS};

use super::widgets::{
    KeyedText, StatusIcon, label_chip, priority_icon_ui, priority_label, status_icon_ui,
};
use super::{BoardEffect, BoardNav, BoardUiState};
use crate::cache::BoardRef;
use crate::cursor::CursorMove;
use crate::event::{self, BoardView, CardView, Priority};
use crate::keys;
use headway::query::{IssueFilter, ListRow, Placement, StateFilter};

/// Height of one list row, fixed so the scroll area lays out only the rows on
/// screen ([`egui::ScrollArea::show_rows`]).
const ROW_HEIGHT: f32 = 32.0;

/// The issue list's view state: whether it shows, what it's asked, its rows
/// and its keyboard cursor. Kept across frames on
/// [`BoardUiState`](super::BoardUiState), so leaving the list and coming
/// back (a back off a card opened from it) finds it as it was left.
#[derive(Default)]
pub(crate) struct ListState {
    /// Whether the list shows, seeded from the nav route each frame like the
    /// graph's epic (see [`BoardUiState::set_list_open`]).
    open: bool,
    /// The query field's text, parsed with [`IssueFilter::parse`].
    query: String,
    /// The All toggle: show done cards too. Off (Active) adds `is:open` to a
    /// query that has no `is:` term of its own; one that does wins.
    all: bool,
    /// The card under the keyboard cursor. A card rather than a row index,
    /// so a re-fold that reorders the rows leaves it on the same card.
    cursor: Option<NoteId>,
    /// Scroll the cursor's row into view on the next draw.
    scroll_to_cursor: bool,
    /// The rows the query keeps, in board order. Valid for
    /// [`seen`](Self::seen)'s sources.
    rows: Vec<ListLine>,
    /// What [`rows`](Self::rows) was computed from.
    seen: Option<ListInputs>,
    /// The header's `"N issues"` count.
    count: KeyedText<usize>,
}

/// One row of the list: where its card sits in the sources it was computed
/// from, by index, and the text a row draws that would otherwise be formatted
/// every frame.
struct ListLine {
    /// Index of the card's board in the sources.
    board: usize,
    /// Where the card sits on that board.
    slot: LineSlot,
    /// The card's id.
    id: NoteId,
    /// The card's word-id (`maple-river-canyon`).
    word_id: String,
}

/// Where a [`ListLine`]'s card sits on its board, by index into the board the
/// rows were computed from.
#[derive(Clone, Copy)]
enum LineSlot {
    /// `columns[col].cards[card]`.
    Column { col: usize, card: usize },
    /// `archived[at]`: listed only for an `is:archived` query.
    Archived(usize),
}

impl LineSlot {
    /// The card this slot names on `board`.
    fn card(self, board: &BoardView) -> &CardView {
        match self {
            LineSlot::Column { col, card } => &board.columns[col].cards[card],
            LineSlot::Archived(at) => &board.archived[at].card,
        }
    }

    /// The status icon for where the card sits on `board`.
    fn icon(self, board: &BoardView) -> StatusIcon {
        match self {
            LineSlot::Column { col, .. } => StatusIcon::for_column(col, board.columns.len()),
            LineSlot::Archived(_) => StatusIcon::Archived,
        }
    }

    /// The name of where the card sits on `board`: its column's, or
    /// "Archived".
    fn place(self, board: &BoardView) -> &str {
        match self {
            LineSlot::Column { col, .. } => &board.columns[col].name,
            LineSlot::Archived(_) => "Archived",
        }
    }
}

/// The inputs the cached rows were computed from: when any differs, the rows
/// are stale.
struct ListInputs {
    /// Each board read, as a handle into its fold; compared by pointer.
    sources: Vec<BoardRef>,
    /// The query text.
    query: String,
    /// The All toggle.
    all: bool,
}

/// Where a list row opens: its card, and the board it's on.
struct RowTarget {
    card: NoteId,
    board: event::BoardCoord,
}

impl ListState {
    /// Whether the list shows.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The card under the keyboard cursor, if any.
    pub(crate) fn cursor(&self) -> Option<NoteId> {
        self.cursor
    }

    /// Show or hide the list.
    pub(super) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Whether the cached rows were computed from other inputs than these.
    /// Compares the boards by pointer and the query by value, so a steady
    /// frame allocates nothing to find out.
    fn stale(&self, sources: &[BoardRef]) -> bool {
        let Some(seen) = &self.seen else {
            return true;
        };
        seen.all != self.all
            || seen.query != self.query
            || seen.sources.len() != sources.len()
            || seen
                .sources
                .iter()
                .zip(sources)
                .any(|(was, now)| !was.same_fold(now))
    }

    /// Recompute the rows from `sources` if they, the query or the toggle
    /// changed since the last time.
    fn refresh(&mut self, sources: &[BoardRef]) {
        if !self.stale(sources) {
            return;
        }
        let filter = self.filter();
        self.rows.clear();
        for (b, board) in sources.iter().enumerate() {
            let mut keep = |card: &CardView, placement: Placement, slot: LineSlot| {
                let row = ListRow {
                    board,
                    card,
                    placement,
                };
                if filter.matches(&row) {
                    self.rows.push(ListLine {
                        board: b,
                        slot,
                        id: card.id,
                        word_id: headway::wordid::encode(card.id.bytes()),
                    });
                }
            };
            for (col, column) in board.columns.iter().enumerate() {
                for (k, card) in column.cards.iter().enumerate() {
                    let slot = LineSlot::Column { col, card: k };
                    keep(card, Placement::Column(column), slot);
                }
            }
            // Archived cards only when the query asks (`is:archived`), as
            // `headway list` does.
            if !filter.wants_archived() {
                continue;
            }
            for (at, archived) in board.archived.iter().enumerate() {
                keep(&archived.card, Placement::Archived, LineSlot::Archived(at));
            }
        }
        self.seen = Some(ListInputs {
            sources: sources.to_vec(),
            query: self.query.clone(),
            all: self.all,
        });
    }

    /// The query as a filter, with Active's `is:open` added when the query
    /// says nothing about state itself — the default `headway list` takes.
    fn filter(&self) -> IssueFilter {
        let mut filter = IssueFilter::parse(&self.query, None);
        if filter.state.is_none() && !self.all {
            filter.state = Some(StateFilter::Open);
        }
        filter
    }

    /// The cursor's row, if the cursor is on a row the list shows.
    fn cursor_row(&self) -> Option<usize> {
        let cursor = self.cursor?;
        self.rows.iter().position(|line| line.id == cursor)
    }

    /// Move the cursor a row down or up, or to the first or last row. With no
    /// cursor (or one on a card the list no longer shows), any move lands on
    /// the first row. Left and right mean nothing in one list.
    pub(crate) fn move_cursor(&mut self, mv: CursorMove) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let to = match (self.cursor_row(), mv) {
            (None, _) | (_, CursorMove::First) => 0,
            (_, CursorMove::Last) => last,
            (Some(at), CursorMove::Down) => (at + 1).min(last),
            (Some(at), CursorMove::Up) => at.saturating_sub(1),
            (Some(_), CursorMove::Left | CursorMove::Right) => return,
        };
        self.cursor = Some(self.rows[to].id);
        self.scroll_to_cursor = true;
    }

    /// Where row `at` opens, read from the `sources` the rows came from.
    fn target(&self, sources: &[BoardRef], at: usize) -> Option<RowTarget> {
        let line = self.rows.get(at)?;
        let board = sources.get(line.board)?;
        Some(RowTarget {
            card: line.id,
            board: event::BoardCoord::new(board.author, board.id.clone()),
        })
    }
}

impl BoardUiState {
    /// Open the issue list over the grid.
    pub(crate) fn open_list(&mut self) {
        self.list.set_open(true);
    }

    /// Leave the issue list for the grid.
    pub(crate) fn close_list(&mut self) {
        self.list.set_open(false);
    }

    /// Seed whether the issue list shows from the chrome global-history route
    /// this frame renders, the list counterpart to
    /// [`set_graph_epic`](Self::set_graph_epic). What the list holds — its
    /// query, toggle and cursor — is left alone, so back/forward onto its
    /// entry finds it as it was left.
    pub fn set_list_open(&mut self, open: bool) {
        self.list.set_open(open);
    }

    /// Open the card under the list's cursor, if there is one.
    pub(crate) fn open_list_cursor(&mut self, view: &BoardView, sources: &[BoardRef]) {
        if let Some(at) = self.list.cursor_row() {
            self.open_list_row(view, sources, at);
        }
    }

    /// Open row `at`'s card: its detail, on top of the list, switching to its
    /// board first when that isn't the board showing. The list stays open
    /// beneath, so leaving the card returns to it.
    fn open_list_row(&mut self, view: &BoardView, sources: &[BoardRef], at: usize) {
        let Some(target) = self.list.target(sources, at) else {
            return;
        };
        if target.board.owner != view.author || target.board.slug != view.id {
            self.raise(BoardEffect::Nav(BoardNav::Switch(target.board)));
        }
        self.list.cursor = Some(target.card);
        self.selected = Some(target.card);
    }
}

/// The issue list's query field's pinned id, shared by the field and the `/`
/// key that focuses it.
pub(crate) fn list_field_id() -> egui::Id {
    egui::Id::unique("headway-list-filter-field")
}

/// Draw the issue list over every board in `sources`, the boards the switcher
/// lists. `view` is the board showing, which a row on another board switches
/// away from when opened. Runs the list's UI tools first, so a key that opens
/// a card or leaves the list lands this frame.
pub(super) fn list_view_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    sources: &[BoardRef],
    state: &mut BoardUiState,
) {
    state.list.refresh(sources);
    while state.list.is_open() && state.selected.is_none() {
        let Some(tool) = state.next_queued_tool() else {
            break;
        };
        keys::apply_list_tool(ui.ctx(), view, sources, state, &tool);
    }

    let mut clicked: Option<usize> = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            list_header_ui(ui, theme, state);
            // A query edited this frame narrows the rows this frame.
            state.list.refresh(sources);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_SM);
            clicked = list_rows_ui(ui, theme, sources, &mut state.list);
        });

    if let Some(at) = clicked {
        state.open_list_row(view, sources, at);
    }
}

/// The list's header: a way back to the board, the count, the Active/All
/// toggle and the query field.
fn list_header_ui(ui: &mut egui::Ui, theme: &ColorTheme, state: &mut BoardUiState) {
    let list = &mut state.list;
    let mut close = false;
    ui.horizontal(|ui| {
        let back =
            egui::Button::new(egui::RichText::new("← Board").color(theme.text_muted)).frame(false);
        if ui.add(back).clicked() {
            close = true;
        }
        ui.add_space(SPACING_SM);
        ui.heading("Issues");
        ui.add_space(SPACING_SM);
        let n = list.rows.len();
        let count = list.count.text(n, |text, n| {
            use std::fmt::Write;
            let _ = write!(text, "{n} issue{}", if n == 1 { "" } else { "s" });
        });
        ui.label(egui::RichText::new(count).color(theme.text_muted));
        ui.add_space(SPACING_MD);
        if ui.selectable_label(!list.all, "Active").clicked() {
            list.all = false;
        }
        if ui.selectable_label(list.all, "All").clicked() {
            list.all = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let field = egui::TextEdit::singleline(&mut list.query)
                .id(list_field_id())
                .desired_width(260.0)
                .hint_text("Filter… e.g. p:high label:ui is:done");
            ui.add(field);
        });
    });
    if close {
        state.close_list();
    }
}

/// The list's rows, only those on screen laid out. Returns the row clicked
/// this frame, if any.
fn list_rows_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    sources: &[BoardRef],
    list: &mut ListState,
) -> Option<usize> {
    if list.rows.is_empty() {
        ui.label(egui::RichText::new("No matching issues.").color(theme.text_muted));
        return None;
    }
    let cursor = list.cursor_row();
    let scroll = std::mem::take(&mut list.scroll_to_cursor);
    let mut clicked = None;
    let row_height = ROW_HEIGHT;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, list.rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // The cursor's row, laid out or not: show_rows starts this ui at
            // the first visible row, so any row's rect is an offset from it.
            if scroll && let Some(at) = cursor {
                let top = ui.max_rect().top() + (at as f32 - range.start as f32) * row_height;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::vec2(ui.available_width(), row_height),
                );
                ui.scroll_to_rect(rect, None);
            }
            for at in range {
                let line = &list.rows[at];
                if list_row_ui(ui, theme, sources, line, cursor == Some(at)).clicked() {
                    clicked = Some(at);
                }
            }
        });
    if let Some(at) = clicked {
        list.cursor = Some(list.rows[at].id);
    }
    clicked
}

/// One row: status, priority, word-id, title, labels and where it sits (its
/// column and board). `on_cursor` rings it as the keyboard cursor's row.
fn list_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    sources: &[BoardRef],
    line: &ListLine,
    on_cursor: bool,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::hover(),
    );
    let board: &BoardView = &sources[line.board];
    let card = line.slot.card(board);
    let place = line.slot.place(board);

    let corner = egui::CornerRadius::same(RADIUS_MD as u8);
    if on_cursor {
        ui.painter()
            .rect_filled(rect, corner, theme.accent.gamma_multiply(0.15));
    } else if ui.rect_contains_pointer(rect) {
        ui.painter()
            .rect_filled(rect, corner, theme.surface_elevated);
    }

    let inner = rect.shrink2(egui::vec2(SPACING_SM, 0.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = SPACING_SM;
            status_icon_ui(ui, theme, line.slot.icon(board), 14.0).on_hover_text(place);
            if card.priority == Priority::None {
                ui.add_space(14.0);
            } else {
                priority_icon_ui(ui, theme, card.priority, 14.0)
                    .on_hover_text(priority_label(card.priority));
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&line.word_id)
                        .small()
                        .color(theme.text_muted.gamma_multiply(0.7)),
                )
                .selectable(false),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = SPACING_SM;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&board.title)
                            .small()
                            .color(theme.text_muted),
                    )
                    .selectable(false),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(place)
                            .small()
                            .color(theme.text_secondary),
                    )
                    .selectable(false),
                );
                for label in card.labels.iter().rev() {
                    label_chip(ui, theme, label);
                }
                ui.add_space(SPACING_XS);
                // The title takes what's left, from the left, cut short with
                // an ellipsis rather than pushing the labels off the row.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&card.title).color(theme.text_primary),
                        )
                        .selectable(false)
                        .truncate(),
                    );
                });
            });
        },
    );

    // Sensed after its contents, so a click anywhere on the row is the
    // row's rather than a label's.
    let id = egui::Id::unique(("headway-list-row", line.id));
    let response = ui.interact(rect, id, egui::Sense::click());
    if on_cursor {
        ui.painter().rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0, theme.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
}
