//! What the shared card-action keys ([`crate::keys::CardAction`]) do to a
//! card, whichever view pressed them: open its explainer, deployed build or
//! agentium session, open its review, move it to Done, send it back with a
//! reason, comment on it, archive it, step to its neighbour. [`crate::keys::apply_card_action`]
//! picks the method for the key and the view; these are the steps it takes. Also the one-line
//! composer the send-back asks its reason in, and `C` its comment, drawn over any view.

use headway::event::{BoardView, CardView, ReviewView};
use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::SPACING_LG;

use super::review::{IN_PROGRESS, Notice, QueueNotice, SessionOpen, send_back_open, session_open};
use super::{BoardEffect, BoardUiState, FoundCard, find_card};
use crate::nav::NavPos;
use crate::store::BoardAction;

/// Which way `n`/`p` step from the current card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardStep {
    /// To the next card (`n`).
    Next,
    /// To the previous card (`p`).
    Prev,
}

/// The one-line composer `X` and `C` open: the line it posts as a comment and
/// sends to the record's agentium session, what it's for ([`ComposerKind`]),
/// the card it's on, and the view it was asked in, which it closes with
/// ([`BoardUiState::retire_stale_reason`]).
pub(crate) struct ReasonComposer {
    /// What Enter does with the line.
    kind: ComposerKind,
    /// The card it's on, fixed when the key opened the composer.
    card: NoteId,
    /// The card's title as the key found it, so the bar can name the card
    /// without a lookup or a copy each frame.
    title: String,
    /// The view the key was pressed in.
    pos: NavPos,
    text: String,
    /// Grab focus on the composer's next layout. Until it lands, the
    /// composer's Enter and Esc are its own ([`BoardUiState::reason_keys_live`]).
    focus: bool,
}

/// What a [`ReasonComposer`]'s Enter does with its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerKind {
    /// `X`: post it as a `review:` comment, tell the session, and move the
    /// card back to In Progress.
    SendBack,
    /// `C`: post it as a comment and tell the session; the card stays where
    /// it is.
    Comment,
}

/// How many body-text lines a detail `j`/`k` scrolls: a mouse-wheel notch's
/// worth, since a single line crawls through a long thread.
const DETAIL_LINES_PER_KEY: f32 = 3.0;

/// A scroll the detail's keys ask of it, applied by [`detail_scroll_ui`] on
/// its next pass. The detail's own, rather than the diff's
/// [`PatchScroll`](notedeck_ui::diff::PatchScroll): it has no files to step
/// through, and its rows aren't a diff's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DetailScroll {
    /// By this many `j`/`k` steps of [`DETAIL_LINES_PER_KEY`] body lines
    /// (negative scrolls up).
    Rows(i32),
    /// By this share of the view's height (negative scrolls up).
    Pages(f32),
    /// To the top of the detail (`gg`).
    Top,
    /// To the bottom of the detail (`G`).
    Bottom,
    /// To this far down from the detail's top, in points: a jump back
    /// (Ctrl-O) to where it was scrolled.
    To(f32),
}

/// How far down a card's detail was scrolled as its last pass ended, for the
/// jump list ([`crate::jump`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DetailSpot {
    /// The card the detail showed.
    pub(crate) card: NoteId,
    /// Its scroll offset from the top, in points.
    pub(crate) offset: f32,
}

impl BoardUiState {
    /// Put up `notice` for [`super::NOTICE_SECS`] from `now` (egui time), in
    /// the view showing now.
    pub(crate) fn set_notice(&mut self, notice: QueueNotice, now: f64) {
        self.notice = Some(Notice {
            what: notice,
            at: now,
            pos: self.nav_pos(),
            seen: None,
        });
    }

