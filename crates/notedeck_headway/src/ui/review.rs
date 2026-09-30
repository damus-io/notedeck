//! The review pane — a card's review records and the commit diff they name,
//! full-pane like the dependency graph — the Review section of the card detail
//! that opens it, and the review queue that walks the pane over the board's In
//! Review column, or over one epic's In Review descendants.
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
use notedeck_ui::diff::PatchScroll;
use std::time::Instant;

use super::card_actions::CardStep;
use super::widgets::{
    ControlSize, MiddleElided, count_badge, detail_heading, secondary_action_button, section_label,
    text_pill, tinted_control, tinted_pill,
};
use super::{BoardEffect, BoardUiState, find_card, pane_hints_ui};
use crate::keys::CardAction;
use crate::nav::{NavPos, ReviewTarget};
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
    /// The record [`location`](Self::location) was built for.
    location_for: Option<NoteId>,
    /// The shown record's `host:path`, elided to the commit line's width.
    location: MiddleElided,
    /// The pane was opened (or moved to another card) since it last drew, so
    /// its trailer search is re-run (see [`ReviewLoader::expire`]).
    reopened: bool,
    /// How wide the header's "Review in session" button drew last frame,
    /// reserved beside the session chip so the title elides short of it. Zero
    /// until it first draws, when [`SESSION_BUTTON_WIDTH_GUESS`] stands in.
    session_button_width: f32,
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

/// The ids of `epic`'s In Review descendants, at any depth, in the epic's
/// [`work_order`](headway::traversal::work_order): the order autowork walks
/// them, so an epic's queue replays its chain card by card. Archived cards,
/// and whatever hangs off them, are left out, as `work_order` leaves them out.
/// Empty when the board has no In Review column.
pub(crate) fn epic_review_cards(view: &BoardView, epic: NoteId) -> Vec<NoteId> {
    let Some(col) = IN_REVIEW.index(view) else {
        return Vec::new();
    };
    let in_review = &view.columns[col].cards;
    let container = headway::event::Container::Card(*epic.bytes());
    headway::traversal::work_order(view, &container)
        .into_iter()
        .filter(|c| c.id != epic && in_review.iter().any(|r| r.id == c.id))
        .map(|c| c.id)
        .collect()
}

/// How many of `epic`'s descendants sit in In Review: the length of
/// [`epic_review_cards`], counted without allocating, for the detail's
/// "Review N" button, which draws every frame. Walks up from each In Review
/// card rather than down from the epic, through parents that are live on the
/// board (so an archived link cuts the chain, as it does in `work_order`),
/// giving up after [`MAX_EPIC_DEPTH`] steps so a parent cycle can't spin.
pub(crate) fn epic_review_count(view: &BoardView, epic: NoteId) -> usize {
    let Some(col) = IN_REVIEW.index(view) else {
        return 0;
    };
    view.columns[col]
        .cards
        .iter()
        .filter(|card| descends_from(view, card.parent, epic))
        .count()
}

/// How deep [`epic_review_count`] climbs looking for the epic.
const MAX_EPIC_DEPTH: usize = 64;

/// Whether the chain of live parents starting at `parent` reaches `epic`.
fn descends_from(view: &BoardView, mut parent: Option<NoteId>, epic: NoteId) -> bool {
    for _ in 0..MAX_EPIC_DEPTH {
        let Some(id) = parent else {
            return false;
        };
        if id == epic {
            return true;
        }
        parent = view.card(id).and_then(|c| c.parent);
    }
    false
}

/// The label of the detail's "Review N" button (the Sub-issues header's twin
/// of `R`), formatted only when N changes. That saves the formatting, not the
/// allocation: egui's `RichText::new` still copies the text into a `String`
/// every frame.
#[derive(Default)]
pub(crate) struct SubtreeReviewLabel {
    /// The N [`text`](Self::text) was formatted for.
    count: Option<usize>,
    /// `"Review N"`.
    text: String,
}

