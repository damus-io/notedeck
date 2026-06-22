use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS,
    STROKE_MEDIUM, STROKE_THIN,
};
use notedeck::{App, AppContext, AppResponse, ColorTheme};

pub use headway::{event, store};

use event::{ArchivedCard, BoardReducer, BoardView, CardView, ColumnView};
use store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

/// How long a card takes to slide from its old slot to its new one when it
/// jumps columns (e.g. a `headway move` landing from the CLI).
const MOVE_ANIM_SECS: f32 = 0.28;

/// A Linear/Trello-style issue & todo tracker app for notedeck.
///
/// The board is backed by nostr events in the local nostrdb: [`BoardSync`] keeps
/// a long-lived reducer over the account's events and the [`BoardView`] folded
/// from them, folding only freshly-arrived notes in as an ndb subscription
/// reports them — not re-walking the history every frame. Every edit is turned
/// into a signed event that is ingested locally (see [`store`]). There is
/// deliberately no relay publishing yet.
pub struct Headway {
    /// Which board this instance manages (single board for now).
    board_id: String,
    /// Transient, per-board UI state.
    state: BoardUiState,
    /// Subscription-backed cache of the reduced board (egui-free, so it's
    /// unit-testable against a bare `Ndb`).
    sync: BoardSync,
    /// Whether we've already auto-seeded a board this session, so we don't try
    /// to seed twice while the first seed is still materialising.
    seeded: bool,
    /// Countdown of follow-up repaints after an async ingest, so we keep waking
    /// up to poll the subscription until the writer thread goes quiet.
    repaint_frames: u8,
}

impl Default for Headway {
    fn default() -> Self {
        Self {
            board_id: store::BOARD_ID.to_string(),
            state: BoardUiState::default(),
            sync: BoardSync::default(),
            seeded: false,
            repaint_frames: 0,
        }
    }
}

