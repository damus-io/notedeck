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

use crate::BoardSummary;
use crate::event::{
    self, ActivityKind, ActivityView, ArchivedCard, BoardView, CardView, ColumnView, CommentView,
};
use crate::store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// Max width the full-pane card detail body is constrained to, so a card reads
/// comfortably instead of stretching across a wide window.
const DETAIL_CONTENT_WIDTH: f32 = 760.0;

/// Width of the card detail's fixed right-hand properties sidebar (status,
/// labels, actions), in the Linear-style wide layout.
const DETAIL_SIDEBAR_WIDTH: f32 = 220.0;

/// Pane width at or above which the detail view shows the properties sidebar;
/// below it the properties stack between the card body and its activity.
const DETAIL_SIDEBAR_MIN: f32 = 720.0;

/// Linear's status-circle yellow, painted for columns in the first half of the
/// board's middle (e.g. In Progress). Deliberately theme-independent, like the
/// status colors it mirrors.
const STATUS_STARTED_EARLY: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xC9, 0x4C);

/// Linear's status-circle green, for columns in the second half of the middle
/// (e.g. In Review).
const STATUS_STARTED_LATE: egui::Color32 = egui::Color32::from_rgb(0x4C, 0xB7, 0x82);

/// Linear's status-circle indigo, for the board's final (done) column.
const STATUS_DONE: egui::Color32 = egui::Color32::from_rgb(0x5E, 0x6A, 0xD2);

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
    /// Whether the detail sheet's "add label" composer is open (it collapses to
    /// a "+ Add label" affordance, Linear-style).
    label_composer: bool,
    /// Buffer backing the "add subissue" field in the detail sheet.
    new_subissue: String,
    /// Whether the detail sheet's "add sub-issue" composer is open.
    subissue_composer: bool,
    /// Buffer backing the "write a comment" composer in the detail pane.
    comment_draft: String,
    /// Whether the archived-cards sheet is open.
    showing_archived: bool,
    /// A board-switcher request raised this frame (switch or create). The app
    /// reads and clears it to act on it; the new-board *composer* is just another
    /// [`InlineEdit`], sharing `edit_text`.
    nav: Option<BoardNav>,
    /// A cross-board card request raised this frame from a card's context menu
    /// (move to / link onto another board). The app reads and clears it to act.
    card_move: Option<CardBoardMove>,
    /// Free-text board filter. Empty means no filtering. Plain words match a
    /// card's title/description/labels/word-id (all must match,
    /// case-insensitive); a `label:foo` token narrows to cards carrying a
    /// label containing `foo`. Pasting a full card reference filters to that
    /// card, switching boards first if it lives on another known board (see
    /// [`CardFilter`] and [`filter_ref_jump`]).
    filter: String,
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

impl BoardUiState {
    /// Take this frame's board-switcher request (switch or create), if any, for
    /// the app to act on. Clears it so it fires once.
    pub fn take_nav(&mut self) -> Option<BoardNav> {
        self.nav.take()
    }

    /// Take this frame's cross-board card request (move or link), if any, for the
    /// app to act on. Clears it so it fires once.
    pub fn take_card_move(&mut self) -> Option<CardBoardMove> {
        self.card_move.take()
    }
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
/// composing a card, renaming a column, adding a column, or naming a new board at
/// any one moment — so they share [`BoardUiState::edit_text`] and
/// [`BoardUiState::focus_edit`] and this enum tracks which (if any) is live.
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
    /// Naming a new board in the switcher.
    NewBoard,
}

/// A request from the board switcher, raised in [`BoardUiState::nav`] for the app
/// to act on. Switching and creating are mutually exclusive, so one enum models
/// the frame's intent rather than a clutch of `Option`s.
pub enum BoardNav {
    /// Switch the active board to this slug.
    Switch(String),
    /// Create (seed) a new board with this display title, then switch to it.
    Create(String),
}

/// Whether a cross-board card request relocates the card or shares it.
#[derive(Clone, Copy)]
pub enum CardBoardOp {
    /// Relocate the card to the target board (remove it from the current one).
    Move,
    /// Also place the card on the target board, keeping it on the current one —
    /// membership is placement-driven, so the same issue lives on both.
    Link,
}