    /// Take down a notice whose view has been left — by a key, a click, or a
    /// global back/forward the route seeded — so it isn't still up when that
    /// view comes back. [`super::board_ui`] runs it after the frame's pane
    /// keys, once per view it draws; `pass` is egui's
    /// [`cumulative_pass_nr`](egui::Context::cumulative_pass_nr).
    ///
    /// A view other than the notice's isn't enough to take it down: during a
    /// chrome nav slide egui_nav draws the stack's top *and* the entry beneath
    /// it, each through `render_nav`, which reseeds the view. A verdict that
    /// finishes the queue puts "Review queue done" up in the view the back
    /// lands on, and for the length of that back the outgoing queue entry is
    /// the top, drawing too. So the notice goes only once a whole pass has
    /// gone by without its view, and meanwhile it draws only in its own
    /// ([`super::notice_ui`]). A pass is counted here when the view is the
    /// notice's before the pane draws, and by `notice_ui` when it draws the
    /// notice, which catches a pane that leaves its view as it draws.
    pub(crate) fn retire_stale_notice(&mut self, pass: u64) {
        let here = self.nav_pos();
        let Some(notice) = &mut self.notice else {
            return;
        };
        let seen = notice.seen.get_or_insert(pass);
        // The last pass went by without its view: it was left, even if this
        // pass is back in it.
        if *seen + 1 < pass {
            self.notice = None;
            return;
        }
        if notice.pos == here {
            *seen = pass;
        }
    }

