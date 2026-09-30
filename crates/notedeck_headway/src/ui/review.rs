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
use notedeck::tokens::{RADIUS_PILL, SPACING_LG, SPACING_MD, SPACING_SM, SPACING_XS};
use notedeck_ui::diff::PatchScroll;
use std::time::Instant;

use super::widgets::{
    MiddleElided, count_badge, detail_heading, secondary_action_button, text_pill, tinted_pill,
};
use super::{BoardUiState, find_card};
use crate::keys;
use crate::nav::ReviewTarget;
use crate::review::{RecordSet, ReviewJob, ReviewLoad, ReviewLoader, ReviewSource, short_sha};
use crate::store::BoardAction;

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
    /// The record [`location`](Self::location) was built for.
    location_for: Option<NoteId>,
    /// The shown record's `host:path`, elided to the commit line's width.
    location: MiddleElided,
    /// The pane was opened (or moved to another card) since it last drew, so
    /// its trailer search is re-run (see [`ReviewLoader::expire`]).
    reopened: bool,
    /// A scroll the queue's keys asked of the open diff, handed to its
    /// [`GitPatchState`](notedeck_ui::diff::GitPatchState) on the pane's next
    /// pass (and dropped there if the diff hasn't loaded).
    scroll: Option<PatchScroll>,
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

    /// Ask the open diff to scroll; applied on the pane's next pass.
    pub(crate) fn scroll(&mut self, request: PatchScroll) {
        self.scroll = Some(request);
    }

    /// The scroll waiting for the pane's next pass, if any.
    #[cfg(test)]
    pub(crate) fn pending_scroll(&self) -> Option<PatchScroll> {
        self.scroll
    }

    /// The record of `card` the pane shows: the picked one, or the newest when
    /// nothing is picked or the card no longer carries the pick.
    pub(crate) fn shown_record<'a>(&self, card: &'a CardView) -> Option<&'a ReviewView> {
        self.shown_index(card).and_then(|i| card.reviews.get(i))
    }

    /// [`shown_record`](Self::shown_record)'s position in the card's records,
    /// `None` when it has none.
    fn shown_index(&self, card: &CardView) -> Option<usize> {
        if card.reviews.is_empty() {
            return None;
        }
        let picked = self
            .record
            .and_then(|id| card.reviews.iter().position(|r| r.id == id));
        Some(picked.unwrap_or(0))
    }
}

/// A column the review queue reads or moves cards into: found by its id, or,
/// on a board that renamed its column ids, by its name (any case).
#[derive(Clone, Copy, Debug)]
pub(crate) struct QueueColumn {
    id: &'static str,
    name: &'static str,
}

/// The column the review queue walks.
const IN_REVIEW: QueueColumn = QueueColumn {
    id: "in-review",
    name: "In Review",
};

/// Where `D` sends a card.
pub(crate) const DONE: QueueColumn = QueueColumn {
    id: "done",
    name: "Done",
};

/// Where `X` sends a card back to.
pub(crate) const IN_PROGRESS: QueueColumn = QueueColumn {
    id: "in-progress",
    name: "In Progress",
};

impl QueueColumn {
    /// This column's index on `view`, by id, else by name.
    pub(crate) fn index(self, view: &BoardView) -> Option<usize> {
        let columns = &view.columns;
        columns.iter().position(|c| c.id == self.id).or_else(|| {
            columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(self.name))
        })
    }
}

/// How long, in seconds, a [`QueueNotice`] stays up.
pub(crate) const NOTICE_SECS: f64 = 3.0;

/// The ids of the board's In Review cards in column order: the review queue's
/// snapshot. Empty when the board has no such column.
pub(crate) fn in_review_cards(view: &BoardView) -> Vec<NoteId> {
    IN_REVIEW.index(view).map_or_else(Vec::new, |i| {
        view.columns[i].cards.iter().map(|c| c.id).collect()
    })
}

