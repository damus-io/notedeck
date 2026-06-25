//! egui rendering for the Headway board.
//!
//! Split out of `lib.rs` to keep that file focused on the data/reducer layer
//! (the [`crate::Headway`] app, its [`crate::BoardSync`], the inline-render
//! cache and the `KindRenderer` impls). Everything here is pure egui rendering
//! plus the transient per-board UI state it threads through; none of it touches
//! the nostrdb subscription/fold machinery.

use std::collections::{HashMap, HashSet};

use enostr::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS,
    STROKE_MEDIUM, STROKE_THIN,
};

use crate::event::{self, ArchivedCard, BoardView, CardView, ColumnView, CommentView};
use crate::store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// Max width the full-pane card detail body is constrained to, so a card reads
/// comfortably instead of stretching across a wide window.
const DETAIL_CONTENT_WIDTH: f32 = 760.0;

/// Width of the comment thread's right rail in the two-column detail layout.
const DETAIL_COMMENTS_WIDTH: f32 = 380.0;

/// Pane width at or above which the detail view splits into two columns (body
/// left, comment thread right); below it the thread stacks under the body.
const DETAIL_TWO_COL_MIN: f32 = 1024.0;

/// How long a card takes to slide from its old slot to its new one when it
/// jumps columns (e.g. a `headway move` landing from the CLI).
const MOVE_ANIM_SECS: f32 = 0.28;

/// Transient, per-board UI state that must persist across frames but isn't part
/// of the data model (e.g. which column has an open "add card" composer).
#[derive(Default)]
pub struct BoardUiState {
    /// Which inline text editor is open on the board, if any.
    edit: InlineEdit,
    /// Shared buffer backing whichever inline editor ([`InlineEdit`]) is open;
    /// only one can be active at a time.
    edit_text: String,
    /// Set when the active inline editor should grab focus once — on open, and
    /// after each card is added, so rapid keyboard entry keeps working.
    focus_edit: bool,
    /// The card whose detail view is open, if any.
    selected: Option<NoteId>,
    /// Which card the detail edit buffers below were seeded from. When this
    /// differs from `selected`, the buffers are refreshed from the board.
    detail_for: Option<NoteId>,
    /// Edit buffer for the selected card's title.
    detail_title: String,
    /// Whether the title is shown rendered or in its raw editor.
    detail_title_mode: EditMode,
    /// Edit buffer for the selected card's description.
    detail_desc: String,
    /// Whether the description is shown rendered or in its raw editor.
    detail_desc_mode: EditMode,
    /// Buffer backing the "add label" field in the detail sheet.
    new_label: String,
    /// Buffer backing the "write a comment" composer in the detail pane.
    comment_draft: String,
    /// Whether the archived-cards sheet is open.
    showing_archived: bool,
    /// Where each card was drawn last frame (screen rect + column), so a card
    /// that has jumped to a new column since can be animated sliding in from its
    /// previous slot rather than teleporting.
    card_pos: HashMap<NoteId, CardPos>,
    /// Cards mid-slide, mapped to the screen rect they're sliding *from*. The
    /// 0→1 progress itself lives in egui's animation manager, keyed by card id
    /// (see [`move_progress_id`]); this only remembers the origin slot.
    moves: HashMap<NoteId, egui::Rect>,
    /// Cards whose next cross-column landing should *not* slide, because the user
    /// just dragged them there by hand — the pointer already carried the card
    /// across, so a slide is redundant. Only relay/CLI moves (which the user
    /// didn't physically perform) animate. A *set*, not a single id, because the
    /// flag must outlive the async ingest between the drop and the folded view
    /// reporting the new column: a second drag can be dropped before the first
    /// move lands, leaving two cards awaiting their move at once. Each entry is
    /// consumed when its move is observed (see [`start_move_anims`]).
    suppress_anim: HashSet<NoteId>,
}

/// A card's on-screen placement last frame: its screen rect and which column it
/// sat in. Used to detect cross-column jumps (drags, detail-sheet moves, or a
/// `headway move` arriving over the relay) and seed the slide animation.
///
/// The column is identified by a hash of its stable id, not its index: indices
/// shift when columns are reordered or removed, which would otherwise read as
/// every card in the board jumping at once.
#[derive(Clone, Copy)]
struct CardPos {
    rect: egui::Rect,
    col: egui::Id,
}

/// The board's inline text editors are mutually exclusive — you can only be
/// composing a card, renaming a column, or adding a column at any one moment —
/// so they share [`BoardUiState::edit_text`] and [`BoardUiState::focus_edit`]
/// and this enum tracks which (if any) is live.
#[derive(Default, PartialEq, Eq)]
enum InlineEdit {
    /// No inline editor open.
    #[default]
    None,
    /// Composing a new card in the column at this index.
    AddCard(usize),
    /// Renaming the column at this index.
    RenameColumn(usize),
    /// Composing a new column.
    AddColumn,
}

/// How the detail sheet shows an editable field (title or description): the
/// finished, read-only render or its raw text editor. The states are mutually
/// exclusive and the one-shot focus grab only has meaning while editing, so an
/// enum models it more honestly than a pair of bools.
#[derive(Default, PartialEq, Eq)]
enum EditMode {
    /// The read-only render — a heading for the title, markdown for the
    /// description — with an affordance to switch into the editor.
    #[default]
    Rendered,
    /// The raw text editor. `focus` requests a one-shot keyboard-focus grab on
    /// the frame the editor opens.
    Editing { focus: bool },
}

/// Pick the opening mode for an editable field: blank fields drop straight into
/// the editor (nothing to render), populated ones show their render first.
fn seed_edit_mode(text: &str) -> EditMode {
    if text.trim().is_empty() {
        EditMode::Editing { focus: false }
    } else {
        EditMode::Rendered
    }
}

