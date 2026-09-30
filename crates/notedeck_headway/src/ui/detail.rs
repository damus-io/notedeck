//! The full-pane card detail view: title and description editors, the
//! properties sidebar, sub-issues (with drag reordering), dependencies,
//! labels, the activity/comment feed, and resolving the frame's edits into a
//! [`BoardAction`].

use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{
    RADIUS_LG, RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS, STROKE_THIN,
};
use notedeck_ui::diff::PatchScroll;

use super::card_actions::detail_scroll_ui;
use super::review::review_section_ui;
use super::widgets::{
    STATUS_DONE, StatusIcon, count_badge, detail_heading, label_color, priority_icon_ui,
    priority_label, secondary_action_button, section_label, status_icon_ui,
};
use super::{BoardUiState, EditMode, find_card, notice_ui, pane_hints_ui, seed_edit_mode};
use crate::event::{
    self, ActivityKind, ActivityView, BoardView, ColumnPos, CommentView, Priority, ReviewView,
};
use crate::store::{self, BoardAction};

/// Max width the full-pane card detail body is constrained to, so a card reads
/// comfortably instead of stretching across a wide window.
const DETAIL_CONTENT_WIDTH: f32 = 760.0;

/// Width of the card detail's fixed right-hand properties sidebar (status,
/// labels, actions), in the Linear-style wide layout.
const DETAIL_SIDEBAR_WIDTH: f32 = 220.0;

/// Pane width at or above which the detail view shows the properties sidebar;
/// below it the properties stack between the card body and its activity.
const DETAIL_SIDEBAR_MIN: f32 = 720.0;

/// Drag-and-drop payload for reordering a card's subissues in the detail sheet:
/// the id of the child being dragged. Distinct from `grid::DragCard` so a subissue
/// drag can never be mistaken for a board-column move.
#[derive(Clone)]
struct DragSubissue(NoteId);

/// The two siblings a subissue drop would land between, in display order: the
/// child it should follow (`after`) and the one it should precede (`before`).
/// Either end is `None` at the top or bottom of the list.
#[derive(Clone, Copy)]
struct SubissueDropGap {
    after: Option<NoteId>,
    before: Option<NoteId>,
}

/// The card detail screen, rendered full-pane in place of the board while a card
/// is selected. A top bar (back to board, current-status pill, ✕) sits above a
/// Linear-style layout: a scrolling reading column (title, description,
/// subissues, then the activity thread — always beneath the body, never beside
/// it) with the card's properties (status, labels, actions) in a fixed right
/// sidebar on wide panes, stacked inline on narrow ones. Edits are emitted as
/// [`BoardAction`]s (title/description commit on focus loss); dismissing
/// (back / ✕ / Escape) clears the selection.
pub(super) fn card_detail_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
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
        state.detail_title_seen = card.title.clone();
        state.detail_desc = card.description.clone();
        state.detail_desc_seen = card.description.clone();
        // A blank title opens straight into the editor; one with content shows
        // its render until the user asks to edit. The description always opens
        // rendered — its empty state is a quiet "Add description…" placeholder
        // (Linear-style) rather than a bare input box.
        state.detail_title_mode = seed_edit_mode(&card.title);
        state.detail_desc_mode = EditMode::Rendered;
        state.new_label.clear();
        state.label_composer = false;
        state.new_subissue.clear();
        state.subissue_composer = false;
        state.comment_draft.clear();
    }

    // Fold remote edits into the buffers live while the same card stays open.
    // Comments, activity, labels and status already render straight from the
    // fresh per-frame board; the title and description render from these edit
    // buffers, so without this they'd only pick up a relay/CLI edit on reopen.
    // Only refresh a field that's shown rendered (not mid-edit), and only when
    // the board's value has moved since we last synced — see `detail_title_seen`.
    if state.detail_title_mode == EditMode::Rendered && card.title != state.detail_title_seen {
        state.detail_title = card.title.clone();
        state.detail_title_seen = card.title.clone();
    }
    if state.detail_desc_mode == EditMode::Rendered && card.description != state.detail_desc_seen {
        state.detail_desc = card.description.clone();
        state.detail_desc_seen = card.description.clone();
    }

    let ctx = DetailCtx {
        card_id,
        reviews: &card.reviews,
        terminal: view.columns[current_col].terminal,
        card_ref: headway::wordid::card_ref(&view.id, card_id.bytes()),
        current_col,
        title: card.title.clone(),
        desc: card.description.clone(),
        labels: card.labels.clone(),
        priority: card.priority,
        // Owned copy so the body can render the status pill and column chips.
        columns: view.columns.iter().map(|c| c.name.clone()).collect(),
        created_at: card.created_at,
        updated_at: card.updated_at,
        comments: card.comments.clone(),
        activity: card.activity.clone(),
        parent: card.parent.map(|pid| DetailParent {
            id: pid,
            title: find_card(view, pid).map(|(_, p)| p.title.clone()),
        }),
        subissues: card
            .subissues
            .iter()
            .map(|s| DetailSubissue {
                id: s.id,
                title: s.title.clone(),
                // Resolve the column id to its display name; a child placed only
                // on another board keeps the raw id (better than nothing).
                column: s.column.as_ref().map(|col_id| {
                    view.columns
                        .iter()
                        .find(|c| &c.id == col_id)
                        .map(|c| c.name.clone())
                        .unwrap_or_else(|| col_id.clone())
                }),
                col_idx: s
                    .column
                    .as_ref()
                    .and_then(|col_id| view.columns.iter().position(|c| &c.id == col_id)),
                done: s.done,
                archived: s.archived,
                on_board: find_card(view, s.id).is_some(),
            })
            .collect(),
        blocked_by: card
            .blocked_by
            .iter()
            .map(|e| detail_edge(view, e))
            .collect(),
        blocks: card.blocks.iter().map(|e| detail_edge(view, e)).collect(),
    };

    // Escape backs out to the board (as `q` does, `crate::keys::detail_keys`).
    // Consume it so it doesn't also fall through to Chrome's Escape handler,
    // which would toggle the side menu.
    let mut outcome = if ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        DetailOutcome::Close
    } else {
        DetailOutcome::None
    };
    // A scroll the detail's keys asked for, applied at the end of whichever
    // scroll area the layout below draws.
    let scroll = state.detail_scroll.take();
    pane_hints_ui(ui, theme, state);

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            detail_pane_topbar_ui(ui, theme, &ctx, &mut state.notice, &mut outcome);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_MD);

            // Linear-style layout: one reading column — body, then activity
            // always beneath it — with the card's properties (status, labels,
            // actions) in a fixed right sidebar on a wide pane. Only the main
            // column scrolls, so the sidebar stays put under a long thread; on
            // a narrow pane the properties stack between body and activity.
            let avail = ui.available_width();
            if avail >= DETAIL_SIDEBAR_MIN {
                let main_w = avail - DETAIL_SIDEBAR_WIDTH - 2.0 * SPACING_LG;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(main_w, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            detail_main_column_ui(
                                ui,
                                theme,
                                app_ctx,
                                &ctx,
                                state,
                                action,
                                &mut outcome,
                                scroll,
                            );
                        },
                    );
                    ui.separator();
                    ui.allocate_ui_with_layout(
                        egui::vec2(DETAIL_SIDEBAR_WIDTH, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(DETAIL_SIDEBAR_WIDTH);
                            detail_properties_ui(ui, theme, &ctx, state, &mut outcome);
                        },
                    );
                });
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        ui.set_max_width(avail.min(DETAIL_CONTENT_WIDTH));
                        detail_body_ui(ui, theme, app_ctx, &ctx, state, action, &mut outcome);
                        ui.add_space(SPACING_MD);
                        ui.separator();
                        ui.add_space(SPACING_MD);
                        detail_properties_ui(ui, theme, &ctx, state, &mut outcome);
                        ui.add_space(SPACING_MD);
                        ui.separator();
                        ui.add_space(SPACING_MD);
                        detail_comments_ui(ui, theme, app_ctx, &ctx, state, &mut outcome);
                        detail_scroll_ui(ui, scroll, top);
                    });
            }
        });

    resolve_detail_outcome(state, action, view, &ctx, outcome);
}

