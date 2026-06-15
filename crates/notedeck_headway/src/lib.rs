use enostr::NoteId;
use nostrdb::Transaction;
use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS,
    STROKE_MEDIUM, STROKE_THIN,
};
use notedeck::{App, AppContext, AppResponse, ColorTheme};

pub mod event;
pub mod store;

use event::{BoardView, CardView, ColumnView};
use store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// A Linear/Trello-style issue & todo tracker app for notedeck.
///
/// The board is backed by nostr events in the local nostrdb: a board is loaded
/// each frame with [`store::load_board`] and reduced into a [`BoardView`], and
/// every edit is turned into a signed event that is ingested locally (see
/// [`store`]). There is deliberately no relay publishing yet.
pub struct Headway {
    /// Which board this instance manages (single board for now).
    board_id: String,
    /// Transient, per-board UI state.
    state: BoardUiState,
    /// Whether we've already auto-seeded a board this session, so we don't try
    /// to seed twice while the first seed is still materialising.
    seeded: bool,
    /// Countdown of follow-up reloads after an async ingest, so a just-written
    /// event surfaces without waiting for the next user interaction.
    refresh_frames: u8,
}

impl Default for Headway {
    fn default() -> Self {
        Self {
            board_id: store::BOARD_ID.to_string(),
            state: BoardUiState::default(),
            seeded: false,
            refresh_frames: 0,
        }
    }
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
    selected: Option<NoteId>,
    /// Which card the detail edit buffers below were seeded from. When this
    /// differs from `selected`, the buffers are refreshed from the board.
    detail_for: Option<NoteId>,
    /// Edit buffer for the selected card's title.
    detail_title: String,
    /// Edit buffer for the selected card's description.
    detail_desc: String,
    /// Buffer backing the "add label" field in the detail sheet.
    new_label: String,
    /// The column index whose title is being renamed inline, if any.
    renaming: Option<usize>,
    /// Buffer backing the column-rename text field.
    rename_text: String,
    /// Set when a rename has just started, to grab focus once.
    focus_rename: bool,
    /// Whether the "add column" composer is open.
    adding_column: bool,
    /// Buffer backing the new-column text field.
    column_text: String,
    /// Set when the add-column composer has just opened, to grab focus once.
    focus_column: bool,
}

/// Drag-and-drop payload: the id of the card being dragged.
#[derive(Clone)]
struct DragCard(NoteId);