/// Drag-and-drop payload: the id of the card being dragged.
#[derive(Clone)]
struct DragCard(NoteId);

/// Render the board (header, columns, the add-column affordance and the floating
/// card detail sheet) and return the edit the user made this frame, if any.
pub fn board_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    // A selected card takes over the whole view as a full-pane detail screen,
    // replacing the board grid until dismissed (back / ✕ / Escape). A selection
    // pointing at a card that no longer exists is dropped so we fall back to the
    // board rather than render an empty pane.
    if state
        .selected
        .is_some_and(|id| find_card(view, id).is_some())
    {
        let mut action: Option<BoardAction> = None;
        card_detail_pane_ui(ui, theme, note_context, view, state, &mut action);
        return action;
    }
    if state.selected.take().is_some() {
        state.detail_for = None;
    }

    let mut action: Option<BoardAction> = None;
    // The card a click landed on this frame; opens the detail view next frame.
    let mut clicked: Option<NoteId> = None;

    // Kick off (and retire) slide animations for any card that changed columns
    // since last frame. This reads last frame's placements, so afterwards we can
    // clear them (keeping the map's capacity) and let the card renderers refill
    // `card_pos` with this frame's rects as they go.
    start_move_anims(ui.ctx(), view, state);
    state.card_pos.clear();

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            // Board header: a muted summary of its contents. The board title is
            // redundant with the app's own chrome, so it isn't repeated here.
            let total: usize = view.columns.iter().map(|c| c.cards.len()).sum();
            let summary = egui::RichText::new(format!(
                "{total} card{} · {} columns",
                if total == 1 { "" } else { "s" },
                view.columns.len()
            ))
            .color(theme.text_muted);
            // Keep the header untouched when nothing is archived; only grow a row
            // (with the entry point to the archived sheet) when there's something.
            if view.archived.is_empty() {
                ui.label(summary);
            } else {
                ui.horizontal(|ui| {
                    ui.label(summary);
                    ui.add_space(SPACING_SM);
                    let label =
                        egui::RichText::new(format!("View archived ({})", view.archived.len()))
                            .color(theme.text_muted);
                    if ui
                        .add(egui::Button::new(label).fill(egui::Color32::TRANSPARENT))
                        .clicked()
                    {
                        state.showing_archived = true;
                    }
                });
            }
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_MD);

            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = SPACING_MD;
                        for col_idx in 0..view.columns.len() {
                            column_ui(ui, theme, view, state, col_idx, &mut action, &mut clicked);
                        }
                        add_column_ui(ui, theme, state, &mut action);
                    });
                });
        });

    if let Some(card_id) = clicked {
        state.selected = Some(card_id);
    }

    // Archived-cards sheet floats above the board.
    archived_sheet_ui(ui, theme, view, state, &mut action);

    action
}

/// A centered, muted message shown when there's no board to render yet.
pub fn empty_state(ui: &mut egui::Ui, theme: &ColorTheme, message: &str) {
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

/// The egui animation-manager id holding a card's 0→1 move-slide progress.
fn move_progress_id(card: &NoteId) -> egui::Id {
    egui::Id::new(("headway-move", card))
}

/// Begin (and retire) card slide animations for this frame.
///
/// A slide starts when a card lands in a different column than it occupied last
/// frame — a drag release, a detail-sheet move, or a `headway move` arriving
/// over the relay all look the same here: the folded view simply reports the
/// card in a new column. We remember the screen rect it left from; the 0→1
/// clock lives in egui's animation manager (seeded to 0 so its first read
/// animates instead of snapping to the target). Finished slides are dropped so
/// the card renders normally again.
fn start_move_anims(ctx: &egui::Context, view: &BoardView, state: &mut BoardUiState) {
    for col in &view.columns {
        let col_key = egui::Id::new(&col.id);
        for card in &col.cards {
            let Some(prev) = state.card_pos.get(&card.id) else {
                continue;
            };
            if prev.col != col_key && !state.moves.contains_key(&card.id) {
                // A manual drag already carried the card across columns, so land
                // it in place rather than sliding it again. Consume the flag here
                // (not on a timer) so it survives the async ingest latency until
                // the move is actually observed.
                if state.suppress_anim.remove(&card.id) {
                    continue;
                }
                state.moves.insert(card.id, prev.rect);
                // Snap the clock to 0 (zero animation time forces a reset even if
                // a prior slide left this id parked at 1.0), so the read below
                // animates 0→1 instead of snapping straight to the target.
                ctx.animate_value_with_time(move_progress_id(&card.id), 0.0, 0.0);
            }
        }
    }
    state.moves.retain(|id, _| {
        ctx.animate_value_with_time(move_progress_id(id), 1.0, MOVE_ANIM_SECS) < 1.0
    });
}

/// Paint a card sliding from `from` toward its final slot `dest`, on a
/// foreground layer so it travels across column (and scroll-area) boundaries
/// unclipped. This is the card's only rendering while it's in flight — the lane
/// merely reserves the `dest` slot. `t` is the raw 0→1 progress; easing here.
fn draw_moving_card(
    ui: &egui::Ui,
    theme: &ColorTheme,
    card: &CardView,
    from: egui::Rect,
    dest: egui::Rect,
    t: f32,
) {
    let pos = from.min + (dest.min - from.min) * egui::emath::easing::cubic_out(t);
    egui::Area::new(egui::Id::new(("headway-move-ghost", card.id)))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ui.ctx(), |ui| {
            ui.set_width(dest.width());
            card_ui(ui, theme, card);
        });
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
    // Fit the column to the height its parent gives us, *minus* this frame's own
    // top+bottom inner margin. Sizing to the full available height made the frame
    // (margins included) a margin taller than its slot, so the bottom of the card
    // list — and the add-card button — spilled past the board's padding and got
    // clipped (worse the more the UI was zoomed in). Floor at zero so a tiny
    // viewport still lays out.
    let height = (ui.available_height() - 2.0 * SPACING_SM).max(0.0);

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
                    if state.edit == InlineEdit::RenameColumn(col_idx) {
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
                        cards_drop_zone(ui, theme, column, state, col_idx, action, clicked);
                    });
            });
        });
}