/// The full-pane detail top bar, a Linear-style breadcrumb: a back affordance,
/// then the card's word-id reference (click to copy — the GUI mirror of what
/// the CLI prints, a stable handle for commits/chat), and a trailing ✕ that
/// also dismisses, with a key's short-lived notice ahead of it.
///
/// The back button steps one entry back in the chrome global history (the same
/// as the browser back chevron), so from a card opened off the board it returns
/// to the board, and from a subissue it returns to the parent card it was drilled
/// from — hence "← Back" rather than a fixed "← Board".
fn detail_pane_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    notice: &mut Option<(super::QueueNotice, f64)>,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        let back = egui::Button::new(egui::RichText::new("← Back").color(theme.text_secondary))
            .fill(egui::Color32::TRANSPARENT)
            .frame(false);
        if ui.add(back).clicked() {
            *outcome = DetailOutcome::Close;
        }
        ui.label(egui::RichText::new("›").color(theme.text_muted));
        // A frameless button, not a Label: labels are selectable by default, so
        // a click would start a text selection instead of a one-click copy.
        // NB: `Button::fill()` silently re-enables the frame, so it must not
        // follow `.frame(false)` — a transparent fill still paints the border.
        let card_ref = egui::Button::new(
            egui::RichText::new(&ctx.card_ref)
                .color(theme.text_muted)
                .small(),
        )
        .frame(false);
        if ui.add(card_ref).on_hover_text("Click to copy").clicked() {
            ui.ctx().copy_text(ctx.card_ref.clone());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let x = egui::Button::new(egui::RichText::new("✕").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(x).clicked() {
                *outcome = DetailOutcome::Close;
            }
            notice_ui(ui, theme, notice);
        });
    });
}

/// The card data the detail sheet needs, copied out of the (immutable)
/// `BoardView` so the sheet body doesn't borrow it while we also mutate `state`.
struct DetailCtx<'a> {
    card_id: NoteId,
    /// The card's review records, newest first — borrowed off the frame's view,
    /// which outlives the pane, rather than cloned every frame.
    reviews: &'a [ReviewView],
    /// The card sits in a terminal ("done") column, so its Review section is
    /// offered even with no records (the pane then searches by trailer).
    terminal: bool,
    /// The card's human-friendly reference, e.g. `headway:maple-river-canyon`:
    /// the board slug plus the word-encoded event id (see [`headway::wordid`]).
    card_ref: String,
    current_col: usize,
    title: String,
    desc: String,
    labels: Vec<String>,
    /// The card's resolved priority, shown as an editable row in the sidebar.
    priority: Priority,
    columns: Vec<String>,
    /// When the card was created / last amended (see [`CardView`](crate::event::CardView)), rendered
    /// as relative times next to the card ref.
    created_at: u64,
    updated_at: u64,
    /// The card's comment thread, oldest first. Rendered flat (replies aren't
    /// indented yet) but each carries its `parent` for forward-compatibility.
    comments: Vec<CommentView>,
    /// The card's derived activity timeline (created / moved / renamed / …),
    /// oldest first, interleaved chronologically with the comments.
    activity: Vec<ActivityView>,
    /// The card's parent, when it's a subissue (rendered as a breadcrumb).
    parent: Option<DetailParent>,
    /// The card's subissues, precomputed for the checklist rows.
    subissues: Vec<DetailSubissue>,
    /// Cards this one is *blocked by*, precomputed for the "Blocked by" section
    /// (editable: each row unblocks, the context menu adds).
    blocked_by: Vec<DetailEdge>,
    /// The reverse edges — cards this one *blocks* — for the read-only "Blocks"
    /// section.
    blocks: Vec<DetailEdge>,
}

/// One row of the detail sheet's dependency lists, precomputed from a card's
/// [`EdgeRef`]. `on_board` records whether the other card is placed here, so the
/// row can be a click-to-open link (like a subissue row) rather than an inert
/// label for a cross-board edge.
struct DetailEdge {
    id: NoteId,
    title: String,
    /// The referenced card is cleared (done/archived), so this edge no longer
    /// holds work back — rendered struck through and dimmed.
    done: bool,
    /// The referenced card's live [`ColumnPos`] on this board, driving its *true*
    /// status circle (a blocker in In Review reads as in-progress even though the
    /// edge is cleared). `None` when it isn't on a live column here — archived,
    /// or a cross-board edge.
    column: Option<ColumnPos>,
    /// Whether the other card is on this board, i.e. clickable to open.
    on_board: bool,
}

/// Resolve one dependency [`EdgeRef`] into a [`DetailEdge`], marking whether the
/// referenced card is placed on this board (so its row can open it) and its live
/// column position (for the true-status circle).
fn detail_edge(view: &BoardView, edge: &event::EdgeRef) -> DetailEdge {
    let count = view.columns.len();
    let column = view
        .columns
        .iter()
        .position(|c| c.cards.iter().any(|card| card.id == edge.id))
        .map(|index| ColumnPos { index, count });
    DetailEdge {
        id: edge.id,
        title: edge.title.clone(),
        done: edge.done,
        column,
        on_board: find_card(view, edge.id).is_some(),
    }
}

/// The open card's parent, resolved for the detail breadcrumb. `title` is
/// `None` when the parent isn't placed on this board — it can't be opened from
/// here, so the breadcrumb falls back to showing its word-id.
struct DetailParent {
    id: NoteId,
    title: Option<String>,
}

/// One row of the detail sheet's subissue checklist, precomputed from the
/// board view (column ids resolved to names) so the render closures don't
/// re-borrow it.
struct DetailSubissue {
    id: NoteId,
    title: String,
    /// The live column's display *name*, or the raw column id for a child
    /// placed only on another board. `None` when unplaced or archived.
    column: Option<String>,
    /// The live column's index on *this* board, for the status circle. `None`
    /// when the child is archived, unplaced, or lives on another board.
    col_idx: Option<usize>,
    done: bool,
    archived: bool,
    /// Whether the child is on this board, i.e. clickable to open its detail.
    on_board: bool,
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
    /// Set the card's priority to this level.
    SetPriority(Priority),
    /// Commit the "add label" field.
    AddLabel,
    /// Remove this label from the card's set.
    RemoveLabel(String),
    /// Post the contents of the comment composer as a new top-level comment.
    AddComment,
    /// Open another card's detail (a subissue row or the parent breadcrumb).
    OpenCard(NoteId),
    /// Commit the "add subissue" field: create a card parented to this one.
    AddSubissue,
    /// Reorder a subissue: place `child` in the work-order gap between two
    /// siblings (`after`/`before`, either `None` at an end). The parent is the
    /// detail card; the fractional rank is computed on resolve.
    ReorderSubissue {
        child: NoteId,
        after: Option<NoteId>,
        before: Option<NoteId>,
    },
    /// Clear this card's parent relation.
    DetachParent,
    /// Lift a blocker: remove the `card`-blocked-by-`on` dependency edge.
    Unblock(NoteId),
}