/// A cross-board card request raised from a card's context menu, in
/// [`BoardUiState::card_move`] for the app to act on. The `card` moves to (or is
/// linked onto) the board with slug `to_board`.
pub struct CardBoardMove {
    pub card: NoteId,
    pub to_board: String,
    pub op: CardBoardOp,
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

/// A parsed board filter. Linear-inspired: whitespace splits the query into
/// AND-ed terms, a `label:` prefix scopes a term to labels, and everything else
/// is free text matched against a card's title, description, labels and its
/// word-id (`maple-river-canyon`), so a card can be pulled up by any of its id
/// words. A pasted reference to a card on *this* board
/// (`headway#maple-river-canyon`, or `#maple-river-canyon` as rendered on the
/// card) matches by its id words — the board prefix carries no signal within a
/// single board. Other `#` terms are plain text: a card citing an issue on
/// another board stays searchable by that citation. (Pasting another *known*
/// board's reference switches to it — see [`filter_ref_jump`].) All matching is
/// case-insensitive substring; an empty filter matches everything.
#[derive(Default)]
struct CardFilter {
    /// Free-text terms (lowercased); each must appear somewhere in the card.
    text: Vec<String>,
    /// `label:` terms (lowercased); each must match one of the card's labels.
    labels: Vec<String>,
}

impl CardFilter {
    /// Parse `query` into a filter for the board whose slug is `board` (needed
    /// to recognise pasted references to this board's own cards).
    fn parse(query: &str, board: &str) -> Self {
        let mut filter = CardFilter::default();
        for term in query.split_whitespace() {
            // Accept `label:` and `l:` as the label-scoping prefix.
            let label = term
                .strip_prefix("label:")
                .or_else(|| term.strip_prefix("l:"));
            match label {
                Some(value) if !value.is_empty() => filter.labels.push(value.to_lowercase()),
                // A bare `label:` with no value isn't a constraint; ignore it.
                Some(_) => {}
                None => match term.split_once('#') {
                    // A reference to a card on *this* board — pasted in full
                    // (`headway#maple-river-canyon`) or as rendered on the card
                    // (`#maple-river-canyon`): the prefix is redundant here, so
                    // match on the id words alone. A term that ends at the `#`
                    // has nothing left to match; ignore it.
                    Some((prefix, words))
                        if prefix.is_empty() || prefix.eq_ignore_ascii_case(board) =>
                    {
                        if !words.is_empty() {
                            filter.text.push(words.to_lowercase());
                        }
                    }
                    // Any other `#` term is plain text — cards can cite issues
                    // on other boards, and those citations stay searchable.
                    _ => filter.text.push(term.to_lowercase()),
                },
            }
        }
        filter
    }

    /// Whether any constraint is set. An inactive filter shows the whole board.
    fn is_active(&self) -> bool {
        !self.text.is_empty() || !self.labels.is_empty()
    }

    /// Does `card` satisfy every term? Label terms must each match some label;
    /// text terms must each appear in the card's combined searchable text.
    fn matches(&self, card: &CardView) -> bool {
        let label_ok = self.labels.iter().all(|needle| {
            card.labels
                .iter()
                .any(|l| l.to_lowercase().contains(needle))
        });
        if !label_ok {
            return false;
        }
        if self.text.is_empty() {
            return true;
        }
        let mut haystack = card.title.to_lowercase();
        haystack.push('\n');
        haystack.push_str(&card.description.to_lowercase());
        for l in &card.labels {
            haystack.push('\n');
            haystack.push_str(&l.to_lowercase());
        }
        // The card's id words (`maple-river-canyon`), already lowercase BIP-39
        // words. No board slug: the filter is scoped to a single board, so the
        // slug carries no signal and would make its text match every card.
        haystack.push('\n');
        haystack.push_str(&headway::wordid::encode(card.id.bytes()));
        self.text.iter().all(|needle| haystack.contains(needle))
    }
}

/// A full reference to a card on *another* board found in the filter query:
/// the raw term as typed, the id words after the `#`, and the referenced
/// board's slug. Borrowed from the query and the board list.
struct CrossBoardRef<'a> {
    /// The whole term as it appears in the query (`otherboard#maple-river-canyon`).
    term: &'a str,
    /// The id words after the `#`; may be empty (`otherboard#` alone).
    words: &'a str,
    /// The referenced board's slug, in its canonical casing from the board list.
    board: &'a str,
}