/// The drop zone wrapping a column's cards, with live insertion-line feedback.
fn cards_drop_zone(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    column: &ColumnView,
    state: &mut BoardUiState,
    col_idx: usize,
    action: &mut Option<BoardAction>,
    clicked: &mut Option<NoteId>,
) {
    let frame = egui::Frame::new().inner_margin(egui::Margin::same(SPACING_XS as i8));

    // Tracks where a release would land (also used to paint the insertion line).
    let mut hover_target: Option<usize> = None;

    // Fill the column body in both axes so the drop target spans the whole lane
    // — a release anywhere in the column lands the card, and empty/sparse lanes
    // still present a generous target instead of a narrow strip. The add-card
    // affordance is rendered inside the zone right beneath the cards, so it stays
    // reachable while the filled space below it remains a valid drop target.
    let fill_height = ui.available_height();
    let fill_width = ui.available_width();

    // Detect drops over a bare, transparent frame rather than `dnd_drop_zone`,
    // which always paints a highlight box around the whole lane. The accent
    // insertion line is the only feedback we want.
    let zone = frame
        .show(ui, |ui| {
            ui.set_min_height(fill_height);
            ui.set_min_width(fill_width);
            ui.spacing_mut().item_spacing.y = SPACING_SM;

            for (row_idx, card) in column.cards.iter().enumerate() {
                // A card mid-slide is drawn once, in flight, on an unclipped
                // foreground layer so it can cross column boundaries. Here we only
                // reserve its destination slot (the card's size is stable across a
                // move, so last frame's rect sizes it) — the lane lays out around
                // the gap, and the card "lands" into it as the slide completes.
                if let Some(from) = state.moves.get(&card.id).copied() {
                    let (dest, _) = ui.allocate_exact_size(from.size(), egui::Sense::hover());
                    state.card_pos.insert(
                        card.id,
                        CardPos {
                            rect: dest,
                            col: egui::Id::new(&column.id),
                        },
                    );
                    let t = ui.ctx().animate_value_with_time(
                        move_progress_id(&card.id),
                        1.0,
                        MOVE_ANIM_SECS,
                    );
                    draw_moving_card(ui, theme, card, from, dest, t);
                    continue;
                }

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

                // Right-click to copy a pasteable `nostr:nevent…` reference to the
                // issue (e.g. for embedding in a notebook note).
                notedeck_ui::context_menu::context_menu(&response, |ui| {
                    if ui.button("Copy Id").clicked() {
                        if let Some(uri) = issue_nostr_uri(&card.id) {
                            ui.ctx().copy_text(uri);
                        }
                        ui.close_menu();
                    }
                });

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

                // Remember where this card landed so next frame can tell if it
                // jumped columns.
                state.card_pos.insert(
                    card.id,
                    CardPos {
                        rect: response.rect,
                        col: egui::Id::new(&column.id),
                    },
                );
            }

            // Keep the composer beneath the cards (inside the filled zone) so the
            // empty space below it still acts as a drop target.
            ui.add_space(SPACING_SM);
            add_card_ui(ui, theme, state, col_idx, action);
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
        // A cross-column drag shouldn't slide afterwards — the pointer already
        // moved the card. (A same-column reorder never changes column, so it
        // never animates; don't flag it, to avoid a stale entry lingering.)
        if !column.cards.iter().any(|c| c.id == payload.0) {
            state.suppress_anim.insert(payload.0);
        }
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
            // Pin the internal vertical rhythm so the card is the same height
            // wherever it's drawn — in a column lane or, mid-move, in a free
            // floating Area — rather than inheriting the caller's item spacing.
            ui.spacing_mut().item_spacing.y = SPACING_SM;

            if !card.labels.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for label in &card.labels {
                        label_chip(ui, theme, label);
                    }
                });
                ui.add_space(SPACING_XS);
            }
            // Title, with the word-id appended muted and small so a card's
            // reference is legible at a glance. A bare `#word-id` — the `#` marks
            // it as a reference, GitHub-style; the `<board>#` prefix is dropped
            // since it's identical on every card, and the detail sheet shows the
            // full `board#word-id` for copy-paste (see [`headway::wordid`]).
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                ui.label(egui::RichText::new(&card.title).color(theme.text_primary));
                ui.label(
                    egui::RichText::new(format!("#{}", headway::wordid::encode(card.id.bytes())))
                        .small()
                        .color(theme.text_muted.gamma_multiply(0.6)),
                );
            });

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