impl SubtreeReviewLabel {
    /// `"Review {count}"`, reformatted only when `count` moved.
    pub(crate) fn text(&mut self, count: usize) -> &str {
        use std::fmt::Write;
        if self.count != Some(count) {
            self.count = Some(count);
            self.text.clear();
            let _ = write!(self.text, "Review {count}");
        }
        &self.text
    }
}

/// What the review queue walks: the board's In Review column (`R` on the
/// grid), or one epic's In Review descendants (`R` on its detail).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum QueueScope {
    /// The whole In Review column, in column order.
    #[default]
    Board,
    /// The In Review cards under this card, in its work-order.
    Epic(NoteId),
}

impl QueueScope {
    /// The scope a [`ReviewQueue`](crate::HeadwayRoute::ReviewQueue) route's
    /// `epic` names.
    pub(crate) fn of(epic: Option<NoteId>) -> Self {
        epic.map_or(QueueScope::Board, QueueScope::Epic)
    }

    /// The epic, for an epic's queue.
    pub(crate) fn epic(self) -> Option<NoteId> {
        match self {
            QueueScope::Board => None,
            QueueScope::Epic(epic) => Some(epic),
        }
    }

    /// The queue's snapshot for this scope, taken now.
    pub(crate) fn snapshot(self, view: &BoardView) -> Vec<NoteId> {
        match self {
            QueueScope::Board => in_review_cards(view),
            QueueScope::Epic(epic) => epic_review_cards(view, epic),
        }
    }

    /// What the header says when this scope has nothing in review: an epic's
    /// empty queue says so of the epic, not the board.
    pub(crate) fn empty_notice(self) -> QueueNotice {
        match self {
            QueueScope::Board => QueueNotice::NothingInReview,
            QueueScope::Epic(_) => QueueNotice::NothingInReviewUnder,
        }
    }
}

/// A short-lived message in the board header or the queue bar, for a key that
/// had nothing to act on. Shown for [`NOTICE_SECS`] seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueNotice {
    /// `R` found the In Review column empty.
    NothingInReview,
    /// `R` on a card's detail found none of its descendants in review.
    NothingInReviewUnder,
    /// A verdict on the queue's last card closed it.
    QueueDone,
    /// `e` on a card whose record has no explainer.
    NoExplainer,
    /// `D` on a board with no Done column.
    NoDoneColumn,
    /// `X` on a board with no In Progress column.
    NoInProgressColumn,
    /// `s`/`S` on a card whose record names no agentium session.
    NoSession,
    /// A card action on the queue's card after it left the board.
    CardGone,
}

/// A [`QueueNotice`] that's up: when it went up (egui time) and in which view.
/// It's about that view, so it only draws there, and it comes down once a
/// pass has gone by without that view (see
/// [`BoardUiState::retire_stale_notice`]) rather than following the user two
/// screens away from its cause.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Notice {
    /// The message.
    pub(crate) what: QueueNotice,
    /// When it went up, in egui time; it shows for [`NOTICE_SECS`] from here.
    pub(crate) at: f64,
    /// The view it went up in.
    pub(crate) pos: NavPos,
    /// The last egui pass that drew its view, or `None` until the first
    /// [`retire_stale_notice`](BoardUiState::retire_stale_notice) after it
    /// went up, which counts the pass it went up in.
    pub(crate) seen: Option<u64>,
}

impl QueueNotice {
    /// The message, as drawn.
    pub(crate) fn text(self) -> &'static str {
        match self {
            QueueNotice::NothingInReview => "Nothing in review",
            QueueNotice::NothingInReviewUnder => "Nothing in review under this card",
            QueueNotice::QueueDone => "Review queue done",
            QueueNotice::NoExplainer => "No explainer on this record",
            QueueNotice::NoDoneColumn => "No Done column on this board",
            QueueNotice::NoInProgressColumn => "No In Progress column on this board",
            QueueNotice::NoSession => "No agentium session on this record",
            QueueNotice::CardGone => "This card has left the board",
        }
    }
}

/// What `s`/`S` (or the header's "Review in session" button) ask of a
/// record's agentium session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionOpen {
    /// `s`: just open the session in Dave.
    Plain,
    /// `S`: open it and ask it to review its own work ([`CODE_REVIEW_PROMPT`]).
    CodeReview,
}