    /// The notice showing in the current view, if any.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<QueueNotice> {
        let here = self.nav_pos();
        self.notice.filter(|n| n.pos == here).map(|n| n.what)
    }

    /// The record of `card` a key acts on: the one the review pane shows, if
    /// it's open on the card, else the newest.
    pub(super) fn acted_record<'a>(&self, card: &'a CardView) -> Option<&'a ReviewView> {
        if self.review.card() == Some(card.id) {
            self.review.shown_record(card)
        } else {
            card.reviews.first()
        }
    }

    /// `card`'s `record`, or for `None` (a key) the [`acted_record`]: what
    /// a card action that takes a record acts on.
    ///
    /// [`acted_record`]: Self::acted_record
    fn card_record<'a>(
        &self,
        card: &'a CardView,
        record: Option<NoteId>,
    ) -> Option<&'a ReviewView> {
        match record {
            Some(record) => card.reviews.iter().find(|r| r.id == record),
            None => self.acted_record(card),
        }
    }

    /// `e`: open the explainer of `card`'s `record` in a browser tab, or say
    /// there's none. `None` (the key) is the [`acted_record`]; a click on a
    /// record's own link in the detail's Review section names that record.
    ///
    /// [`acted_record`]: Self::acted_record
    pub(crate) fn open_explainer(
        &mut self,
        ctx: &egui::Context,
        view: &BoardView,
        card: NoteId,
        record: Option<NoteId>,
    ) {
        let url = find_card(view, card)
            .and_then(|found| self.card_record(found.card, record))
            .and_then(|r| r.fields.explainer.as_deref());
        match url {
            Some(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            None => self.set_notice(QueueNotice::NoExplainer, ctx.input(|i| i.time)),
        }
    }

    /// Open the deployed build of `card`'s `record` in a browser tab. `None`
    /// is the [`acted_record`]; a click on a record's own link in the detail's
    /// Review section names that record. Only a record with a deploy draws
    /// the link, so one without has nothing to say.
    ///
    /// [`acted_record`]: Self::acted_record
    pub(crate) fn open_deploy(
        &self,
        ctx: &egui::Context,
        view: &BoardView,
        card: NoteId,
        record: Option<NoteId>,
    ) {
        let url = find_card(view, card)
            .and_then(|found| self.card_record(found.card, record))
            .and_then(|r| r.fields.deploy.as_deref());
        if let Some(url) = url {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
    }

    /// `s`/`S`: ask the app to open the agentium session of `card`'s `record`
    /// as `how` says, as a [`BoardEffect::Open`]. `None` (the key) is the
    /// [`acted_record`]; a click on a record's session chip in the detail's
    /// Review section names that record. A record with no session only says
    /// so.
    ///
    /// [`acted_record`]: Self::acted_record
    pub(crate) fn open_card_session(
        &mut self,
        view: &BoardView,
        card: NoteId,
        how: SessionOpen,
        record: Option<NoteId>,
        now: f64,
    ) {
        let Some(FoundCard { card, .. }) = find_card(view, card) else {
            return;
        };
        let card_ref = headway::wordid::card_ref(&view.id, card.id.bytes());
        let open = self
            .card_record(card, record)
            .and_then(|r| session_open(&r.fields, &card_ref, how));
        match open {
            Some(open) => self.raise(BoardEffect::Open(open)),
            None => self.set_notice(QueueNotice::NoSession, now),
        }
    }

    /// `r`: open `card`'s review pane over its detail, on `record` or, for
    /// `None` (the key), its newest. The app's nav diff puts the detail's
    /// entry under the pane's when the pane wasn't opened from it (the grid, a
    /// pane's `n`/`p`), so the pane's `q` always backs out to the card.
    pub(crate) fn open_review(&mut self, card: NoteId, record: Option<NoteId>) {
        self.review.open(card, record);
        self.selected = Some(card);
    }

    /// `r` in the review queue: point the pane back at `card`'s newest
    /// record. The queue already shows the card, and leaving it must not land
    /// on the card's detail, so the selection stays what the queue was opened
    /// over.
    pub(crate) fn newest_record(&mut self, card: NoteId) {
        self.review.open(card, None);
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
        self.detail_field_focused = false;
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
        self.open_review(next, None);
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
    pub(crate) fn scroll_detail(&mut self, request: DetailScroll) {
        self.detail_scroll = Some(request);
    }

    /// Latch, as a detail pass ends, how far down `card`'s detail is
    /// scrolled.
    pub(crate) fn latch_detail_offset(&mut self, card: NoteId, offset: f32) {
        self.detail_spot = Some(DetailSpot { card, offset });
    }

    /// How far down the open card's detail was scrolled as its last pass
    /// ended, or `None` with no card open (or one not drawn yet).
    pub(crate) fn detail_spot(&self) -> Option<DetailSpot> {
        self.detail_spot
            .filter(|spot| self.selected == Some(spot.card))
    }

    /// The scroll the detail keys left for the detail's next pass.
    #[cfg(test)]
    pub(crate) fn detail_scroll(&self) -> Option<DetailScroll> {
        self.detail_scroll
    }

    /// Note, as a detail pass ends, whether a widget in it holds the
    /// keyboard (the title or description editor, a composer), for
    /// [`detail_field_focused`](Self::detail_field_focused) to read next pass.
    pub(crate) fn latch_detail_focus(&mut self, ctx: &egui::Context) {
        self.detail_field_focused = ctx.memory(|m| m.focused().is_some());
    }

    /// Whether a widget in the detail held the keyboard as the last detail
    /// pass ended. If nothing holds it now, this pass's key is an Esc egui has
    /// already spent: it drops a text field's focus on Esc as the pass begins,
    /// before chrome's keymap (which stands down only while a widget has
    /// focus) reads the press. So `headway_back` isn't available
    /// ([`crate::ui_tools::available`]): the Esc falls through to the detail,
    /// whose field commits its edit as it lays out and whose backstop
    /// swallows the key, rather than the detail closing over the edit. The
    /// next Esc leaves the card.
    pub(crate) fn detail_field_focused(&self) -> bool {
        self.detail_field_focused
    }

    /// `X`: open the reason composer for `card`. Nothing opens on a board it
    /// couldn't be sent back on.
    pub(crate) fn start_reject(&mut self, view: &BoardView, card: NoteId, now: f64) {
        if IN_PROGRESS.index(view).is_none() && find_card(view, card).is_some() {
            self.set_notice(QueueNotice::NoInProgressColumn, now);
            return;
        }
        self.open_composer(view, card, ComposerKind::SendBack);
    }

    /// `C`: send the line comments drafted on `card`'s record at once, if
    /// there are any ([`send_review_comments`]); else open the comment
    /// composer for `card`.
    ///
    /// [`send_review_comments`]: Self::send_review_comments
    pub(crate) fn start_comment(
        &mut self,
        view: &BoardView,
        card: NoteId,
        now: f64,
    ) -> Option<BoardAction> {
        if self.has_queued_comments(view, card) {
            return self.send_review_comments(view, card, now);
        }
        self.open_composer(view, card, ComposerKind::Comment);
        None
    }

    /// Open the one-line composer on `card`, if it's on the board.
    fn open_composer(&mut self, view: &BoardView, card: NoteId, kind: ComposerKind) {
        let Some(FoundCard { card: found, .. }) = find_card(view, card) else {
            return;
        };
        self.reason = Some(ReasonComposer {
            kind,
            card,
            title: found.title.clone(),
            pos: self.nav_pos(),
            text: String::new(),
            focus: true,
        });
    }

    /// Close an `X` composer whose view has gone: a click onto another card,
    /// the detail's ✕, the graph, or a global back/forward the route seeded.
    /// [`super::board_ui`] runs it before the frame's pane keys, so a stale
    /// composer never takes their Enter.
    pub(crate) fn retire_stale_reason(&mut self) {
        let pos = self.nav_pos();
        if self.reason.as_ref().is_some_and(|r| r.pos != pos) {
            self.reason = None;
        }
    }

    /// Whether the `X`/`C` composer's Enter and Esc are its own this frame: its
    /// field has the keyboard, or is about to take it (the frame the key opened
    /// it), or nothing else does. While another widget has focus — a comment
    /// box, a title editor — they're that widget's.
    pub(crate) fn reason_keys_live(&self, focused: Option<egui::Id>) -> bool {
        let Some(composer) = &self.reason else {
            return false;
        };
        composer.focus || focused.is_none_or(|id| id == reason_field_id())
    }

    /// Enter in the one-line composer: for `X`, the send-back below; for `C`,
    /// [`send_line_comment`](Self::send_line_comment). An empty line sends
    /// nothing and keeps the composer open.
    pub(crate) fn submit_reject(&mut self, view: &BoardView, now: f64) -> Option<BoardAction> {
        let composer = self.reason.take()?;
        let line = composer.text.trim();
        let action = match composer.kind {
            ComposerKind::SendBack => self.send_back(view, composer.card, line, now),
            ComposerKind::Comment => self.send_line_comment(view, composer.card, line),
        };
        if action.is_none() && line.is_empty() {
            self.reason = Some(composer);
        }
        action
    }

    /// Enter in the `X` composer: post the reason as a `review:` comment
    /// now, move the card to In Progress on the next frame (a frame applies one
    /// action), and, on the queue's current card, step the queue on. The
    /// reason also goes to the agentium session of the card's record
    /// ([`acted_record`]) as a [`BoardEffect::Open`], so the agent that made
    /// the commit hears it; a record with no session just goes without. An
    /// empty reason posts nothing and keeps the composer open.
    ///
    /// [`acted_record`]: Self::acted_record
    fn send_back(
        &mut self,
        view: &BoardView,
        card: NoteId,
        reason: &str,
        now: f64,
    ) -> Option<BoardAction> {
        if reason.is_empty() {
            return None;
        }
        let FoundCard { card: found, .. } = find_card(view, card)?;
        let body = format!("review: {reason}");
        let card_ref = headway::wordid::card_ref(&view.id, card.bytes());
        // Before the queue steps on: the record is the one the pane shows.
        let open = self
            .acted_record(found)
            .and_then(|r| send_back_open(&r.fields, &card_ref, reason));
        let to_col = IN_PROGRESS.index(view)?;
        if let Some(open) = open {
            self.raise(BoardEffect::Open(open));
        }
        self.follow_up = Some(move_to_end(view, card, to_col));
        if self.queue.current() == Some(card) {
            self.advance_queue(view, now);
        }
        Some(BoardAction::AddComment {
            card,
            body,
            reply_to: None,
        })
    }

    /// Esc in the one-line composer: close it, sending nothing.
    pub(crate) fn cancel_reject(&mut self) {
        self.reason = None;
    }

    /// Whether the one-line composer (`X`'s or `C`'s) is open.
    pub(crate) fn rejecting(&self) -> bool {
        self.reason.is_some()
    }

    /// The one-line composer's text, empty when it's closed.
    #[cfg(test)]
    pub(crate) fn reason_text(&self) -> &str {
        self.reason.as_ref().map_or("", |r| r.text.as_str())
    }

    /// Lay the one-line composer out alone, if it's open, for the keymap tests.
    #[cfg(test)]
    pub(crate) fn reason_test_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(composer) = &mut self.reason {
            reason_composer_ui(ui, &ColorTheme::current(ui.ctx()), composer);
        }
    }

    /// After a verdict on the queue's card: step to the next card, or, on the
    /// last, leave the queue saying it's done.
    pub(crate) fn advance_queue(&mut self, view: &BoardView, now: f64) {
        if self.queue.at_end() {
            self.close_queue(view);
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
    let col = find_card(view, card)?.col;
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

/// The id of the one-line composer's text field, so tests (and focus requests) can
/// find it.
pub(crate) fn reason_field_id() -> egui::Id {
    egui::Id::unique("headway-review-reason")
}

/// The one-line composer across the top of the view it was asked in, while it's
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

/// Share of the composer's row the card's title may take; the rest is the
/// reason field's.
const TITLE_SHARE: f32 = 0.4;

/// The one-line composer's field, after what Enter does and the card it does
/// it to.
fn reason_composer_ui(ui: &mut egui::Ui, theme: &ColorTheme, composer: &mut ReasonComposer) {
    let (verb, hint) = match composer.kind {
        ComposerKind::SendBack => (
            egui::RichText::new("Send back").color(theme.destructive),
            "Why? Enter comments, tells its session, moves it to In Progress; Esc cancels",
        ),
        ComposerKind::Comment => (
            egui::RichText::new("Comment").color(theme.accent),
            "Enter comments and tells its session; Esc cancels",
        ),
    };
    ui.horizontal(|ui| {
        ui.label(verb);
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width() * TITLE_SHARE);
            ui.add(egui::Label::new(egui::RichText::new(&composer.title).strong()).truncate());
        });
        let field = egui::TextEdit::singleline(&mut composer.text)
            .id(reason_field_id())
            .desired_width(f32::INFINITY)
            .hint_text(hint);
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
/// to the content's first or last point, and a jump back by scrolling that
/// far below the content's first point (`top` is where the content started).
pub(super) fn detail_scroll_ui(ui: &mut egui::Ui, scroll: Option<DetailScroll>, top: egui::Pos2) {
    let Some(scroll) = scroll else {
        return;
    };
    // `scroll_with_delta` moves the content: a negative delta scrolls down.
    let down = match scroll {
        DetailScroll::Rows(rows) => {
            rows as f32 * DETAIL_LINES_PER_KEY * ui.text_style_height(&egui::TextStyle::Body)
        }
        DetailScroll::Pages(pages) => pages * ui.clip_rect().height(),
        DetailScroll::Top => {
            ui.scroll_to_rect(
                egui::Rect::from_min_size(top, egui::Vec2::ZERO),
                Some(egui::Align::TOP),
            );
            return;
        }
        DetailScroll::Bottom => {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            return;
        }
        DetailScroll::To(offset) => {
            ui.scroll_to_rect(
                egui::Rect::from_min_size(top + egui::vec2(0.0, offset), egui::Vec2::ZERO),
                Some(egui::Align::TOP),
            );
            return;
        }
    };
    ui.scroll_with_delta(egui::vec2(0.0, -down));
}