/// A label pill with a trailing ✕ to remove it. Returns true if ✕ was clicked.
fn removable_label_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, label: &str) -> bool {
    let color = label_color(label);
    let mut remove = false;
    egui::Frame::new()
        .fill(color.gamma_multiply(0.30))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(label).small().color(theme.text_primary));
                if ui
                    .add(egui::Button::new(egui::RichText::new("✕").small()).frame(false))
                    .on_hover_text(format!("Remove {label}"))
                    .clicked()
                {
                    remove = true;
                }
            });
        });
    remove
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
    if state.edit == InlineEdit::AddCard(col_idx) {
        let empty = state.edit_text.is_empty();
        let edit = egui::TextEdit::multiline(&mut state.edit_text)
            .hint_text("Card title…")
            .desired_rows(2)
            .desired_width(f32::INFINITY);

        // egui always paints the hint with `weak_text_color()` (it ignores any
        // RichText color), which lands brighter than our muted token. That color
        // is `tint(text_color, noninteractive.weak_bg_fill)`, so pointing both
        // inputs at `text_muted` makes it resolve to exactly `text_muted`. Scope
        // it to the empty field so it only tints the hint, never typed text.
        let edit_response = ui
            .scope(|ui| {
                if empty {
                    let v = ui.visuals_mut();
                    v.override_text_color = Some(theme.text_muted);
                    v.widgets.noninteractive.weak_bg_fill = theme.text_muted;
                }
                ui.add(edit)
            })
            .inner;

        // Grab focus when the composer first opens (and after each add) so you
        // can start typing immediately without clicking into the field.
        let refocusing = state.focus_edit;
        if refocusing {
            edit_response.request_focus();
            state.focus_edit = false;
        }

        // Enter (without Shift) commits the card; Shift+Enter inserts a line
        // break so multi-line titles are still possible. A multiline field
        // swallows Enter into a newline, so `lost_focus()` never fires on it —
        // sense the key directly while focused instead, and trim the stray
        // newline back off the title below.
        let submit = edit_response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));

        let add = ui.button("Add").clicked();

        if escape {
            state.edit_text.clear();
            state.edit = InlineEdit::None;
        } else if submit || add {
            let title = state.edit_text.trim().to_string();
            state.edit_text.clear();
            if title.is_empty() {
                // Committing nothing means "I'm done" — close the composer.
                state.edit = InlineEdit::None;
            } else {
                *action = Some(BoardAction::AddCard {
                    col: col_idx,
                    title,
                    labels: vec![],
                });
                // Keep the composer open and refocused so you can rattle off
                // several cards in a row without re-clicking "+ Add card".
                state.focus_edit = true;
            }
        } else if !refocusing && edit_response.lost_focus() {
            // No Cancel button: clicking away (or Tab) dismisses the composer.
            // Guarded by `refocusing` so the focus we re-grab right after an add
            // isn't misread as a blur that closes it.
            state.edit_text.clear();
            state.edit = InlineEdit::None;
        }
    } else {
        let add = ui.add(
            egui::Button::new(egui::RichText::new("+ Add card").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false),
        );
        if add.clicked() {
            state.edit = InlineEdit::AddCard(col_idx);
            state.edit_text.clear();
            state.focus_edit = true;
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
        egui::TextEdit::singleline(&mut state.edit_text)
            .desired_width(f32::INFINITY)
            .hint_text("Column title…"),
    );
    if state.focus_edit {
        resp.request_focus();
        state.focus_edit = false;
    }

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.edit = InlineEdit::None;
    } else if resp.lost_focus() {
        let name = state.edit_text.trim().to_string();
        if !name.is_empty() {
            *action = Some(BoardAction::RenameColumn { col: col_idx, name });
        }
        state.edit = InlineEdit::None;
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
        // Board-level: copy a pasteable `nostr:naddr…` reference to this board.
        if ui.button("Copy Id").clicked() {
            if let Some(uri) = board_nostr_uri(&view.author, &view.id) {
                ui.ctx().copy_text(uri);
            }
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Rename").clicked() {
            state.edit_text = view.columns[col_idx].name.clone();
            state.edit = InlineEdit::RenameColumn(col_idx);
            state.focus_edit = true;
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
                if state.edit == InlineEdit::AddColumn {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut state.edit_text)
                            .desired_width(f32::INFINITY)
                            .hint_text("Column title…"),
                    );
                    if state.focus_edit {
                        resp.request_focus();
                        state.focus_edit = false;
                    }
                    let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    ui.add_space(SPACING_SM);
                    ui.horizontal(|ui| {
                        let add = ui.button("Add").clicked() || submit;
                        let cancel = ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if add {
                            let name = state.edit_text.trim().to_string();
                            if !name.is_empty() {
                                *action = Some(BoardAction::AddColumn { name });
                            }
                            state.edit_text.clear();
                            state.edit = InlineEdit::None;
                        } else if cancel {
                            state.edit_text.clear();
                            state.edit = InlineEdit::None;
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
                        state.edit = InlineEdit::AddColumn;
                        state.edit_text.clear();
                        state.focus_edit = true;
                    }
                }
            });
        });
}