/// The dimmed full-screen backdrop behind the sheet. Returns true if it was
/// clicked (a tap outside the sheet, which closes the detail).
pub(super) fn detail_scrim_ui(ui: &mut egui::Ui, screen: egui::Rect) -> bool {
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
pub(super) fn detail_sheet_frame(theme: &ColorTheme, pad: f32) -> egui::Frame {
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

/// The wide layout's scrolling main column: the card body with the activity
/// thread beneath it, width-capped so it reads like a document while the
/// properties sidebar sits fixed to its right. Takes the detail keys' `scroll`.
#[allow(clippy::too_many_arguments)]
fn detail_main_column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
    outcome: &mut DetailOutcome,
    scroll: Option<PatchScroll>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let top = ui.cursor().min;
            ui.set_max_width(ui.available_width().min(DETAIL_CONTENT_WIDTH));
            detail_body_ui(ui, theme, app_ctx, ctx, state, action, outcome);
            ui.add_space(SPACING_MD);
            ui.separator();
            ui.add_space(SPACING_MD);
            detail_comments_ui(ui, theme, app_ctx, ctx, state, outcome);
            detail_scroll_ui(ui, scroll, top);
        });
}

/// The card body proper: breadcrumb, title, ref, description and subissues.
/// Title/description commit directly on focus loss; the rest is collected into
/// `outcome` and resolved by the caller. Properties (status, labels, actions)
/// live in [`detail_properties_ui`], Linear-style.
fn detail_body_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
    outcome: &mut DetailOutcome,
) {
    if let Some(parent) = &ctx.parent {
        detail_parent_breadcrumb_ui(ui, theme, parent, outcome);
        ui.add_space(SPACING_XS);
    }
    detail_title_section_ui(ui, ctx, state, action);

    ui.add_space(SPACING_MD);
    detail_description_section_ui(ui, theme, app_ctx, ctx, state, action);

    ui.add_space(SPACING_LG);
    detail_subissues_section_ui(ui, theme, ctx, state, outcome);

    // A card with sub-issues is an epic, so offer its dependency graph. This is
    // the epic entry point into the graph view: `open_graph` sets the local graph
    // mode (and reframes the scene), which the app's `reconcile_nav` diffs into a
    // pushed `HeadwayRoute::Graph` — exactly as a card click becomes a pushed
    // `Card` entry — so opening the graph joins the chrome global-nav stack and a
    // global-back returns to this epic's detail.
    if !ctx.subissues.is_empty() {
        ui.add_space(SPACING_MD);
        if secondary_action_button(ui, theme, "☍ View dependency graph").clicked() {
            state.open_graph(ctx.card_id);
        }
    }

    // The commit(s) that finished the card, and the entry point into its review
    // pane — which, like the graph, `reconcile_nav` turns into a pushed
    // `HeadwayRoute::Review` entry.
    if !ctx.reviews.is_empty() || ctx.terminal {
        ui.add_space(SPACING_LG);
        review_section_ui(ui, theme, app_ctx, ctx.card_id, ctx.reviews, state);
    }
}