/// Which way a queue step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueStep {
    /// To the next card (`n`, `]`).
    Next,
    /// To the previous card (`p`, `[`).
    Prev,
}

/// Chord steps the review queue can be waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueuePending {
    /// `g` was pressed; a second `g` scrolls the diff to the top.
    G,
}

/// A short-lived message in the board header or the queue bar, for a key that
/// had nothing to act on. Shown for [`NOTICE_SECS`] seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueNotice {
    /// `R` found the In Review column empty.
    NothingInReview,
    /// A verdict on the queue's last card closed it.
    QueueDone,
    /// `o` on a record with no explainer.
    NoExplainer,
    /// `D` on a board with no Done column.
    NoDoneColumn,
    /// `X` on a board with no In Progress column.
    NoInProgressColumn,
}

impl QueueNotice {
    /// The message, as drawn.
    pub(crate) fn text(self) -> &'static str {
        match self {
            QueueNotice::NothingInReview => "Nothing in review",
            QueueNotice::QueueDone => "Review queue done",
            QueueNotice::NoExplainer => "No explainer on this record",
            QueueNotice::NoDoneColumn => "No Done column on this board",
            QueueNotice::NoInProgressColumn => "No In Progress column on this board",
        }
    }
}

/// The `X` composer's one-line reason, posted as a `review:` comment.
#[derive(Default)]
pub(crate) struct ReasonComposer {
    pub(crate) text: String,
    /// Grab focus on the composer's next layout (the frame after `X`).
    pub(crate) focus: bool,
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
    /// The `X` reason composer, while it's open. It owns the keyboard: the
    /// queue's keys stand down bar its Enter and Esc.
    pub(crate) reason: Option<ReasonComposer>,
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

    /// Whether the current card is the snapshot's last.
    pub(crate) fn at_end(&self) -> bool {
        self.index + 1 >= self.cards.len()
    }

    /// Close the queue, keeping its snapshot. Returns the card it was showing.
    /// An open reason composer is dropped with it.
    pub(crate) fn close(&mut self) -> Option<NoteId> {
        let current = self.current();
        self.open = false;
        self.reason = None;
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

/// Draw the review queue: the `X` reason composer while it's open, the
/// which-key strip while it's pinned or a `g` is pending, and the review pane
/// for the queue's current card in the rest (its header carries where the pane
/// is in the queue and a peek at the next card, see [`QueueHeader`]). Starts
/// the next card's load too, so stepping to it is instant. The queue's keys
/// ([`crate::keys::queue_keys`]) have already run this frame.
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

    if let Some(composer) = &mut state.queue.reason {
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: SPACING_LG as i8,
                right: SPACING_LG as i8,
                top: SPACING_LG as i8,
                bottom: 0,
            })
            .show(ui, |ui| reason_composer_ui(ui, theme, composer));
    }

    // Reserved from the bottom before the pane lays out, since the diff's
    // scroll area takes every point of height left (as the grid's strip).
    if let Some(hints) = keys::queue_key_hints(state) {
        egui::TopBottomPanel::bottom("headway-queue-key-hints")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: SPACING_LG as i8,
                right: SPACING_LG as i8,
                top: SPACING_MD as i8,
                bottom: SPACING_MD as i8,
            }))
            .show_inside(ui, |ui| keys::key_hints_ui(ui, theme, hints));
    }

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

/// The id of the `X` composer's text field, so tests (and focus requests) can
/// find it.
pub(crate) fn reason_field_id() -> egui::Id {
    egui::Id::new("headway-review-reason")
}

/// The `X` composer: a one-line reason field. Enter and Esc are the queue
/// keymap's, read before this lays out ([`crate::keys::queue_keys`]), so the
/// field only has to take the text and its focus.
fn reason_composer_ui(ui: &mut egui::Ui, theme: &ColorTheme, composer: &mut ReasonComposer) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Send back:").color(theme.destructive));
        let field = egui::TextEdit::singleline(&mut composer.text)
            .id(reason_field_id())
            .desired_width(f32::INFINITY)
            .hint_text("Why? Enter comments and moves it to In Progress, Esc cancels");
        let response = ui.add(field);
        if std::mem::take(&mut composer.focus) {
            response.request_focus();
        }
    });
}