impl Headway {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule several follow-up reloads so a just-ingested event (ingest is
    /// async, on a writer thread) shows up promptly.
    fn bump_refresh(&mut self) {
        self.refresh_frames = 8;
    }

    /// Burn down the refresh countdown, requesting a delayed repaint each step.
    fn pump_refresh(&mut self, ui: &egui::Ui) {
        if self.refresh_frames > 0 {
            self.refresh_frames -= 1;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}

impl App for Headway {
    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let theme = ColorTheme::current(ui.ctx());

        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Load the current board view out of the local nostrdb.
        let view = Transaction::new(ctx.ndb)
            .ok()
            .and_then(|txn| store::load_board(ctx.ndb, &txn, &author, &self.board_id));

        let Some(view) = view else {
            // No board yet: auto-seed one for an account that can sign.
            match &signer {
                Some(secret) => {
                    if !self.seeded {
                        store::seed_default_board(ctx.ndb, &author, secret, &self.board_id);
                        self.seeded = true;
                        self.bump_refresh();
                    }
                    empty_state(ui, &theme, "Setting up your board…");
                }
                None => empty_state(
                    ui,
                    &theme,
                    "Sign in with a key to create your Headway board.",
                ),
            }
            self.pump_refresh(ui);
            return AppResponse::default();
        };

        let state = &mut self.state;
        let mut action: Option<BoardAction> = None;
        // The card a click landed on this frame; opens the detail view below.
        let mut clicked: Option<NoteId> = None;

        egui::Frame::new()
            .inner_margin(egui::Margin::same(SPACING_LG as i8))
            .show(ui, |ui| {
                // Board header: title plus a muted summary of its contents.
                ui.heading(&view.title);
                ui.add_space(SPACING_XS);
                let total: usize = view.columns.iter().map(|c| c.cards.len()).sum();
                ui.label(
                    egui::RichText::new(format!(
                        "{total} card{} · {} columns",
                        if total == 1 { "" } else { "s" },
                        view.columns.len()
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
                            for col_idx in 0..view.columns.len() {
                                column_ui(
                                    ui,
                                    &theme,
                                    &view,
                                    state,
                                    col_idx,
                                    &mut action,
                                    &mut clicked,
                                );
                            }
                            add_column_ui(ui, &theme, state, &mut action);
                        });
                    });
            });

        if let Some(card_id) = clicked {
            state.selected = Some(card_id);
        }

        // Detail view floats above the board; emits edit actions like the rest.
        card_detail_ui(ui, &theme, &view, state, &mut action);

        // Apply the collected action by ingesting events locally. Mutations need
        // a signing key; a watch-only account simply can't edit.
        if let (Some(action), Some(secret)) = (action, &signer) {
            store::apply(ctx.ndb, &self.board_id, &view, &author, secret, action);
            self.bump_refresh();
        }

        self.pump_refresh(ui);
        AppResponse::default()
    }
}

/// A centered, muted message shown when there's no board to render yet.
fn empty_state(ui: &mut egui::Ui, theme: &ColorTheme, message: &str) {
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

/// Find a card anywhere on the board, returning its column index and view.
fn find_card(view: &BoardView, card: NoteId) -> Option<(usize, &CardView)> {
    view.columns
        .iter()
        .enumerate()
        .find_map(|(i, col)| col.cards.iter().find(|c| c.id == card).map(|c| (i, c)))
}

/// Render one column: header, the draggable card list (a drop zone), and the
/// add-card composer.
fn column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
    col_idx: usize,
    action: &mut Option<BoardAction>,
    clicked: &mut Option<NoteId>,
) {
    let column = &view.columns[col_idx];
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
                // Header: title (editable inline) + count badge + a "⋯" menu
                // for renaming, reordering and deleting the column.
                ui.horizontal(|ui| {
                    if state.renaming == Some(col_idx) {
                        column_rename_field(ui, state, col_idx, action);
                    } else {
                        ui.label(egui::RichText::new(&column.name).strong());
                        count_badge(ui, theme, column.cards.len());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            column_menu(ui, theme, state, view, col_idx, action)
                        });
                    }
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
    column: &ColumnView,
    col_idx: usize,
    action: &mut Option<BoardAction>,
    clicked: &mut Option<NoteId>,
) {
    let frame = egui::Frame::new().inner_margin(egui::Margin::same(SPACING_XS as i8));

    // Tracks where a release would land (also used to paint the insertion line).
    let mut hover_target: Option<usize> = None;

    // Grow the drop zone to fill the column body so empty/sparse lists still
    // present a generous target, reserving room for the add-card affordance
    // that follows. Falls back to a small minimum when space is tight.
    let reserve = 2.0 * SPACING_LG;
    let fill_height = (ui.available_height() - reserve).max(SPACING_LG);

    let fill_width = ui.available_width();

    // Detect drops over a bare, transparent frame rather than `dnd_drop_zone`,
    // which always paints a highlight box around the whole lane. The accent
    // insertion line is the only feedback we want.
    let zone = frame
        .show(ui, |ui| {
            // Fill the column body in both axes so the drop target spans the whole
            // lane — otherwise an empty column collapses to a narrow vertical strip.
            ui.set_min_height(fill_height);
            ui.set_min_width(fill_width);
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
        })
        .response;

    // Empty columns have no card to anchor an insertion line against; draw one
    // at the top of the bare lane while a card hovers so there's still feedback.
    if column.cards.is_empty() && zone.dnd_hover_payload::<DragCard>().is_some() {
        let rect = zone.rect;
        let inset = SPACING_SM;
        ui.painter().hline(
            (rect.left() + inset)..=(rect.right() - inset),
            rect.top() + inset,
            egui::Stroke::new(STROKE_THIN, theme.accent),
        );
    }

    // A release in this zone: use the hovered insertion row, else append to end.
    if let Some(payload) = zone.dnd_release_payload::<DragCard>() {
        let row = hover_target.unwrap_or(column.cards.len());
        *action = Some(BoardAction::MoveCard {
            card: payload.0,
            to_col: col_idx,
            to_row: row,
        });
    }
}

/// Render a single card as a styled, draggable surface.
fn card_ui(ui: &mut egui::Ui, theme: &ColorTheme, card: &CardView) {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !card.labels.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for label in &card.labels {
                        label_chip(ui, theme, label);
                    }
                });
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

