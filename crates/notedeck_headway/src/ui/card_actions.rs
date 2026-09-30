//! What the shared card-action keys ([`crate::keys::CardAction`]) do to a
//! card, whichever view pressed them: open its explainer or agentium session,
//! open its review, move it to Done, send it back with a reason, archive it,
//! step to its neighbour. [`crate::keys::apply_card_action`] picks the method
//! for the key and the view; these are the steps it takes. Also the `X`
//! composer the send-back asks its reason in, drawn over any view.

use headway::event::{BoardView, CardView, ReviewView};
use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::SPACING_LG;
use notedeck_ui::diff::PatchScroll;

use super::review::{DONE, IN_PROGRESS, QueueNotice, SessionOpen, session_open};
use super::{BoardUiState, find_card};
use crate::store::BoardAction;

/// Which way `n`/`p` step from the current card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardStep {
    /// To the next card (`n`).
    Next,
    /// To the previous card (`p`).
    Prev,
}

/// The `X` composer: the one-line reason a send-back posts as a `review:`
/// comment, and the card it sends back.
pub(crate) struct ReasonComposer {
    /// The card being sent back, fixed when `X` opened the composer.
    card: NoteId,
    text: String,
    /// Grab focus on the composer's next layout (the frame after `X`).
    focus: bool,
}

/// How many body-text lines a detail `j`/`k` scrolls: a mouse-wheel notch's
/// worth, since a single line crawls through a long thread.
const DETAIL_LINES_PER_KEY: f32 = 3.0;

impl BoardUiState {
    /// Put up `notice` for [`super::NOTICE_SECS`] from `now` (egui time).
    pub(crate) fn set_notice(&mut self, notice: QueueNotice, now: f64) {
        self.notice = Some((notice, now));
    }