/// Find the first term in `query` that is a reference to a card on another
/// known board (`otherboard#maple-river-canyon`). Terms referencing `board`
/// itself are the filter's business ([`CardFilter::parse`]), and prefixes that
/// aren't a known board slug are plain search text; both return `None` here.
fn cross_board_ref<'a>(
    query: &'a str,
    board: &str,
    boards: &'a [BoardSummary],
) -> Option<CrossBoardRef<'a>> {
    query.split_whitespace().find_map(|term| {
        let (prefix, words) = term.split_once('#')?;
        if prefix.eq_ignore_ascii_case(board) {
            return None;
        }
        let target = boards.iter().find(|b| b.id.eq_ignore_ascii_case(prefix))?;
        Some(CrossBoardRef {
            term,
            words,
            board: &target.id,
        })
    })
}

/// React to a full cross-board reference pasted into the filter field: a
/// `otherboard#maple-river-canyon` term addresses a card on another board, so
/// the intuitive read is "take me there" — raise a switch to that board and
/// reduce the term to its id words, which then filter the target board down to
/// the referenced card. Runs only on an edit of the field, not per frame.
fn filter_ref_jump(view: &BoardView, boards: &[BoardSummary], state: &mut BoardUiState) {
    let Some(r) = cross_board_ref(&state.filter, &view.id, boards) else {
        return;
    };
    let rewritten = state
        .filter
        .split_whitespace()
        .map(|t| if t == r.term { r.words } else { t })
        .collect::<Vec<_>>()
        .join(" ");
    state.nav = Some(BoardNav::Switch(r.board.to_string()));
    state.filter = rewritten;
}