/// The review queue's verdicts and the rest of what its keys do beyond
/// stepping (see [`crate::keys::queue_keys`]).
impl BoardUiState {
    /// Put up `notice` for [`NOTICE_SECS`] from `now` (egui time).
    pub(crate) fn set_notice(&mut self, notice: QueueNotice, now: f64) {
        self.notice = Some((notice, now));
    }

    /// The notice showing, if any.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<QueueNotice> {
        self.notice.map(|(n, _)| n)
    }

    /// Ask the open diff for `request`.
    pub(crate) fn scroll_review(&mut self, request: PatchScroll) {
        self.review.scroll(request);
    }

    /// The queue's current card, if it's still on `view`.
    fn queue_card<'a>(&self, view: &'a BoardView) -> Option<&'a CardView> {
        let current = self.queue.current()?;
        find_card(view, current).map(|(_, card)| card)
    }

    /// Open the explainer of the record the pane shows in a browser tab, or
    /// say there's none.
    pub(crate) fn open_explainer(&mut self, ctx: &egui::Context, view: &BoardView) {
        let url = self
            .queue_card(view)
            .and_then(|card| self.review.shown_record(card))
            .and_then(|r| r.fields.explainer.as_deref());
        match url {
            Some(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            None => self.set_notice(QueueNotice::NoExplainer, ctx.input(|i| i.time)),
        }
    }

    /// Leave the queue for the current card's detail. The queue keeps its
    /// place, so backing out of the detail lands on its entry where it was.
    pub(crate) fn open_queue_card(&mut self) {
        let Some(card) = self.queue.close() else {
            return;
        };
        self.review.close();
        self.cursor = Some(card);
        self.selected = Some(card);
    }

    /// `D`: move the current card to the end of the Done column and step on.
    /// `None` (and a notice) on a board without a Done column.
    pub(crate) fn accept_queue_card(&mut self, view: &BoardView, now: f64) -> Option<BoardAction> {
        let card = self.queue_card(view)?.id;
        let Some(to_col) = DONE.index(view) else {
            self.set_notice(QueueNotice::NoDoneColumn, now);
            return None;
        };
        self.advance_queue(now);
        Some(move_to_end(view, card, to_col))
    }

    /// `X`: open the reason composer for the current card. Nothing opens on a
    /// board it couldn't be sent back on.
    pub(crate) fn start_reject(&mut self, view: &BoardView, now: f64) {
        if self.queue_card(view).is_none() {
            return;
        }
        if IN_PROGRESS.index(view).is_none() {
            self.set_notice(QueueNotice::NoInProgressColumn, now);
            return;
        }
        self.queue.reason = Some(ReasonComposer {
            text: String::new(),
            focus: true,
        });
    }

    /// Enter in the reason composer: post the reason as a `review:` comment
    /// now, move the card to In Progress on the next frame (a frame applies one
    /// action), and step on. An empty reason posts nothing and keeps the
    /// composer open.
    pub(crate) fn submit_reject(&mut self, view: &BoardView, now: f64) -> Option<BoardAction> {
        let reason = self.queue.reason.as_ref()?.text.trim();
        if reason.is_empty() {
            return None;
        }
        let body = format!("review: {reason}");
        self.queue.reason = None;
        let card = self.queue_card(view)?.id;
        let to_col = IN_PROGRESS.index(view)?;
        self.follow_up = Some(move_to_end(view, card, to_col));
        self.advance_queue(now);
        Some(BoardAction::AddComment {
            card,
            body,
            reply_to: None,
        })
    }

    /// Esc in the reason composer: close it, sending nothing.
    pub(crate) fn cancel_reject(&mut self) {
        self.queue.reason = None;
    }

    /// Whether the `X` reason composer is open.
    pub(crate) fn rejecting(&self) -> bool {
        self.queue.reason.is_some()
    }

    /// The `X` composer's text, empty when it's closed.
    #[cfg(test)]
    pub(crate) fn reason_text(&self) -> &str {
        self.queue.reason.as_ref().map_or("", |r| r.text.as_str())
    }

    /// The scroll the queue's keys left for the diff's next pass.
    #[cfg(test)]
    pub(crate) fn review_scroll(&self) -> Option<PatchScroll> {
        self.review.pending_scroll()
    }

    /// Lay the `X` composer out alone, if it's open, for the keymap tests.
    #[cfg(test)]
    pub(crate) fn reason_test_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(composer) = &mut self.queue.reason {
            reason_composer_ui(ui, &ColorTheme::current(ui.ctx()), composer);
        }
    }

    /// After a verdict: step to the next card, or, on the last, leave the queue
    /// saying it's done.
    fn advance_queue(&mut self, now: f64) {
        if self.queue.at_end() {
            self.close_queue();
            self.set_notice(QueueNotice::QueueDone, now);
        } else {
            self.step_queue(QueueStep::Next);
        }
    }

    /// Take the action a verdict left for the frame after its own (the move
    /// behind an `X`'s comment).
    pub(crate) fn take_follow_up(&mut self) -> Option<BoardAction> {
        self.follow_up.take()
    }
}