/// Transient, per-board UI state that must persist across frames but isn't part
/// of the data model (e.g. which column has an open "add card" composer).
#[derive(Default)]
struct BoardUiState {
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
    /// Edit buffer for the selected card's description.
    detail_desc: String,
    /// Whether the description is shown rendered or in its raw editor.
    detail_desc_mode: DescMode,
    /// Buffer backing the "add label" field in the detail sheet.
    new_label: String,
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

/// How the detail sheet renders the open card's description. The states are
/// mutually exclusive and the one-shot focus grab only has meaning while
/// editing, so an enum models it more honestly than a pair of bools.
#[derive(Default, PartialEq, Eq)]
enum DescMode {
    /// Rendered markdown (read-only), with an edit affordance to switch over.
    #[default]
    Rendered,
    /// The raw multiline editor. `focus` requests a one-shot keyboard-focus
    /// grab on the frame the editor opens.
    Editing { focus: bool },
}

/// Drag-and-drop payload: the id of the card being dragged.
#[derive(Clone)]
struct DragCard(NoteId);

impl Headway {
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule a short burst of repaints so a just-ingested event (ingest is
    /// async, on a writer thread) gets polled and surfaced promptly.
    fn wake(&mut self) {
        self.repaint_frames = 8;
    }

    /// Burn down the repaint countdown, requesting a delayed repaint each step.
    fn pump_repaint(&mut self, ui: &egui::Ui) {
        if self.repaint_frames > 0 {
            self.repaint_frames -= 1;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}

/// Subscription-backed, *online* reducer for one account's board.
///
/// Holds a live nostrdb subscription to the account's headway events **and a
/// long-lived [`BoardReducer`]** that persists across frames. The first poll
/// folds the whole history once to seed the reducer; every later poll feeds it
/// only the freshly-arrived notes ([`event::reduce_delta`]) — an incremental
/// step, not a re-walk of the history. The reducer is rebuilt from scratch only
/// on a first load or an account switch.
///
/// This is correct because the fold is commutative and idempotent: applying a
/// delta to an up-to-date reducer lands in the same state as a full re-fold.
///
/// Deliberately free of any egui dependency so it can be unit-tested against a
/// bare `Ndb`.
#[derive(Default)]
struct BoardSync {
    /// The last reduced board. `None` means "no such board" (or not loaded yet).
    view: Option<BoardView>,
    /// The accumulator, kept alive across polls so new notes fold in
    /// incrementally. `None` until the first full fold (and again after an
    /// account switch), which is the signal to re-fold from scratch.
    reducer: Option<BoardReducer>,
    /// Live subscription to `sub_author`'s headway events; polling it is how we
    /// learn the board changed (including our own async ingests landing).
    sub: Option<Subscription>,
    /// The account `sub`/`reducer`/`view` belong to, so we resubscribe and
    /// re-fold on an account switch.
    sub_author: Option<Pubkey>,
    /// Test-only count of full-history re-folds, used to assert that an ordinary
    /// change folds in as a delta rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

impl BoardSync {
    /// Ensure a live subscription to `author`, drain it, and update the cached
    /// board. Returns `true` if the board was (re)reduced this call — a first
    /// load, an account switch, or new notes folded in — so the caller can
    /// schedule follow-up repaints. The cached board is read via
    /// [`view`](Self::view).
    fn poll(&mut self, ndb: &mut Ndb, author: &Pubkey, board_id: &str) -> bool {
        self.sync_subscription(ndb, author);

        let Some(sub) = self.sub else {
            // Subscribe failed: degrade to a full reload each frame so edits show.
            self.reload(ndb, author, board_id);
            return true;
        };

        let keys = ndb.poll_for_notes(sub, 64);

        // First load (or just resubscribed): fold the whole history once to seed
        // the long-lived reducer.
        if self.reducer.is_none() {
            self.reload(ndb, author, board_id);
            return true;
        }

        // Nothing new since the last poll: the cached view stands, no re-fold.
        if keys.is_empty() {
            return false;
        }

        // Incremental: fold only the freshly-arrived notes into the live reducer
        // and re-finalize (O(cards)). Commutative/idempotent, so this matches a
        // full re-fold without walking the whole history.
        if let Ok(txn) = Transaction::new(ndb) {
            let reducer = self.reducer.as_mut().expect("reducer present");
            event::reduce_delta(reducer, ndb, &txn, &keys);
            self.view = event::pick_board(reducer, author, board_id);
        }
        true
    }

    /// The cached board, if one has been folded.
    fn view(&self) -> Option<&BoardView> {
        self.view.as_ref()
    }

    /// Re-fold the whole event history into a fresh reducer (seeding or after an
    /// account switch) and pick out our board.
    fn reload(&mut self, ndb: &Ndb, author: &Pubkey, board_id: &str) {
        let reducer = Transaction::new(ndb)
            .ok()
            .and_then(|txn| event::fold_board(ndb, &txn, author));
        self.view = reducer
            .as_ref()
            .and_then(|r| event::pick_board(r, author, board_id));
        self.reducer = reducer;
        #[cfg(test)]
        {
            self.full_reloads += 1;
        }
    }

    /// Ensure we hold a live subscription to `author`'s headway events,
    /// resubscribing (and dropping the cached reducer) when the selected account
    /// changes. A fresh subscription only reports *future* ingests, so the next
    /// poll does a one-off full fold to pick up what's already there.
    fn sync_subscription(&mut self, ndb: &mut Ndb, author: &Pubkey) {
        if self.sub.is_some() && self.sub_author.as_ref() == Some(author) {
            return;
        }
        if let Some(old) = self.sub.take() {
            let _ = ndb.unsubscribe(old);
        }
        self.sub = ndb.subscribe(&[event::headway_filter(author)]).ok();
        self.sub_author = Some(*author);
        // New account (or first run): drop the cache so the next poll re-folds.
        self.view = None;
        self.reducer = None;
    }
}

impl App for Headway {
    fn kind_renderers(&self) -> Vec<Box<dyn notedeck::KindRenderer>> {
        // One cache shared by both renderers so an issue and its board, when both
        // are referenced, fold off a single subscription + reducer per board.
        let cache = Rc::new(RefCell::new(InlineBoardCache::default()));
        vec![
            Box::new(HeadwayIssueRenderer {
                cache: cache.clone(),
            }),
            Box::new(HeadwayBoardRenderer { cache }),
        ]
    }

    fn render(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let theme = ColorTheme::current(ui.ctx());

        let author = *ctx.accounts.selected_account_pubkey();
        // Copy the secret out so we don't hold a borrow on `accounts` while we
        // also touch `ndb`. `None` for a pubkey-only (watch) account.
        let signer: Option<[u8; 32]> = ctx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes());

        // Keep a live subscription to this account's events and re-fold the
        // cached board only when something changed (first load, account switch,
        // or our own async ingests landing); keep waking while it streams in.
        if self.sync.poll(ctx.ndb, &author, &self.board_id) {
            self.wake();
        }

        if self.sync.view().is_none() {
            // No board yet: auto-seed one for an account that can sign.
            match &signer {
                Some(secret) => {
                    if !self.seeded {
                        store::seed_default_board(
                            ctx.ndb,
                            &author,
                            secret,
                            &self.board_id,
                            &mut store::NoPublish,
                        );
                        self.seeded = true;
                        self.wake();
                    }
                    empty_state(ui, &theme, "Setting up your board…");
                }
                None => empty_state(
                    ui,
                    &theme,
                    "Sign in with a key to create your Headway board.",
                ),
            }
            self.pump_repaint(ui);
            return AppResponse::default();
        }

        // Render against the cached view; `sync` and `state` are disjoint fields.
        let action = board_ui(
            ui,
            &theme,
            self.sync.view().expect("view present"),
            &mut self.state,
        );

        // Apply the collected action by ingesting events locally. Mutations need
        // a signing key; a watch-only account simply can't edit.
        if let (Some(action), Some(secret)) = (action, &signer) {
            let view = self.sync.view().expect("view present");
            store::apply(
                ctx.ndb,
                &self.board_id,
                view,
                &author,
                secret,
                action,
                &mut store::NoPublish,
            );
            self.wake();
        }

        self.pump_repaint(ui);
        AppResponse::default()
    }
}

/// Render the board (header, columns, the add-column affordance and the floating
/// card detail sheet) and return the edit the user made this frame, if any.
fn board_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    view: &BoardView,
    state: &mut BoardUiState,
) -> Option<BoardAction> {
    let mut action: Option<BoardAction> = None;
    // The card a click landed on this frame; opens the detail view below.
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
            // Board header: title plus a muted summary of its contents.
            ui.heading(&view.title);
            ui.add_space(SPACING_XS);
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

    // Detail view floats above the board; emits edit actions like the rest.
    card_detail_ui(ui, theme, view, state, &mut action);

    // Archived-cards sheet floats above the board too.
    archived_sheet_ui(ui, theme, view, state, &mut action);

    action
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
        // A blank card opens straight into the editor; one with content shows
        // the rendered markdown until the user asks to edit.
        state.detail_desc_mode = if card.description.trim().is_empty() {
            DescMode::Editing { focus: false }
        } else {
            DescMode::Rendered
        };
        state.new_label.clear();
    }

    let ctx = DetailCtx {
        card_id,
        current_col,
        title: card.title.clone(),
        desc: card.description.clone(),
        labels: card.labels.clone(),
        // Owned copy so the body can render the status pill and column chips.
        columns: view.columns.iter().map(|c| c.name.clone()).collect(),
    };

    let screen = ui.ctx().screen_rect();
    let pad = SPACING_LG;
    // Narrow viewports get a near-full-width sheet; wider ones a centered modal.
    let sheet_width = if notedeck::ui::is_narrow(ui.ctx()) {
        screen.width() - 2.0 * pad
    } else {
        460.0
    };
    // Cap the body so a long card stays on-screen and scrolls instead.
    let max_body_height = screen.height() - 6.0 * pad;

    let mut outcome = if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
        DetailOutcome::Close
    } else {
        DetailOutcome::None
    };

