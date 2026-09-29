//! The review pane — a card's review records and the commit diff they name,
//! full-pane like the dependency graph — the Review section of the card detail
//! that opens it, and the review queue that walks the pane over the board's In
//! Review column.
//!
//! The pane is "render the review for card X": which card and record are open
//! live in [`ReviewUi`], seeded from the [`Review`](crate::HeadwayRoute::Review)
//! nav route, and everything slow (resolving the commit, fetching it from the host
//! that recorded it, `git show`) runs on a [`ReviewLoader`] worker so the frame
//! only ever draws what has already arrived.

use headway::event::{BoardView, CardView, ReviewFields, ReviewView};
use headway::git;
use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS};
use std::time::Instant;

use super::widgets::{count_badge, detail_heading};
use super::{BoardUiState, find_card};
use crate::nav::ReviewTarget;
use crate::review::{RecordSet, ReviewJob, ReviewLoad, ReviewLoader, ReviewSource, short_sha};

/// The review pane's slice of [`BoardUiState`]: which card is open, which of
/// its records is picked, and the loader its commits come through.
#[derive(Default)]
pub(crate) struct ReviewUi {
    /// The card whose review pane is open. Seeded from the nav route like
    /// [`graph_epic`](BoardUiState::graph_epic), so the nav stack decides.
    card: Option<NoteId>,
    /// Which of the card's records is shown, by note id; `None` is the newest.
    /// Resolved against [`CardView::reviews`] each frame, falling back to the
    /// newest when the card no longer carries it.
    record: Option<NoteId>,
    /// The route target [`seed`](Self::seed) last applied. The record is only
    /// re-seeded when the route changes, so a pick made in the pane isn't
    /// overwritten by the entry's own route on the next frame.
    seeded: Option<ReviewTarget>,
    /// The card [`card_ref`](Self::card_ref) was formatted for.
    ref_for: Option<NoteId>,
    /// The open card's `headway:<board>/<word-id>`, formatted once per card
    /// rather than every frame.
    card_ref: String,
    /// The pane was opened (or moved to another card) since it last drew, so
    /// its trailer search is re-run (see [`ReviewLoader::expire`]).
    reopened: bool,
    /// The loads, cached per record until they go stale.
    loader: ReviewLoader,
}

impl ReviewUi {
    /// The card whose review pane is open, if any.
    pub(crate) fn card(&self) -> Option<NoteId> {
        self.card
    }

    /// The picked record's note id, `None` for the newest: what a
    /// [`Review`](crate::HeadwayRoute::Review) push snapshots.
    pub(crate) fn record(&self) -> Option<NoteId> {
        self.record
    }

    /// Seed the open review from the nav route (see
    /// [`BoardUiState::set_review`]). The card is seeded every frame, like the
    /// graph's epic; the record only when the route's target differs from the
    /// last one seeded, i.e. on landing on another entry.
    pub(crate) fn seed(&mut self, target: Option<ReviewTarget>) {
        if target != self.seeded {
            self.seeded = target;
            if let Some(target) = target {
                self.record = target.record;
            }
        }
        self.set_card(target.map(|t| t.card));
    }

    /// Point the pane at `card`, flagging a re-open when it moves to a card.
    fn set_card(&mut self, card: Option<NoteId>) {
        self.reopened |= card.is_some() && card != self.card;
        self.card = card;
    }

    /// Open `card`'s review on `record` (`None` = newest).
    pub(crate) fn open(&mut self, card: NoteId, record: Option<NoteId>) {
        self.set_card(Some(card));
        self.record = record;
    }

    /// Close the pane, back to the card's detail.
    pub(crate) fn close(&mut self) {
        self.card = None;
    }
}

/// The column the review queue walks, by id; boards that renamed their columns
/// fall back to [`IN_REVIEW_NAME`].
const IN_REVIEW_ID: &str = "in-review";

/// The In Review column's name, matched case-insensitively when no column has
/// [`IN_REVIEW_ID`].
const IN_REVIEW_NAME: &str = "In Review";

/// How long, in seconds, the "Nothing in review" notice stays up after an `R`
/// that found the In Review column empty.
pub(crate) const EMPTY_QUEUE_NOTICE: f64 = 3.0;