/// A deterministic color for a label, derived from its text.
fn label_color(label: &str) -> egui::Color32 {
    let mut h: usize = 0;
    for b in label.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as usize);
    }
    PALETTE[h % PALETTE.len()]
}

/// A small colored pill showing a label's text.
fn label_chip(ui: &mut egui::Ui, theme: &ColorTheme, label: &str) {
    let color = label_color(label);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.30))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).small().color(theme.text_primary));
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
                    *action = Some(BoardAction::AddCard {
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

/// The inline text field shown in a column header while renaming. Commits on
/// Enter or focus loss, cancels on Escape.
fn column_rename_field(
    ui: &mut egui::Ui,
    state: &mut BoardUiState,
    col_idx: usize,
    action: &mut Option<BoardAction>,
) {
    let resp = ui.add(
        egui::TextEdit::singleline(&mut state.rename_text)
            .desired_width(f32::INFINITY)
            .hint_text("Column title…"),
    );
    if state.focus_rename {
        resp.request_focus();
        state.focus_rename = false;
    }

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.renaming = None;
    } else if resp.lost_focus() {
        let name = state.rename_text.trim().to_string();
        if !name.is_empty() {
            *action = Some(BoardAction::RenameColumn { col: col_idx, name });
        }
        state.renaming = None;
    }
}

/// The "⋯" overflow menu in a column header: rename, reorder, delete.
fn column_menu(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut BoardUiState,
    view: &BoardView,
    col_idx: usize,
    action: &mut Option<BoardAction>,
) {
    let n = view.columns.len();
    ui.menu_button("⋯", |ui| {
        if ui.button("Rename").clicked() {
            state.rename_text = view.columns[col_idx].name.clone();
            state.renaming = Some(col_idx);
            state.focus_rename = true;
            ui.close_menu();
        }
        if ui
            .add_enabled(col_idx > 0, egui::Button::new("Move left"))
            .clicked()
        {
            *action = Some(BoardAction::MoveColumn {
                from: col_idx,
                to: col_idx - 1,
            });
            ui.close_menu();
        }
        if ui
            .add_enabled(col_idx + 1 < n, egui::Button::new("Move right"))
            .clicked()
        {
            *action = Some(BoardAction::MoveColumn {
                from: col_idx,
                to: col_idx + 1,
            });
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(egui::RichText::new("Delete column").color(theme.destructive))
            .clicked()
        {
            *action = Some(BoardAction::RemoveColumn { col: col_idx });
            ui.close_menu();
        }
    });
}

/// The "add a column" affordance at the right end of the board: a ghost column
/// that expands into a title composer when clicked.
fn add_column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(RADIUS_LG as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(COLUMN_WIDTH);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                if state.adding_column {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.column_text)
                            .desired_width(f32::INFINITY)
                            .hint_text("Column title…"),
                    );
                    if state.focus_column {
                        resp.request_focus();
                        state.focus_column = false;
                    }
                    let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    ui.add_space(SPACING_SM);
                    ui.horizontal(|ui| {
                        let add = ui.button("Add").clicked() || submit;
                        let cancel = ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if add {
                            let name = state.column_text.trim().to_string();
                            if !name.is_empty() {
                                *action = Some(BoardAction::AddColumn { name });
                            }
                            state.column_text.clear();
                            state.adding_column = false;
                        } else if cancel {
                            state.column_text.clear();
                            state.adding_column = false;
                        }
                    });
                } else {
                    let add = ui.add(
                        egui::Button::new(
                            egui::RichText::new("+ Add column").color(theme.text_muted),
                        )
                        .fill(egui::Color32::TRANSPARENT)
                        .frame(false),
                    );
                    if add.clicked() {
                        state.adding_column = true;
                        state.column_text.clear();
                        state.focus_column = true;
                    }
                }
            });
        });
}