/// Render the board (header, columns, the add-column affordance and the floating
/// card detail sheet) and return the edit the user made this frame, if any.
pub fn board_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    view: &BoardView,
    boards: &[BoardSummary],
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
            // Parsed once per frame from the persisted query; reflects the prior
            // frame's keystroke, which is imperceptible in an immediate-mode UI.
            let filter = CardFilter::parse(&state.filter, &view.id);

            // Board switcher: the active board's title as a dropdown listing the
            // account's other boards, plus a "+ New board" composer.
            board_switcher(ui, theme, view, boards, state);
            ui.add_space(SPACING_SM);

            // Board header: a muted summary of its contents.
            let total: usize = view.columns.iter().map(|c| c.cards.len()).sum();
            let summary_text = if filter.is_active() {
                let matched = view
                    .columns
                    .iter()
                    .flat_map(|c| &c.cards)
                    .filter(|c| filter.matches(c))
                    .count();
                format!(
                    "{matched} of {total} cards · {} columns",
                    view.columns.len()
                )
            } else {
                format!(
                    "{total} card{} · {} columns",
                    if total == 1 { "" } else { "s" },
                    view.columns.len()
                )
            };
            let summary = egui::RichText::new(summary_text).color(theme.text_muted);
            ui.horizontal(|ui| {
                ui.label(summary);
                // The archived entry point only appears when there's something
                // behind it, so the header stays quiet on a fresh board.
                if !view.archived.is_empty() {
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
                }
                // Filter field, right-aligned. Packs from the right so the clear
                // affordance trails the input.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !state.filter.is_empty()
                        && ui
                            .add(egui::Button::new("✕").frame(false))
                            .on_hover_text("Clear filter")
                            .clicked()
                    {
                        state.filter.clear();
                    }
                    // Pin a stable id: the ✕ button is only laid out when the
                    // filter is non-empty, and in this right_to_left layout it
                    // sits ahead of the field. Without a fixed id, typing the
                    // first character inserts that button and shifts egui's
                    // auto-generated id for the field, so it loses focus.
                    let field = egui::TextEdit::singleline(&mut state.filter)
                        .id(egui::Id::new("headway-filter-field"))
                        .desired_width(220.0)
                        .hint_text("Filter… e.g. label:bug perf");
                    // A pasted reference to a card on another board switches
                    // to that board (see [`filter_ref_jump`]).
                    if ui.add(field).changed() {
                        filter_ref_jump(view, boards, state);
                    }
                });
            });
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
                                theme,
                                view,
                                boards,
                                state,
                                &filter,
                                col_idx,
                                &mut action,
                                &mut clicked,
                            );
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
#[allow(clippy::too_many_arguments)]
fn column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    boards: &[BoardSummary],
    state: &mut BoardUiState,
    filter: &CardFilter,
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
                        // When filtering, the badge reflects how many of this
                        // column's cards match rather than the column's total.
                        let count = if filter.is_active() {
                            column.cards.iter().filter(|c| filter.matches(c)).count()
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
    filter: &CardFilter,
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
                // Filtered-out cards are simply not drawn. `row_idx` stays the
                // card's true position in the column, so drag-reorder targeting
                // against the remaining visible cards still lands correctly.
                if filter.is_active() && !filter.matches(card) {
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
                state.card_move = Some(CardBoardMove {
                    card,
                    to_board: board.id.clone(),
                    op,
                });
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
            // reference is legible at a glance. A bare `#word-id` — the `#` marks
            // it as a reference, GitHub-style; the `<board>#` prefix is dropped
            // since it's identical on every card, and the detail sheet shows the
            // full `board#word-id` for copy-paste (see [`headway::wordid`]).
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                // A muted ↳ marks the card as someone's subissue; its parent is
                // named in the detail view's breadcrumb.
                if card.parent.is_some() {
                    ui.label(egui::RichText::new("↳").small().color(theme.text_muted));
                }
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
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8))
        .inner_margin(egui::Margin::symmetric(SPACING_SM as i8, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!("{done}/{}", subissues.len()))
                    .small()
                    .color(theme.text_muted),
            );
        })
        .response
        .on_hover_text(format!("{done} of {} subissues done", subissues.len()));
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
            // Extend (don't wrap) so the chip reports its full natural width.
            // Otherwise, when the wrapping row runs out of horizontal space, the
            // text inside the last chip wraps character-by-character (vertical
            // `p/e/r/f`) instead of the whole chip moving to the next row.
            ui.add(
                egui::Label::new(egui::RichText::new(label).small().color(theme.text_primary))
                    .extend(),
            );
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