/// The message `S` sends into a record's session. [`session_open`] appends the
/// record's commit and card as ` (commit <short sha>, card <card ref>)`, so
/// the session knows which of its commits is meant.
pub(crate) const CODE_REVIEW_PROMPT: &str =
    "launch a /code-review for the work done in this session";

/// The [`AppAction::Open`](notedeck::AppAction::Open) request that opens
/// `fields`' agentium session `how` asks, or `None` when the record names no
/// session. `card_ref` is the record's card, as `headway:<board>/<word-id>`.
/// Allocates, so it's built on the key press or click, never per frame.
pub(crate) fn session_open(
    fields: &ReviewFields,
    card_ref: &str,
    how: SessionOpen,
) -> Option<notedeck::OpenUri> {
    let session = fields.agentium.as_deref()?;
    let msg = match how {
        SessionOpen::Plain => None,
        SessionOpen::CodeReview => Some(match fields.commit.as_deref() {
            Some(sha) => format!(
                "{CODE_REVIEW_PROMPT} (commit {}, card {card_ref})",
                short_sha(sha)
            ),
            None => format!("{CODE_REVIEW_PROMPT} (card {card_ref})"),
        }),
    };
    Some(notedeck::OpenUri {
        reference: session.to_owned(),
        msg,
    })
}

/// The review queue: a snapshot of the In Review cards its [`QueueScope`]
/// covers, taken when `R` opens it, and which of them the review pane shows.
///
/// A snapshot so that a verdict moving a card out of In Review doesn't
/// reshuffle what's left under the reviewer. It outlives the queue closing, so
/// a back/forward onto the queue's history entry reopens it where it was left;
/// the next `R` takes a fresh one.
#[derive(Default)]
pub(crate) struct ReviewQueue {
    /// What the snapshot covers: the board, or one epic's subtree.
    scope: QueueScope,
    /// The scope's In Review cards when the queue opened, in column order
    /// (the board) or work-order (an epic).
    cards: Vec<NoteId>,
    /// The position in [`cards`](Self::cards) the pane shows.
    index: usize,
    /// Whether the queue is showing. Seeded from the nav route like the
    /// graph's epic, so the nav stack decides.
    open: bool,
    /// `"3 / 12"`, formatted when the position changes rather than every frame.
    position: String,
    /// For an epic's queue, `"in <word-id>"`, and the epic's title for its
    /// hover: formatted when the queue opens.
    scope_label: Option<ScopeLabel>,
}

/// The header's name for an epic's queue, formatted once when it opens.
pub(crate) struct ScopeLabel {
    /// `"in <word-id>"`.
    pub(crate) text: String,
    /// The epic's title, shown on hover.
    pub(crate) title: String,
}

impl ReviewQueue {
    /// Open the queue over `scope`'s snapshot `cards` at its first card.
    /// Returns `false`, leaving the queue closed and its previous snapshot
    /// alone, when there are none. The header's name for an epic's scope is
    /// formatted here from `view`.
    pub(crate) fn start(
        &mut self,
        view: &BoardView,
        scope: QueueScope,
        cards: Vec<NoteId>,
    ) -> bool {
        if cards.is_empty() {
            return false;
        }
        self.scope = scope;
        self.scope_label = scope.epic().map(|epic| ScopeLabel {
            text: format!("in {}", headway::wordid::encode(epic.bytes())),
            title: find_card(view, epic).map_or_else(String::new, |(_, c)| c.title.clone()),
        });
        self.cards = cards;
        self.open = true;
        self.go_to(0);
        true
    }

    /// What the queue walks.
    pub(crate) fn scope(&self) -> QueueScope {
        self.scope
    }

    /// The header's name for an epic's queue; `None` for the board's.
    pub(crate) fn scope_label(&self) -> Option<&ScopeLabel> {
        self.scope_label.as_ref()
    }