/// The card detail screen, rendered full-pane in place of the board while a card
/// is selected. A top bar (back to board, current-status pill, ✕) sits above a
/// scrollable body holding the title, description, labels, status and card
/// actions. Edits are emitted as [`BoardAction`]s (title/description commit on
/// focus loss); dismissing (back / ✕ / Escape) clears the selection. Unlike the
/// old floating sheet this gives a card room to breathe — and room for the
/// comment thread that lives alongside it.
fn card_detail_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    view: &BoardView,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    // board_ui only calls us when the selection resolves; re-resolve here to get
    // the card and its column, dropping a now-stale selection defensively.
    let Some(card_id) = state.selected else {
        return;
    };
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
        // A blank field opens straight into the editor; one with content shows
        // its render (heading / markdown) until the user asks to edit.
        state.detail_title_mode = seed_edit_mode(&card.title);
        state.detail_desc_mode = seed_edit_mode(&card.description);
        state.new_label.clear();
        state.comment_draft.clear();
    }

    let ctx = DetailCtx {
        card_id,
        card_ref: format!("{}#{}", view.id, headway::wordid::encode(card_id.bytes())),
        current_col,
        title: card.title.clone(),
        desc: card.description.clone(),
        labels: card.labels.clone(),
        // Owned copy so the body can render the status pill and column chips.
        columns: view.columns.iter().map(|c| c.name.clone()).collect(),
        comments: card.comments.clone(),
    };

    // Escape backs out to the board. Consume it so it doesn't also fall through
    // to Chrome's Escape handler, which would toggle the side menu.
    let mut outcome = if ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        DetailOutcome::Close
    } else {
        DetailOutcome::None
    };

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            detail_pane_topbar_ui(ui, theme, &ctx, &mut outcome);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_MD);

            // On a wide pane the card body and its comment thread sit side by
            // side; on a narrow one the thread stacks beneath the body. Shrink to
            // content height (only the width is pinned) so the trailing actions
            // sit right under the body rather than floating in the centre of an
            // over-tall scroll area.
            let avail = ui.available_width();
            let two_col = avail >= DETAIL_TWO_COL_MIN;
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if two_col {
                        // Cap the combined width so body+thread don't sprawl, then
                        // split: comments take a fixed right rail, the body the rest
                        // (floored so it never collapses).
                        let total =
                            avail.min(DETAIL_CONTENT_WIDTH + SPACING_LG + DETAIL_COMMENTS_WIDTH);
                        let body_w = (total - DETAIL_COMMENTS_WIDTH - SPACING_LG).max(360.0);
                        ui.set_max_width(total);
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(body_w, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_width(body_w);
                                    detail_body_ui(ui, theme, &ctx, state, action, &mut outcome);
                                },
                            );
                            ui.add_space(SPACING_LG);
                            ui.allocate_ui_with_layout(
                                egui::vec2(DETAIL_COMMENTS_WIDTH, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_width(DETAIL_COMMENTS_WIDTH);
                                    detail_comments_ui(
                                        ui,
                                        theme,
                                        note_context,
                                        &ctx,
                                        state,
                                        &mut outcome,
                                    );
                                },
                            );
                        });
                    } else {
                        let content_width = avail.min(DETAIL_CONTENT_WIDTH);
                        ui.set_max_width(content_width);
                        detail_body_ui(ui, theme, &ctx, state, action, &mut outcome);
                        ui.add_space(SPACING_MD);
                        ui.separator();
                        ui.add_space(SPACING_MD);
                        detail_comments_ui(ui, theme, note_context, &ctx, state, &mut outcome);
                    }
                });
        });

    resolve_detail_outcome(state, action, view, &ctx, outcome);
}

/// The full-pane detail top bar: a back-to-board button, the card's current
/// status pill, and a trailing ✕ that also dismisses. All three resolve to
/// [`DetailOutcome::Close`].
fn detail_pane_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        let back = egui::Button::new(egui::RichText::new("← Board").color(theme.text_secondary))
            .fill(egui::Color32::TRANSPARENT)
            .frame(false);
        if ui.add(back).clicked() {
            *outcome = DetailOutcome::Close;
        }
        ui.add_space(SPACING_SM);
        status_pill(ui, theme, &ctx.columns[ctx.current_col]);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let x = egui::Button::new(egui::RichText::new("✕").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(x).clicked() {
                *outcome = DetailOutcome::Close;
            }
        });
    });
}

/// The card data the detail sheet needs, copied out of the (immutable)
/// `BoardView` so the sheet body doesn't borrow it while we also mutate `state`.
struct DetailCtx {
    card_id: NoteId,
    /// The card's human-friendly reference, e.g. `headway:maple-river-canyon`:
    /// the board slug plus the word-encoded event id (see [`headway::wordid`]).
    card_ref: String,
    current_col: usize,
    title: String,
    desc: String,
    labels: Vec<String>,
    columns: Vec<String>,
    /// The card's comment thread, oldest first. Rendered flat (replies aren't
    /// indented yet) but each carries its `parent` for forward-compatibility.
    comments: Vec<CommentView>,
}

/// The single user intent collected while rendering the detail sheet, resolved
/// into a [`BoardAction`] after the UI closures return. At most one is produced
/// per frame (distinct buttons / keys are mutually exclusive), so one enum
/// models it better — and resolves more obviously — than a bag of bools.
#[derive(Default)]
enum DetailOutcome {
    #[default]
    None,
    /// Dismiss the detail pane (back, ✕, or Escape).
    Close,
    Delete,
    Archive,
    /// Move the card to the column at this index.
    MoveTo(usize),
    /// Commit the "add label" field.
    AddLabel,
    /// Remove this label from the card's set.
    RemoveLabel(String),
    /// Post the contents of the comment composer as a new top-level comment.
    AddComment,
}

/// The dimmed full-screen backdrop behind the sheet. Returns true if it was
/// clicked (a tap outside the sheet, which closes the detail).
fn detail_scrim_ui(ui: &mut egui::Ui, screen: egui::Rect) -> bool {
    egui::Area::new(egui::Id::new("headway-detail-scrim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ui.ctx(), |ui| {
            let resp = ui.allocate_response(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
            resp
        })
        .inner
        .clicked()
}

/// The elevated card surface the sheet's contents are drawn into. A builder, not
/// a renderer, so it intentionally has no `_ui` suffix.
fn detail_sheet_frame(theme: &ColorTheme, pad: f32) -> egui::Frame {
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
}

/// The scrollable sheet body: title, description, labels, status and delete.
/// Title/description commit directly on focus loss; the rest is collected into
/// `outcome` and resolved by the caller.
fn detail_body_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
    outcome: &mut DetailOutcome,
) {
    detail_title_section_ui(ui, ctx, state, action);
    detail_card_ref_ui(ui, theme, ctx);

    ui.add_space(SPACING_MD);
    detail_description_section_ui(ui, theme, ctx, state, action);

    ui.add_space(SPACING_MD);
    detail_labels_section_ui(ui, theme, ctx, state, outcome);

    // Move the card between lanes without dragging.
    if ctx.columns.len() > 1 {
        ui.add_space(SPACING_MD);
        detail_status_section_ui(ui, theme, ctx, outcome);
    }

    ui.add_space(SPACING_MD);
    ui.separator();
    ui.add_space(SPACING_SM);
    // A plain horizontal row (not a right-to-left, full-height layout): in the
    // full-pane view the body lays out in the whole viewport, and a centre-aligned
    // layout would float these actions in the middle of the empty space below the
    // card. Left-aligned directly under the separator keeps them anchored to the
    // content.
    ui.horizontal(|ui| {
        if ui.button("Archive").clicked() {
            *outcome = DetailOutcome::Archive;
        }
        if ui
            .button(egui::RichText::new("Delete card").color(theme.destructive))
            .clicked()
        {
            *outcome = DetailOutcome::Delete;
        }
    });
}