    /// The notice showing, if any.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<QueueNotice> {
        self.notice.map(|(n, _)| n)
    }

    /// The record of `card` a key acts on: the one the review pane shows, if
    /// it's open on the card, else the newest.
    fn acted_record<'a>(&self, card: &'a CardView) -> Option<&'a ReviewView> {
        if self.review.card() == Some(card.id) {
            self.review.shown_record(card)
        } else {
            card.reviews.first()
        }
    }

    /// `e`: open the explainer of `card`'s record ([`acted_record`]) in a
    /// browser tab, or say there's none.
    ///
    /// [`acted_record`]: Self::acted_record
    pub(crate) fn open_explainer(&mut self, ctx: &egui::Context, view: &BoardView, card: NoteId) {
        let url = find_card(view, card)
            .and_then(|(_, card)| self.acted_record(card))
            .and_then(|r| r.fields.explainer.as_deref());
        match url {
            Some(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            None => self.set_notice(QueueNotice::NoExplainer, ctx.input(|i| i.time)),
        }
    }

    /// `s`/`S`: ask the app to open the agentium session of `card`'s record
    /// ([`acted_record`]) as `how` says; the request waits in
    /// [`take_open`](Self::take_open) for [`super::board_ui`] to raise. A
    /// record with no session only says so.
    ///
    /// [`acted_record`]: Self::acted_record
    pub(crate) fn open_card_session(
        &mut self,
        view: &BoardView,
        card: NoteId,
        how: SessionOpen,
        now: f64,
    ) {
        let Some((_, card)) = find_card(view, card) else {
            return;
        };
        let card_ref = headway::wordid::card_ref(&view.id, card.id.bytes());
        let open = self
            .acted_record(card)
            .and_then(|r| session_open(&r.fields, &card_ref, how));
        match open {
            Some(open) => self.open = Some(open),
            None => self.set_notice(QueueNotice::NoSession, now),
        }
    }

    /// Take the session open a key asked for this frame, if any, for the app
    /// to raise as an [`AppAction::Open`](notedeck::AppAction::Open). Clears
    /// it so it fires once.
    pub(crate) fn take_open(&mut self) -> Option<notedeck::OpenUri> {
        self.open.take()
    }

    /// `r`: open `card`'s review pane on its newest record, over its detail
    /// (which a back off the pane returns to when the pane was opened from
    /// it).
    pub(crate) fn open_review(&mut self, card: NoteId) {
        self.review.open(card, None);
        self.selected = Some(card);
    }

    /// Leave a plain review pane for `card`'s detail (its `q`/`Esc`/`Enter`).
    pub(crate) fn back_to_detail(&mut self, card: NoteId) {
        self.review.close();
        self.selected = Some(card);
    }

    /// Leave the detail (and any review pane over it) for the grid: the
    /// detail's `q`, or what archiving its card does.
    pub(crate) fn leave_card(&mut self) {
        self.review.close();
        self.selected = None;
        self.detail_for = None;
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

    /// The queue's current card, while it's open.
    pub(crate) fn queue_card(&self) -> Option<NoteId> {
        self.queue.current()
    }

    /// `n`/`p` in a plain review pane: open the review of the card next to
    /// `card` in its column, on its newest record. The cursor follows, so
    /// backing out to the grid lands on it. Stops at the column's ends.
    pub(crate) fn step_review(&mut self, view: &BoardView, card: NoteId, step: CardStep) {
        let Some(next) = column_neighbour(view, card, step) else {
            return;
        };
        self.open_review(next);
        self.set_cursor(next);
    }

    /// `n`/`p` in the detail: open the card next to `card` in its column. The
    /// app's nav diff pushes it as a card→card drill, so back walks the steps.
    pub(crate) fn step_detail(&mut self, view: &BoardView, card: NoteId, step: CardStep) {
        let Some(next) = column_neighbour(view, card, step) else {
            return;
        };
        self.selected = Some(next);
        self.set_cursor(next);
    }

    /// Ask the detail to scroll; applied on its next pass.
    pub(crate) fn scroll_detail(&mut self, request: PatchScroll) {
        self.detail_scroll = Some(request);
    }

    /// The scroll the detail keys left for the detail's next pass.
    #[cfg(test)]
    pub(crate) fn detail_scroll(&self) -> Option<PatchScroll> {
        self.detail_scroll
    }

    /// `D`: move `card` to the end of the Done column. On the queue's current
    /// card the queue steps on, as it does after any verdict. `None` (and a
    /// notice) on a board without a Done column.
    pub(crate) fn accept_card(
        &mut self,
        view: &BoardView,
        card: NoteId,
        now: f64,
    ) -> Option<BoardAction> {
        find_card(view, card)?;
        let Some(to_col) = DONE.index(view) else {
            self.set_notice(QueueNotice::NoDoneColumn, now);
            return None;
        };
        if self.queue.current() == Some(card) {
            self.advance_queue(now);
        }
        Some(move_to_end(view, card, to_col))
    }

    /// `X`: open the reason composer for `card`. Nothing opens on a board it
    /// couldn't be sent back on.
    pub(crate) fn start_reject(&mut self, view: &BoardView, card: NoteId, now: f64) {
        if find_card(view, card).is_none() {
            return;
        }
        if IN_PROGRESS.index(view).is_none() {
            self.set_notice(QueueNotice::NoInProgressColumn, now);
            return;
        }
        self.reason = Some(ReasonComposer {
            card,
            text: String::new(),
            focus: true,
        });
    }

    /// Enter in the reason composer: post the reason as a `review:` comment
    /// now, move the card to In Progress on the next frame (a frame applies one
    /// action), and, on the queue's current card, step the queue on. An empty
    /// reason posts nothing and keeps the composer open.
    pub(crate) fn submit_reject(&mut self, view: &BoardView, now: f64) -> Option<BoardAction> {
        let composer = self.reason.as_ref()?;
        let reason = composer.text.trim();
        if reason.is_empty() {
            return None;
        }
        let body = format!("review: {reason}");
        let card = composer.card;
        self.reason = None;
        find_card(view, card)?;
        let to_col = IN_PROGRESS.index(view)?;
        self.follow_up = Some(move_to_end(view, card, to_col));
        if self.queue.current() == Some(card) {
            self.advance_queue(now);
        }
        Some(BoardAction::AddComment {
            card,
            body,
            reply_to: None,
        })
    }

    /// Esc in the reason composer: close it, sending nothing.
    pub(crate) fn cancel_reject(&mut self) {
        self.reason = None;
    }

    /// Whether the `X` reason composer is open.
    pub(crate) fn rejecting(&self) -> bool {
        self.reason.is_some()
    }

    /// The `X` composer's text, empty when it's closed.
    #[cfg(test)]
    pub(crate) fn reason_text(&self) -> &str {
        self.reason.as_ref().map_or("", |r| r.text.as_str())
    }

    /// Lay the `X` composer out alone, if it's open, for the keymap tests.
    #[cfg(test)]
    pub(crate) fn reason_test_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(composer) = &mut self.reason {
            reason_composer_ui(ui, &ColorTheme::current(ui.ctx()), composer);
        }
    }

    /// After a verdict on the queue's card: step to the next card, or, on the
    /// last, leave the queue saying it's done.
    pub(crate) fn advance_queue(&mut self, now: f64) {
        if self.queue.at_end() {
            self.close_queue();
            self.set_notice(QueueNotice::QueueDone, now);
        } else {
            self.step_queue(CardStep::Next);
        }
    }

    /// Take the action a verdict left for the frame after its own (the move
    /// behind an `X`'s comment).
    pub(crate) fn take_follow_up(&mut self) -> Option<BoardAction> {
        self.follow_up.take()
    }
}