/// The card's properties — status, labels, dependency edges, dates and the
/// archive/delete actions — rendered as a flat, frameless Linear-style sidebar:
/// each group is a muted section label over its rows, separated only by
/// whitespace (no bordered panels). Fixed to the right on a wide pane, stacked
/// between the body and activity on a narrow one.
fn detail_properties_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    section_label(ui, theme, "Properties");
    ui.add_space(SPACING_SM);
    // Move the card between lanes without dragging (meaningless on a
    // single-column board).
    if ctx.columns.len() > 1 {
        detail_status_row_ui(ui, theme, ctx, outcome);
        ui.add_space(SPACING_SM);
    }
    detail_priority_row_ui(ui, theme, ctx, outcome);
    ui.add_space(SPACING_SM);

    // Same `rel_time` as the comment thread, so the two read alike. The
    // updated time only appears once it has drifted past creation.
    ui.label(
        egui::RichText::new(format!(
            "Created {}",
            headway::fmt::rel_time(ctx.created_at)
        ))
        .small()
        .color(theme.text_muted),
    );
    if ctx.updated_at > ctx.created_at {
        ui.label(
            egui::RichText::new(format!(
                "Updated {}",
                headway::fmt::rel_time(ctx.updated_at)
            ))
            .small()
            .color(theme.text_muted),
        );
    }

    // Generous whitespace between groups is what separates them now the borders
    // are gone — the Linear sidebar rhythm.
    ui.add_space(SPACING_LG);
    detail_labels_section_ui(ui, theme, ctx, state, outcome);

    // Dependency edges get their own flat group under Labels, Linear-style,
    // rather than sitting in the main column where they read as sub-issues.
    // Skipped entirely when the card has no edges, so no empty heading shows.
    if !ctx.blocked_by.is_empty() || !ctx.blocks.is_empty() {
        ui.add_space(SPACING_LG);
        detail_dependencies_section_ui(ui, theme, ctx, outcome);
    }

    // Quiet, frameless actions under the groups — Linear keeps these out of
    // the way rather than as filled buttons.
    ui.add_space(SPACING_LG);
    ui.horizontal(|ui| {
        let archive = egui::Button::new(
            egui::RichText::new("Archive")
                .small()
                .color(theme.text_muted),
        )
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);
        if ui.add(archive).clicked() {
            *outcome = DetailOutcome::Archive;
        }
        let delete = egui::Button::new(
            egui::RichText::new("Delete card")
                .small()
                .color(theme.destructive),
        )
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);
        if ui.add(delete).clicked() {
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

/// The card's description: rendered markdown flowing straight under the title
/// (no section header, Linear-style), with a muted ✎ affordance beneath it and
/// double-click on the rendered text as the shortcut into the raw multiline
/// editor. Edits commit as a [`BoardAction::EditDescription`] when the editor
/// loses focus, which also returns the section to its rendered view.
///
/// Rendered through the ref-aware markdown path, so a `headway:board/word-word-word`
/// mention of another card draws as that card's live status chip (resolved by
/// our own [`HeadwayRefParser`](crate::HeadwayRefParser)) and opens it on click —
/// descriptions cross-reference constantly, so this is the surface the inline
/// reference registry pays for most.
fn detail_description_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
) {
    match state.detail_desc_mode {
        EditMode::Rendered => {
            // Empty description: a quiet Linear-style placeholder that becomes
            // the editor on click, instead of dropping a fresh card straight
            // into a big input box.
            if state.detail_desc.trim().is_empty() {
                let add = egui::Button::new(
                    egui::RichText::new("Add description…").color(theme.text_muted),
                )
                .frame(false);
                if ui.add(add).clicked() {
                    state.detail_desc_mode = EditMode::Editing { focus: true };
                }
                return;
            }

            // Render with interactive task-list checkboxes; a click flips the
            // box in `detail_desc` in place and we persist it like any edit.
            // The detail pane holds no transaction; open one here and pass it in
            // so a `headway:board/word-word-word` in the description resolves to its chip.
            let mut note_ctx = app_ctx.note_context();
            let txn = nostrdb::Transaction::new(note_ctx.ndb).expect("detail txn");
            let scope = ui.scope(|ui| {
                notedeck_ui::markdown::render_markdown_with_refs_editable(
                    ui,
                    &mut note_ctx,
                    &txn,
                    &mut state.detail_desc,
                )
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
            // With no section header, this muted row under the text is the
            // discoverable way into the editor (double-click is the shortcut).
            ui.add_space(SPACING_XS);
            let edit = egui::Button::new(egui::RichText::new("✎").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(edit).on_hover_text("Edit description").clicked() {
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
            // Commit on focus loss and drop back to the rendered view — an
            // empty description renders as the "Add description…" placeholder.
            if desc_resp.lost_focus() {
                if state.detail_desc != ctx.desc {
                    *action = Some(BoardAction::EditDescription {
                        card: ctx.card_id,
                        description: state.detail_desc.clone(),
                    });
                }
                state.detail_desc_mode = EditMode::Rendered;
            }
        }
    }
}

/// A muted "↳ subissue of <parent>" breadcrumb above the title. The parent name
/// is a click-to-open button when the parent is on this board; a trailing ✕
/// detaches the card from it.
fn detail_parent_breadcrumb_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    parent: &DetailParent,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        ui.label(
            egui::RichText::new("↳ subissue of")
                .small()
                .color(theme.text_muted),
        );
        match &parent.title {
            Some(title) => {
                // A Link, not a boxed button: plain text with an underline on
                // hover, the way Linear treats issue references.
                let link = egui::Link::new(
                    egui::RichText::new(title)
                        .small()
                        .color(theme.text_secondary),
                );
                if ui.add(link).on_hover_text("Open parent").clicked() {
                    *outcome = DetailOutcome::OpenCard(parent.id);
                }
            }
            // The parent lives on another board (or is archived); name it by
            // reference since it can't be opened from here.
            None => {
                ui.label(
                    egui::RichText::new(headway::wordid::encode(parent.id.bytes()))
                        .small()
                        .color(theme.text_muted),
                );
            }
        }
        let x = egui::Button::new(egui::RichText::new("✕").small().color(theme.text_muted))
            .frame(false);
        if ui.add(x).on_hover_text("Detach from parent").clicked() {
            *outcome = DetailOutcome::DetachParent;
        }
    });
}

/// Sub-issues section: the derived checklist, Linear-style — a painted status
/// circle (derived from where the child sits on its board, never a stored
/// tick), the child's title (click to open when it's on this board) and a
/// muted column/archived hint — plus a collapsed "+ Add sub-issue" affordance
/// that opens an inline composer creating a card already parented to this one.
fn detail_subissues_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        detail_heading(ui, theme, "Sub-issues");
        if !ctx.subissues.is_empty() {
            let done = ctx.subissues.iter().filter(|s| s.done).count();
            // Linear's rollup donut: the same started-pie, filled to the
            // completed fraction, next to a muted count.
            status_icon_ui(
                ui,
                theme,
                StatusIcon::Started(done as f32 / ctx.subissues.len() as f32),
                12.0,
            );
            ui.label(
                egui::RichText::new(format!("{done}/{}", ctx.subissues.len()))
                    .small()
                    .color(theme.text_muted),
            );
        }
    });
    ui.add_space(SPACING_XS);

    // Drag-reorder: each row carries a grip handle (the drag source) and senses
    // a hovering drag to record the siblings straddling the drop; the list frame
    // catches the release. The fractional rank is computed later in
    // `resolve_detail_outcome` (it needs the board view) — here we report intent.
    let mut drop_gap: Option<SubissueDropGap> = None;
    let list = ui
        .vertical(|ui| {
            for (i, sub) in ctx.subissues.iter().enumerate() {
                let row = subissue_row_ui(ui, theme, ctx, sub, outcome);
                if let Some(gap) = subissue_drop_target(ui, theme, &row, i, &ctx.subissues) {
                    drop_gap = Some(gap);
                }
            }
        })
        .response;
    commit_subissue_drop(&list, drop_gap, &ctx.subissues, outcome);

    if !ctx.subissues.is_empty() {
        ui.add_space(SPACING_XS);
    }

    // Collapsed behind "+ Add sub-issue" (Linear-style); once open, commit on
    // the button or Enter. The new card lands in the first column, parented to
    // this one, and the composer stays open for rapid entry.
    if !state.subissue_composer {
        let add = egui::Button::new(
            egui::RichText::new("+ Add sub-issue")
                .small()
                .color(theme.text_muted),
        )
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);
        if ui.add(add).clicked() {
            state.subissue_composer = true;
            state.focus_edit = true;
        }
        return;
    }
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.new_subissue)
                .desired_width(220.0)
                .hint_text("Add sub-issue…"),
        );
        if state.focus_edit {
            field.request_focus();
            state.focus_edit = false;
        }
        let submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Add").clicked() || submit {
            *outcome = DetailOutcome::AddSubissue;
        }
    });
}

/// One subissue row: a drag grip, the child's status circle, its title (a link
/// that opens the child when it's on this board), a placement hint and the muted
/// word-id. Returns the row response so the caller can sense a hovering drag.
fn subissue_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    sub: &DetailSubissue,
    outcome: &mut DetailOutcome,
) -> egui::Response {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        // The grip is a separate drag source so the title link's click-to-open
        // stays untouched.
        subissue_drag_handle(ui, theme, sub.id);
        // The child's status circle shows its *true* column status: a child in a
        // terminal-but-not-last column (e.g. In Review) reads as in-progress, not
        // done, even though it still counts toward the rollup and clears its
        // dependents. An off-board child we can't position falls back to
        // done-if-finished (covers archived, whose `done` is set), else a todo
        // ring. Derived from the board so it can never go stale.
        let icon = match sub.col_idx {
            Some(i) => StatusIcon::for_column(i, ctx.columns.len()),
            None if sub.done => StatusIcon::Done,
            None => StatusIcon::Todo,
        };
        status_icon_ui(ui, theme, icon, 14.0);
        if sub.on_board {
            // A Link, not a boxed button: the row reads as plain text and
            // underlines on hover, like Linear's sub-issue list.
            let link = egui::Link::new(egui::RichText::new(&sub.title).color(theme.text_primary));
            if ui.add(link).on_hover_text("Open subissue").clicked() {
                *outcome = DetailOutcome::OpenCard(sub.id);
            }
        } else {
            ui.label(egui::RichText::new(&sub.title).color(theme.text_primary));
        }
        // Where the child sits: archived trumps the column (an archived child has
        // no live column to show).
        let hint = if sub.archived {
            Some("archived")
        } else {
            sub.column.as_deref()
        };
        if let Some(hint) = hint {
            ui.label(
                egui::RichText::new(format!("({hint})"))
                    .small()
                    .color(theme.text_muted),
            );
        }
        ui.label(
            egui::RichText::new(headway::wordid::encode(sub.id.bytes()))
                .small()
                .color(theme.text_muted.gamma_multiply(0.6)),
        );
    })
    .response
}