    /// Whether the queue is showing.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Seed whether the queue shows, and over what, from the nav route. The
    /// same scope as the snapshot reopens it where it was left. Another scope
    /// (back/forward from an epic's queue onto the board's, or the reverse)
    /// drops the snapshot and stays open without one, for
    /// [`needs_snapshot`](Self::needs_snapshot) to retake it against the
    /// board this frame.
    pub(crate) fn set_open(&mut self, scope: Option<QueueScope>) {
        let Some(scope) = scope else {
            self.open = false;
            return;
        };
        if scope != self.scope {
            self.scope = scope;
            self.scope_label = None;
            self.cards.clear();
            self.index = 0;
        }
        self.open = true;
    }

    /// Whether the queue is open without a snapshot, because
    /// [`set_open`](Self::set_open) was handed a scope other than its own.
    pub(crate) fn needs_snapshot(&self) -> bool {
        self.open && self.cards.is_empty()
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
    pub(crate) fn step(&mut self, step: CardStep) {
        let index = match step {
            CardStep::Next if self.index + 1 < self.cards.len() => self.index + 1,
            CardStep::Prev if self.index > 0 => self.index - 1,
            CardStep::Next | CardStep::Prev => return,
        };
        self.go_to(index);
    }

    /// Whether the current card is the snapshot's last.
    pub(crate) fn at_end(&self) -> bool {
        self.index + 1 >= self.cards.len()
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

/// Draw the review queue: the which-key strip while it's pinned or a `g` is
/// pending, and the review pane for the queue's current card in the rest (its
/// header carries where the pane is in the queue and a peek at the next card,
/// see [`QueueHeader`]). Starts the next card's load too, so stepping to it is
/// instant. The queue's keys ([`crate::keys::review_pane_keys`]) have already
/// run this frame, and any `X` composer is drawn above.
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

    pane_hints_ui(ui, theme, state);

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
        state.close_queue(view);
    }
}

/// What the review keys ask of the open diff.
impl BoardUiState {
    /// Ask the open diff for `request`.
    pub(crate) fn scroll_review(&mut self, request: PatchScroll) {
        self.review.scroll(request);
    }