/// The ids of the board's In Review cards in column order: the review queue's
/// snapshot. Empty when the board has no such column.
pub(crate) fn in_review_cards(view: &BoardView) -> Vec<NoteId> {
    let column = view
        .columns
        .iter()
        .find(|c| c.id == IN_REVIEW_ID)
        .or_else(|| {
            view.columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(IN_REVIEW_NAME))
        });
    column.map_or_else(Vec::new, |c| c.cards.iter().map(|c| c.id).collect())
}

/// Which way a queue step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueStep {
    /// To the next card (`n`, `]`).
    Next,
    /// To the previous card (`p`, `[`).
    Prev,
}

/// The review queue: a snapshot of the board's In Review cards, taken when `R`
/// opens it, and which of them the review pane shows.
///
/// A snapshot so that a verdict moving a card out of In Review doesn't
/// reshuffle what's left under the reviewer. It outlives the queue closing, so
/// a back/forward onto the queue's history entry reopens it where it was left;
/// the next `R` takes a fresh one.
#[derive(Default)]
pub(crate) struct ReviewQueue {
    /// The In Review cards, in column order, when the queue opened.
    cards: Vec<NoteId>,
    /// The position in [`cards`](Self::cards) the pane shows.
    index: usize,
    /// Whether the queue is showing. Seeded from the nav route like the
    /// graph's epic, so the nav stack decides.
    open: bool,
    /// `"3 / 12"`, formatted when the position changes rather than every frame.
    position: String,
}

impl ReviewQueue {
    /// Open the queue over `cards` at its first card. Returns `false`, leaving
    /// the queue closed and its previous snapshot alone, when there are none.
    pub(crate) fn start(&mut self, cards: Vec<NoteId>) -> bool {
        if cards.is_empty() {
            return false;
        }
        self.cards = cards;
        self.open = true;
        self.go_to(0);
        true
    }

    /// Whether the queue is showing.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Seed whether the queue shows from the nav route. There's nothing to show
    /// without a snapshot, so a queue entry reached before any `R` (none can be,
    /// today) draws the board.
    pub(crate) fn set_open(&mut self, open: bool) {
        self.open = open && !self.cards.is_empty();
    }

    /// The card the pane shows, while the queue is open.
    pub(crate) fn current(&self) -> Option<NoteId> {
        if !self.open {
            return None;
        }
        self.cards.get(self.index).copied()
    }

    /// The card after the current one, if the queue is open and has one.
    pub(crate) fn next_card(&self) -> Option<NoteId> {
        self.current()?;
        self.cards.get(self.index + 1).copied()
    }

    /// The review target the pane opens on: the current card's newest record.
    pub(crate) fn target(&self) -> Option<ReviewTarget> {
        self.current()
            .map(|card| ReviewTarget { card, record: None })
    }

    /// Step one card `step`'s way, stopping at either end (no wrap).
    pub(crate) fn step(&mut self, step: QueueStep) {
        let index = match step {
            QueueStep::Next if self.index + 1 < self.cards.len() => self.index + 1,
            QueueStep::Prev if self.index > 0 => self.index - 1,
            QueueStep::Next | QueueStep::Prev => return,
        };
        self.go_to(index);
    }

    /// Close the queue, keeping its snapshot. Returns the card it was showing.
    pub(crate) fn close(&mut self) -> Option<NoteId> {
        let current = self.current();
        self.open = false;
        current
    }

    /// Where the pane is in the queue, as `"3 / 12"`.
    pub(crate) fn position(&self) -> &str {
        &self.position
    }

    /// Move to `index` and re-format the position.
    fn go_to(&mut self, index: usize) {
        use std::fmt::Write;
        self.index = index;
        self.position.clear();
        let _ = write!(self.position, "{} / {}", index + 1, self.cards.len());
    }
}