/// Title section: rendered as a heading by default, switching to a single-line
/// editor when clicked. The edit commits as a [`BoardAction::EditTitle`] on
/// focus loss and returns to the rendered view. Titles are plain text — no
/// markdown — and an empty edit is rejected so a card always keeps a title.
fn detail_title_section_ui(
    ui: &mut egui::Ui,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    match state.detail_title_mode {
        EditMode::Rendered => {
            // The heading itself is the click target into the editor.
            let resp = ui
                .add(
                    egui::Label::new(egui::RichText::new(&state.detail_title).heading())
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Click to edit");
            if resp.clicked() {
                state.detail_title_mode = EditMode::Editing { focus: true };
            }
        }
        EditMode::Editing { focus } => {
            let title_resp = ui.add(
                egui::TextEdit::singleline(&mut state.detail_title)
                    .font(egui::TextStyle::Heading)
                    .desired_width(f32::INFINITY)
                    .hint_text("Title"),
            );
            if focus {
                title_resp.request_focus();
                state.detail_title_mode = EditMode::Editing { focus: false };
            }
            // Commit on focus loss and drop back to the rendered heading, unless
            // the title is now empty — keep editing so a blank title can't stick.
            if title_resp.lost_focus() {
                let title = state.detail_title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                if title != ctx.title {
                    *action = Some(BoardAction::EditTitle {
                        card: ctx.card_id,
                        title,
                    });
                }
                state.detail_title_mode = EditMode::Rendered;
            }
        }
    }
}

/// The card's word-id reference, rendered as a muted, click-to-copy caption
/// directly under the title — the GUI mirror of what the CLI prints, and a
/// stable handle to paste into commits/chat (see [`headway::wordid`]).
fn detail_card_ref_ui(ui: &mut egui::Ui, theme: &ColorTheme, ctx: &DetailCtx) {
    // A frameless button, not a Label: in this egui version labels are selectable
    // by default, so a click starts a text selection and "copy" is only reachable
    // through the built-in right-click menu. A button gives a true one-click copy.
    let btn = egui::Button::new(
        egui::RichText::new(&ctx.card_ref)
            .color(theme.text_muted)
            .small(),
    )
    .frame(false)
    .fill(egui::Color32::TRANSPARENT);
    if ui.add(btn).on_hover_text("Click to copy").clicked() {
        ui.ctx().copy_text(ctx.card_ref.clone());
    }
}

/// Description section: rendered markdown by default with an ✎ edit affordance,
/// switching to a raw multiline editor on demand (or a double-click on the
/// rendered text). Edits commit as a [`BoardAction::EditDescription`] when the
/// editor loses focus, which also returns the section to its rendered view.
fn detail_description_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    ui.horizontal(|ui| {
        section_label(ui, theme, "Description");
        // Offer the edit affordance just right of the label, only while showing
        // rendered markdown.
        if matches!(state.detail_desc_mode, EditMode::Rendered) {
            let edit = egui::Button::new(egui::RichText::new("✎").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(edit).on_hover_text("Edit description").clicked() {
                state.detail_desc_mode = EditMode::Editing { focus: true };
            }
        }
    });
    ui.add_space(SPACING_XS);

    match state.detail_desc_mode {
        EditMode::Rendered => {
            // Render with interactive task-list checkboxes; a click flips the
            // box in `detail_desc` in place and we persist it like any edit.
            let scope = ui.scope(|ui| {
                notedeck_ui::markdown::render_markdown_editable(&mut state.detail_desc, ui)
            });
            let toggled = scope.inner;
            // The whole rendered block is a double-click target into the editor.
            let resp = scope.response.interact(egui::Sense::click());
            if toggled {
                *action = Some(BoardAction::EditDescription {
                    card: ctx.card_id,
                    description: state.detail_desc.clone(),
                });
            }
            if resp.double_clicked() {
                state.detail_desc_mode = EditMode::Editing { focus: true };
            }
        }
        EditMode::Editing { focus } => {
            let desc_resp = ui.add(
                egui::TextEdit::multiline(&mut state.detail_desc)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("Add more detail… (markdown supported)"),
            );
            if focus {
                desc_resp.request_focus();
                state.detail_desc_mode = EditMode::Editing { focus: false };
            }
            // Commit on focus loss and drop back to the rendered view, unless the
            // description is still empty (nothing to render, so stay in the editor).
            if desc_resp.lost_focus() {
                if state.detail_desc != ctx.desc {
                    *action = Some(BoardAction::EditDescription {
                        card: ctx.card_id,
                        description: state.detail_desc.clone(),
                    });
                }
                if !state.detail_desc.trim().is_empty() {
                    state.detail_desc_mode = EditMode::Rendered;
                }
            }
        }
    }
}

/// Labels section: removable chips plus an "add label" field.
fn detail_labels_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    section_label(ui, theme, "Labels");
    ui.add_space(SPACING_XS);
    ui.horizontal_wrapped(|ui| {
        for label in &ctx.labels {
            if removable_label_chip_ui(ui, theme, label) {
                *outcome = DetailOutcome::RemoveLabel(label.clone());
            }
        }
    });
    ui.add_space(SPACING_XS);
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.new_label)
                .desired_width(140.0)
                .hint_text("Add label…"),
        );
        let submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Add").clicked() || submit {
            *outcome = DetailOutcome::AddLabel;
        }
    });
}

