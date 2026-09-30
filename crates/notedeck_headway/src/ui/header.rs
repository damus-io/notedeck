//! The board header: the private-relay sync indicator, the board switcher, the
//! "View" options menu and the filtered-count badge.

use notedeck::ColorTheme;
use notedeck::tokens::{RADIUS_PILL, SPACING_SM, SPACING_XS, STROKE_THIN};

use super::{BoardNav, BoardUiState, InlineEdit};
use crate::BoardSummary;
use crate::event::{self, BoardView};

/// Whether the board is reaching a private relay for cross-device sync, shown as
/// a small status dot in the board header. Derived each frame in
/// [`crate::Headway::render`] from the resolved private relay set and the relay
/// pool's live connection status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncStatus {
    /// No private relay configured — the board lives only on this device.
    LocalOnly,
    /// A private relay is configured but not currently connected, so edits
    /// aren't reaching the user's other devices yet.
    Offline,
    /// Connected to a private relay; edits sync across devices.
    Syncing,
}

/// A small colored dot + label leading the board header, telling the user
/// whether the board is syncing to a private relay. Hover reveals the detail and
/// the next step (mark a relay private / check the relay is online). Colors
/// mirror the relay-management status pill (`selection.bg_fill` / `warn_fg_color`).
pub(super) fn sync_indicator(ui: &mut egui::Ui, theme: &ColorTheme, status: SyncStatus) {
    // `filled` distinguishes an active relay (solid dot) from local-only (hollow
    // ring). The dot is painted rather than a ○/● glyph, whose font metrics sit
    // high and misalign with the adjacent text.
    let (filled, color, label, tip): (bool, egui::Color32, &str, &str) = match status {
        SyncStatus::Syncing => (
            true,
            ui.visuals().selection.bg_fill,
            "Synced",
            "Syncing to your private relay — edits reach your other devices.",
        ),
        SyncStatus::Offline => (
            true,
            ui.visuals().warn_fg_color,
            "Not connected",
            "A private relay is set but not connected, so this board isn't \
             syncing right now. Check that the relay is online in relay settings.",
        ),
        SyncStatus::LocalOnly => (
            false,
            theme.text_muted,
            "Local only",
            "This board lives only on this device. Mark a relay as private in \
             relay settings to sync it across your devices.",
        ),
    };
    let resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACING_XS;
            // Allocate over the text row height so the dot centers on the label.
            let radius = 4.0;
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(radius * 2.0, ui.text_style_height(&egui::TextStyle::Body)),
                egui::Sense::hover(),
            );
            if filled {
                ui.painter().circle_filled(rect.center(), radius, color);
            } else {
                ui.painter().circle_stroke(
                    rect.center(),
                    radius,
                    egui::Stroke::new(1.5_f32, color),
                );
            }
            ui.label(egui::RichText::new(label).color(color));
        })
        .response;
    resp.on_hover_text(tip);
}

/// Paint a small downward funnel — the "filtered" mark leading the
/// [`filtered_badge`] — `size` px wide, vertically centered on the current text
/// row, in `color`. Painted rather than drawn from a glyph because the bundled
/// font has no funnel (and the near symbols render as tofu), and because the
/// board already hand-paints its other status marks (the status circles, the
/// sync dot) for the same reason.
fn filter_funnel(ui: &mut egui::Ui, color: egui::Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(size, ui.text_style_height(&egui::TextStyle::Body)),
        egui::Sense::hover(),
    );
    // Normalised coordinates inside a `size`-square box centered in the row.
    let origin = rect.center() - egui::vec2(size, size) * 0.5;
    let p = |nx: f32, ny: f32| origin + egui::vec2(nx, ny) * size;
    let painter = ui.painter();
    // The bowl (wide mouth narrowing to the neck) and the stem below it, both
    // filled — the silhouette reads as a funnel even at this size.
    painter.add(egui::Shape::convex_polygon(
        vec![p(0.12, 0.24), p(0.88, 0.24), p(0.56, 0.55), p(0.44, 0.55)],
        color,
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![p(0.44, 0.55), p(0.56, 0.55), p(0.56, 0.82), p(0.44, 0.82)],
        color,
        egui::Stroke::NONE,
    ));
}

