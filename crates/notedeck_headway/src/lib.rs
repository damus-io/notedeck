use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_PILL, RADIUS_SM, SPACING_LG, SPACING_MD, SPACING_SM,
    SPACING_XS, STROKE_MEDIUM, STROKE_THIN,
};
use notedeck::{App, AppContext, AppResponse, ColorTheme};

mod model;

pub use model::{Board, Card, Column};

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// A Linear/Trello-style issue & todo tracker app for notedeck.
///
/// The board currently runs on an in-memory [`Board`] (see [`model`]); the nostr
/// event model is intentionally deferred until the UX settles.
#[derive(Default)]
pub struct Headway {
    board: Board,
    state: BoardUiState,
}

/// Transient, per-board UI state that must persist across frames but isn't part
/// of the data model (e.g. which column has an open "add card" composer).
#[derive(Default)]
struct BoardUiState {
    /// The column index currently composing a new card, if any.
    composing: Option<usize>,
    /// Buffer backing the new-card text field.
    compose_text: String,
    /// The card whose detail view is open, if any.
    selected: Option<u64>,
}

/// Drag-and-drop payload: the stable id of the card being dragged.
#[derive(Clone)]
struct DragCard(u64);

/// A pending board mutation collected during rendering and applied afterwards,
/// so the render pass can borrow the board immutably.
enum BoardAction {
    /// Move `card_id` into `(column, row)`.
    Move {
        card_id: u64,
        col: usize,
        row: usize,
    },
    /// Append a new card with `title` to `column`.
    Add { col: usize, title: String },
}

impl Headway {
    pub fn new() -> Self {
        Self::default()
    }
}

impl App for Headway {
    fn render(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let theme = ColorTheme::current(ui.ctx());

        // Disjoint borrows: the render closure reads `board` immutably while
        // mutating `state`; structural board edits are collected and applied after.
        let Headway { board, state } = self;
        let mut action: Option<BoardAction> = None;
        // The card a click landed on this frame; opens the detail view below.
        let mut clicked: Option<u64> = None;

        egui::Frame::new()
            .inner_margin(egui::Margin::same(SPACING_LG as i8))
            .show(ui, |ui| {
                // Board header: title plus a muted summary of its contents.
                ui.heading(&board.title);
                ui.add_space(SPACING_XS);
                let total: usize = board.columns.iter().map(|c| c.cards.len()).sum();
                ui.label(
                    egui::RichText::new(format!(
                        "{total} card{} · {} columns",
                        if total == 1 { "" } else { "s" },
                        board.columns.len()
                    ))
                    .color(theme.text_muted),
                );
                ui.add_space(SPACING_SM);
                ui.separator();
                ui.add_space(SPACING_MD);

                egui::ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = SPACING_MD;
                            for col_idx in 0..board.columns.len() {
                                column_ui(
                                    ui,
                                    &theme,
                                    board,
                                    state,
                                    col_idx,
                                    &mut action,
                                    &mut clicked,
                                );
                            }
                        });
                    });
            });

        if let Some(action) = action {
            match action {
                BoardAction::Move { card_id, col, row } => board.move_card(card_id, col, row),
                BoardAction::Add { col, title } => {
                    let id = board.next_id();
                    if let Some(column) = board.columns.get_mut(col) {
                        column.cards.push(Card::new(id, title));
                    }
                }
            }
        }

        if let Some(card_id) = clicked {
            state.selected = Some(card_id);
        }

        // Detail view floats above the board; it borrows the board mutably to
        // edit the selected card in place, so it runs after the render pass.
        card_detail_ui(ui, &theme, board, state);

        AppResponse::default()
    }
}

/// Render one column: header, the draggable card list (a drop zone), and the
/// add-card composer.
fn column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    board: &Board,
    state: &mut BoardUiState,
    col_idx: usize,
    action: &mut Option<BoardAction>,
    clicked: &mut Option<u64>,
) {
    let column = &board.columns[col_idx];
    let height = ui.available_height();

    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(RADIUS_LG as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(COLUMN_WIDTH);
            ui.set_min_height(height);

            // Force a top-down interior: the board arranges columns with a
            // horizontal layout (`horizontal_top`), and that direction is
            // inherited by this frame — without this the cards would stack
            // left-to-right instead of vertically.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                // Header: title + a pill badge with the card count.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&column.title).strong());
                    count_badge(ui, theme, column.cards.len());
                });
                ui.add_space(SPACING_SM);

                egui::ScrollArea::vertical()
                    .id_salt(("headway-col", col_idx))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        cards_drop_zone(ui, theme, column, col_idx, action, clicked);
                        ui.add_space(SPACING_SM);
                        add_card_ui(ui, theme, state, col_idx, action);
                    });
            });
        });
}