/// A small dotted grip that is the drag source for a subissue row. Painted (not
/// a glyph) so it never depends on font coverage, and shows a grab cursor on
/// hover to read as draggable.
fn subissue_drag_handle(ui: &mut egui::Ui, theme: &ColorTheme, id: NoteId) {
    let resp = ui
        .dnd_drag_source(
            egui::Id::new(("headway-subissue-drag", id)),
            DragSubissue(id),
            |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 16.0), egui::Sense::hover());
                let color = theme.text_muted.gamma_multiply(0.7);
                for row in 0..3 {
                    for col in 0..2 {
                        let center = egui::pos2(
                            rect.left() + 3.0 + col as f32 * 4.0,
                            rect.center().y - 4.0 + row as f32 * 4.0,
                        );
                        ui.painter().circle_filled(center, 0.9, color);
                    }
                }
            },
        )
        .response;
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
}

/// While a subissue drag hovers `row`, draw an insertion line at the nearer edge
/// and return the gap (the siblings straddling the drop) the card would land in.
fn subissue_drop_target(
    ui: &egui::Ui,
    theme: &ColorTheme,
    row: &egui::Response,
    i: usize,
    subs: &[DetailSubissue],
) -> Option<SubissueDropGap> {
    let pointer = ui.input(|i| i.pointer.interact_pos())?;
    row.dnd_hover_payload::<DragSubissue>()?;
    let rect = row.rect;
    let above = pointer.y < rect.center().y;
    let y = if above { rect.top() } else { rect.bottom() };
    ui.painter().hline(
        rect.x_range(),
        y,
        egui::Stroke::new(STROKE_THIN, theme.accent),
    );
    Some(if above {
        SubissueDropGap {
            after: (i > 0).then(|| subs[i - 1].id),
            before: Some(subs[i].id),
        }
    } else {
        SubissueDropGap {
            after: Some(subs[i].id),
            before: subs.get(i + 1).map(|s| s.id),
        }
    })
}

/// Commit a released subissue drag: land it in the hovered gap (or append to the
/// end when let go past the last row), unless it was dropped back beside itself.
fn commit_subissue_drop(
    list: &egui::Response,
    drop_gap: Option<SubissueDropGap>,
    subs: &[DetailSubissue],
    outcome: &mut DetailOutcome,
) {
    let Some(payload) = list.dnd_release_payload::<DragSubissue>() else {
        return;
    };
    let gap = drop_gap.unwrap_or(SubissueDropGap {
        after: subs.last().map(|s| s.id),
        before: None,
    });
    if gap.after == Some(payload.0) || gap.before == Some(payload.0) {
        return;
    }
    *outcome = DetailOutcome::ReorderSubissue {
        child: payload.0,
        after: gap.after,
        before: gap.before,
    };
}

/// The card's dependency edges, a Linear-style sidebar block under Labels: a
/// "Blocked by" list of the cards holding this one back (each row unblockable
/// with a trailing ✕, the way the parent breadcrumb detaches) and a read-only
/// "Blocks" list of the cards it holds back. Both are omitted when empty; the
/// caller ([`detail_properties_ui`]) skips the wrapping panel entirely when
/// there are no edges at all, so an empty bordered block never shows. Adding a
/// blocker lives in the board card's context menu (`grid::card_blocker_menu`), like
/// re-parenting.
fn detail_dependencies_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    if !ctx.blocked_by.is_empty() {
        ui.horizontal(|ui| {
            section_label(ui, theme, "Blocked by");
            // A dim ⊘ next to the heading when at least one blocker is still
            // open — the same signal the board listing carries.
            if ctx.blocked_by.iter().any(|e| !e.done) {
                ui.label(egui::RichText::new("⊘").small().color(theme.text_muted))
                    .on_hover_text("Held back by an unfinished blocker");
            }
        });
        ui.add_space(SPACING_XS);
        for edge in &ctx.blocked_by {
            detail_edge_row_ui(ui, theme, edge, true, outcome);
        }
    }

    if !ctx.blocks.is_empty() {
        // Separate the two lists only when both are present, so a single-list
        // panel doesn't carry dead space above its heading.
        if !ctx.blocked_by.is_empty() {
            ui.add_space(SPACING_MD);
        }
        section_label(ui, theme, "Blocks");
        ui.add_space(SPACING_XS);
        for edge in &ctx.blocks {
            detail_edge_row_ui(ui, theme, edge, false, outcome);
        }
    }
}

/// One dependency row, Linear-style: a single clean line of a cleared/open
/// status circle and the other card's title, truncated to an ellipsis rather
/// than wrapped. The title opens the card when it's on this board (struck
/// through once the edge is cleared). Only while the row is hovered do the muted
/// word-id and — when `unblockable` (the "Blocked by" side) — a trailing ✕ that
/// lifts the blocker appear on the right. The reverse "Blocks" side is read-only
/// (the edge is owned by the other card's blocker set), so it passes
/// `unblockable = false` and never shows a ✕.
fn detail_edge_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    edge: &DetailEdge,
    unblockable: bool,
    outcome: &mut DetailOutcome,
) {
    // Reveal the word-id and unblock ✕ only while the row is hovered. Hover is
    // read from last frame's registered response so we know it before laying the
    // row out — the trailing controls reflow the title's truncation, which we
    // can't decide mid-layout.
    let row_id = ui.make_persistent_id(("headway-edge-row", edge.id, unblockable));
    let hovered = ui
        .ctx()
        .read_response(row_id)
        .is_some_and(|r| r.contains_pointer());

    // The icon shows the blocker's *true* column status — a blocker in In Review
    // reads as in-progress, not done, even though the edge is cleared (the
    // strikethrough below carries the cleared signal). Off-board/archived blockers
    // we can't position fall back to done-if-cleared, else a plain ring.
    let icon = match edge.column {
        Some(pos) => StatusIcon::for_column(pos.index, pos.count),
        None if edge.done => StatusIcon::Done,
        None => StatusIcon::Todo,
    };
    // A cleared blocker reads as struck-through and muted, so an open one is the
    // eye's anchor (the CLI's `[x]`/`[ ]` distinction).
    let title = if edge.done {
        egui::RichText::new(&edge.title)
            .strikethrough()
            .color(theme.text_muted)
    } else {
        egui::RichText::new(&edge.title).color(theme.text_primary)
    };

    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        status_icon_ui(ui, theme, icon, 14.0);
        // Trailing controls pin to the right on hover; the title fills whatever
        // is left and truncates to one line with an ellipsis.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if hovered && unblockable {
                let x = egui::Button::new(egui::RichText::new("✕").small().color(theme.text_muted))
                    .frame(false);
                if ui.add(x).on_hover_text("Remove blocker").clicked() {
                    *outcome = DetailOutcome::Unblock(edge.id);
                }
            }
            if hovered {
                ui.label(
                    egui::RichText::new(headway::wordid::encode(edge.id.bytes()))
                        .small()
                        .color(theme.text_muted.gamma_multiply(0.6)),
                );
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let label = egui::Label::new(title).truncate();
                if edge.on_board {
                    let resp = ui
                        .add(label.sense(egui::Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Open card");
                    if resp.clicked() {
                        *outcome = DetailOutcome::OpenCard(edge.id);
                    }
                } else {
                    ui.add(label);
                }
            });
        });
    });

    // Register this frame's row rect under the stable id so next frame's hover
    // read resolves; repaint on change so the reveal isn't a frame late.
    let resp = ui.interact(row.response.rect, row_id, egui::Sense::hover());
    if resp.contains_pointer() != hovered {
        ui.ctx().request_repaint();
    }
}