    /// The scroll the review keys left for the diff's next pass.
    #[cfg(test)]
    pub(crate) fn review_scroll(&self) -> Option<PatchScroll> {
        self.review.pending_scroll()
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
/// session; the queue's position, its "in <word-id>" epic label when it walks
/// an epic, and its next card when the pane is the queue's; explainer), the
/// record picker when there are several, the resolve status, and the commit's
/// diff filling the rest. ← Back closes it, as `q`/`Esc` do
/// ([`crate::keys::review_pane_keys`]).
pub(super) fn review_pane_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    view: &BoardView,
    card: &CardView,
    state: &mut BoardUiState,
) {
    let queue = (state.queue.current() == Some(card.id)).then(|| QueueHeader {
        position: state.queue.position(),
        scope: state.queue.scope_label(),
        next: state
            .queue
            .next_card()
            .and_then(|c| find_card(view, c))
            .map(|(_, c)| c.title.as_str()),
    });
    let here = state.nav_pos();
    let notice = &mut state.notice;
    let effects = &mut state.effects;
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
            let header = ReviewHeader {
                card,
                record,
                queue,
            };
            if let Some(open) = review_topbar_ui(ui, theme, app_ctx, header, review, notice, here) {
                effects.push(BoardEffect::Open(open));
            }
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

/// Room the header keeps for its "Review in session" button before the button
/// has drawn once and measured itself (see [`ReviewUi::session_button_width`]),
/// so the title doesn't run under it on the pane's first frame. About the
/// label's width at the default body size.
const SESSION_BUTTON_WIDTH_GUESS: f32 = 120.0;

/// What the review header draws: the card, the shown record, and the queue's
/// part while the pane is the queue's. All borrowed, so the header formats
/// nothing.
struct ReviewHeader<'a> {
    /// The card under review.
    card: &'a CardView,
    /// The record the pane shows, if the card has any.
    record: Option<&'a ReviewView>,
    /// The queue's position and next card, while the pane shows the queue's
    /// current card.
    queue: Option<QueueHeader<'a>>,
}

/// The queue's part of the review header, while the pane shows the queue's
/// current card. Everything is borrowed, so the header formats nothing.
struct QueueHeader<'a> {
    /// `"3 / 12"`, cached on the [`ReviewQueue`].
    position: &'a str,
    /// An epic's queue's `"in <word-id>"`, cached on the [`ReviewQueue`].
    scope: Option<&'a ScopeLabel>,
    /// The next card's title, if the queue has one.
    next: Option<&'a str>,
}

/// The pane's header, one row. Left: ← Back, the card ref (click copies), the
/// title elided to one line, the record's agentium session chip capped at
/// [`SESSION_CHIP_MAX_WIDTH`], and a "Review in session" button that does
/// `S`. Right: in the queue, which epic it walks (if it's an epic's), its
/// position as a pill and the next card's title as a muted peek (at most
/// [`PEEK_SHARE`] of the row); a key's short-lived
/// notice, when it's about `here`; the record's explainer link at the far end. On a narrow screen the
/// peek goes, the card ref and the button with it, and the chip shrinks to its
/// status dot.
///
/// The right side lays out first, right to left, so the title knows how much
/// room is left to elide into.
///
/// Returns the session open the button asked for. The pane raises it as a
/// [`BoardEffect::Open`], the one way out an `S` takes too.
fn review_topbar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    header: ReviewHeader<'_>,
    review: &mut ReviewUi,
    notice: &mut Option<Notice>,
    here: NavPos,
) -> Option<notedeck::OpenUri> {
    let ReviewHeader {
        card,
        record,
        queue,
    } = header;
    let fields = record.map(|r| &r.fields);
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut open = None;
    ui.horizontal(|ui| {
        let peek_width = ui.available_width() * PEEK_SHARE;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(url) = fields.and_then(|f| f.explainer.as_deref()) {
                explainer_link_ui(ui, theme, url);
            }
            if let Some(queue) = queue {
                queue_header_ui(ui, theme, queue, (!narrow).then_some(peek_width));
            }
            super::notice_ui(ui, theme, notice, here);
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
                let gap = ui.spacing().item_spacing.x;
                let button = if narrow {
                    0.0
                } else if review.session_button_width > 0.0 {
                    review.session_button_width + gap
                } else {
                    SESSION_BUTTON_WIDTH_GUESS + gap
                };
                let reserve = session.map_or(0.0, |_| chip_width + gap + button);
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - reserve).max(0.0));
                    ui.add(egui::Label::new(egui::RichText::new(&card.title).strong()).truncate());
                });
                let Some(session) = session else {
                    return;
                };
                session_chip_ui(ui, theme, app_ctx, session, chip_width);
                if narrow {
                    return;
                }
                if review_in_session_button(ui, theme, &mut review.session_button_width) {
                    open = fields
                        .and_then(|f| session_open(f, &review.card_ref, SessionOpen::CodeReview));
                }
            });
        });
    });
    open
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
/// the position pill, and, in an epic's queue, which epic (`in <word-id>`,
/// its title on hover).
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
    if let Some(scope) = queue.scope {
        ui.label(
            egui::RichText::new(&scope.text)
                .small()
                .color(theme.text_muted),
        )
        .on_hover_text(&scope.title);
    }
}