/// The card next to `card` in its column, `step`'s way; `None` at the
/// column's end or for a card not on the board. Unfiltered: the detail and
/// the review pane don't draw through the grid's filter.
fn column_neighbour(view: &BoardView, card: NoteId, step: CardStep) -> Option<NoteId> {
    let (col, _) = find_card(view, card)?;
    let cards = &view.columns[col].cards;
    let at = cards.iter().position(|c| c.id == card)?;
    let next = match step {
        CardStep::Next => at + 1,
        CardStep::Prev => at.checked_sub(1)?,
    };
    cards.get(next).map(|c| c.id)
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

/// The id of the `X` composer's text field, so tests (and focus requests) can
/// find it.
pub(crate) fn reason_field_id() -> egui::Id {
    egui::Id::new("headway-review-reason")
}

/// The `X` composer across the top of whichever view is showing, while it's
/// open. Its Enter and Esc are read before anything lays out
/// ([`crate::keys::pane_keys`]), so the field only has to take the text and
/// its focus.
pub(super) fn reason_bar_ui(ui: &mut egui::Ui, theme: &ColorTheme, state: &mut BoardUiState) {
    let Some(composer) = &mut state.reason else {
        return;
    };
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: SPACING_LG as i8,
            right: SPACING_LG as i8,
            top: SPACING_LG as i8,
            bottom: 0,
        })
        .show(ui, |ui| reason_composer_ui(ui, theme, composer));
}

/// The `X` composer's one-line reason field.
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

/// Apply a detail scroll key inside the detail's scroll area. Called after its
/// content, so this area's end takes the delta rather than a scroll area
/// nested in the content: a line (`j`/`k`, [`DETAIL_LINES_PER_KEY`] body
/// lines) or a fraction of the view by delta, the top or bottom by scrolling
/// to the content's first or last point (`top` is where the content started).
pub(super) fn detail_scroll_ui(ui: &mut egui::Ui, scroll: Option<PatchScroll>, top: egui::Pos2) {
    let Some(scroll) = scroll else {
        return;
    };
    // `scroll_with_delta` moves the content: a negative delta scrolls down.
    let down = match scroll {
        PatchScroll::Rows(rows) => {
            rows as f32 * DETAIL_LINES_PER_KEY * ui.text_style_height(&egui::TextStyle::Body)
        }
        PatchScroll::Pages(pages) => pages * ui.clip_rect().height(),
        PatchScroll::Top => {
            ui.scroll_to_rect(
                egui::Rect::from_min_size(top, egui::Vec2::ZERO),
                Some(egui::Align::TOP),
            );
            return;
        }
        PatchScroll::Bottom => {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            return;
        }
        PatchScroll::File(_) | PatchScroll::NextFile | PatchScroll::PrevFile => return,
    };
    ui.scroll_with_delta(egui::vec2(0.0, -down));
}