/// Labels section: removable Linear-style chips (a colored dot in a neutral
/// outline pill) plus a collapsed "+ Add label" affordance opening the
/// composer field.
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
            if detail_label_chip_ui(ui, theme, label) {
                *outcome = DetailOutcome::RemoveLabel(label.clone());
            }
        }
    });
    ui.add_space(SPACING_XS);
    if !state.label_composer {
        let add = egui::Button::new(
            egui::RichText::new("+ Add label")
                .small()
                .color(theme.text_muted),
        )
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);
        if ui.add(add).clicked() {
            state.label_composer = true;
            state.focus_edit = true;
        }
        return;
    }
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.new_label)
                .desired_width(110.0)
                .hint_text("Add label…"),
        );
        if state.focus_edit {
            field.request_focus();
            state.focus_edit = false;
        }
        let submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Add").clicked() || submit {
            *outcome = DetailOutcome::AddLabel;
        }
    });
}

/// One of the detail sheet's label chips, Linear-style: a colored dot and the
/// label's name in a neutral outline pill, with a trailing ✕ to remove it.
/// Returns true if ✕ was clicked.
fn detail_label_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, label: &str) -> bool {
    let mut remove = false;
    egui::Frame::new()
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, label_color(label));
                // Extend (don't wrap) so the chip reports its full natural
                // width and wraps as a whole (see `label_chip`).
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(label)
                            .small()
                            .color(theme.text_secondary),
                    )
                    .extend(),
                );
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("✕").small().color(theme.text_muted))
                            .frame(false),
                    )
                    .on_hover_text(format!("Remove {label}"))
                    .clicked()
                {
                    remove = true;
                }
            });
        });
    remove
}

/// The Properties panel's status row: the current column's status circle and
/// name, opening a dropdown of every column (each with its own circle) to move
/// the card without dragging — Linear's status control.
fn detail_status_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    let n = ctx.columns.len();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        status_icon_ui(ui, theme, StatusIcon::for_column(ctx.current_col, n), 14.0);
        let name = egui::RichText::new(&ctx.columns[ctx.current_col]).color(theme.text_primary);
        // Flatten the dropdown's idle state so the row reads as plain text
        // (Linear-style); hover still highlights it as clickable.
        ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.menu_button(name, |ui| {
            for (i, col) in ctx.columns.iter().enumerate() {
                let selected = i == ctx.current_col;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACING_XS;
                    status_icon_ui(ui, theme, StatusIcon::for_column(i, n), 14.0);
                    if ui.selectable_label(selected, col).clicked() {
                        if !selected {
                            *outcome = DetailOutcome::MoveTo(i);
                        }
                        ui.close_menu();
                    }
                });
            }
        });
    });
}

/// The card's priority as a flat dropdown row, mirroring [`detail_status_row_ui`]:
/// the current priority's icon + label reads as plain text, and the menu lists
/// every level (including "No priority" to clear it). A pick raises
/// [`DetailOutcome::SetPriority`].
fn detail_priority_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    const LEVELS: [Priority; 5] = [
        Priority::None,
        Priority::Urgent,
        Priority::High,
        Priority::Medium,
        Priority::Low,
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        priority_icon_ui(ui, theme, ctx.priority, 14.0);
        let label = egui::RichText::new(priority_label(ctx.priority)).color(theme.text_primary);
        // Flatten the idle background so the row reads as plain text; hover still
        // highlights it as clickable.
        ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.menu_button(label, |ui| {
            for level in LEVELS {
                let selected = level == ctx.priority;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = SPACING_XS;
                    priority_icon_ui(ui, theme, level, 14.0);
                    if ui
                        .selectable_label(selected, priority_label(level))
                        .clicked()
                    {
                        if !selected {
                            *outcome = DetailOutcome::SetPriority(level);
                        }
                        ui.close_menu();
                    }
                });
            }
        });
    });
}

/// The open card's activity feed, Linear-style: a section heading with a
/// comment count, then the derived activity timeline (created / moved /
/// renamed / …) interleaved chronologically with the comment thread, and a
/// composer to add a comment. Replies aren't indented yet (`store now, render
/// flat`) but each comment still shows who it threads under. Posting is
/// collected into `outcome`.
fn detail_comments_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        detail_heading(ui, theme, "Activity");
        if !ctx.comments.is_empty() {
            count_badge(ui, theme, ctx.comments.len());
        }
    });
    ui.add_space(SPACING_SM);

    // One read txn for the whole feed; each kind-1111 comment is drawn with
    // the shared notedeck_ui note renderer so headway comments carry the same
    // pfp/name/time chrome they'd get anywhere else in notedeck. Both lists
    // arrive sorted oldest-first, so a two-pointer merge interleaves them
    // without allocating; a same-second tie shows the activity row first.
    let txn = nostrdb::Transaction::new(app_ctx.ndb).ok();
    let (mut ai, mut ci) = (0, 0);
    while ai < ctx.activity.len() || ci < ctx.comments.len() {
        let comment_first = match (ctx.activity.get(ai), ctx.comments.get(ci)) {
            (Some(a), Some(c)) => c.created_at < a.created_at,
            (None, Some(_)) => true,
            _ => false,
        };
        if comment_first {
            comment_note_ui(ui, theme, app_ctx, txn.as_ref(), &ctx.comments[ci]);
            ci += 1;
        } else {
            activity_row_ui(
                ui,
                theme,
                app_ctx,
                txn.as_ref(),
                &ctx.activity[ai],
                ctx.columns.len(),
            );
            ai += 1;
        }
    }

    ui.add_space(SPACING_MD);
    detail_comment_composer_ui(ui, theme, state, outcome);
}