/// The drop zone wrapping a column's cards, with live insertion-line feedback.
fn cards_drop_zone(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    column: &Column,
    col_idx: usize,
    action: &mut Option<BoardAction>,
    clicked: &mut Option<u64>,
) {
    let frame = egui::Frame::new().inner_margin(egui::Margin::same(SPACING_XS as i8));

    // Tracks where a release would land (also used to paint the insertion line).
    let mut hover_target: Option<usize> = None;

    let (_, dropped) = ui.dnd_drop_zone::<DragCard, ()>(frame, |ui| {
        ui.set_min_height(SPACING_LG);
        ui.spacing_mut().item_spacing.y = SPACING_SM;

        for (row_idx, card) in column.cards.iter().enumerate() {
            let card_id = egui::Id::new(("headway-card", card.id));
            let response = ui
                .dnd_drag_source(card_id, DragCard(card.id), |ui| {
                    card_ui(ui, theme, card);
                })
                .response;

            // `dnd_drag_source` only senses dragging, so it never reports a
            // click on its own — layer in click sensing so a plain tap (press +
            // release without a drag) opens the card detail.
            let response = response.interact(egui::Sense::click());
            if response.clicked() {
                *clicked = Some(card.id);
            }

            // Hover affordance: cards are clickable, so highlight the border and
            // switch to a pointing-hand cursor when the pointer is over one.
            if response.hovered() {
                ui.painter().rect_stroke(
                    response.rect,
                    egui::CornerRadius::same(RADIUS_MD as u8),
                    egui::Stroke::new(STROKE_MEDIUM, theme.border_strong),
                    egui::StrokeKind::Inside,
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            // While something hovers this card, draw an insertion line and record
            // the resulting row so a release lands there.
            if let (Some(pointer), Some(_payload)) = (
                ui.input(|i| i.pointer.interact_pos()),
                response.dnd_hover_payload::<DragCard>(),
            ) {
                let rect = response.rect;
                let stroke = egui::Stroke::new(STROKE_THIN, theme.accent);
                let insert_row = if pointer.y < rect.center().y {
                    ui.painter().hline(rect.x_range(), rect.top(), stroke);
                    row_idx
                } else {
                    ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                    row_idx + 1
                };
                hover_target = Some(insert_row);
            }
        }
    });

    // A release in this zone: use the hovered insertion row, else append to end.
    if let Some(payload) = dropped {
        let row = hover_target.unwrap_or(column.cards.len());
        *action = Some(BoardAction::Move {
            card_id: payload.0,
            col: col_idx,
            row,
        });
    }
}

/// Render a single card as a styled, draggable surface.
fn card_ui(ui: &mut egui::Ui, theme: &ColorTheme, card: &Card) {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(color) = card.label.map(|i| PALETTE[i % PALETTE.len()]) {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 4.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(RADIUS_PILL as u8), color);
                ui.add_space(SPACING_XS);
            }
            ui.label(egui::RichText::new(&card.title).color(theme.text_primary));

            // A one-line preview hints that the card has more detail behind it.
            if !card.description.is_empty() {
                ui.add_space(2.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&card.description)
                            .small()
                            .color(theme.text_muted),
                    )
                    .truncate(),
                );
            }
        });
}

/// A small rounded pill showing a count (e.g. cards in a column).
fn count_badge(ui: &mut egui::Ui, theme: &ColorTheme, n: usize) {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(n.to_string())
                    .small()
                    .color(theme.text_muted),
            );
        });
}

/// The inline "add a card" affordance for a column.
fn add_card_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut BoardUiState,
    col_idx: usize,
    action: &mut Option<BoardAction>,
) {
    if state.composing == Some(col_idx) {
        let edit = egui::TextEdit::multiline(&mut state.compose_text)
            .hint_text("Card title…")
            .desired_rows(2)
            .desired_width(f32::INFINITY);
        let edit_response = ui.add(edit);

        let submit_chord = edit_response.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);

        ui.horizontal(|ui| {
            let add = ui.button("Add").clicked() || submit_chord;
            let cancel =
                ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));

            if add {
                let title = state.compose_text.trim().to_string();
                if !title.is_empty() {
                    *action = Some(BoardAction::Add {
                        col: col_idx,
                        title,
                    });
                }
                state.compose_text.clear();
                state.composing = None;
            } else if cancel {
                state.compose_text.clear();
                state.composing = None;
            }
        });
    } else {
        let add = ui.add(
            egui::Button::new(egui::RichText::new("+ Add card").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false),
        );
        if add.clicked() {
            state.composing = Some(col_idx);
            state.compose_text.clear();
        }
    }
}