/// The card detail editor, shown as a responsive modal sheet while a card is
/// selected: a near-full-width sheet on narrow (mobile) viewports, a centered
/// card on wider ones. Edits are emitted as [`BoardAction`]s (title/description
/// commit on focus loss); closing clears the selection. Rendered as overlay
/// layers (scrim + sheet) rather than a draggable window so it feels native on
/// touch.
fn card_detail_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    let Some(card_id) = state.selected else {
        return;
    };

    // The selected card may have been removed elsewhere; drop a dangling
    // selection rather than rendering an empty sheet.
    let Some((current_col, card)) = find_card(view, card_id) else {
        state.selected = None;
        state.detail_for = None;
        return;
    };

    // (Re)seed the edit buffers when the open card changes. The board is
    // immutable here, so live editing happens against these buffers and is
    // committed as events on focus loss.
    if state.detail_for != Some(card_id) {
        state.detail_for = Some(card_id);
        state.detail_title = card.title.clone();
        state.detail_desc = card.description.clone();
        state.new_label.clear();
    }

    let card_title = card.title.clone();
    let card_desc = card.description.clone();
    let card_labels = card.labels.clone();

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
    let mut move_to: Option<usize> = None;
    let mut add_label = false;

    // Owned copy so the body can render the status pill and column chips.
    let columns: Vec<String> = view.columns.iter().map(|c| c.name.clone()).collect();

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
                        status_pill(ui, theme, &columns[current_col]);
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
                            let title_resp = ui.add(
                                egui::TextEdit::singleline(&mut state.detail_title)
                                    .font(egui::TextStyle::Heading)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Title"),
                            );
                            if title_resp.lost_focus() {
                                let title = state.detail_title.trim().to_string();
                                if !title.is_empty() && title != card_title {
                                    *action = Some(BoardAction::EditTitle {
                                        card: card_id,
                                        title,
                                    });
                                }
                            }

                            ui.add_space(SPACING_MD);
                            section_label(ui, theme, "Description");
                            ui.add_space(SPACING_XS);
                            let desc_resp = ui.add(
                                egui::TextEdit::multiline(&mut state.detail_desc)
                                    .desired_rows(4)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Add more detail…"),
                            );
                            if desc_resp.lost_focus() && state.detail_desc != card_desc {
                                *action = Some(BoardAction::EditDescription {
                                    card: card_id,
                                    description: state.detail_desc.clone(),
                                });
                            }

                            ui.add_space(SPACING_MD);
                            section_label(ui, theme, "Labels");
                            ui.add_space(SPACING_XS);
                            ui.horizontal_wrapped(|ui| {
                                for label in &card_labels {
                                    label_chip(ui, theme, label);
                                }
                            });
                            ui.add_space(SPACING_XS);
                            ui.horizontal(|ui| {
                                let field = ui.add(
                                    egui::TextEdit::singleline(&mut state.new_label)
                                        .desired_width(140.0)
                                        .hint_text("Add label…"),
                                );
                                let submit = field.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if ui.button("Add").clicked() || submit {
                                    add_label = true;
                                }
                            });

                            // Move the card between lanes without dragging.
                            if columns.len() > 1 {
                                ui.add_space(SPACING_MD);
                                section_label(ui, theme, "Status");
                                ui.add_space(SPACING_XS);
                                ui.horizontal_wrapped(|ui| {
                                    for (i, name) in columns.iter().enumerate() {
                                        let selected = i == current_col;
                                        if ui.selectable_label(selected, name).clicked()
                                            && !selected
                                        {
                                            move_to = Some(i);
                                        }
                                    }
                                });
                            }

                            ui.add_space(SPACING_MD);
                            ui.separator();
                            ui.add_space(SPACING_SM);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .button(
                                            egui::RichText::new("Delete card")
                                                .color(theme.destructive),
                                        )
                                        .clicked()
                                    {
                                        delete = true;
                                    }
                                },
                            );
                        });
                });
        });

    // Resolve the high-level outcomes after the sheet closure. Explicit buttons
    // win over an incidental focus-loss edit collected above.
    if delete {
        *action = Some(BoardAction::DeleteCard { card: card_id });
        state.selected = None;
        state.detail_for = None;
    } else if let Some(to) = move_to {
        let to_row = view.columns[to].cards.len();
        *action = Some(BoardAction::MoveCard {
            card: card_id,
            to_col: to,
            to_row,
        });
    } else if add_label {
        let new = state.new_label.trim().to_string();
        if !new.is_empty() && !card_labels.contains(&new) {
            let mut labels = card_labels.clone();
            labels.push(new);
            *action = Some(BoardAction::SetLabels {
                card: card_id,
                labels,
            });
        }
        state.new_label.clear();
    } else if close {
        state.selected = None;
        state.detail_for = None;
    }
}

/// A small muted, uppercase section heading used inside the card detail sheet.
fn section_label(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(theme.text_muted),
    );
}

/// A filled pill showing the card's current column (status) in the sheet header.
fn status_pill(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .color(theme.text_secondary),
            );
        });
}