/// The board switcher in the header: the active board's title as a dropdown that
/// lists the account's boards (current one marked), with a "+ New board" entry
/// that opens an inline name composer. Requests are raised in [`BoardUiState::nav`]
/// for the app to act on; this function never mutates the board itself.
fn board_switcher(
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

    let label = egui::RichText::new(format!("{}  ⏷", view.title))
        .size(18.0)
        .strong()
        .color(theme.text_primary);
    ui.menu_button(label, |ui| {
        for board in boards {
            let current = board.id == view.id;
            if ui.selectable_label(current, &board.title).clicked() {
                if !current {
                    state.nav = Some(BoardNav::Switch(board.id.clone()));
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
/// Linear-style layout: a scrolling reading column (title, description,
/// subissues, then the activity thread — always beneath the body, never beside
/// it) with the card's properties (status, labels, actions) in a fixed right
/// sidebar on wide panes, stacked inline on narrow ones. Edits are emitted as
/// [`BoardAction`]s (title/description commit on focus loss); dismissing
/// (back / ✕ / Escape) clears the selection.
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

    let ctx = DetailCtx {
        card_id,
        card_ref: format!("{}#{}", view.id, headway::wordid::encode(card_id.bytes())),
        current_col,
        title: card.title.clone(),
        desc: card.description.clone(),
        labels: card.labels.clone(),
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
                                note_context,
                                &ctx,
                                state,
                                action,
                                &mut outcome,
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
                        ui.set_max_width(avail.min(DETAIL_CONTENT_WIDTH));
                        detail_body_ui(ui, theme, &ctx, state, action, &mut outcome);
                        ui.add_space(SPACING_MD);
                        ui.separator();
                        ui.add_space(SPACING_MD);
                        detail_properties_ui(ui, theme, &ctx, state, &mut outcome);
                        ui.add_space(SPACING_MD);
                        ui.separator();
                        ui.add_space(SPACING_MD);
                        detail_comments_ui(ui, theme, note_context, &ctx, state, &mut outcome);
                    });
            }
        });

    resolve_detail_outcome(state, action, view, &ctx, outcome);
}

/// The full-pane detail top bar, a Linear-style breadcrumb: back-to-board,
/// then the card's word-id reference (click to copy — the GUI mirror of what
/// the CLI prints, a stable handle for commits/chat), and a trailing ✕ that
/// also dismisses.
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
    /// When the card was created / last amended (see [`CardView`]), rendered
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
    /// Clear this card's parent relation.
    DetachParent,
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

/// The wide layout's scrolling main column: the card body with the activity
/// thread beneath it, width-capped so it reads like a document while the
/// properties sidebar sits fixed to its right.
fn detail_main_column_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    action: &mut Option<BoardAction>,
    outcome: &mut DetailOutcome,
) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(DETAIL_CONTENT_WIDTH));
            detail_body_ui(ui, theme, ctx, state, action, outcome);
            ui.add_space(SPACING_MD);
            ui.separator();
            ui.add_space(SPACING_MD);
            detail_comments_ui(ui, theme, note_context, ctx, state, outcome);
        });
}

/// The card body proper: breadcrumb, title, ref, description and subissues.
/// Title/description commit directly on focus loss; the rest is collected into
/// `outcome` and resolved by the caller. Properties (status, labels, actions)
/// live in [`detail_properties_ui`], Linear-style.
fn detail_body_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
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
    detail_description_section_ui(ui, theme, ctx, state, action);

    ui.add_space(SPACING_LG);
    detail_subissues_section_ui(ui, theme, ctx, state, outcome);
}

/// The card's properties — status, labels, dates and the archive/delete
/// actions — rendered as Linear-style bordered sidebar panels: fixed to the
/// right on a wide pane, stacked between the body and activity on a narrow
/// one.
fn detail_properties_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    state: &mut BoardUiState,
    outcome: &mut DetailOutcome,
) {
    sidebar_panel(theme).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_label(ui, theme, "Properties");
        ui.add_space(SPACING_SM);
        // Move the card between lanes without dragging (meaningless on a
        // single-column board).
        if ctx.columns.len() > 1 {
            detail_status_row_ui(ui, theme, ctx, outcome);
            ui.add_space(SPACING_SM);
        }

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
    });

    ui.add_space(SPACING_SM);
    sidebar_panel(theme).show(ui, |ui| {
        ui.set_width(ui.available_width());
        detail_labels_section_ui(ui, theme, ctx, state, outcome);
    });

    // Quiet, frameless actions under the panels — Linear keeps these out of
    // the way rather than as filled buttons.
    ui.add_space(SPACING_SM);
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
fn detail_description_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
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
                    egui::RichText::new(format!("#{}", headway::wordid::encode(parent.id.bytes())))
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

    for sub in &ctx.subissues {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SPACING_XS;
            // The child's status circle, derived from the board so it can
            // never go stale. Off-board children fall back to a todo ring.
            let icon = if sub.done || sub.archived {
                StatusIcon::Done
            } else {
                sub.col_idx
                    .map(|i| StatusIcon::for_column(i, ctx.columns.len()))
                    .unwrap_or(StatusIcon::Todo)
            };
            status_icon_ui(ui, theme, icon, 14.0);
            if sub.on_board {
                // A Link, not a boxed button: the row reads as plain text and
                // underlines on hover, like Linear's sub-issue list.
                let link =
                    egui::Link::new(egui::RichText::new(&sub.title).color(theme.text_primary));
                if ui.add(link).on_hover_text("Open subissue").clicked() {
                    *outcome = DetailOutcome::OpenCard(sub.id);
                }
            } else {
                ui.label(egui::RichText::new(&sub.title).color(theme.text_primary));
            }
            // Where the child sits: archived trumps the column (an archived
            // child has no live column to show).
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
                egui::RichText::new(format!("#{}", headway::wordid::encode(sub.id.bytes())))
                    .small()
                    .color(theme.text_muted.gamma_multiply(0.6)),
            );
        });
    }
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

