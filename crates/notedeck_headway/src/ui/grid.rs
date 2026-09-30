//! The kanban grid: columns, the card drop zone and drag/drop, card context
//! menus, the card tile itself, the column slide animation and the inline
//! add-card / add-column / rename-column editors.

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{
    RADIUS_LG, RADIUS_MD, SPACING_SM, SPACING_XS, STROKE_MEDIUM, STROKE_THICK, STROKE_THIN,
};

use super::filter::ViewFilter;
use super::widgets::{
    StatusIcon, board_nostr_uri, count_badge, issue_nostr_uri, label_chip, priority_icon_ui,
    priority_label, progress_pill, status_icon_ui,
};
use super::{BoardEffect, BoardUiState, CardBoardMove, CardBoardOp, CardPos, InlineEdit};
use crate::BoardSummary;
use crate::event::{self, BoardView, CardView, ColumnView, Priority};
use crate::store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// How long a card takes to slide from its old slot to its new one when it
/// jumps columns (e.g. a `headway move` landing from the CLI).
const MOVE_ANIM_SECS: f32 = 0.28;

/// Drag-and-drop payload: the id of the card being dragged.
#[derive(Clone)]
struct DragCard(NoteId);

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
pub(super) fn start_move_anims(ctx: &egui::Context, view: &BoardView, state: &mut BoardUiState) {
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
#[allow(clippy::too_many_arguments)]
pub(super) fn column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
    filter: &ViewFilter,
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
                        // The column's positional status circle, tying the
                        // board header to the detail pane's visual language
                        // (Linear renders its board headers the same way).
                        status_icon_ui(
                            ui,
                            theme,
                            StatusIcon::for_column(col_idx, view.columns.len()),
                            14.0,
                        );
                        ui.label(egui::RichText::new(&column.name).strong());
                        // When narrowed, the badge reflects how many of this
                        // column's cards show rather than the column's total.
                        let count = if filter.is_active() {
                            column.cards.iter().filter(|c| filter.shows(c)).count()
                        } else {
                            column.cards.len()
                        };
                        count_badge(ui, theme, count);
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
                        cards_drop_zone(
                            ui, theme, view, boards, column, state, filter, col_idx, action,
                            clicked,
                        );
                    });
            });
        });
}

