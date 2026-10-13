//! Inline card/board widgets rendered by the `KindRenderer`s when a headway
//! event is referenced elsewhere in the app (e.g. inside a note or a chat).

use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_MD, SPACING_SM, SPACING_XS, STROKE_THIN};

use super::widgets::{StatusIcon, card_frame_ui, count_badge, status_icon_ui};
use crate::event::{self, BoardView, CardView, ColumnPos};

/// Render a card's *resolved* state (latest subject, labels and cover applied),
/// as folded off its board. This is the full-card block-embed shape
/// ([`notedeck::RenderContext::Embed`]), drawn for a surface that gives the
/// reference its own box — the markdown scanner asks for the
/// [`card_chip_ui`] chip instead, since it always splices within a run of text.
/// [`issue_inline_ui`] is only the fallback when the board can't be folded.
pub fn card_inline_ui(ui: &mut egui::Ui, theme: &ColorTheme, card: &CardView) -> egui::Response {
    card_frame_ui(ui, theme, &card.labels, &card.title, &card.description)
}

/// A compact, single-line inline reference to a card: a Linear-style status
/// icon (derived from the card's column position) followed by its title, in a
/// small rounded pill. This is the default in-prose shape
/// ([`notedeck::RenderContext::Inline`]) — versus the full [`card_inline_ui`]
/// card used for block embeds.
///
/// `status` is where the card sits, which picks its icon: its live column's,
/// an archive box when archived, or a muted backlog ring for a card on no
/// folded board. `chrome` is the surface's
/// [`InlineChrome`](notedeck::InlineChrome) hint: the pill, or the icon and
/// title alone.
///
/// Senses only hover: the renderer wraps it in [`notedeck::open_on_click`],
/// which adds the click and pointer cursor unless the surface asked for an
/// [`Inert`](notedeck::Interaction::Inert) chip.
pub fn card_chip_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    title: &str,
    status: ChipStatus,
    chrome: notedeck::InlineChrome,
) -> egui::Response {
    let icon = match status {
        ChipStatus::Column(pos) => StatusIcon::for_column(pos.index, pos.count),
        ChipStatus::Archived => StatusIcon::Archived,
        // Nothing folded to say where it is: the unstarted icon.
        ChipStatus::Unplaced => StatusIcon::Backlog,
    };
    notedeck_ui::inline_chip(ui, theme, title, chrome, |ui, size| {
        status_icon_ui(ui, theme, icon, size);
    })
}

/// Where a [`card_chip_ui`] chip's card sits, which picks its status icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipStatus {
    /// In a live column at this position.
    Column(ColumnPos),
    /// Archived off its board.
    Archived,
    /// On no folded board, so drawn from its creation-time snapshot.
    Unplaced,
}

impl ChipStatus {
    /// The status of a card found on a folded board, from its live column:
    /// `None` there means archived (see [`event::LocatedCard::column`]).
    pub fn located(column: Option<ColumnPos>) -> ChipStatus {
        column.map_or(ChipStatus::Archived, ChipStatus::Column)
    }
}

/// Render a single headway issue (kind 1621) from its *creation-time* snapshot:
/// the subject, body and inline labels on the 1621 note itself, before any later
/// rename/label/cover edits. Used only as a fallback for [`card_inline_ui`] when
/// the owning board isn't available locally to fold.
pub fn issue_inline_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    issue: &event::IssueEvent,
) -> egui::Response {
    card_frame_ui(ui, theme, &issue.inline_labels, &issue.subject, &issue.body)
}

/// Render a headway board (kind 30619) as a compact, read-only summary: the
/// title, an optional description preview, and a column-name + card-count chip
/// per column.
pub fn board_inline_ui(ui: &mut egui::Ui, theme: &ColorTheme, view: &BoardView) -> egui::Response {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            // Force left alignment; the notebook lays node content out centered.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new(&view.title)
                        .strong()
                        .color(theme.text_primary),
                );
                if !view.description.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&view.description)
                                .small()
                                .color(theme.text_muted),
                        )
                        .truncate(),
                    );
                }
                ui.add_space(SPACING_XS);
                ui.horizontal_wrapped(|ui| {
                    for col in &view.columns {
                        ui.label(
                            egui::RichText::new(&col.name)
                                .small()
                                .color(theme.text_secondary),
                        );
                        count_badge(ui, theme, col.cards.len());
                        ui.add_space(SPACING_SM);
                    }
                });
            });
        })
        .response
}