/// The header's "Review in session" button (the `S` key): opens the record's
/// agentium session asking it for a `/code-review` of its work. Records its
/// drawn width in `width`, which the header reserves next frame so the title
/// elides short of it. Returns whether it was clicked.
fn review_in_session_button(ui: &mut egui::Ui, theme: &ColorTheme, width: &mut f32) -> bool {
    let text = egui::RichText::new("Review in session").color(theme.accent);
    let response = ui
        .add(egui::Button::new(text).frame(false))
        .on_hover_text("Open the session and ask it to /code-review this commit (S)");
    *width = response.rect.width();
    response.clicked()
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
    tinted_control(
        ui,
        egui::RichText::new(short_sha(sha)).monospace(),
        theme.accent,
        ControlSize::Pill,
        egui::Sense::click(),
    )
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
                    tinted_pill(ui, "patch truncated", theme.warning);
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

/// The review pane's frameless "Explainer ↗" link that opens `url` in a new
/// browser tab.
fn explainer_link_ui(ui: &mut egui::Ui, theme: &ColorTheme, url: &str) {
    let text = egui::RichText::new(EXPLAINER).color(theme.accent);
    if ui
        .add(egui::Button::new(text).frame(false))
        .on_hover_text(url)
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

/// The explainer link's text.
const EXPLAINER: &str = "Explainer ↗";

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
/// registered reference parser (Dave's), or as small plain monospace text when
/// no parser resolves it (Dave isn't loaded, or the session is unknown here).
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
                // `family`, not `.monospace()`: that sets the text style too
                // and would undo `.small()`, drawing the chip at body size.
                egui::RichText::new(session)
                    .small()
                    .family(egui::FontFamily::Monospace)
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
    /// The card's record count, for the sidebar block's heading pill.
    count_label: String,
    /// "All N records ›", the sidebar block's line into the pane.
    records_label: String,
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
        self.count_label = reviews.len().to_string();
        self.records_label = format!("All {} records ›", reviews.len());
    }
}

/// The card detail's Review section: a heading with the record count, the
/// records newest first (at most [`RECORDS_SHOWN`] of them until "Show all N"),
/// and a "Review diff" button that opens the pane on the newest. A card with no
/// records but sitting in a terminal column still offers the pane, which then
/// looks the commit up by the card's `Headway:` trailer — how cards finished
/// before review records existed stay reviewable. The caller draws it only for
/// a card with records or in a terminal column.
///
/// A click comes back as a [`CardAction`] naming the record it was on, for
/// the detail to apply through [`crate::keys::apply_card_action`] as the
/// sidebar's clicks and the keys are: a row's sha or subject is
/// `Review(Some(record))`, its explainer `Explainer(Some(record))`, and the
/// button `r`'s own `Review(None)`.
pub(super) fn review_section_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card_id: NoteId,
    reviews: &[ReviewView],
    section: &mut ReviewSection,
) -> Option<CardAction> {
    section.sync(card_id, reviews);

    ui.horizontal(|ui| {
        detail_heading(ui, theme, "Review");
        if !reviews.is_empty() {
            count_badge(ui, theme, reviews.len());
        }
    });
    ui.add_space(SPACING_SM);

    let mut picked = None;
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
        // Only the newest row's parts are what `r` and `e` act on here, so
        // only its hovers name them.
        let keyed = i == 0;
        if let Some(action) = record_row_ui(ui, theme, app_ctx, r, &mut location.text, keyed) {
            picked = Some(action);
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
    if secondary_action_button(ui, theme, "± Review diff")
        .on_hover_text("Review the newest commit's diff (r)")
        .clicked()
    {
        picked = Some(CardAction::Review(None));
    }
    picked
}

/// One record in the detail's Review section, on two lines. First the sha pill
/// and the commit subject, elided to the row (in full on hover); then, indented
/// under the subject in small muted text, where it was made — `location`
/// (`host:path`, elided in its middle), `⎇ branch` — its agentium session chip
/// and its explainer link, `·` between those it has. A click on the sha or the
/// subject is [`CardAction::Review`] of this record, on the explainer
/// [`CardAction::Explainer`] of it. `keyed` (the newest row) has the sha and
/// explainer hovers name their keys.
fn record_row_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    record: &ReviewView,
    location: &mut MiddleElided,
    keyed: bool,
) -> Option<CardAction> {
    let fields = &record.fields;
    let mut clicked = false;
    let mut picked = None;
    let mut indent = 0.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_SM;
        let pill = record_sha_ui(ui, theme, fields, keyed);
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
                if record_explainer_ui(ui, theme, url, keyed).clicked() {
                    picked = Some(CardAction::Explainer(Some(record.id)));
                }
            }
        });
    });
    if clicked {
        picked = Some(CardAction::Review(Some(record.id)));
    }
    picked
}