/// The open card's activity feed, Linear-style: a section heading with a
/// comment count, then the derived activity timeline (created / moved /
/// renamed / …) interleaved chronologically with the comment thread, and a
/// composer to add a comment. Replies aren't indented yet (`store now, render
/// flat`) but each comment still shows who it threads under. Posting is
/// collected into `outcome`.
fn detail_comments_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    note_context: &mut notedeck::NoteContext,
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
    let txn = nostrdb::Transaction::new(note_context.ndb).ok();
    let (mut ai, mut ci) = (0, 0);
    while ai < ctx.activity.len() || ci < ctx.comments.len() {
        let comment_first = match (ctx.activity.get(ai), ctx.comments.get(ci)) {
            (Some(a), Some(c)) => c.created_at < a.created_at,
            (None, Some(_)) => true,
            _ => false,
        };
        if comment_first {
            comment_note_ui(ui, theme, note_context, txn.as_ref(), &ctx.comments[ci]);
            ci += 1;
        } else {
            activity_row_ui(
                ui,
                theme,
                note_context,
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
    note_context: &mut notedeck::NoteContext,
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

        strong(ui, &author_name(note_context, txn, &activity.author));
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
                    None => strong(ui, &format!("#{}", headway::wordid::encode(parent.bytes()))),
                }
            }
            ActivityKind::ParentRemoved => muted(ui, "detached from its parent"),
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
    note_context: &notedeck::NoteContext,
    txn: Option<&nostrdb::Transaction>,
    author: &[u8; 32],
) -> String {
    txn.and_then(|txn| note_context.ndb.get_profile_by_pubkey(txn, author).ok())
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
    // keeps each comment compact in the thread.
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
                    labels: vec![],
                    parent: Some(ctx.card_id),
                });
            }
            state.new_subissue.clear();
        }
        DetailOutcome::DetachParent => {
            *action = Some(BoardAction::SetParent {
                card: ctx.card_id,
                parent: None,
            });
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

/// A small muted, sentence-case group heading (Linear-style) used for the
/// detail sheet's sidebar panels.
fn section_label(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme.text_muted));
}

/// A semibold sentence-case heading for the detail pane's main-column sections
/// ("Sub-issues", "Activity"), matching Linear's typography rather than the
/// muted all-caps sidebar labels.
fn detail_heading(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(14.0)
            .strong()
            .color(theme.text_primary),
    );
}

/// The subtle bordered, rounded panel each sidebar group (Properties, Labels)
/// sits in — Linear's sidebar look. A builder, so no `_ui` suffix.
fn sidebar_panel(theme: &ColorTheme) -> egui::Frame {
    egui::Frame::new()
        .fill(theme.surface_secondary)
        .stroke(egui::Stroke::new(STROKE_THIN, theme.border_default))
        .corner_radius(egui::CornerRadius::same(RADIUS_MD as u8))
        .inner_margin(egui::Margin::same(SPACING_SM as i8))
}

/// Which Linear-style status circle to paint for a column or subissue.
#[derive(Clone, Copy)]
enum StatusIcon {
    /// The first column: a dashed muted ring.
    Backlog,
    /// The second column: a plain muted ring.
    Todo,
    /// A middle column: a ring with a pie-slice fill of this fraction —
    /// yellow through the first half of the middle columns, green after.
    Started(f32),
    /// The final column (or an archived child): a filled indigo disc with a
    /// check.
    Done,
}