/// A [`BoardAction::MoveCard`] taking `card` to the end of column `to_col`, as
/// the detail pane's column picker does.
fn move_to_end(view: &BoardView, card: NoteId, to_col: usize) -> BoardAction {
    BoardAction::MoveCard {
        card,
        to_col,
        to_row: view.columns[to_col].cards.len(),
    }
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

/// Draw the review pane for `card`: a one-row header (back, card ref, title,
/// session; the queue's position and next card when the pane is the queue's;
/// explainer), the record picker when there are several, the resolve status, and
/// the commit's diff filling the rest. Escape or ← Back closes it.
pub(super) fn review_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    card: &CardView,
    state: &mut BoardUiState,
) {
    if ui
        .ctx()
        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        state.review.close();
        return;
    }
    let queue = (state.queue.current() == Some(card.id)).then(|| QueueHeader {
        position: state.queue.position(),
        next: state
            .queue
            .next_card()
            .and_then(|c| find_card(view, c))
            .map(|(_, c)| c.title.as_str()),
        notice: &mut state.notice,
    });
    let review = &mut state.review;
    if review.ref_for != Some(card.id) {
        review.ref_for = Some(card.id);
        review.card_ref = headway::wordid::card_ref(&view.id, card.id.bytes());
    }
    review
        .loader
        .note_records(card.id, RecordSet::of(&card.reviews));
    let shown = review.shown_index(card).unwrap_or(0);
    let record = card.reviews.get(shown);
    if let Some(r) = record
        && review.location_for != Some(r.id)
    {
        review.location_for = Some(r.id);
        review.location = MiddleElided::new(host_path(&r.fields));
    }

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
    // A queue key's scroll goes to the diff if it's in; one asked of a diff
    // still loading is dropped rather than jumping it once it lands.
    if let Some(request) = review.scroll.take()
        && let Some(ReviewLoad::Ready(loaded)) = review.loader.get_mut(source)
    {
        loaded.patch_state.scroll(request);
    }

    egui::Frame::new()
        .inner_margin(egui::Margin::same(SPACING_LG as i8))
        .show(ui, |ui| {
            review_topbar_ui(ui, theme, app_ctx, card, record, review, queue);
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
                record_summary_ui(ui, theme, &r.fields, &card.title, &mut review.location);
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

/// Share of the header row the next card's title may take in the queue.
const PEEK_SHARE: f32 = 0.4;

/// Widest the header's session chip draws; a longer session title ellipsizes.
const SESSION_CHIP_MAX_WIDTH: f32 = 220.0;

/// The queue's part of the review header, while the pane shows the queue's
/// current card. Everything is borrowed, so the header formats nothing.
struct QueueHeader<'a> {
    /// `"3 / 12"`, cached on the [`ReviewQueue`].
    position: &'a str,
    /// The next card's title, if the queue has one.
    next: Option<&'a str>,
    /// The queue keys' short-lived message, shown beside the position.
    notice: &'a mut Option<(QueueNotice, f64)>,
}

/// The pane's header, one row. Left: ← Back, the card ref (click copies), the
/// title elided to one line, and the record's agentium session chip capped at
/// [`SESSION_CHIP_MAX_WIDTH`]. Right: in the queue, its position as a pill and
/// the next card's title as a muted peek (at most [`PEEK_SHARE`] of the row);
/// the record's explainer link at the far end. On a narrow screen the peek
/// goes, the card ref with it, and the chip shrinks to its status dot.
///
/// The right side lays out first, right to left, so the title knows how much
/// room is left to elide into.
fn review_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card: &CardView,
    record: Option<&ReviewView>,
    review: &mut ReviewUi,
    queue: Option<QueueHeader<'_>>,
) {
    let fields = record.map(|r| &r.fields);
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    ui.horizontal(|ui| {
        let peek_width = ui.available_width() * PEEK_SHARE;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(url) = fields.and_then(|f| f.explainer.as_deref()) {
                explainer_link_ui(ui, theme, url);
            }
            if let Some(queue) = queue {
                queue_header_ui(ui, theme, queue, (!narrow).then_some(peek_width));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let back =
                    egui::Button::new(egui::RichText::new("← Back").color(theme.text_secondary))
                        .fill(egui::Color32::TRANSPARENT)
                        .frame(false);
                if ui.add(back).clicked() {
                    review.close();
                }
                ui.label(egui::RichText::new("›").color(theme.text_muted));
                if !narrow {
                    card_ref_ui(ui, theme, &review.card_ref);
                }

                let session = fields.and_then(|f| f.agentium.as_deref());
                let chip_width = if narrow {
                    ui.text_style_height(&egui::TextStyle::Body)
                } else {
                    SESSION_CHIP_MAX_WIDTH
                };
                let reserve = session.map_or(0.0, |_| chip_width + ui.spacing().item_spacing.x);
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - reserve).max(0.0));
                    ui.add(egui::Label::new(egui::RichText::new(&card.title).strong()).truncate());
                });
                if let Some(session) = session {
                    session_chip_ui(ui, theme, app_ctx, session, chip_width);
                }
            });
        });
    });
}