/// The card detail editor, shown as a responsive modal sheet while a card is
/// selected: a near-full-width sheet on narrow (mobile) viewports, a centered
/// card on wider ones. Edits the card in place; closing or deleting clears the
/// selection. Rendered as overlay layers (scrim + sheet) rather than a
/// draggable window so it feels native on touch.
fn card_detail_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    board: &mut Board,
    state: &mut BoardUiState,
) {
    let Some(card_id) = state.selected else {
        return;
    };

    // The selected card may have been removed elsewhere; drop a dangling
    // selection rather than rendering an empty sheet.
    if board.card_mut(card_id).is_none() {
        state.selected = None;
        return;
    }

    let screen = ui.ctx().screen_rect();
    // Narrow viewports get a near-full-width sheet; wider ones a centered modal.
    let compact = notedeck::ui::is_narrow(ui.ctx());
    let pad = SPACING_LG;
    let sheet_width = if compact {
        screen.width() - 2.0 * pad
    } else {
        460.0
    };
    // Cap the body so a long card stays on-screen and scrolls instead.
    let max_body_height = screen.height() - 6.0 * pad;

    let mut close = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
    let mut delete = false;

    // Dimmed scrim behind the sheet; a tap outside closes the detail.
    let scrim = egui::Area::new(egui::Id::new("headway-detail-scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ui.ctx(), |ui| {
            let resp = ui.allocate_response(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
            resp
        });
    if scrim.inner.clicked() {
        close = true;
    }

    egui::Area::new(egui::Id::new(("headway-detail", card_id)))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme.surface_primary)
                .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
                .corner_radius(egui::CornerRadius::same(RADIUS_LG as u8))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(120),
                })
                .inner_margin(egui::Margin::same(pad as i8))
                .show(ui, |ui| {
                    ui.set_width(sheet_width);

                    // Header: a label plus an explicit close button, since the
                    // sheet has no draggable window chrome to dismiss it.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Card").color(theme.text_muted));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let x =
                                egui::Button::new(egui::RichText::new("✕").color(theme.text_muted))
                                    .fill(egui::Color32::TRANSPARENT)
                                    .frame(false);
                            if ui.add(x).clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.add_space(SPACING_SM);

                    egui::ScrollArea::vertical()
                        .max_height(max_body_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            // Re-borrow inside the closure to satisfy the borrow checker.
                            let Some(card) = board.card_mut(card_id) else {
                                return;
                            };

                            ui.add(
                                egui::TextEdit::singleline(&mut card.title)
                                    .font(egui::TextStyle::Heading)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Title"),
                            );

                            ui.add_space(SPACING_MD);
                            ui.label(egui::RichText::new("Description").color(theme.text_muted));
                            ui.add_space(SPACING_XS);
                            ui.add(
                                egui::TextEdit::multiline(&mut card.description)
                                    .desired_rows(4)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Add more detail…"),
                            );

                            ui.add_space(SPACING_MD);
                            ui.label(egui::RichText::new("Label").color(theme.text_muted));
                            ui.add_space(SPACING_XS);
                            label_picker(ui, theme, card);

                            ui.add_space(SPACING_LG);
                            if ui
                                .button(egui::RichText::new("Delete card").color(theme.destructive))
                                .clicked()
                            {
                                delete = true;
                            }
                        });
                });
        });

    if delete {
        board.remove_card(card_id);
        state.selected = None;
    } else if close {
        state.selected = None;
    }
}

/// A wrapping row of color swatches (plus a "None" option) for choosing a
/// card's label color.
fn label_picker(ui: &mut egui::Ui, theme: &ColorTheme, card: &mut Card) {
    let radius = egui::CornerRadius::same(RADIUS_SM as u8);
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(card.label.is_none(), "None").clicked() {
            card.label = None;
        }
        for (i, color) in PALETTE.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
            ui.painter().rect_filled(rect, radius, *color);
            if card.label == Some(i) {
                ui.painter().rect_stroke(
                    rect,
                    radius,
                    egui::Stroke::new(2.0, theme.text_primary),
                    egui::StrokeKind::Inside,
                );
            }
            if resp.clicked() {
                card.label = Some(i);
            }
        }
    });
}