/// Draw the review queue: a bar with where the pane is in the queue and a peek
/// at the next card's title, above the review pane for the queue's current
/// card. Starts the next card's load too, so stepping to it is instant. The
/// queue's keys ([`crate::keys::queue_keys`]) have already run this frame.
pub(super) fn review_queue_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    current: NoteId,
    state: &mut BoardUiState,
) {
    // The route seeds this too (see `Headway::render_nav`); seeding it here
    // as well covers a chrome-less embedding and the frame a step lands on.
    state.review.seed(state.queue.target());

    if let Some((_, next)) = state.queue.next_card().and_then(|c| find_card(view, c)) {
        prefetch(app_ctx, view, next, &mut state.review.loader);
    }

    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: SPACING_LG as i8,
            right: SPACING_LG as i8,
            top: SPACING_LG as i8,
            bottom: 0,
        })
        .show(ui, |ui| queue_bar_ui(ui, theme, view, &state.queue));

    let Some((_, card)) = find_card(view, current) else {
        // A card that left the board since the snapshot (archived, moved to
        // another board) keeps its place in the queue, so the position still
        // adds up; there's just nothing to review.
        egui::Frame::new()
            .inner_margin(egui::Margin::same(SPACING_LG as i8))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("This card is no longer on the board.")
                        .color(theme.text_muted),
                );
            });
        return;
    };
    review_pane_ui(ui, theme, app_ctx, view, card, state);
    // The pane's own ← Back closes the review; in the queue that leaves it.
    if state.review.card().is_none() {
        state.close_queue();
    }
}

/// The queue's bar: its name, the position (`3 / 12`) and the next card's
/// title, muted, as a peek. Every string is borrowed, so nothing is formatted
/// per frame.
fn queue_bar_ui(ui: &mut egui::Ui, theme: &ColorTheme, view: &BoardView, queue: &ReviewQueue) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Review queue").strong());
        ui.label(egui::RichText::new(queue.position()).color(theme.accent));
        let Some((_, next)) = queue.next_card().and_then(|c| find_card(view, c)) else {
            return;
        };
        ui.add_space(SPACING_MD);
        ui.label(egui::RichText::new("Next:").small().color(theme.text_muted));
        ui.label(
            egui::RichText::new(next.title.as_str())
                .small()
                .color(theme.text_muted),
        );
    });
}

/// Start the load of `card`'s newest record, if it hasn't been, so the queue's
/// next card is ready by the time it's stepped to. A card with no record is
/// left alone: its trailer search is re-run when the pane opens on it anyway
/// (see [`ReviewLoader::expire`]), so loading it early would only run it twice.
fn prefetch(
    app_ctx: &notedeck::AppContext,
    view: &BoardView,
    card: &CardView,
    loader: &mut ReviewLoader,
) {
    let Some(record) = card.reviews.first() else {
        return;
    };
    let source = ReviewSource::Record {
        card: card.id,
        record: record.id,
    };
    if loader.contains(source) {
        return;
    }
    let card_ref = headway::wordid::card_ref(&view.id, card.id.bytes());
    start_load(app_ctx, view, &card_ref, Some(record), source, loader);
}

/// Draw the review pane for `card`: a topbar (back, card ref, title, explainer,
/// session), the record picker when there are several, the resolve status, and
/// the commit's diff filling the rest. Escape or ← Back closes it.
pub(super) fn review_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    card: &CardView,
    state: &mut BoardUiState,
) {
    let review = &mut state.review;
    if ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        review.close();
        return;
    }
    if review.ref_for != Some(card.id) {
        review.ref_for = Some(card.id);
        review.card_ref = headway::wordid::card_ref(&view.id, card.id.bytes());
    }
    review
        .loader
        .note_records(card.id, RecordSet::of(&card.reviews));
    let shown = review
        .record
        .and_then(|id| card.reviews.iter().position(|r| r.id == id))
        .unwrap_or(0);
    let record = card.reviews.get(shown);

    let source = match record {
        Some(r) => ReviewSource::Record {
            card: card.id,
            record: r.id,
        },
        None => ReviewSource::Trailer(card.id),
    };
    let reopened = std::mem::take(&mut review.reopened);
    if let Some(left) = review.loader.expire(source, Instant::now(), reopened) {
        // A failed load restarts on its own once its backoff is up, so draw
        // the frame that does it even if nothing else moves.
        ui.ctx().request_repaint_after(left);
    }
    start_load(
        app_ctx,
        view,
        &review.card_ref,
        record,
        source,
        &mut review.loader,
    );
    review.loader.poll(app_ctx.i18n);

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            review_topbar_ui(ui, theme, app_ctx, card, record, review);
            ui.add_space(SPACING_SM);
            ui.separator();
            ui.add_space(SPACING_SM);

            if card.reviews.len() > 1 {
                if let Some(picked) = record_picker_ui(ui, &card.reviews, shown) {
                    review.record = Some(picked);
                }
                ui.add_space(SPACING_XS);
            }
            if let Some(r) = record {
                ui.horizontal_wrapped(|ui| record_summary_ui(ui, theme, &r.fields));
            }
            ui.add_space(SPACING_XS);
            load_ui(ui, theme, &mut review.loader, source);
        });
}