/// A record's sha pill, the mouse twin of `r` on it, shared by the detail's
/// Review section rows and its sidebar block. `keyed` has its hover name the
/// key, for the record `r` acts on.
fn record_sha_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    fields: &ReviewFields,
    keyed: bool,
) -> egui::Response {
    let sha = fields.commit.as_deref().unwrap_or("(no commit)");
    let hover = if keyed {
        "Review this commit's diff (r)"
    } else {
        "Review this commit's diff"
    };
    sha_pill(ui, theme, sha).on_hover_text(hover)
}

/// A record's small accent "Explainer ↗" link, the mouse twin of `e` on it,
/// shared by the detail's Review section rows and its sidebar block. Its hover
/// names `e` when `keyed` (the record `e` acts on), then the url.
fn record_explainer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    url: &str,
    keyed: bool,
) -> egui::Response {
    let hover = if keyed {
        "Open the explainer (e)"
    } else {
        "Open the explainer"
    };
    sidebar_link(ui, theme, EXPLAINER).on_hover_ui(|ui| {
        ui.label(hover);
        ui.label(egui::RichText::new(url).small().color(theme.text_muted));
    })
}

/// The card detail sidebar's Review block: the newest record's sha pill and
/// subject, its agentium session chip, its explainer, and, with several
/// records, an "All N records ›" line into the pane. On a wide pane the sidebar stays put beside the scrolling thread, so
/// the review is a click away however far down the comments someone has read.
///
/// Each affordance is the mouse twin of a detail key, and its hover names the
/// key: the sha and the records line are `r`, the explainer is `e`. (The
/// session chip opens its session itself; `s`/`S` stay keys only.) A click comes back as that key's
/// [`CardAction`] for the detail to apply through
/// [`crate::keys::apply_card_action`], the path the keys take, so a click and
/// its key can't drift apart. Draws nothing for a card with no records.
pub(super) fn review_sidebar_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    card_id: NoteId,
    reviews: &[ReviewView],
    section: &mut ReviewSection,
) -> Option<CardAction> {
    let newest = reviews.first()?;
    let fields = &newest.fields;
    section.sync(card_id, reviews);
    let mut picked = None;

    ui.horizontal(|ui| {
        section_label(ui, theme, "Review");
        if reviews.len() > 1 {
            text_pill(ui, theme, &section.count_label);
        }
    });
    ui.add_space(SPACING_XS);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_SM;
        if record_sha_ui(ui, theme, fields, true).clicked() {
            picked = Some(CardAction::Review(None));
        }
        // A truncated label shows its full text on hover by itself.
        if let Some(subject) = fields.title.as_deref() {
            let subject = egui::RichText::new(subject)
                .small()
                .color(theme.text_secondary);
            ui.add(egui::Label::new(subject).truncate());
        }
    });

    let session = fields.agentium.as_deref();
    if let Some(session) = session {
        ui.add_space(SPACING_XS);
        let width = ui.available_width().min(SESSION_CHIP_MAX_WIDTH);
        session_chip_ui(ui, theme, app_ctx, session, width);
    }

    if let Some(url) = fields.explainer.as_deref() {
        ui.add_space(SPACING_XS);
        if record_explainer_ui(ui, theme, url, true).clicked() {
            picked = Some(CardAction::Explainer(None));
        }
    }

    if reviews.len() > 1 {
        ui.add_space(SPACING_XS);
        let text = egui::RichText::new(&section.records_label)
            .small()
            .color(theme.text_secondary);
        if ui
            .add(egui::Button::new(text).frame(false))
            .on_hover_text("Open the review pane (r)")
            .clicked()
        {
            picked = Some(CardAction::Review(None));
        }
    }
    picked
}