    // Dimmed scrim behind the sheet; a tap outside closes the detail.
    if detail_scrim_ui(ui, screen) {
        outcome = DetailOutcome::Close;
    }

    egui::Area::new(egui::Id::new(("headway-detail", card_id)))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            detail_sheet_frame(theme, pad).show(ui, |ui| {
                ui.set_width(sheet_width);
                detail_header_ui(ui, theme, &ctx, &mut outcome);
                ui.add_space(SPACING_SM);
                egui::ScrollArea::vertical()
                    .max_height(max_body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        detail_body_ui(ui, theme, &ctx, state, action, &mut outcome);
                    });
            });
        });

    resolve_detail_outcome(state, action, view, &ctx, outcome);
}

/// The card data the detail sheet needs, copied out of the (immutable)
/// `BoardView` so the sheet body doesn't borrow it while we also mutate `state`.
struct DetailCtx {
    card_id: NoteId,
    current_col: usize,
    title: String,
    desc: String,
    labels: Vec<String>,
    columns: Vec<String>,
}

/// The single user intent collected while rendering the detail sheet, resolved
/// into a [`BoardAction`] after the UI closures return. At most one is produced
/// per frame (distinct buttons / keys are mutually exclusive), so one enum
/// models it better — and resolves more obviously — than a bag of bools.
#[derive(Default)]
enum DetailOutcome {
    #[default]
    None,
    /// Dismiss the sheet (✕, a tap on the scrim, or Escape).
    Close,
    Delete,
    Archive,
    /// Move the card to the column at this index.
    MoveTo(usize),
    /// Commit the "add label" field.
    AddLabel,
    /// Remove this label from the card's set.
    RemoveLabel(String),
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

/// Sheet header: a "Card" label, the current-status pill, and a close button
/// (the sheet has no draggable window chrome to dismiss it).
fn detail_header_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    ctx: &DetailCtx,
    outcome: &mut DetailOutcome,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Card").color(theme.text_muted));
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
    let title_resp = ui.add(
        egui::TextEdit::singleline(&mut state.detail_title)
            .font(egui::TextStyle::Heading)
            .desired_width(f32::INFINITY)
            .hint_text("Title"),
    );
    if title_resp.lost_focus() {
        let title = state.detail_title.trim().to_string();
        if !title.is_empty() && title != ctx.title {
            *action = Some(BoardAction::EditTitle {
                card: ctx.card_id,
                title,
            });
        }
    }

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
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .button(egui::RichText::new("Delete card").color(theme.destructive))
            .clicked()
        {
            *outcome = DetailOutcome::Delete;
        }
        if ui.button("Archive").clicked() {
            *outcome = DetailOutcome::Archive;
        }
    });
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
        if matches!(state.detail_desc_mode, DescMode::Rendered) {
            let edit = egui::Button::new(egui::RichText::new("✎").color(theme.text_muted))
                .fill(egui::Color32::TRANSPARENT)
                .frame(false);
            if ui.add(edit).on_hover_text("Edit description").clicked() {
                state.detail_desc_mode = DescMode::Editing { focus: true };
            }
        }
    });
    ui.add_space(SPACING_XS);

    match state.detail_desc_mode {
        DescMode::Rendered => {
            // The whole rendered block is a double-click target into the editor.
            let resp = ui
                .scope(|ui| notedeck_ui::markdown::render_markdown(&state.detail_desc, ui))
                .response
                .interact(egui::Sense::click());
            if resp.double_clicked() {
                state.detail_desc_mode = DescMode::Editing { focus: true };
            }
        }
        DescMode::Editing { focus } => {
            let desc_resp = ui.add(
                egui::TextEdit::multiline(&mut state.detail_desc)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("Add more detail… (markdown supported)"),
            );
            if focus {
                desc_resp.request_focus();
                state.detail_desc_mode = DescMode::Editing { focus: false };
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
                    state.detail_desc_mode = DescMode::Rendered;
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

    let mut close = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
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

// ---------------------------------------------------------------------------
// Inline renderers — drawing a single headway entity referenced from elsewhere
// (e.g. a `nostr:` link in a notebook note), via notedeck's `KindRenderer`
// registry. These are read-only and self-contained, unlike the editable board.
// ---------------------------------------------------------------------------

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

/// Per-board fold cache shared by the inline renderers, so referenced headway
/// entities resolve to their *current* state without re-folding the whole event
/// history every frame.
///
/// Mirrors [`BoardSync`] but keyed by `(board author, board_id)` for arbitrarily
/// many boards (an inline reference can point at any board, not just the open
/// one), driven by a `&Ndb` (the [`notedeck::KindRenderer`] render path has no
/// `&mut Ndb`). Each board holds a live subscription + long-lived reducer; the
/// first touch folds the history once to seed it and every later frame folds in
/// only the freshly-arrived notes ([`event::reduce_delta`]). Subscriptions are
/// kept for the app's lifetime — there's no eviction, since the set of referenced
/// boards is small and bounded by what the user actually views.
#[derive(Default)]
struct InlineBoardCache {
    boards: HashMap<(Pubkey, String), InlineBoard>,
    /// Test-only count of full-history folds, to assert later frames fold deltas
    /// rather than re-walking the whole log.
    #[cfg(test)]
    full_reloads: u32,
}

/// One board's cached subscription + reducer within [`InlineBoardCache`].
#[derive(Default)]
struct InlineBoard {
    reducer: Option<BoardReducer>,
    sub: Option<Subscription>,
}

impl InlineBoardCache {
    /// Bring the cached reducer for `(author, board_id)` up to date with the
    /// local db and return it. Seeds with a one-off full fold on first touch
    /// (and folds every frame if the subscription couldn't be created), then
    /// folds only freshly-arrived notes in on later frames.
    fn reducer(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        author: &Pubkey,
        board_id: &str,
    ) -> Option<&BoardReducer> {
        let key = (*author, board_id.to_owned());
        let mut seeded = false;
        {
            let board = self.boards.entry(key.clone()).or_default();
            if board.sub.is_none() {
                board.sub = ndb.subscribe(&[event::headway_filter(author)]).ok();
            }
            match board.sub {
                // No subscription: fold the whole history each frame so edits show.
                None => {
                    board.reducer = event::fold_board(ndb, txn, author);
                    seeded = true;
                }
                Some(sub) => {
                    let keys = ndb.poll_for_notes(sub, 64);
                    if board.reducer.is_none() {
                        // First touch (a fresh subscription only reports *future*
                        // ingests): fold the existing history once to seed.
                        board.reducer = event::fold_board(ndb, txn, author);
                        seeded = true;
                    } else if !keys.is_empty() {
                        // Incremental: fold only the new notes into the live
                        // reducer. Commutative/idempotent, so it matches a re-fold.
                        if let Some(reducer) = board.reducer.as_mut() {
                            event::reduce_delta(reducer, ndb, txn, &keys);
                        }
                    }
                }
            }
        }
        #[cfg(test)]
        if seeded {
            self.full_reloads += 1;
        }
        let _ = seeded;
        self.boards.get(&key).and_then(|b| b.reducer.as_ref())
    }
}

/// Renders a headway issue (kind 1621) referenced inline, e.g. from a notebook
/// note. Registered into [`notedeck::KindRendererRegistry`] at app startup.
///
/// The kind-1621 note is only the card's *creation-time* snapshot; its current
/// title/labels/description come from folding the owning board's later edits. So
/// we fold the board (cached, see [`InlineBoardCache`]) and render the resolved
/// [`event::CardView`], falling back to the raw snapshot if the board isn't local.
pub struct HeadwayIssueRenderer {
    cache: Rc<RefCell<InlineBoardCache>>,
}

impl notedeck::KindRenderer for HeadwayIssueRenderer {
    fn id(&self) -> &'static str {
        "headway.issue"
    }
    fn name(&self) -> &'static str {
        "Headway issue"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_ISSUE]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        txn: &Transaction,
        note: &nostrdb::Note,
    ) -> egui::Response {
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Issue(issue)) = event::parse(note) else {
            return ui.weak("invalid headway issue");
        };
        let author = Pubkey::new(issue.board_author);
        // Resolve the card's current state off the (cached) folded board.
        let card = self
            .cache
            .borrow_mut()
            .reducer(note_context.ndb, txn, &author, &issue.board_id)
            .and_then(|reducer| event::pick_card(reducer, &author, &issue.board_id, &issue.id));
        match card {
            Some(card) => card_inline_ui(ui, &theme, &card),
            // Board not local to fold: show the creation-time snapshot.
            None => issue_inline_ui(ui, &theme, &issue),
        }
    }
}