/// The drop zone wrapping a column's cards, with live insertion-line feedback.
#[allow(clippy::too_many_arguments)]
fn cards_drop_zone(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    column: &ColumnView,
    state: &mut BoardUiState,
    filter: &ViewFilter,
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
                // Filtered- or hidden-out cards are simply not drawn. `row_idx`
                // stays the card's true position in the column, so drag-reorder
                // targeting against the remaining visible cards still lands
                // correctly.
                if !filter.shows(card) {
                    continue;
                }

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
                    // No ring while in flight, but a cursor card still scrolls its
                    // landing slot into view.
                    if state.scroll_this_frame && state.cursor == Some(card.id) {
                        ui.scroll_to_rect(dest, None);
                    }
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
                    // Cross-board: relocate (move) or share (link) the card onto
                    // another of the account's boards. Membership is
                    // placement-driven, so a link keeps the current board too.
                    if boards.iter().any(|b| b.id != view.id) {
                        ui.separator();
                        card_board_submenu(
                            ui,
                            "Move to board",
                            card.id,
                            &view.id,
                            boards,
                            state,
                            CardBoardOp::Move,
                        );
                        card_board_submenu(
                            ui,
                            "Link to board",
                            card.id,
                            &view.id,
                            boards,
                            state,
                            CardBoardOp::Link,
                        );
                    }
                    // Subissues: parent this card under another card on the
                    // board, or detach it from its current parent.
                    card_parent_menu(ui, view, card, action);
                    // Dependencies: block this card on another, or lift an
                    // existing blocker.
                    card_blocker_menu(ui, view, card, action);
                });

                // Border: the keyboard cursor's accent ring outranks the hover
                // highlight. Cards are clickable either way, so hovering one
                // still switches to a pointing-hand cursor.
                let is_cursor = state.cursor == Some(card.id);
                if is_cursor {
                    ui.painter().rect_stroke(
                        response.rect,
                        egui::CornerRadius::same(RADIUS_MD as u8),
                        egui::Stroke::new(STROKE_THICK, theme.accent),
                        egui::StrokeKind::Inside,
                    );
                } else if response.hovered() {
                    ui.painter().rect_stroke(
                        response.rect,
                        egui::CornerRadius::same(RADIUS_MD as u8),
                        egui::Stroke::new(STROKE_MEDIUM, theme.border_strong),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // Minimal scroll (not centred) so stepping through a column
                // doesn't jitter it on every press. Reaches both the column's
                // vertical and the board's horizontal scroll area.
                if is_cursor && state.scroll_this_frame {
                    response.scroll_to_me(None);
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

/// A card context-menu submenu (`Move to board` / `Link to board`) listing every
/// board except the current `source_board`. Picking one raises a
/// [`CardBoardMove`] on `state` for the app to act on.
fn card_board_submenu(
    ui: &mut egui::Ui,
    label: &str,
    card: NoteId,
    source_board: &str,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
    op: CardBoardOp,
) {
    ui.menu_button(label, |ui| {
        for board in boards.iter().filter(|b| b.id != source_board) {
            if ui.button(&board.title).clicked() {
                state.raise(BoardEffect::CardMove(CardBoardMove {
                    card,
                    to_board: board.id.clone(),
                    op,
                }));
                ui.close_menu();
            }
        }
    });
}

/// Context-menu parent controls: a "Set parent" submenu listing every card that
/// could become this card's parent — filtered with the same cycle guard the
/// store applies on write, so nothing the menu offers can be refused — plus a
/// detach entry when the card already has a parent. Draws nothing on a board
/// where neither applies (a single-card board with no parent set).
///
/// Immediate-mode: this runs while the menu is open, so candidates are walked
/// as an iterator (twice — once to probe, once to draw) rather than collected.
fn card_parent_menu(
    ui: &mut egui::Ui,
    view: &BoardView,
    card: &CardView,
    action: &mut Option<BoardAction>,
) {
    let candidates = || {
        view.columns
            .iter()
            .flat_map(|c| &c.cards)
            .filter(|p| p.id != card.id && !crate::store::would_cycle(view, card.id, p.id))
    };
    let has_candidates = candidates().next().is_some();
    if !has_candidates && card.parent.is_none() {
        return;
    }
    ui.separator();
    if has_candidates {
        ui.menu_button("Set parent", |ui| {
            for parent in candidates() {
                let current = card.parent == Some(parent.id);
                if ui
                    .selectable_label(current, menu_title(&parent.title).as_ref())
                    .clicked()
                {
                    if !current {
                        *action = Some(BoardAction::SetParent {
                            card: card.id,
                            parent: Some(parent.id),
                        });
                    }
                    ui.close_menu();
                }
            }
        });
    }
    if card.parent.is_some() && ui.button("Detach from parent").clicked() {
        *action = Some(BoardAction::SetParent {
            card: card.id,
            parent: None,
        });
        ui.close_menu();
    }
}

/// Context-menu dependency controls, mirroring [`card_parent_menu`]: an "Add
/// blocker" submenu listing every card this one could be blocked on — filtered
/// with [`store::would_block_cycle`](crate::store::would_block_cycle), the same cycle guard the write path
/// applies, so nothing offered can be refused — plus a "Remove blocker" entry
/// per current edge. Draws nothing when neither applies (a lone card with no
/// blockers).
///
/// Immediate-mode: candidates are walked as an iterator (probe, then draw)
/// rather than collected, so an open menu allocates nothing per frame.
fn card_blocker_menu(
    ui: &mut egui::Ui,
    view: &BoardView,
    card: &CardView,
    action: &mut Option<BoardAction>,
) {
    // A card can block on any other card that isn't already its blocker and
    // wouldn't close a dependency loop — the exact rule `store::apply` enforces.
    let candidates = || {
        view.columns.iter().flat_map(|c| &c.cards).filter(|p| {
            p.id != card.id
                && !card.blocked_by.iter().any(|e| e.id == p.id)
                && !crate::store::would_block_cycle(view, card.id, p.id)
        })
    };
    let has_candidates = candidates().next().is_some();
    if !has_candidates && card.blocked_by.is_empty() {
        return;
    }
    ui.separator();
    if has_candidates {
        ui.menu_button("Add blocker", |ui| {
            for blocker in candidates() {
                if ui.button(menu_title(&blocker.title).as_ref()).clicked() {
                    *action = Some(BoardAction::Block {
                        card: card.id,
                        on: blocker.id,
                    });
                    ui.close_menu();
                }
            }
        });
    }
    if !card.blocked_by.is_empty() {
        ui.menu_button("Remove blocker", |ui| {
            for edge in &card.blocked_by {
                if ui.button(menu_title(&edge.title).as_ref()).clicked() {
                    *action = Some(BoardAction::Unblock {
                        card: card.id,
                        on: edge.id,
                    });
                    ui.close_menu();
                }
            }
        });
    }
}

/// Clamp a card title to a menu-friendly length so one long title doesn't
/// stretch the whole context menu. Borrows when the title already fits, so the
/// common case doesn't allocate.
fn menu_title(title: &str) -> std::borrow::Cow<'_, str> {
    const MAX_CHARS: usize = 40;
    match title.char_indices().nth(MAX_CHARS) {
        None => std::borrow::Cow::Borrowed(title),
        Some((clip, _)) => std::borrow::Cow::Owned(format!("{}…", &title[..clip])),
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
            // reference is legible at a glance. Just the bare `word-id` — the
            // `headway:<board>/` prefix is dropped since it's identical on every
            // card, and the detail sheet shows the full `headway:<board>/<word-id>`
            // for copy-paste (see [`headway::wordid`]).
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                // A dim ⊘ flags a card held back by an unfinished blocker — the
                // GUI mirror of the CLI listing's blocked prefix. It leads the
                // row so a blocked card reads as such at a glance.
                if card.is_blocked() {
                    ui.label(egui::RichText::new("⊘").small().color(theme.text_muted))
                        .on_hover_text("Blocked by unfinished work");
                }
                // The priority glyph leads the row, matching the CLI's board
                // listing; unprioritised cards stay unadorned.
                if card.priority != Priority::None {
                    priority_icon_ui(ui, theme, card.priority, 12.0)
                        .on_hover_text(priority_label(card.priority));
                }
                // A muted ↳ marks the card as someone's subissue; its parent is
                // named in the detail view's breadcrumb.
                if card.parent.is_some() {
                    ui.label(egui::RichText::new("↳").small().color(theme.text_muted));
                }
                ui.label(egui::RichText::new(&card.title).color(theme.text_primary));
                ui.label(
                    egui::RichText::new(headway::wordid::encode(card.id.bytes()))
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

            // Subissue rollup: how many of the card's children are done.
            if !card.subissues.is_empty() {
                ui.add_space(2.0);
                subissue_progress_pill(ui, theme, &card.subissues);
            }
        });
}

/// A compact `n/m` pill showing how many of a card's subissues are done —
/// derived from where the children sit on their boards, never stored.
fn subissue_progress_pill(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    subissues: &[event::SubissueView],
) {
    let done = subissues.iter().filter(|s| s.done).count();
    progress_pill(ui, theme, done, subissues.len())
        .on_hover_text(format!("{done} of {} subissues done", subissues.len()));
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
            // The `c` key can open it at the foot of a long or off-screen
            // column; bring it into view (a no-op when it already is).
            edit_response.scroll_to_me(None);
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
                    description: String::new(),
                    labels: vec![],
                    parent: None,
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
            state.open_add_card(col_idx);
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
    let menu = ui.menu_button("⋯", |ui| {
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
    // Open this frame: hold the board keys off it (see `grid_menu_open`).
    state.grid_menu_open |= menu.inner.is_some();
}

/// The "add a column" affordance at the right end of the board: a ghost column
/// that expands into a title composer when clicked.
pub(super) fn add_column_ui(
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
