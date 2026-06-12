use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_SM, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS,
    STROKE_THIN,
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

        egui::Frame::new()
            .inner_margin(egui::Margin::same(SPACING_LG as i8))
            .show(ui, |ui| {
                ui.heading(&board.title);
                ui.add_space(SPACING_MD);

                egui::ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = SPACING_MD;
                            for col_idx in 0..board.columns.len() {
                                column_ui(ui, &theme, board, state, col_idx, &mut action);
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

            // Header: title + card count.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&column.title).strong());
                ui.label(
                    egui::RichText::new(format!("{}", column.cards.len())).color(theme.text_muted),
                );
            });
            ui.add_space(SPACING_SM);

            egui::ScrollArea::vertical()
                .id_salt(("headway-col", col_idx))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    cards_drop_zone(ui, theme, column, col_idx, action);
                    ui.add_space(SPACING_SM);
                    add_card_ui(ui, theme, state, col_idx, action);
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
                let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 4.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(RADIUS_SM as u8), color);
                ui.add_space(SPACING_XS);
            }
            ui.label(egui::RichText::new(&card.title).color(theme.text_primary));
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