/// The header's "Filtered" affordance: an accent pill shown whenever the board
/// is narrowing what it displays ([`ViewFilter::is_active`](super::ViewFilter::is_active)). It states how many
/// of how many cards are showing, so a board narrowed by a search or a view
/// option never passes for the whole board — the gap the card
/// `injury-enlist-swarm` flagged. Returns the pill's response so the caller can
/// clear the narrowing on a click.
pub(super) fn filtered_badge(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    shown: usize,
    total: usize,
) -> egui::Response {
    // A filtered board is an *advisory* state — "heads up, you're not seeing
    // everything" — so it wears the theme's warm `warning` hue rather than the
    // raw selection purple, which read as garish against the neutral greys. A
    // faint tint plus a defined border makes a crisp chip that clearly stands
    // out without a heavy saturated fill (see [`label_chip`] for the fill-only
    // idiom this deliberately departs from).
    let accent = theme.warning;
    egui::Frame::new()
        .fill(accent.gamma_multiply(0.14))
        .stroke(egui::Stroke::new(STROKE_THIN, accent.gamma_multiply(0.55)))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = SPACING_XS;
            // The funnel leads the pill in the accent hue, tying the chip to the
            // "narrowing" idea before the words are read.
            filter_funnel(ui, accent, 11.0);
            ui.label(
                egui::RichText::new(format!("Filtered · {shown} of {total} shown"))
                    .small()
                    .strong()
                    .color(theme.text_primary),
            );
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Showing a narrowed view — click to clear the filter and view options")
}

/// The header's "View" menu: Linear-style display options for the board grid.
/// Today it holds a single toggle — hide sub-issue cards — but it's a menu, not a
/// lone checkbox, so further view options (grouping, ordering, hidden columns)
/// can slot in beside it. Mutates [`BoardUiState::hide_subissues`] directly and
/// stays open on toggle so several options can be flipped in one visit.
pub(super) fn view_options_menu(ui: &mut egui::Ui, theme: &ColorTheme, state: &mut BoardUiState) {
    // Tint the trigger when an option is active, so the menu itself signals that
    // the grid is being narrowed even before it's opened. It wears the same warm
    // `warning` hue as the [`filtered_badge`] pill — the two are the same signal
    // — rather than the raw selection purple.
    let label = if state.hide_subissues {
        egui::RichText::new("☰ View").strong().color(theme.warning)
    } else {
        egui::RichText::new("☰ View").color(theme.text_secondary)
    };
    let menu = ui.menu_button(label, |ui| {
        ui.checkbox(&mut state.hide_subissues, "Hide sub-issues")
            .on_hover_text(
                "Hide cards that are a sub-issue of another card. They still \
                 appear as checklist rows inside their parent.",
            );
    });
    // Open this frame: hold the board keys off it (see `grid_menu_open`).
    state.grid_menu_open |= menu.inner.is_some();
}

/// The board switcher in the header: the active board's title as a dropdown that
/// lists the account's boards (current one marked), with a "+ New board" entry
/// that opens an inline name composer. Requests are raised in [`BoardUiState::nav`]
/// for the app to act on; this function never mutates the board itself.
pub(super) fn board_switcher(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
) {
    // Naming a new board takes over the switcher with a single-line composer.
    if state.edit == InlineEdit::NewBoard {
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.edit_text)
                    .desired_width(220.0)
                    .hint_text("New board name…"),
            );
            if state.focus_edit {
                resp.request_focus();
                state.focus_edit = false;
            }
            let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let create = ui.button("Create").clicked();

            if escape {
                state.edit_text.clear();
                state.edit = InlineEdit::None;
            } else if submit || create {
                let title = state.edit_text.trim().to_string();
                state.edit_text.clear();
                state.edit = InlineEdit::None;
                if !title.is_empty() {
                    state.nav = Some(BoardNav::Create(title));
                }
            }
        });
        return;
    }

    let label = egui::RichText::new(format!("{}  ▾", view.title))
        .size(18.0)
        .strong()
        .color(theme.text_primary);
    let menu = ui.menu_button(label, |ui| {
        for board in boards {
            // Match the active board by coordinate (owner + slug), so a joined
            // board that shares a slug with the open one isn't marked current.
            let current = board.id == view.id && board.owner == view.author;
            if ui.selectable_label(current, &board.title).clicked() {
                if !current {
                    state.nav = Some(BoardNav::Switch(event::BoardCoord::new(
                        board.owner,
                        board.id.clone(),
                    )));
                }
                ui.close_menu();
            }
        }
        ui.separator();
        if ui
            .add(egui::Button::new("+ New board").frame(false))
            .clicked()
        {
            state.edit = InlineEdit::NewBoard;
            state.edit_text.clear();
            state.focus_edit = true;
            ui.close_menu();
        }
    });
    // Open this frame: hold the board keys off it (see `grid_menu_open`).
    state.grid_menu_open |= menu.inner.is_some();
}