impl StatusIcon {
    /// The icon for the column at `idx` on a board of `n` columns, mapped
    /// positionally the way Linear maps status types: first = backlog,
    /// second = todo, last = done, and anything between = started, filled in
    /// proportion to how far along the middle it sits.
    fn for_column(idx: usize, n: usize) -> Self {
        if idx + 1 >= n {
            Self::Done
        } else if idx == 0 {
            Self::Backlog
        } else if idx == 1 {
            Self::Todo
        } else {
            Self::Started((idx - 1) as f32 / (n - 2) as f32)
        }
    }

    /// The circle's stroke/fill color: muted for unstarted, Linear's yellow →
    /// green through the middle, indigo for done.
    fn color(self, theme: &ColorTheme) -> egui::Color32 {
        match self {
            Self::Backlog | Self::Todo => theme.text_muted,
            Self::Started(f) if f <= 0.5 => STATUS_STARTED_EARLY,
            Self::Started(_) => STATUS_STARTED_LATE,
            Self::Done => STATUS_DONE,
        }
    }
}

/// Paint one Linear-style status circle, `size` px square, and return its
/// response (hover only; callers wrap it when a click target is needed).
fn status_icon_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    icon: StatusIcon,
    size: f32,
) -> egui::Response {
    use std::f32::consts::TAU;

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    let center = rect.center();
    let r = size * 0.5 - 1.0;
    let stroke = egui::Stroke::new(1.5, icon.color(theme));

    // Sample an arc of the ring into line points. Twelve o'clock start,
    // clockwise, matching Linear's pies.
    let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
        let steps = 16;
        (0..=steps)
            .map(|k| {
                let a = -TAU / 4.0 + from + (to - from) * k as f32 / steps as f32;
                center + r * egui::vec2(a.cos(), a.sin())
            })
            .collect()
    };

    match icon {
        StatusIcon::Backlog => {
            // A dashed ring: six dashes with even gaps.
            for k in 0..6 {
                let a0 = k as f32 / 6.0 * TAU;
                painter.add(egui::Shape::line(arc(a0, a0 + TAU / 10.0), stroke));
            }
        }
        StatusIcon::Todo => {
            painter.circle_stroke(center, r, stroke);
        }
        StatusIcon::Started(f) => {
            painter.circle_stroke(center, r, stroke);
            // The pie wedge: centre plus an inset arc of the fill fraction.
            let inset = r - 2.5;
            let mut points = vec![center];
            points.extend(
                arc(0.0, TAU * f.clamp(0.0, 1.0))
                    .into_iter()
                    .map(|p| center + (p - center) * (inset / r)),
            );
            painter.add(egui::Shape::convex_polygon(
                points,
                stroke.color,
                egui::Stroke::NONE,
            ));
        }
        StatusIcon::Done => {
            painter.circle_filled(center, r + 0.5, stroke.color);
            // The check, drawn as two strokes on the disc.
            let p = |dx: f32, dy: f32| center + r * egui::vec2(dx, dy);
            let check = egui::Stroke::new(1.5, egui::Color32::WHITE);
            painter.line_segment([p(-0.45, 0.05), p(-0.1, 0.4)], check);
            painter.line_segment([p(-0.1, 0.4), p(0.5, -0.3)], check);
        }
    }
    resp
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

#[cfg(test)]
mod tests {
    use super::*;

    fn card(title: &str, description: &str, labels: &[&str]) -> CardView {
        CardView {
            id: NoteId::new([0u8; 32]),
            author: [0u8; 32],
            title: title.to_string(),
            description: description.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            rank: String::new(),
            placed_at: 0,
            created_at: 0,
            updated_at: 0,
            comments: vec![],
            activity: vec![],
            parent: None,
            subissues: vec![],
        }
    }

    /// Board slug used by tests that don't care about reference parsing.
    const BOARD: &str = "headway";

    #[test]
    fn empty_filter_is_inactive_and_matches_all() {
        let f = CardFilter::parse("   ", BOARD);
        assert!(!f.is_active());
        assert!(f.matches(&card("anything", "", &[])));
    }

    #[test]
    fn text_terms_match_title_description_and_labels() {
        let c = card("Fix the bar", "wobbles on resize", &["ui"]);
        assert!(CardFilter::parse("bar", BOARD).matches(&c));
        assert!(CardFilter::parse("WOBBLES", BOARD).matches(&c)); // case-insensitive
        assert!(CardFilter::parse("ui", BOARD).matches(&c)); // also searches labels
        assert!(!CardFilter::parse("missing", BOARD).matches(&c));
    }