/// Status section: a chip per column to move the card without dragging.
fn detail_status_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    section_label(ui, theme, "Status");
    ui.add_space(SPACING_XS);
    ui.horizontal_wrapped(|ui| {
        for (i, name) in ctx.columns.iter().enumerate() {
            let selected = i == ctx.current_col;
            if ui.selectable_label(selected, name).clicked() && !selected {
                *outcome = DetailOutcome::MoveTo(i);
            }
        }
    });
}

/// The comment thread for the open card: a section heading with a count, the
/// flat list of comments (oldest first), and a composer to add one. Replies
/// aren't indented yet (`store now, render flat`) but each comment still shows
/// who it threads under. Posting is collected into `outcome`.
fn detail_comments_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        section_label(ui, theme, "Comments");
        if !ctx.comments.is_empty() {
            count_badge(ui, theme, ctx.comments.len());
        }
    });
    ui.add_space(SPACING_SM);

    if ctx.comments.is_empty() {
        ui.label(
            egui::RichText::new("No comments yet.")
                .small()
                .color(theme.text_muted),
        );
    } else {
        // One read txn for the whole thread; each kind-1111 comment is drawn with
        // the shared notedeck_ui note renderer so headway comments carry the same
        // pfp/name/time chrome they'd get anywhere else in notedeck.
        let txn = nostrdb::Transaction::new(note_context.ndb).ok();
        for comment in &ctx.comments {
            comment_note_ui(ui, theme, note_context, txn.as_ref(), comment);
        }
    }

    ui.add_space(SPACING_MD);
    detail_comment_composer_ui(ui, theme, state, outcome);
}

/// Render one comment with the shared notedeck_ui note renderer, loading the
/// kind-1111 event from the local db by id. A threaded reply gets a small
/// "↳ reply to" caption above the note (the thread still renders flat). Falls
/// back to the self-contained [`comment_row_ui`] if the note isn't in the db.
fn comment_note_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    txn: Option<&nostrdb::Transaction>,
    comment: &CommentView,
) {
    let note = txn.and_then(|txn| {
        note_context
            .ndb
            .get_note_by_id(txn, comment.id.bytes())
            .ok()
    });
    let Some(note) = note else {
        comment_row_ui(ui, theme, comment);
        return;
    };

    // The renderer shows author/time/body; the reply target is the one thing it
    // can't convey while the thread is flat, so surface it as a caption.
    if let Some(parent) = &comment.parent {
        ui.label(
            egui::RichText::new(format!(
                "↳ reply to #{}",
                headway::wordid::encode(parent.bytes())
            ))
            .small()
            .color(theme.text_muted),
        );
    }

    // No actionbar/options menu: there's no relay publishing or note-nav wired up
    // here, so the reply/zap/repost affordances would be dead. A small framed pfp
    // keeps each comment compact in the thread rail.
    let flags = notedeck_ui::NoteOptions::SelectableText
        | notedeck_ui::NoteOptions::SmallPfp
        | notedeck_ui::NoteOptions::Framed;
    notedeck_ui::NoteView::new(note_context, &note, flags).show(ui);
}

/// A single comment: an attribution line (short author, relative time, word-id,
/// and a "↳ reply to" marker for threaded replies) above its markdown body. The
/// fallback for [`comment_note_ui`] when the kind-1111 event isn't in the db.
fn comment_row_ui(ui: &mut egui::Ui, theme: &ColorTheme, comment: &CommentView) {
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_SM;
                ui.label(
                    egui::RichText::new(headway::fmt::short_author(&comment.author))
                        .small()
                        .strong()
                        .color(theme.text_secondary),
                );
                ui.label(
                    egui::RichText::new(headway::fmt::rel_time(comment.created_at))
                        .small()
                        .color(theme.text_muted),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "#{}",
                        headway::wordid::encode(comment.id.bytes())
                    ))
                    .small()
                    .color(theme.text_muted.gamma_multiply(0.6)),
                );
                if let Some(parent) = &comment.parent {
                    ui.label(
                        egui::RichText::new(format!(
                            "↳ reply to #{}",
                            headway::wordid::encode(parent.bytes())
                        ))
                        .small()
                        .color(theme.text_muted),
                    );
                }
            });
            ui.add_space(SPACING_XS);
            notedeck_ui::markdown::render_markdown(&comment.body, ui);
        });
    ui.add_space(SPACING_SM);
}

/// The "write a comment" composer at the foot of the thread: a multiline field
/// plus a Comment button. Posts on the button or ⌘/Ctrl+Enter (a multiline field
/// keeps plain Enter for newlines). Empty drafts are ignored.
fn detail_comment_composer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    let resp = ui.add(
        egui::TextEdit::multiline(&mut state.comment_draft)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("Write a comment… (markdown supported)"),
    );
    let submit =
        resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.add_space(SPACING_XS);
    ui.horizontal(|ui| {
        let post = ui
            .add_enabled(
                !state.comment_draft.trim().is_empty(),
                egui::Button::new("Comment"),
            )
            .clicked();
        if post || submit {
            *outcome = DetailOutcome::AddComment;
        }
        ui.label(
            egui::RichText::new("⌘↵ to post")
                .small()
                .color(theme.text_muted),
        );
    });
}

