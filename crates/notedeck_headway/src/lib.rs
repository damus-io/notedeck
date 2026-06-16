use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::tokens::{
    PALETTE, RADIUS_LG, RADIUS_MD, RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS,
    STROKE_MEDIUM, STROKE_THIN,
};
use notedeck::{App, AppContext, AppResponse, ColorTheme};

pub mod event;
pub mod store;

use event::{ArchivedCard, BoardReducer, BoardView, CardView, ColumnView};
use store::BoardAction;

/// Width of a single kanban column.
const COLUMN_WIDTH: f32 = 280.0;

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
    /// Whether the archived-cards sheet is open.
    showing_archived: bool,
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
                        store::seed_default_board(ctx.ndb, &author, secret, &self.board_id);
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
            store::apply(ctx.ndb, &self.board_id, view, &author, secret, action);
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

    // Span the column's full width so empty/sparse lanes still present a wide
    // drop target, but let the height track the cards so the add-card
    // affordance sits right beneath them instead of being pushed to the bottom.
    let fill_width = ui.available_width();

    // Detect drops over a bare, transparent frame rather than `dnd_drop_zone`,
    // which always paints a highlight box around the whole lane. The accent
    // insertion line is the only feedback we want.
    let zone = frame
        .show(ui, |ui| {
            // Fill the column width so the drop target spans the whole lane —
            // otherwise an empty column collapses to a narrow vertical strip.
            ui.set_min_height(SPACING_LG);
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
    section_label(ui, theme, "Description");
    ui.add_space(SPACING_XS);
    let desc_resp = ui.add(
        egui::TextEdit::multiline(&mut state.detail_desc)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .hint_text("Add more detail…"),
    );
    if desc_resp.lost_focus() && state.detail_desc != ctx.desc {
        *action = Some(BoardAction::EditDescription {
            card: ctx.card_id,
            description: state.detail_desc.clone(),
        });
    }

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
            store::seed_default_board(&self.ndb, &self.kp.pubkey, &self.secret(), store::BOARD_ID);
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
                },
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
                },
            );
        }
        t.wait(|v| v.columns[1].cards.len() == 3);

        assert_eq!(
            t.sync.full_reloads, 1,
            "the edit triggered a full re-fold instead of a delta"
        );
    }
}