/// Renders a headway board (kind 30619) referenced inline. The note is the
/// addressable board event; we recover its `(author, board_id)` and fold the
/// full board (cached, see [`InlineBoardCache`]) off the local db to summarise it.
pub struct HeadwayBoardRenderer {
    cache: Rc<RefCell<InlineBoardCache>>,
}

impl notedeck::KindRenderer for HeadwayBoardRenderer {
    fn id(&self) -> &'static str {
        "headway.board"
    }
    fn name(&self) -> &'static str {
        "Headway board"
    }
    fn kinds(&self) -> &'static [u32] {
        &[event::KIND_BOARD]
    }
    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut notedeck::NoteContext,
        txn: &Transaction,
        note: &nostrdb::Note,
    ) -> egui::Response {
        let theme = ColorTheme::current(ui.ctx());
        let Some(event::HeadwayEvent::Board(board)) = event::parse(note) else {
            return ui.weak("invalid headway board");
        };
        let author = Pubkey::new(board.author);
        let view = self
            .cache
            .borrow_mut()
            .reducer(note_context.ndb, txn, &author, &board.id)
            .and_then(|reducer| event::pick_board(reducer, &author, &board.id));
        match view {
            Some(view) => board_inline_ui(ui, &theme, &view),
            None => ui.weak("headway board not found"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use nostrdb::{Config, Ndb};
    use std::time::{Duration, Instant};

    /// A headless harness driving a [`BoardSync`] against a bare `Ndb` — the
    /// subscription / poll / refold logic with no egui in sight. Mirrors the
    /// `store::tests::TestNdb` poll-loop pattern (ingest is async).
    struct TestSync {
        ndb: Ndb,
        _dir: tempfile::TempDir,
        kp: FullKeypair,
        sync: BoardSync,
    }

    impl TestSync {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
            Self {
                ndb,
                _dir: dir,
                kp: FullKeypair::generate(),
                sync: BoardSync::default(),
            }
        }

        fn secret(&self) -> [u8; 32] {
            self.kp.secret_key.secret_bytes()
        }

        /// One poll cycle against this account's board. Returns whether the
        /// board was re-folded this call.
        fn poll(&mut self) -> bool {
            self.sync
                .poll(&mut self.ndb, &self.kp.pubkey, store::BOARD_ID)
        }

        fn seed(&mut self) {
            store::seed_default_board(
                &self.ndb,
                &self.kp.pubkey,
                &self.secret(),
                store::BOARD_ID,
                &mut store::NoPublish,
            );
        }

        /// Poll until the cached view satisfies `pred` (ingest is async). Fails
        /// the test if it never holds.
        fn wait<F: Fn(&BoardView) -> bool>(&mut self, pred: F) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.poll();
                if self.sync.view().is_some_and(&pred) {
                    return;
                }
                assert!(Instant::now() < deadline, "sync predicate never held");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// Poll until the subscription stops reporting new notes, so the cache
        /// is quiescent (the async writer has drained).
        fn drain(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.poll() {
                assert!(Instant::now() < deadline, "sync never quiesced");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn total_cards(view: &BoardView) -> usize {
        view.columns.iter().map(|c| c.cards.len()).sum()
    }

    /// Subscribing before seeding, then polling, materialises the whole board
    /// from events already in ndb.
    #[test]
    fn poll_materialises_the_board() {
        let mut t = TestSync::new();
        // Subscribe first so the seed's ingests are reported as new notes.
        t.poll();
        t.seed();

        t.wait(|v| total_cards(v) == 7);
        let view = t.sync.view().expect("board loaded");
        assert_eq!(
            view.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Backlog", "Todo", "In Progress", "Done"]
        );
        assert_eq!(view.columns[0].cards.len(), 3);
    }

    /// An edit ingested after the initial load is picked up on a later poll —
    /// the cache reflects the change, not a stale snapshot.
    #[test]
    fn poll_reloads_on_change() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| v.columns[1].cards.len() == 2);

        // Apply against the cached pre-edit view (as render does).
        {
            let view = t.sync.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Fresh card".to_string(),
                    labels: vec![],
                },
                &mut store::NoPublish,
            );
        }

        // The new card only appears if a later poll re-folded the board.
        t.wait(|v| v.columns[1].cards.len() == 3);
        let view = t.sync.view().expect("board loaded");
        assert_eq!(view.columns[1].cards.last().unwrap().title, "Fresh card");
    }

    /// Once quiescent, polling with nothing new must NOT re-fold — this is the
    /// whole point of the cache (no per-frame walk of the event history).
    #[test]
    fn poll_does_not_refold_when_idle() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| total_cards(v) == 7);
        t.drain();

        assert!(
            !t.poll(),
            "cache re-folded with no new events — the per-frame fold is back"
        );
    }

    /// A change after the initial load is absorbed incrementally: the live
    /// reducer folds the delta, with no additional full-history re-fold. Guards
    /// against a regression to reload-on-every-change.
    #[test]
    fn poll_folds_changes_as_a_delta() {
        let mut t = TestSync::new();
        t.poll();
        t.seed();
        t.wait(|v| v.columns[1].cards.len() == 2);
        t.drain();

        // Seeding does exactly one full fold; everything since is incremental.
        assert_eq!(
            t.sync.full_reloads, 1,
            "seeding should fold the history once"
        );

        {
            let view = t.sync.view().expect("board loaded");
            store::apply(
                &t.ndb,
                store::BOARD_ID,
                view,
                &t.kp.pubkey,
                &t.secret(),
                store::BoardAction::AddCard {
                    col: 1,
                    title: "Delta card".to_string(),
                    labels: vec![],
                },
                &mut store::NoPublish,
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3);

        assert_eq!(
            t.sync.full_reloads, 1,
            "the edit triggered a full re-fold instead of a delta"
        );
    }

    /// The inline-renderer cache ([`InlineBoardCache`]) folds the history once on
    /// first touch and then absorbs later edits as deltas via its subscription —
    /// never re-walking the history per frame. The render-path counterpart to
    /// [`poll_folds_changes_as_a_delta`], driven by `&Ndb` like the renderers.
    #[test]
    fn inline_cache_folds_once_then_deltas() {
        let dir = tempfile::TempDir::new().unwrap();
        let ndb = Ndb::new(dir.path().to_str().unwrap(), &Config::new()).unwrap();
        let kp = FullKeypair::generate();
        let mut cache = InlineBoardCache::default();

        // One cache cycle (what a renderer does each frame): bring the cached
        // reducer up to date and fold out the board.
        let fold = |cache: &mut InlineBoardCache, ndb: &Ndb| -> Option<BoardView> {
            let txn = Transaction::new(ndb).unwrap();
            cache
                .reducer(ndb, &txn, &kp.pubkey, store::BOARD_ID)
                .and_then(|r| event::pick_board(r, &kp.pubkey, store::BOARD_ID))
        };

        // Subscribe (seeding an empty reducer) before the board exists, so the
        // seed's ingests arrive as subscription deltas rather than a re-fold.
        fold(&mut cache, &ndb);
        store::seed_default_board(
            &ndb,
            &kp.pubkey,
            &kp.secret_key.secret_bytes(),
            store::BOARD_ID,
            &mut store::NoPublish,
        );

        // Poll until the board materialises (ingest is async on a writer thread).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fold(&mut cache, &ndb).is_some_and(|v| total_cards(&v) == 7) {
                break;
            }
            assert!(Instant::now() < deadline, "inline board never materialised");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Exactly one full fold — the initial empty seed; every event since
        // (the whole seeded board) folded in incrementally as deltas.
        assert_eq!(
            cache.full_reloads, 1,
            "inline cache re-walked the history instead of folding deltas"
        );
    }
}