/// The card's `headway:<board>/<word-id>`, small and muted; a click copies it.
/// A frameless button, not a Label, so a click copies rather than starting a
/// text selection (as the detail topbar's ref).
fn card_ref_ui(ui: &mut egui::Ui, theme: &ColorTheme, card_ref: &str) {
    let button = egui::Button::new(
        egui::RichText::new(card_ref)
            .color(theme.text_muted)
            .small(),
    )
    .frame(false);
    if ui.add(button).on_hover_text("Click to copy").clicked() {
        ui.ctx().copy_text(card_ref.to_owned());
    }
}

/// The queue's side of the header, laid out right to left: the next card's
/// title as a muted peek no wider than `peek_width` (none on a narrow screen),
/// then the position pill, then any notice.
fn queue_header_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    queue: QueueHeader<'_>,
    peek_width: Option<f32>,
) {
    if let Some((next, width)) = queue.next.zip(peek_width) {
        let muted = |text: &str| egui::RichText::new(text).small().color(theme.text_muted);
        ui.scope(|ui| {
            ui.set_max_width(width);
            ui.add(egui::Label::new(muted(next)).truncate());
        });
        ui.label(muted("Next:"));
    }
    text_pill(ui, theme, queue.position);
    super::notice_ui(ui, theme, queue.notice);
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

/// Share of the commit line a record's `host:path` may take before it's
/// elided in its middle.
const LOCATION_SHARE: f32 = 0.4;

/// A record's `host:path`, or whichever of the two it has. Built once per
/// record, for [`MiddleElided`].
fn host_path(fields: &ReviewFields) -> String {
    match (fields.host.as_deref(), fields.path.as_deref()) {
        (Some(host), Some(path)) => format!("{host}:{path}"),
        (Some(part), None) | (None, Some(part)) => part.to_string(),
        (None, None) => String::new(),
    }
}

/// The pane's commit line: the sha as a pill (click copies it) and the subject,
/// unless it's the card's title already in the header; then, on the right, where
/// the record was made — `location` (`host:path`, elided in its middle past
/// [`LOCATION_SHARE`] of the row, full on hover) and `⎇ branch`, small and muted.
/// A narrow screen leaves the location out so the subject keeps the row, as the
/// header leaves out the card ref.
///
/// The right side lays out first, right to left, so the subject knows how much
/// room is left to elide into.
fn record_summary_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    fields: &ReviewFields,
    card_title: &str,
    location: &mut MiddleElided,
) {
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        let location_width = ui.available_width() * LOCATION_SHARE;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !narrow {
                elided_location_ui(ui, theme, fields, location, location_width);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if let Some(sha) = fields.commit.as_deref()
                    && sha_pill(ui, theme, sha).on_hover_text(sha).clicked()
                {
                    ui.ctx().copy_text(sha.to_owned());
                }
                if let Some(subject) = fields.title.as_deref().filter(|t| *t != card_title) {
                    let subject = egui::RichText::new(subject).color(theme.text_primary);
                    ui.add(egui::Label::new(subject).truncate());
                }
            });
        });
    });
}