    #[test]
    fn multiple_text_terms_are_anded() {
        let c = card("Fix the bar", "wobbles on resize", &[]);
        assert!(CardFilter::parse("fix wobbles", BOARD).matches(&c));
        assert!(!CardFilter::parse("fix nope", BOARD).matches(&c));
    }

    #[test]
    fn label_token_scopes_to_labels_only() {
        let c = card("perf work", "", &["bug", "headway"]);
        assert!(CardFilter::parse("label:bug", BOARD).matches(&c));
        assert!(CardFilter::parse("l:head", BOARD).matches(&c)); // short prefix, substring
        // `perf` is in the title but not a label, so a label: term rejects it.
        assert!(!CardFilter::parse("label:perf", BOARD).matches(&c));
    }

    #[test]
    fn label_and_text_terms_combine() {
        let c = card("perf work", "", &["bug"]);
        assert!(CardFilter::parse("label:bug perf", BOARD).matches(&c));
        assert!(!CardFilter::parse("label:bug missing", BOARD).matches(&c));
    }

    #[test]
    fn bare_label_prefix_is_not_a_constraint() {
        let f = CardFilter::parse("label:", BOARD);
        assert!(!f.is_active());
        assert!(f.matches(&card("whatever", "", &[])));
    }

    /// The test card's id is all zeroes, which encodes to
    /// `abandon-abandon-abandon` (see [`headway::wordid`]).
    #[test]
    fn text_terms_match_word_id() {
        let c = card("Fix the bar", "", &[]);
        assert!(CardFilter::parse("abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("abandon-abandon-abandon", BOARD).matches(&c));
        assert!(!CardFilter::parse("zoo", BOARD).matches(&c));
    }

    /// A pasted reference to a card on this board matches by its id words,
    /// whether it carries the board prefix (`headway#…`, any casing) or just
    /// the on-card `#…` form.
    #[test]
    fn own_board_reference_matches_by_id_words() {
        let c = card("Fix the bar", "", &[]);
        assert!(CardFilter::parse("headway#abandon-abandon-abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("HEADWAY#abandon", BOARD).matches(&c));
        assert!(CardFilter::parse("#abandon", BOARD).matches(&c));
        assert!(!CardFilter::parse("headway#zoo", BOARD).matches(&c));
        // The stripped prefix alone isn't a constraint.
        let f = CardFilter::parse("headway#", BOARD);
        assert!(!f.is_active());
    }

    /// A `#` term whose prefix isn't this board is plain search text: it finds
    /// cards that cite that reference, not this board's ids.
    #[test]
    fn foreign_reference_is_plain_text() {
        let citing = card("dup", "see other#abandon-abandon-abandon", &[]);
        let plain = card("dup", "", &[]);
        assert!(CardFilter::parse("other#abandon", BOARD).matches(&citing));
        assert!(!CardFilter::parse("other#abandon", BOARD).matches(&plain));
    }

    #[test]
    fn cross_board_ref_finds_only_known_foreign_boards() {
        let boards = vec![
            BoardSummary {
                id: "headway".into(),
                title: "Headway".into(),
            },
            BoardSummary {
                id: "notebook".into(),
                title: "Notebook".into(),
            },
        ];
        // A known foreign board's reference is found, slug in canonical casing.
        let r = cross_board_ref("bug NOTEBOOK#maple-river-canyon", "headway", &boards)
            .expect("foreign ref");
        assert_eq!(r.term, "NOTEBOOK#maple-river-canyon");
        assert_eq!(r.words, "maple-river-canyon");
        assert_eq!(r.board, "notebook");
        // Own-board and unknown-prefix terms are not jumps.
        assert!(cross_board_ref("headway#maple", "headway", &boards).is_none());
        assert!(cross_board_ref("other#maple c#", "headway", &boards).is_none());
        assert!(cross_board_ref("no refs here", "headway", &boards).is_none());
    }
}