/// Resolve the collected [`DetailOutcome`] into a single [`BoardAction`].
fn resolve_detail_outcome(
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
    view: &BoardView,
    ctx: &DetailCtx,
    outcome: DetailOutcome,
) {
    // Removing a card from the board also dismisses its (now stale) sheet.
    let mut close = || {
        state.selected = None;
        state.detail_for = None;
    };

    match outcome {
        DetailOutcome::None => {}
        DetailOutcome::Close => close(),
        DetailOutcome::Delete => {
            *action = Some(BoardAction::DeleteCard { card: ctx.card_id });
            close();
        }
        DetailOutcome::Archive => {
            *action = Some(BoardAction::ArchiveCard { card: ctx.card_id });
            close();
        }
        DetailOutcome::MoveTo(to) => {
            let to_row = view.columns[to].cards.len();
            *action = Some(BoardAction::MoveCard {
                card: ctx.card_id,
                to_col: to,
                to_row,
            });
        }
        DetailOutcome::RemoveLabel(target) => {
            // Republish the set without the removed label (labels are latest-wins).
            let labels: Vec<String> = ctx
                .labels
                .iter()
                .filter(|l| **l != target)
                .cloned()
                .collect();
            *action = Some(BoardAction::SetLabels {
                card: ctx.card_id,
                labels,
            });
        }
        DetailOutcome::AddLabel => {
            let new = state.new_label.trim().to_string();
            if !new.is_empty() && !ctx.labels.contains(&new) {
                let mut labels = ctx.labels.clone();
                labels.push(new);
                *action = Some(BoardAction::SetLabels {
                    card: ctx.card_id,
                    labels,
                });
            }
            state.new_label.clear();
        }
        DetailOutcome::AddComment => {
            let body = state.comment_draft.trim().to_string();
            if !body.is_empty() {
                *action = Some(BoardAction::AddComment {
                    card: ctx.card_id,
                    body,
                    // Flat composer posts top-level comments; threaded replies
                    // aren't wired into the GUI yet (the model carries `parent`).
                    reply_to: None,
                });
            }
            state.comment_draft.clear();
        }
    }
}

/// The archived-cards sheet: an overlay listing cards taken off the board, each
/// with a Restore button that re-places it into the column it came from. Mirrors
/// the detail sheet's scrim + centered-card presentation.
fn archived_sheet_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    if !state.showing_archived {
        return;
    }

    let screen = ui.ctx().screen_rect();
    let pad = SPACING_LG;
    let sheet_width = if notedeck::ui::is_narrow(ui.ctx()) {
        screen.width() - 2.0 * pad
    } else {
        460.0
    };
    let max_body_height = screen.height() - 6.0 * pad;

    // Consume Escape so closing the sheet doesn't also fall through to Chrome's
    // Escape handler, which would toggle the side menu.
    let mut close = ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if detail_scrim_ui(ui, screen) {
        close = true;
    }

    egui::Area::new(egui::Id::new("headway-archived"))
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
                egui::ScrollArea::vertical()
                    .max_height(max_body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if view.archived.is_empty() {
                            ui.label(
                                egui::RichText::new("No archived cards.").color(theme.text_muted),
                            );
                        }
                        for entry in &view.archived {
                            archived_row_ui(ui, theme, view, entry, action);
                        }
                    });
            });
        });

    if close {
        state.showing_archived = false;
    }
}

/// One row in the archived sheet: the card's title, where it came from, and a
/// Restore button.
fn archived_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    entry: &ArchivedCard,
    action: &mut Option<BoardAction>,
) {
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
                    ui.label(egui::RichText::new(title).strong());
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

/// A `nostr:nevent…` URI for an issue card, ready to paste into a notebook note
/// (or anywhere else that resolves nostr refs). Carries the issue kind as a hint.
fn issue_nostr_uri(card_id: &NoteId) -> Option<String> {
    use nostr::nips::nip19::ToBech32;
    let event_id = nostr::EventId::from_slice(card_id.bytes()).ok()?;
    let nevent = nostr::nips::nip19::Nip19Event::new(event_id, Vec::<String>::new())
        .kind(nostr::Kind::from(event::KIND_ISSUE as u16));
    Some(format!("nostr:{}", nevent.to_bech32().ok()?))
}

/// A `nostr:naddr…` URI for a board, addressing the replaceable board event by
/// its `(kind, author, identifier)` coordinate.
fn board_nostr_uri(author: &[u8; 32], board_id: &str) -> Option<String> {
    use nostr::nips::nip19::ToBech32;
    let pubkey = nostr::PublicKey::from_slice(author).ok()?;
    let mut coord =
        nostr::nips::nip01::Coordinate::new(nostr::Kind::from(event::KIND_BOARD as u16), pubkey);
    coord.identifier = board_id.to_string();
    Some(format!("nostr:{}", coord.to_bech32().ok()?))
}

/// Render a compact, read-only headway card frame: an optional label row, a
/// title, and a one-line body preview. Shared by [`issue_inline_ui`] (the
/// creation-time snapshot) and [`card_inline_ui`] (the folded current state).
fn card_frame_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    labels: &[String],
    title: &str,
    body: &str,
) -> egui::Response {
    egui::Frame::new()
        .fill(theme.surface_elevated)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            // The notebook lays node content out centered (egui's `Ui::put`);
            // force left alignment so the card reads like a card, not centered.
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(ui.available_width());
                if !labels.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for label in labels {
                            label_chip(ui, theme, label);
                        }
                    });
                    ui.add_space(SPACING_XS);
                }
                ui.label(egui::RichText::new(title).color(theme.text_primary));
                if !body.is_empty() {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(body).small().color(theme.text_muted))
                            .truncate(),
                    );
                }
            });
        })
        .response
}

/// Render a card's *resolved* state (latest subject, labels and cover applied),
/// as folded off its board. This is what an inline issue reference should show;
/// [`issue_inline_ui`] is only the fallback when the board can't be folded.
pub fn card_inline_ui(ui: &mut egui::Ui, theme: &ColorTheme, card: &CardView) -> egui::Response {
    card_frame_ui(ui, theme, &card.labels, &card.title, &card.description)
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