/// The commit line's right side, laid out right to left: `⎇ branch`, then
/// `location` elided to `location_width`, small and muted.
fn elided_location_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    fields: &ReviewFields,
    location: &mut MiddleElided,
    location_width: f32,
) {
    if let Some(branch) = fields.branch.as_deref() {
        let muted = |text: &str| egui::RichText::new(text).small().color(theme.text_muted);
        ui.label(muted(branch));
        ui.label(muted("⎇"));
    }
    if !location.is_empty() {
        location.ui(ui, theme.text_muted, location_width);
    }
}

/// A commit's short sha as an accent monospace pill, for the caller to give a
/// hover text and a click: the pane copies the sha, the detail's Review
/// section opens the pane on the record.
fn sha_pill(ui: &mut egui::Ui, theme: &ColorTheme, sha: &str) -> egui::Response {
    let pill = egui::Button::new(
        egui::RichText::new(short_sha(sha))
            .monospace()
            .color(theme.accent),
    )
    .fill(theme.surface_elevated)
    .stroke(egui::Stroke::NONE)
    .corner_radius(egui::CornerRadius::same(RADIUS_PILL as u8));
    ui.add(pill)
}

/// The load's status and, once it's in, the diff: a spinner while the worker
/// runs, git's own error (command and stderr, verbatim, so a broken ssh or path
/// can be fixed by hand) with a retry, or one line above the diff with who
/// wrote the commit and when, where it came from (the full sentence, with the
/// repo's path, on hover), and whether its patch was cut short.
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
                ui.spacing_mut().item_spacing.x = SPACING_XS;
                let muted = |text: &str| egui::RichText::new(text).small().color(theme.text_muted);
                ui.label(muted(&loaded.byline.short))
                    .on_hover_text(loaded.byline.full.as_str());
                ui.label(muted("·"));
                // A trailer match may not be the recorded commit, so its source
                // is the warning.
                let source_color = if loaded.by_trailer {
                    theme.warning
                } else {
                    theme.text_muted
                };
                ui.label(
                    egui::RichText::new(loaded.source.as_str())
                        .small()
                        .color(source_color),
                )
                .on_hover_text(loaded.source_hover.as_str());
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
                if loaded.commit.truncated {
                    tinted_pill(ui, theme, "patch truncated", theme.warning);
                }
            });
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
    explainer_link(ui, url, egui::RichText::new(EXPLAINER).color(theme.accent));
}