/// Start `source`'s load if it hasn't been: the record (or the card's trailer)
/// plus where on this host to look for it. Allocates only on the frame a load
/// starts; every later frame is a map lookup.
fn start_load(
    app_ctx: &notedeck::AppContext,
    view: &BoardView,
    card_ref: &str,
    record: Option<&ReviewView>,
    source: ReviewSource,
    loader: &mut ReviewLoader,
) {
    if loader.contains(source) {
        return;
    }
    let checkouts = git::known_checkouts(view, loader.local_host(), None);
    let job = ReviewJob {
        record: record.map(|r| r.fields.clone()),
        card_ref: card_ref.to_string(),
        checkouts,
        cache_root: app_ctx
            .path
            .path(notedeck::DataPathType::Cache)
            .join("headway")
            .join("git"),
    };
    loader.start(source, job, app_ctx.waker.clone());
}

/// The pane's top bar: ← Back, the card ref (click copies), the title, and —
/// right-aligned — the record's explainer link and its agentium session chip.
fn review_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card: &CardView,
    record: Option<&ReviewView>,
    review: &mut ReviewUi,
) {
    ui.horizontal(|ui| {
        let back = egui::Button::new(egui::RichText::new("← Back").color(theme.text_secondary))
            .fill(egui::Color32::TRANSPARENT)
            .frame(false);
        if ui.add(back).clicked() {
            review.close();
        }
        ui.label(egui::RichText::new("›").color(theme.text_muted));
        // A frameless button, not a Label, so a click copies rather than
        // starting a text selection (as the detail topbar's ref).
        let card_ref = egui::Button::new(
            egui::RichText::new(&review.card_ref)
                .color(theme.text_muted)
                .small(),
        )
        .frame(false);
        if ui.add(card_ref).on_hover_text("Click to copy").clicked() {
            ui.ctx().copy_text(review.card_ref.clone());
        }
        ui.label(egui::RichText::new(&card.title).strong());
        let Some(fields) = record.map(|r| &r.fields) else {
            return;
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(url) = fields.explainer.as_deref() {
                explainer_link_ui(ui, theme, url);
            }
            if let Some(session) = fields.agentium.as_deref() {
                agentium_chip_ui(ui, theme, app_ctx, session);
            }
        });
    });
}

/// One selectable chip per record, newest first, labelled by short sha, with
/// the `shown`-th selected. Returns the note id of a chip clicked this frame.
fn record_picker_ui(ui: &mut egui::Ui, reviews: &[ReviewView], shown: usize) -> Option<NoteId> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        for (i, r) in reviews.iter().enumerate() {
            let sha = r.fields.commit.as_deref().map_or("(no commit)", short_sha);
            let chip = ui.selectable_label(shown == i, egui::RichText::new(sha).monospace());
            let chip = match r.fields.title.as_deref() {
                Some(title) => chip.on_hover_text(title),
                None => chip,
            };
            if chip.clicked() {
                picked = Some(r.id);
            }
        }
    });
    picked
}