/// A small frameless accent link in the sidebar's Review block.
fn sidebar_link(ui: &mut egui::Ui, theme: &ColorTheme, text: &str) -> egui::Response {
    let text = egui::RichText::new(text).small().color(theme.accent);
    ui.add(egui::Button::new(text).frame(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::link;
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
        let view = board(vec![]);
        let mut queue = ReviewQueue::default();
        assert!(!queue.start(&view, QueueScope::Board, vec![]));
        assert!(!queue.is_open());

        assert!(queue.start(&view, QueueScope::Board, ids.clone()));
        assert_eq!((queue.current(), queue.position()), (Some(ids[0]), "1 / 3"));
        assert_eq!(queue.next_card(), Some(ids[1]));
        queue.step(CardStep::Prev);
        assert_eq!(queue.current(), Some(ids[0]), "no wrap back");

        queue.step(CardStep::Next);
        queue.step(CardStep::Next);
        assert_eq!((queue.current(), queue.position()), (Some(ids[2]), "3 / 3"));
        assert_eq!(queue.next_card(), None);
        queue.step(CardStep::Next);
        assert_eq!(queue.current(), Some(ids[2]), "no wrap forward");
        queue.step(CardStep::Prev);
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
        queue.set_open(Some(QueueScope::Board));
        assert_eq!(queue.current(), Some(ids[1]));

        // A failed start leaves the last snapshot alone.
        queue.close();
        assert!(!queue.start(&view, QueueScope::Board, vec![]));
        queue.set_open(Some(QueueScope::Board));
        assert_eq!(queue.current(), Some(ids[1]));
    }

    /// A route asking for the queue over a scope it holds no snapshot of
    /// (none yet, or another scope's) opens it empty, asking for one to be
    /// taken against the board; its own scope reopens where it was.
    #[test]
    fn queue_route_of_another_scope_asks_for_a_snapshot() {
        let epic = NoteId::new([9; 32]);
        let ids: Vec<NoteId> = (1..=2).map(|i| NoteId::new([i; 32])).collect();
        let view = board(vec![]);
        let mut queue = ReviewQueue::default();
        queue.set_open(Some(QueueScope::Board));
        assert!(queue.needs_snapshot());
        assert_eq!(queue.current(), None);

        assert!(queue.start(&view, QueueScope::Epic(epic), ids.clone()));
        queue.step(CardStep::Next);
        queue.close();
        queue.set_open(Some(QueueScope::Epic(epic)));
        assert!(!queue.needs_snapshot());
        assert_eq!(queue.current(), Some(ids[1]));

        queue.set_open(Some(QueueScope::Board));
        assert!(queue.needs_snapshot());
        assert_eq!(queue.scope(), QueueScope::Board);
        assert!(queue.scope_label().is_none());
        queue.set_open(None);
        assert!(!queue.is_open());
    }

    /// An epic's queue walks its In Review descendants at every depth, in the
    /// epic's work-order rather than column order, and leaves out the rest:
    /// In Review cards outside the epic, and a card under an archived link
    /// (off the board, so `work_order` doesn't descend into it). The
    /// allocation-free count agrees.
    #[test]
    fn epic_review_cards_walk_the_subtree_in_work_order() {
        let id = |i: u8| NoteId::new([i; 32]);
        let (epic, sub_epic, a, b, done, outside, archived, orphan) =
            (id(1), id(2), id(3), id(4), id(5), id(6), id(7), id(8));
        let mut view = board(vec![
            column("todo", "Todo", &[epic, sub_epic]),
            column("in-review", "In Review", &[outside, a, b, orphan]),
            column("done", "Done", &[done]),
        ]);
        // epic ─┬─ sub_epic ─┬─ b        (in review)
        //       │            └─ done
        //       ├─ a                     (in review)
        //       └─ archived ── orphan    (in review, under an archived link)
        link(&mut view, epic, sub_epic);
        link(&mut view, epic, a);
        link(&mut view, sub_epic, b);
        link(&mut view, sub_epic, done);
        link(&mut view, epic, archived);
        link(&mut view, archived, orphan);
        view.columns[1].cards[3].parent = Some(archived);

        assert_eq!(epic_review_cards(&view, epic), vec![b, a]);
        assert_eq!(epic_review_count(&view, epic), 2);
        assert_eq!(epic_review_cards(&view, sub_epic), vec![b]);
        assert_eq!(epic_review_count(&view, sub_epic), 1);
        assert!(epic_review_cards(&view, a).is_empty());
        assert_eq!(epic_review_count(&view, a), 0);

        let no_column = board(vec![column("todo", "Todo", &[epic, a])]);
        assert!(epic_review_cards(&no_column, epic).is_empty());
        assert_eq!(epic_review_count(&no_column, epic), 0);
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