/// The explainer link's text.
const EXPLAINER: &str = "Explainer ↗";

/// A frameless link drawn as `text` that opens `url` in a new browser tab.
fn explainer_link(ui: &mut egui::Ui, url: &str, text: egui::RichText) {
    if ui
        .add(egui::Button::new(text).frame(false))
        .on_hover_text(url)
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

/// [`agentium_chip_ui`] no wider than `max_width`: a longer session title
/// ellipsizes, and its full text shows on hover.
fn session_chip_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    session: &str,
    max_width: f32,
) {
    ui.scope(|ui| {
        ui.set_max_width(max_width);
        agentium_chip_ui(ui, theme, app_ctx, session);
    });
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
        ui.add(
            egui::Label::new(
                egui::RichText::new(session)
                    .small()
                    .monospace()
                    .color(theme.text_muted),
            )
            .truncate(),
        );
    }
}

/// The least room a record's session chip is squeezed into before it moves to
/// the next line of the record's second line.
const SESSION_CHIP_MIN_WIDTH: f32 = 96.0;

/// How many records the detail's Review section lists before "Show all N".
const RECORDS_SHOWN: usize = 3;

/// A record's `host:path` in the detail's Review section, elided in its middle
/// to fit its row.
struct RecordLocation {
    /// The record it was built for.
    record: NoteId,
    text: MiddleElided,
}

/// The card detail's Review section's slice of [`BoardUiState`]: what its rows
/// draw that has to be built, built once per card and set of records rather
/// than each frame, and whether every record shows.
#[derive(Default)]
pub(crate) struct ReviewSection {
    /// The card the state below is for.
    card: Option<NoteId>,
    /// One per record, in the card's order (newest first).
    locations: Vec<RecordLocation>,
    /// "Show all N", for the card's N records.
    show_all_label: String,
    /// Whether every record shows rather than the newest [`RECORDS_SHOWN`].
    /// Starts off again for each card.
    show_all: bool,
}

impl ReviewSection {
    /// Rebuild for `card`'s `reviews` if the card or its records changed since
    /// the last frame; a new card also starts with the list folded.
    fn sync(&mut self, card: NoteId, reviews: &[ReviewView]) {
        let same_records = self.locations.len() == reviews.len()
            && self
                .locations
                .iter()
                .zip(reviews)
                .all(|(l, r)| l.record == r.id);
        if self.card == Some(card) && same_records {
            return;
        }
        if self.card != Some(card) {
            self.show_all = false;
        }
        self.card = Some(card);
        self.locations = reviews
            .iter()
            .map(|r| RecordLocation {
                record: r.id,
                text: MiddleElided::new(host_path(&r.fields)),
            })
            .collect();
        self.show_all_label = format!("Show all {}", reviews.len());
    }
}