/// A record's one-line summary: short sha and subject, then where it was
/// recorded (`host:path`, branch). Laid out as separate labels so nothing is
/// formatted per frame.
fn record_summary_ui(ui: &mut egui::Ui, theme: &ColorTheme, fields: &ReviewFields) {
    ui.spacing_mut().item_spacing.x = SPACING_XS;
    if let Some(sha) = fields.commit.as_deref() {
        ui.label(
            egui::RichText::new(short_sha(sha))
                .monospace()
                .color(theme.accent),
        );
    }
    if let Some(title) = fields.title.as_deref() {
        ui.label(egui::RichText::new(title).color(theme.text_primary));
    }
    ui.add_space(SPACING_SM);
    record_location_ui(ui, theme, fields);
}

/// Where a record was made — `host:path ⎇ branch` in small muted text, each
/// part its own label (host and path packed tight) so nothing is formatted per
/// frame. Parts the record lacks are left out.
fn record_location_ui(ui: &mut egui::Ui, theme: &ColorTheme, fields: &ReviewFields) {
    let muted = |text: &str| egui::RichText::new(text).small().color(theme.text_muted);
    if let Some(host) = fields.host.as_deref() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(muted(host));
            if let Some(path) = fields.path.as_deref() {
                ui.label(muted(":"));
                ui.label(muted(path));
            }
        });
    }
    if let Some(branch) = fields.branch.as_deref() {
        ui.label(muted("⎇"));
        ui.label(muted(branch));
    }
}

/// The load's status and, once it's in, the diff: a spinner while the worker
/// runs, git's own error (command and stderr, verbatim, so a broken ssh or path
/// can be fixed by hand) with a retry, or where the commit was found above its
/// diff.
fn load_ui(ui: &mut egui::Ui, theme: &ColorTheme, loader: &mut ReviewLoader, source: ReviewSource) {
    let mut retry = false;
    match loader.get_mut(source) {
        None => {}
        Some(ReviewLoad::Pending { note }) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(note.as_str()).color(theme.text_muted));
            });
        }
        Some(ReviewLoad::Failed(err)) => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("git failed:").color(theme.destructive));
                ui.label(egui::RichText::new("git").monospace());
                ui.label(egui::RichText::new(err.command.as_str()).monospace());
            });
            ui.label(
                egui::RichText::new(err.stderr.as_str())
                    .monospace()
                    .color(theme.text_secondary),
            );
            ui.add_space(SPACING_XS);
            retry = ui.button("Retry").clicked();
        }
        Some(ReviewLoad::Ready(loaded)) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(loaded.found.as_str())
                        .small()
                        .color(theme.text_muted),
                );
                if loaded.by_trailer {
                    ui.label(
                        egui::RichText::new("hash differs from the record? (rebased)")
                            .small()
                            .color(theme.warning),
                    )
                    .on_hover_text(
                        "The recorded commit couldn't be found, so this is the newest \
                         commit carrying the card's Headway trailer.",
                    );
                }
            });
            ui.label(
                egui::RichText::new(loaded.byline.as_str())
                    .small()
                    .color(theme.text_muted),
            );
            if loaded.commit.truncated {
                ui.label(
                    egui::RichText::new("patch truncated")
                        .small()
                        .color(theme.warning),
                );
            }
            ui.add_space(SPACING_MD);
            notedeck_ui::diff::git_patch_ui(&loaded.patch, &mut loaded.patch_state, ui);
        }
    }
    if retry {
        loader.retry(source);
    }
}

/// A frameless "Explainer ↗" link that opens `url` in a new browser tab.
fn explainer_link_ui(ui: &mut egui::Ui, theme: &ColorTheme, url: &str) {
    let link =
        egui::Button::new(egui::RichText::new("Explainer ↗").color(theme.accent)).frame(false);
    if ui.add(link).on_hover_text(url).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

/// An `agentium:<word-id>` session drawn as its live inline chip through the
/// registered reference parser (Dave's), or as plain monospace text when no
/// parser resolves it (Dave isn't loaded, or the session is unknown here).
fn agentium_chip_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    session: &str,
) {
    let mut note_ctx = app_ctx.note_context();
    let drawn = nostrdb::Transaction::new(note_ctx.ndb).is_ok_and(|txn| {
        notedeck_ui::markdown::render_reference(
            ui,
            &mut note_ctx,
            &txn,
            session,
            notedeck::RenderContext::Inline,
        )
    });
    if !drawn {
        ui.label(
            egui::RichText::new(session)
                .small()
                .monospace()
                .color(theme.text_muted),
        );
    }
}

