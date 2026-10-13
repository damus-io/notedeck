//! The archived-cards sheet, listing a board's archived cards for restore,
//! searchable with the board filter's query language and each one a way into
//! its (archived) detail.

use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_LG, SPACING_SM, SPACING_XS};

use super::BoardUiState;
use super::detail::{detail_scrim_ui, detail_sheet_frame};
use crate::event::{ArchivedCard, BoardView};
use crate::store::BoardAction;
use headway::query::{IssueFilter, ListRow, Placement};

/// The archived-cards sheet: an overlay listing cards taken off the board, each
/// with a Restore button that re-places it into the column it came from, under a
/// filter field read as an [`IssueFilter`] (`label:ui relay`), so a long archive
/// is searchable. A card's title opens its detail. Mirrors the detail sheet's
/// scrim + centered-card presentation.
pub(super) fn archived_sheet_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    if !state.showing_archived {
        return;
    }

    let screen = ui.ctx().content_rect();
    let pad = SPACING_LG;
    let sheet_width = if notedeck::ui::is_narrow(ui.ctx()) {
        screen.width() - 2.0 * pad
    } else {
        460.0
    };
    let max_body_height = screen.height() - 6.0 * pad;
    // Parsed each frame the sheet is open; an empty query allocates nothing.
    let filter = IssueFilter::parse(&state.archived_query, Some(&view.id));
    let mut open: Option<nostrdb_net::NoteId> = None;

    // Consume Escape so closing the sheet doesn't also fall through to Chrome's
    // Escape handler, which would toggle the side menu.
    let mut close = ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if detail_scrim_ui(ui, screen) {
        close = true;
    }

    egui::Area::new(egui::Id::unique("headway-archived"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            detail_sheet_frame(theme, pad).show(ui, |ui| {
                ui.set_width(sheet_width);
                ui.horizontal(|ui| {
                    ui.heading("Archived");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let x = egui::Button::new(egui::RichText::new("✕").color(theme.text_muted))
                            .fill(egui::Color32::TRANSPARENT)
                            .frame(false);
                        if ui.add(x).clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(SPACING_SM);
                let field = egui::TextEdit::singleline(&mut state.archived_query)
                    .id(archived_field_id())
                    .desired_width(f32::INFINITY)
                    .hint_text("Filter archived… e.g. label:bug perf");
                let field = ui.add(field);
                if std::mem::take(&mut state.focus_archived_query) {
                    field.request_focus();
                }
                ui.add_space(SPACING_SM);
                egui::ScrollArea::vertical()
                    .max_height(max_body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if view.archived.is_empty() {
                            ui.label(
                                egui::RichText::new("No archived cards.").color(theme.text_muted),
                            );
                        }
                        let mut shown = 0;
                        for entry in &view.archived {
                            let row = ListRow {
                                board: view,
                                card: &entry.card,
                                placement: Placement::Archived,
                            };
                            if !filter.matches(&row) {
                                continue;
                            }
                            shown += 1;
                            if archived_row_ui(ui, theme, view, entry, action) {
                                open = Some(entry.card.id);
                            }
                        }
                        if shown == 0 && !view.archived.is_empty() {
                            ui.label(
                                egui::RichText::new("No matching archived cards.")
                                    .color(theme.text_muted),
                            );
                        }
                    });
            });
        });

    // A title clicked opens that card's detail, which shows it archived with
    // its own Restore; the sheet steps aside for it.
    if let Some(card) = open {
        state.selected = Some(card);
        close = true;
    }
    if close {
        state.showing_archived = false;
    }
}

/// The archived sheet's filter field's pinned id, so the board filter's
/// `is:archived` hand-off can focus it.
fn archived_field_id() -> egui::Id {
    egui::Id::unique("headway-archived-filter-field")
}

/// One row in the archived sheet: the card's title, where it came from, and a
/// Restore button. Returns whether the title was clicked, to open the card.
fn archived_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    entry: &ArchivedCard,
    action: &mut Option<BoardAction>,
) -> bool {
    let mut open = false;
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let title = if entry.card.title.is_empty() {
                        "Untitled card"
                    } else {
                        &entry.card.title
                    };
                    let title = egui::Button::new(egui::RichText::new(title).strong()).frame(false);
                    open = ui
                        .add(title)
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked();
                    // Where it'll be restored to, resolved to the column's name.
                    let from = entry
                        .from
                        .as_deref()
                        .and_then(|id| view.columns.iter().find(|c| c.id == id))
                        .map(|c| c.name.as_str())
                        .unwrap_or("first column");
                    ui.label(
                        egui::RichText::new(format!("from {from}"))
                            .small()
                            .color(theme.text_muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Restore").clicked() {
                        *action = Some(BoardAction::RestoreCard {
                            card: entry.card.id,
                        });
                    }
                });
            });
        });
    ui.add_space(SPACING_XS);
    open
}