/// The card detail's Review section: a heading with the record count, the
/// records newest first (at most [`RECORDS_SHOWN`] of them until "Show all N"),
/// and a "Review diff" button that opens the pane on the newest. A card with no
/// records but sitting in a terminal column still offers the pane, which then
/// looks the commit up by the card's `Headway:` trailer — how cards finished
/// before review records existed stay reviewable. The caller draws it only for
/// a card with records or in a terminal column.
pub(super) fn review_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card_id: NoteId,
    reviews: &[ReviewView],
    state: &mut BoardUiState,
) {
    let section = &mut state.review_section;
    section.sync(card_id, reviews);

    ui.horizontal(|ui| {
        detail_heading(ui, theme, "Review");
        if !reviews.is_empty() {
            count_badge(ui, theme, reviews.len());
        }
    });
    ui.add_space(SPACING_SM);

    // `Some(record)` once a row or the button was clicked; the inner `None`
    // (the button) opens on the newest record.
    let mut open: Option<Option<NoteId>> = None;
    let shown = if section.show_all {
        reviews.len()
    } else {
        RECORDS_SHOWN
    };
    let rows = reviews.iter().zip(&mut section.locations).take(shown);
    for (i, (r, location)) in rows.enumerate() {
        if i > 0 {
            ui.add_space(SPACING_SM);
        }
        if record_row_ui(ui, theme, app_ctx, &r.fields, &mut location.text) {
            open = Some(Some(r.id));
        }
    }
    if reviews.len() > RECORDS_SHOWN {
        ui.add_space(SPACING_XS);
        let label = if section.show_all {
            "Show fewer"
        } else {
            section.show_all_label.as_str()
        };
        let toggle = egui::RichText::new(label)
            .small()
            .color(theme.text_secondary);
        if ui.add(egui::Button::new(toggle).frame(false)).clicked() {
            section.show_all = !section.show_all;
        }
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

    ui.add_space(SPACING_MD);
    if secondary_action_button(ui, theme, "± Review diff").clicked() {
        open = Some(None);
    }
    if let Some(record) = open {
        state.review.open(card_id, record);
    }
}

/// One record in the detail's Review section, on two lines. First the sha pill
/// and the commit subject, elided to the row (in full on hover); then, indented
/// under the subject in small muted text, where it was made — `location`
/// (`host:path`, elided in its middle), `⎇ branch` — its agentium session chip
/// and its explainer link, `·` between those it has. Returns whether the sha or
/// the subject was clicked, which opens the pane on this record.
fn record_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    fields: &ReviewFields,
    location: &mut MiddleElided,
) -> bool {
    let mut clicked = false;
    let mut indent = 0.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_SM;
        let sha = fields.commit.as_deref().unwrap_or("(no commit)");
        let pill = sha_pill(ui, theme, sha).on_hover_text("Review this commit's diff");
        clicked |= pill.clicked();
        indent = pill.rect.width() + ui.spacing().item_spacing.x;
        if let Some(subject) = fields.title.as_deref() {
            let subject = egui::Label::new(egui::RichText::new(subject).color(theme.text_primary))
                .truncate()
                .sense(egui::Sense::click());
            clicked |= ui
                .add(subject)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked();
        }
    });

    ui.horizontal(|ui| {
        ui.add_space(indent);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = SPACING_XS;
            let muted = |text: &str| egui::RichText::new(text).small().color(theme.text_muted);
            // A `·` before every part but the first the record has.
            let mut first = true;
            let mut dot = |ui: &mut egui::Ui| {
                if !std::mem::take(&mut first) {
                    ui.label(muted("·"));
                }
            };
            if !location.is_empty() {
                dot(ui);
                let width = ui.available_width() * LOCATION_SHARE;
                location.ui(ui, theme.text_muted, width);
            }
            if let Some(branch) = fields.branch.as_deref() {
                dot(ui);
                ui.label(muted("⎇"));
                ui.label(muted(branch));
            }
            if let Some(session) = fields.agentium.as_deref() {
                dot(ui);
                // The chip's scope doesn't wrap by itself: it would overflow the
                // line and widen the whole column. So it takes the room left on
                // the line, or starts a new one when that's too little. (In a
                // wrapping layout `available_width` is a whole line's.)
                if ui.available_size_before_wrap().x < SESSION_CHIP_MIN_WIDTH {
                    ui.end_row();
                }
                let width = ui
                    .available_size_before_wrap()
                    .x
                    .min(SESSION_CHIP_MAX_WIDTH);
                session_chip_ui(ui, theme, app_ctx, session, width);
            }
            if let Some(url) = fields.explainer.as_deref() {
                dot(ui);
                let text = egui::RichText::new(EXPLAINER).small().color(theme.accent);
                explainer_link(ui, url, text);
            }
        });
    });
    clicked
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