/// The card detail's Review section: a heading, one row per review record
/// (newest first; its sha and subject open the review pane on that record), and
/// a "Review diff" button. A card with no records but sitting in a terminal
/// column still offers the pane, which then looks the commit up by the card's
/// `Headway:` trailer — how cards finished before review records existed stay
/// reviewable. The caller draws it only for a card with records or in a
/// terminal column.
pub(super) fn review_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card_id: NoteId,
    reviews: &[ReviewView],
    state: &mut BoardUiState,
) {
    ui.horizontal(|ui| {
        detail_heading(ui, theme, "Review");
        if !reviews.is_empty() {
            count_badge(ui, theme, reviews.len());
        }
    });
    ui.add_space(SPACING_XS);

    // `Some(record)` once a row or the button was clicked; the inner `None`
    // (the button) opens on the newest record.
    let mut open: Option<Option<NoteId>> = None;
    for r in reviews {
        ui.horizontal_wrapped(|ui| {
            let fields = &r.fields;
            let sha = fields.commit.as_deref().map_or("(no commit)", short_sha);
            let title = fields.title.as_deref().unwrap_or("");
            let row = egui::Button::new(egui::RichText::new(sha).monospace().color(theme.accent))
                .frame(false);
            if ui
                .add(row)
                .on_hover_text("Review this commit's diff")
                .clicked()
            {
                open = Some(Some(r.id));
            }
            let subject = egui::Button::new(egui::RichText::new(title).color(theme.text_primary))
                .frame(false);
            if ui.add(subject).clicked() {
                open = Some(Some(r.id));
            }
            record_location_ui(ui, theme, fields);
            if let Some(session) = fields.agentium.as_deref() {
                agentium_chip_ui(ui, theme, app_ctx, session);
            }
            if let Some(url) = fields.explainer.as_deref() {
                explainer_link_ui(ui, theme, url);
            }
        });
    }
    if reviews.is_empty() {
        ui.label(
            egui::RichText::new(
                "No review record — the pane will look for the card's Headway trailer.",
            )
            .small()
            .color(theme.text_muted),
        );
    }

    ui.add_space(SPACING_XS);
    let button = egui::Button::new(egui::RichText::new("⧉ Review diff").color(theme.accent))
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);
    if ui.add(button).clicked() {
        open = Some(None);
    }
    if let Some(record) = open {
        state.review.open(card_id, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use headway::event::ColumnView;

    /// A column with id `id`, name `name` and one bare card per id in `cards`.
    fn column(id: &str, name: &str, cards: &[NoteId]) -> ColumnView {
        ColumnView {
            id: id.to_string(),
            name: name.to_string(),
            terminal: false,
            cards: cards
                .iter()
                .map(|&card| CardView {
                    id: card,
                    ..crate::ui::tests::card("", "", &[])
                })
                .collect(),
        }
    }

    /// A board holding `columns`, otherwise empty.
    fn board(columns: Vec<ColumnView>) -> BoardView {
        BoardView {
            id: "b".to_string(),
            author: [0; 32],
            title: String::new(),
            description: String::new(),
            created_at: 0,
            columns,
            archived: vec![],
        }
    }

    /// The queue snapshots the `in-review` column in column order, falling
    /// back to a column named In Review (any case) on a board whose ids differ,
    /// and is empty on a board with neither.
    #[test]
    fn in_review_cards_reads_the_in_review_column() {
        let ids: Vec<NoteId> = (1..=4).map(|i| NoteId::new([i; 32])).collect();
        let by_id = board(vec![
            column("todo", "In review", &ids[..1]),
            column("in-review", "Checking", &ids[1..3]),
        ]);
        assert_eq!(in_review_cards(&by_id), ids[1..3]);

        let by_name = board(vec![column("c2", "in REVIEW", &ids[3..])]);
        assert_eq!(in_review_cards(&by_name), ids[3..]);

        assert!(in_review_cards(&board(vec![column("todo", "Todo", &ids)])).is_empty());
    }

    /// The queue steps both ways and stops at its ends, keeping its position
    /// label in step; closing keeps the snapshot, and an empty snapshot never
    /// opens it.
    #[test]
    fn queue_steps_within_its_snapshot() {
        let ids: Vec<NoteId> = (1..=3).map(|i| NoteId::new([i; 32])).collect();
        let mut queue = ReviewQueue::default();
        assert!(!queue.start(vec![]));
        assert!(!queue.is_open());

        assert!(queue.start(ids.clone()));
        assert_eq!((queue.current(), queue.position()), (Some(ids[0]), "1 / 3"));
        assert_eq!(queue.next_card(), Some(ids[1]));
        queue.step(QueueStep::Prev);
        assert_eq!(queue.current(), Some(ids[0]), "no wrap back");

        queue.step(QueueStep::Next);
        queue.step(QueueStep::Next);
        assert_eq!((queue.current(), queue.position()), (Some(ids[2]), "3 / 3"));
        assert_eq!(queue.next_card(), None);
        queue.step(QueueStep::Next);
        assert_eq!(queue.current(), Some(ids[2]), "no wrap forward");
        queue.step(QueueStep::Prev);
        assert_eq!(
            queue.target(),
            Some(ReviewTarget {
                card: ids[1],
                record: None
            })
        );

        // Closing keeps the place; the route reopening it lands back there.
        assert_eq!(queue.close(), Some(ids[1]));
        assert_eq!((queue.current(), queue.target()), (None, None));
        queue.set_open(true);
        assert_eq!(queue.current(), Some(ids[1]));

        // A failed start leaves the last snapshot alone.
        queue.close();
        assert!(!queue.start(vec![]));
        queue.set_open(true);
        assert_eq!(queue.current(), Some(ids[1]));
    }

    /// With no snapshot, a route asking for the queue can't open it.
    #[test]
    fn queue_route_without_a_snapshot_stays_closed() {
        let mut queue = ReviewQueue::default();
        queue.set_open(true);
        assert!(!queue.is_open());
    }

    /// Opening the pane, or moving it to another card, flags a re-open once;
    /// the route re-seeding the same card every frame, or closing it, doesn't.
    #[test]
    fn set_card_flags_a_reopen_only_on_a_change_to_a_card() {
        let (a, b) = (NoteId::new([1; 32]), NoteId::new([2; 32]));
        let mut review = ReviewUi::default();
        review.set_card(Some(a));
        assert!(std::mem::take(&mut review.reopened));
        review.set_card(Some(a));
        assert!(!review.reopened);
        review.set_card(None);
        assert!(!review.reopened);
        review.open(a, None);
        assert!(std::mem::take(&mut review.reopened));
        review.set_card(Some(b));
        assert!(review.reopened);
    }

    /// The route seeds the record when it changes, and only then: a pick made
    /// in the pane survives the same route re-seeding every frame, while
    /// landing on another entry restores the record that entry carries.
    #[test]
    fn seed_applies_the_route_record_only_when_the_route_changes() {
        let (a, b) = (NoteId::new([1; 32]), NoteId::new([2; 32]));
        let (r1, r2) = (NoteId::new([3; 32]), NoteId::new([4; 32]));
        let on = |card, record| Some(ReviewTarget { card, record });
        let mut review = ReviewUi::default();

        review.seed(on(a, Some(r1)));
        assert_eq!((review.card(), review.record()), (Some(a), Some(r1)));

        // A pick in the pane, then the same route re-seeding: the pick stays.
        review.record = Some(r2);
        review.seed(on(a, Some(r1)));
        assert_eq!(review.record(), Some(r2));

        // Another card's entry, then back onto the first: each gets its own.
        review.seed(on(b, None));
        assert_eq!((review.card(), review.record()), (Some(b), None));
        review.seed(on(a, Some(r1)));
        assert_eq!((review.card(), review.record()), (Some(a), Some(r1)));

        // Leaving the review closes the pane; a detail-opened pick then lands
        // as its own pushed route unchanged.
        review.seed(None);
        assert_eq!(review.card(), None);
        review.open(b, Some(r2));
        review.seed(on(b, Some(r2)));
        assert_eq!((review.card(), review.record()), (Some(b), Some(r2)));
    }
}