/// One derived activity-timeline row, Linear-style: a small gutter icon (the
/// destination's status circle for a move, a plain dot otherwise), the actor,
/// a muted phrase describing the change, and a relative time. Deliberately
/// much quieter than a comment card — these rows are the connective tissue
/// between comments, not content.
fn activity_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    activity: &ActivityView,
    ncols: usize,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;

        match &activity.kind {
            ActivityKind::Moved {
                to_idx: Some(i), ..
            }
            | ActivityKind::Restored {
                to_idx: Some(i), ..
            } => {
                status_icon_ui(ui, theme, StatusIcon::for_column(*i, ncols), 12.0);
            }
            _ => {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 2.0, theme.text_muted);
            }
        }

        let strong = |ui: &mut egui::Ui, s: &str| {
            ui.label(egui::RichText::new(s).small().color(theme.text_secondary));
        };
        let muted = |ui: &mut egui::Ui, s: &str| {
            ui.label(egui::RichText::new(s).small().color(theme.text_muted));
        };

        strong(ui, &author_name(app_ctx, txn, &activity.author));
        match &activity.kind {
            ActivityKind::Created => muted(ui, "created the card"),
            ActivityKind::Moved { from, to, .. } => {
                match from {
                    Some(from) => {
                        muted(ui, "moved from");
                        strong(ui, from);
                        muted(ui, "to");
                    }
                    None => muted(ui, "moved to"),
                }
                strong(ui, to);
            }
            ActivityKind::Archived => muted(ui, "archived the card"),
            ActivityKind::Restored { to, .. } => {
                muted(ui, "restored the card to");
                strong(ui, to);
            }
            ActivityKind::Renamed { to } => {
                muted(ui, "renamed the card to");
                strong(ui, &format!("“{to}”"));
            }
            ActivityKind::DescriptionEdited => muted(ui, "updated the description"),
            ActivityKind::LabelsChanged { added, removed } => {
                if !added.is_empty() {
                    muted(
                        ui,
                        if added.len() == 1 {
                            "added label"
                        } else {
                            "added labels"
                        },
                    );
                    strong(ui, &added.join(", "));
                }
                if !removed.is_empty() {
                    if !added.is_empty() {
                        muted(ui, "and removed");
                    } else {
                        muted(
                            ui,
                            if removed.len() == 1 {
                                "removed label"
                            } else {
                                "removed labels"
                            },
                        );
                    }
                    strong(ui, &removed.join(", "));
                }
            }
            ActivityKind::ParentSet { parent, title } => {
                muted(ui, "set the parent to");
                match title {
                    Some(title) => strong(ui, title),
                    None => strong(ui, &headway::wordid::encode(parent.bytes())),
                }
            }
            ActivityKind::FieldChanged { field, to } => {
                use headway::event::Field;
                if to.is_empty() {
                    muted(ui, &format!("cleared the {}", field.label()));
                } else {
                    let verb = match field {
                        Field::Priority => "set priority to",
                        Field::Due => "set the due date to",
                        Field::Estimate => "set the estimate to",
                    };
                    muted(ui, verb);
                    strong(ui, to);
                }
            }
            ActivityKind::ParentRemoved => muted(ui, "detached from its parent"),
            ActivityKind::Review { commit, host } => {
                match commit {
                    Some(commit) => {
                        muted(ui, "recorded commit");
                        strong(ui, commit.get(..7).unwrap_or(commit));
                    }
                    None => muted(ui, "recorded a review"),
                }
                if let Some(host) = host {
                    muted(ui, "on");
                    strong(ui, host);
                }
            }
        }
        muted(ui, "·");
        muted(ui, &headway::fmt::rel_time(activity.created_at));
    });
    ui.add_space(SPACING_XS);
}

/// Resolve a pubkey to its profile display name out of the local db, falling
/// back to the shared short-hex handle ([`headway::fmt::short_author`]) when
/// no usable profile is known.
fn author_name(
    app_ctx: &notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    author: &[u8; 32],
) -> String {
    txn.and_then(|txn| app_ctx.ndb.get_profile_by_pubkey(txn, author).ok())
        .and_then(|record| {
            let name = notedeck::name::get_display_name(Some(&record));
            name.display_name.or(name.username).map(str::to_owned)
        })
        .unwrap_or_else(|| headway::fmt::short_author(author))
}

/// Render one comment with the shared notedeck_ui note renderer, loading the
/// kind-1111 event from the local db by id. A threaded reply gets a small
/// "↳ reply to" caption above the note (the thread still renders flat). Falls
/// back to the self-contained [`comment_row_ui`] if the note isn't in the db.
fn comment_note_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    comment: &CommentView,
) {
    let note = txn.and_then(|txn| app_ctx.ndb.get_note_by_id(txn, comment.id.bytes()).ok());
    let Some(note) = note else {
        comment_row_ui(ui, theme, comment);
        return;
    };

    // The renderer shows author/time/body; the reply target is the one thing it
    // can't convey while the thread is flat, so surface it as a caption.
    if let Some(parent) = &comment.parent {
        ui.label(
            egui::RichText::new(format!(
                "↳ reply to {}",
                headway::wordid::encode(parent.bytes())
            ))
            .small()
            .color(theme.text_muted),
        );
    }

    // No actionbar/options menu: there's no relay publishing or note-nav wired up
    // here, so the reply/zap/repost affordances would be dead. A small framed pfp
    // keeps each comment compact in the thread.
    let flags = notedeck_ui::NoteOptions::SelectableText
        | notedeck_ui::NoteOptions::SmallPfp
        | notedeck_ui::NoteOptions::InlineReferences
        | notedeck_ui::NoteOptions::Framed;
    let mut note_context = app_ctx.note_context();
    notedeck_ui::NoteView::new(&mut note_context, &note, flags).show(ui);
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
                    egui::RichText::new(headway::wordid::encode(comment.id.bytes()))
                        .small()
                        .color(theme.text_muted.gamma_multiply(0.6)),
                );
                if let Some(parent) = &comment.parent {
                    ui.label(
                        egui::RichText::new(format!(
                            "↳ reply to {}",
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

/// The "leave a comment" composer at the foot of the thread, Linear-style: a
/// bordered rounded panel holding a frameless multiline field with a round ↑
/// submit button in its bottom-right corner. Posts on the button or
/// ⌘/Ctrl+Enter (a multiline field keeps plain Enter for newlines). Empty
/// drafts are ignored.
fn detail_comment_composer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    let mut submit = false;
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let resp = ui.add(
                egui::TextEdit::multiline(&mut state.comment_draft)
                    .frame(false)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Leave a comment…"),
            );
            submit = resp.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            // A fixed-height row for the submit button: a bare right-to-left
            // `with_layout` would greedily absorb all remaining vertical space
            // and balloon the panel to the bottom of the pane.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 24.0),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    let ready = !state.comment_draft.trim().is_empty();
                    let post = egui::Button::new(
                        egui::RichText::new("↑")
                            .color(egui::Color32::WHITE)
                            .strong(),
                    )
                    .fill(if ready {
                        STATUS_DONE
                    } else {
                        theme.surface_elevated
                    })
                    .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
                    .min_size(egui::vec2(24.0, 24.0));
                    if ui
                        .add_enabled(ready, post)
                        .on_hover_text("Post (⌘↵)")
                        .clicked()
                    {
                        submit = true;
                    }
                },
            );
        });
    if submit {
        *outcome = DetailOutcome::AddComment;
    }
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
        DetailOutcome::SetPriority(priority) => {
            *action = Some(BoardAction::SetPriority {
                card: ctx.card_id,
                priority,
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
        DetailOutcome::OpenCard(id) => {
            // Swap the detail to the other card; the edit buffers reseed next
            // frame because `detail_for` no longer matches the selection.
            state.selected = Some(id);
        }
        DetailOutcome::AddSubissue => {
            let title = state.new_subissue.trim().to_string();
            if !title.is_empty() {
                *action = Some(BoardAction::AddCard {
                    // New subissues land in the first column, like the CLI's
                    // `add --parent` default.
                    col: 0,
                    title,
                    description: String::new(),
                    labels: vec![],
                    parent: Some(ctx.card_id),
                });
            }
            state.new_subissue.clear();
        }
        DetailOutcome::ReorderSubissue {
            child,
            after,
            before,
        } => {
            resolve_subissue_reorder(view, ctx.card_id, child, after, before, action);
        }
        DetailOutcome::DetachParent => {
            *action = Some(BoardAction::SetParent {
                card: ctx.card_id,
                parent: None,
            });
        }
        DetailOutcome::Unblock(on) => {
            *action = Some(BoardAction::Unblock {
                card: ctx.card_id,
                on,
            });
        }
    }
}

/// Turn a subissue drop into a work-order edit on the `card:<parent>` container.
///
/// Two paths, because the lazy sort (sequenced children lead by rank, the rest
/// follow by creation order) means a lone [`BoardAction::SetSequence`] can only
/// land a card *ahead of every unsequenced sibling* — so dropping between two
/// unsequenced siblings would snap the card to the top instead of the gap:
///
/// - **Already fully sequenced:** a single `SetSequence` insert against the
///   dragged card's (sequenced) neighbours, via the shared [`store::seq_rank`]
///   kernel. This is the steady state named in the epic's breakdown.
/// - **Not fully sequenced:** promote the whole container to an explicit order
///   with [`BoardAction::ReorderSubissues`], placing the dragged card in the
///   drop gap. One drag makes the list fully sequenced; every later drag takes
///   the cheap single-insert path above. The lazy *migration* default is
///   untouched — only an explicit drag promotes.
fn resolve_subissue_reorder(
    view: &BoardView,
    parent: NoteId,
    child: NoteId,
    after: Option<NoteId>,
    before: Option<NoteId>,
    action: &mut Option<BoardAction>,
) {
    let Some((_, card)) = find_card(view, parent) else {
        return;
    };
    if card.subissues.iter().all(|s| s.seq.is_some()) {
        let container = event::Container::Card(*parent.bytes());
        let position = subissue_seq_position(view, parent, after, before);
        // A rank only fails to compute when an anchor lost its seq between
        // frames; every sibling is sequenced here, so drop the reorder rather
        // than guess.
        if let Ok(rank) = store::seq_rank(view, &container, child, &position) {
            *action = Some(BoardAction::SetSequence {
                card: child,
                container,
                rank,
            });
        }
        return;
    }
    // `card.subissues` is already in display order, so lift the dragged child
    // out and reinsert it in the drop gap to get the promoted order.
    let siblings: Vec<NoteId> = card.subissues.iter().map(|s| s.id).collect();
    let order = reorder_ids(&siblings, child, before);
    *action = Some(BoardAction::ReorderSubissues { parent, order });
}

/// Rebuild a display-ordered sibling id list with `child` moved into the drop
/// gap: lifted out, then reinserted just above `before` (or appended when
/// `before` is `None`, i.e. dropped past the last row). Pure, so the
/// gap → promoted-order mapping is unit-testable on its own.
fn reorder_ids(siblings: &[NoteId], child: NoteId, before: Option<NoteId>) -> Vec<NoteId> {
    let mut order: Vec<NoteId> = siblings.iter().copied().filter(|&id| id != child).collect();
    let idx = before
        .and_then(|b| order.iter().position(|&id| id == b))
        .unwrap_or(order.len());
    order.insert(idx, child);
    order
}

/// Where to insert a dragged subissue within an already fully-sequenced
/// container (the caller only takes this path once every sibling has a seq
/// rank): anchor `After` the card above, else `Before` the card below. The
/// `First`/`Last` fallbacks are unreachable for that caller but kept so the
/// helper stays a total gap → position mapping.
fn subissue_seq_position(
    view: &BoardView,
    parent: NoteId,
    after: Option<NoteId>,
    before: Option<NoteId>,
) -> store::SeqPosition {
    seq_position_for_gap(
        after,
        after.is_some_and(|id| subissue_is_sequenced(view, parent, id)),
        before,
        before.is_some_and(|id| subissue_is_sequenced(view, parent, id)),
    )
}

/// The pure gap → [`store::SeqPosition`] policy, split from the board-view
/// lookup so it's unit-testable on its own: anchor on a sequenced neighbour when
/// there is one (`After` the card above, else `Before` the card below), else
/// fall to the ends — `First` at the very top, `Last` when dropping into the
/// still-unsequenced tail.
fn seq_position_for_gap(
    after: Option<NoteId>,
    after_sequenced: bool,
    before: Option<NoteId>,
    before_sequenced: bool,
) -> store::SeqPosition {
    if let (Some(a), true) = (after, after_sequenced) {
        return store::SeqPosition::After(a);
    }
    if let (Some(b), true) = (before, before_sequenced) {
        return store::SeqPosition::Before(b);
    }
    if after.is_none() {
        store::SeqPosition::First
    } else {
        store::SeqPosition::Last
    }
}

/// Whether a subissue of `parent` already carries a seq rank — only sequenced
/// siblings can anchor an `After`/`Before` insert.
fn subissue_is_sequenced(view: &BoardView, parent: NoteId, child: NoteId) -> bool {
    find_card(view, parent)
        .map(|(_, c)| c.subissues.iter().any(|s| s.id == child && s.seq.is_some()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subissue_drop_gap_maps_to_seq_position() {
        let a = NoteId::new([1u8; 32]);
        let b = NoteId::new([2u8; 32]);
        use store::SeqPosition::*;

        // Anchors on the sequenced card above the gap.
        assert!(matches!(
            seq_position_for_gap(Some(a), true, Some(b), true),
            After(x) if x == a
        ));
        // Above unsequenced but below sequenced: anchor on the card below.
        assert!(matches!(
            seq_position_for_gap(Some(a), false, Some(b), true),
            Before(x) if x == b
        ));
        // Top of the list (no card above) with an unsequenced neighbour -> First.
        assert!(matches!(
            seq_position_for_gap(None, false, Some(b), false),
            First
        ));
        // Dropping into the unsequenced tail (a card above, none sequenced) -> Last.
        assert!(matches!(
            seq_position_for_gap(Some(a), false, None, false),
            Last
        ));
    }

    #[test]
    fn reorder_ids_moves_child_into_the_drop_gap() {
        let ids: Vec<NoteId> = (1..=4u8).map(|n| NoteId::new([n; 32])).collect();
        let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);

        // Drop D above B (gap `before = B`): D lands just before B.
        assert_eq!(reorder_ids(&ids, d, Some(b)), vec![a, d, b, c]);
        // Drop D to the very top (gap `before` = the old first card A): D leads.
        assert_eq!(reorder_ids(&ids, d, Some(a)), vec![d, a, b, c]);
        // Drop B below C, i.e. above D: reinserted just before D.
        assert_eq!(reorder_ids(&ids, b, Some(d)), vec![a, c, b, d]);
        // Dropped past the last row (`before = None`): appended to the end.
        assert_eq!(reorder_ids(&ids, b, None), vec![a, c, d, b]);
    }
}
