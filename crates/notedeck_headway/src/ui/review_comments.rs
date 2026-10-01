//! Inline review comments in the review pane: pick lines of the diff and
//! write a comment on them, or write one on the commit as a whole with
//! nothing picked, and send the lot at once — each published as a review
//! comment on the record (see [`headway::event::build_review_comment`]) and all
//! of them, in one message, to the record's agentium session.
//!
//! Until they're sent the comments are drafts, kept per record in
//! [`ReviewDrafts`], so they survive stepping through the queue and back. The
//! patch widget knows nothing of comments: it reports the picked lines
//! ([`PatchSelection`]) and draws whatever rows it's handed under them
//! ([`PatchNote`]), and this module turns drafts and posted comments into
//! those rows.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use headway::event::{BoardView, LineSide, ReviewLocation, ReviewView};
use nostrdb_net::NoteId;
use notedeck::ColorTheme;
use notedeck::tokens::{SPACING_SM, SPACING_XS};
use notedeck_ui::diff::{
    DiffSide, GitPatch, LineKind, LineSpan, PatchNote, PatchNoteKind, PatchSelection,
};

use super::detail::comment_view_ui;
use super::review::{QueueNotice, review_comments_open};
use super::widgets::secondary_action_button;
use super::{BoardEffect, BoardUiState, find_card};
use crate::review::LoadedReview;
use crate::store::{BoardAction, NewReviewComment};

/// A comment written on a record's commit and not sent yet: on picked lines
/// of its diff, or on the commit as a whole. Everything the send needs is
/// built when it's added, so sending formats nothing per comment but the
/// message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftComment {
    /// The lines it's on, or `None` for a comment on the commit as a whole.
    pub(crate) at: Option<DraftLines>,
    pub(crate) body: String,
}

/// Where on the diff a [`DraftComment`] on picked lines sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftLines {
    /// The file (index into the loaded patch's files) and its lines it's on.
    pub(crate) file: usize,
    pub(crate) lines: Range<usize>,
    /// Where it points, as the published event says it.
    pub(crate) location: ReviewLocation,
    /// `path:42-48`, or `path:old 3-6` for deleted lines: how the message and
    /// the pane name its place.
    pub(crate) place: String,
    /// The picked lines, each with its `+`/`-`/` ` prefix.
    pub(crate) quote: String,
}

/// How the pane's draft list names a comment on the commit as a whole, where
/// a comment on lines shows its place.
const WHOLE_COMMIT: &str = "this commit";

impl DraftComment {
    /// A draft of `body` on the commit as a whole (`c` with nothing picked).
    pub(crate) fn on_commit(body: String) -> Self {
        Self { at: None, body }
    }

    /// How the pane names where the draft is: its place, or
    /// [`WHOLE_COMMIT`].
    fn place(&self) -> &str {
        self.at
            .as_ref()
            .map_or(WHOLE_COMMIT, |at| at.place.as_str())
    }

    /// A draft of `body` on `picked` lines of `patch`, which was loaded from
    /// `commit`. `None` when the pick covers no numbered line.
    pub(crate) fn new(
        patch: &GitPatch,
        picked: &PatchSelection,
        commit: &str,
        body: String,
    ) -> Option<Self> {
        let file = patch.files().get(picked.file)?;
        let span = file.line_span(picked.lines.clone())?;
        let side = match span.side {
            DiffSide::New => LineSide::New,
            DiffSide::Old => LineSide::Old,
        };
        let location = ReviewLocation {
            path: file.path().to_owned(),
            commit: commit.to_owned(),
            start: span.start,
            end: span.end,
            side,
        };
        let mut quote = String::new();
        for line in &file.lines[picked.lines.clone()] {
            let prefix = match line.kind {
                LineKind::Context => ' ',
                LineKind::Delete => '-',
                LineKind::Insert => '+',
                LineKind::NoNewline => continue,
            };
            if !quote.is_empty() {
                quote.push('\n');
            }
            quote.push(prefix);
            quote.push_str(patch.text(line.text));
        }
        Some(Self {
            at: Some(DraftLines {
                file: picked.file,
                lines: picked.lines.clone(),
                place: place(file.path(), span),
                location,
                quote,
            }),
            body,
        })
    }
}

/// Where a posted comment points, as [`place`] names a pick:
/// `path:42-48` or `path:old 3-6`.
pub(super) fn location_place(location: &ReviewLocation) -> String {
    let span = LineSpan {
        side: diff_side(location.side),
        start: location.start,
        end: location.end,
    };
    place(&location.path, span)
}

/// The diff widget's name for a review comment's side.
fn diff_side(side: LineSide) -> DiffSide {
    match side {
        LineSide::New => DiffSide::New,
        LineSide::Old => DiffSide::Old,
    }
}

/// `path:42`, `path:42-48`, or `path:old 3-6` for deleted lines.
fn place(path: &str, span: LineSpan) -> String {
    let old = match span.side {
        DiffSide::New => "",
        DiffSide::Old => "old ",
    };
    if span.start == span.end {
        format!("{path}:{old}{}", span.start)
    } else {
        format!("{path}:{old}{}-{}", span.start, span.end)
    }
}

/// The review pane's unsent comments and the comment being written, a slice
/// of [`ReviewUi`](super::review::ReviewUi).
#[derive(Default)]
pub(crate) struct ReviewDrafts {
    /// Unsent comments per review record, in the order they were added.
    by_record: HashMap<NoteId, Vec<DraftComment>>,
    /// Bumped on every change to a draft, so the diff's rows for them are
    /// rebuilt only then (see [`sync_notes`]).
    rev: u64,
    /// The comment being written on the picked lines, or on the commit.
    composer: String,
    /// The record whose commit as a whole the composer is open on, since
    /// `c` with nothing picked. Picking lines takes the composer over.
    whole: Option<NoteId>,
    /// `c` asked for the composer's field to take the keyboard.
    focus: bool,
    /// The pick [`pick_label`](Self::pick_label) was formatted for.
    pick_for: Option<PatchSelection>,
    /// The picked lines' place, e.g. `src/lib.rs:42-48`, formatted once per
    /// pick.
    pick_label: String,
    /// The count [`send_label`](Self::send_label) was formatted for.
    send_for: usize,
    /// `Send N comments`, formatted once per count.
    send_label: String,
}

impl ReviewDrafts {
    /// `record`'s unsent comments.
    pub(crate) fn of(&self, record: NoteId) -> &[DraftComment] {
        self.by_record.get(&record).map_or(&[], Vec::as_slice)
    }

    /// Add `draft` to `record`'s.
    pub(crate) fn add(&mut self, record: NoteId, draft: DraftComment) {
        self.by_record.entry(record).or_default().push(draft);
        self.rev += 1;
    }

    /// Drop `record`'s draft `i`.
    fn remove(&mut self, record: NoteId, i: usize) {
        if let Some(drafts) = self.by_record.get_mut(&record)
            && i < drafts.len()
        {
            drafts.remove(i);
            self.rev += 1;
        }
    }

    /// Take all of `record`'s drafts, to send them.
    fn take(&mut self, record: NoteId) -> Vec<DraftComment> {
        let drafts = self.by_record.remove(&record).unwrap_or_default();
        if !drafts.is_empty() {
            self.rev += 1;
        }
        drafts
    }

    /// `c`: have the composer's field take the keyboard on its next pass.
    pub(crate) fn focus_composer(&mut self) {
        self.focus = true;
    }

    /// `c` with nothing picked: open the composer on `record`'s commit as a
    /// whole, with the keyboard.
    fn open_on_commit(&mut self, record: NoteId) {
        self.whole = Some(record);
        self.focus = true;
    }

    /// `Send N comments` for `record`'s drafts, or `None` when it has none.
    /// Formatted only when the count changes.
    pub(crate) fn send_label(&mut self, record: NoteId) -> Option<&str> {
        let n = self.of(record).len();
        if n == 0 {
            return None;
        }
        if self.send_for != n {
            self.send_for = n;
            self.send_label = if n == 1 {
                "Send 1 comment".to_owned()
            } else {
                format!("Send {n} comments")
            };
        }
        Some(&self.send_label)
    }
}

/// The id of the composer's text field, so a test can find it.
pub(crate) fn composer_field_id() -> egui::Id {
    egui::Id::unique("headway-review-comment-composer")
}

/// Hand the diff its rows for `record`'s comments — posted ones and drafts —
/// when they changed since the last pass: a posted comment lands under its
/// lines when it's on this commit and they're in this diff. Only builds on a
/// change, so a steady frame allocates nothing here.
fn sync_notes(loaded: &mut LoadedReview, record: &ReviewView, drafts: &ReviewDrafts) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (record.id, drafts.rev, record.comments.len()).hash(&mut hasher);
    let stamp = hasher.finish();
    if loaded.patch_state.notes_stamp() == Some(stamp) {
        return;
    }
    let patch = &loaded.patch;
    let posted = record.comments.iter().enumerate().filter_map(|(key, c)| {
        let loc = c.location.as_ref()?;
        if loc.commit != loaded.commit.sha {
            return None;
        }
        let file = patch.file_named(&loc.path)?;
        let span = LineSpan {
            side: diff_side(loc.side),
            start: loc.start,
            end: loc.end,
        };
        Some(PatchNote {
            file,
            lines: patch.files()[file].lines_in(span)?,
            text: c.body.clone(),
            kind: PatchNoteKind::Posted,
            // Drawn whole, author and all, by `posted_comment_ui`.
            caller_draws: true,
            key,
        })
    });
    // A comment on the commit as a whole has no rows; `comments_ui` lists it.
    let unsent = drafts
        .of(record.id)
        .iter()
        .enumerate()
        .filter_map(|(key, d)| {
            let at = d.at.as_ref()?;
            Some(PatchNote {
                file: at.file,
                lines: at.lines.clone(),
                text: format!("Draft: {}", d.body),
                kind: PatchNoteKind::Draft,
                caller_draws: false,
                key,
            })
        });
    let notes = posted.chain(unsent).collect();
    loaded.patch_state.set_notes(patch, notes, stamp);
}

/// Posted review comment `key` of `record`, where the diff hands it its rows:
/// drawn by the note renderer the card's own comments use, so it shows who
/// wrote it ([`comment_view_ui`]). The comment's event comes from the db under
/// `txn`; when it isn't there, a plain line with the author's short handle
/// stands in.
pub(super) fn posted_comment_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    txn: Option<&nostrdb::Transaction>,
    record: &ReviewView,
    key: usize,
) {
    let Some(comment) = record.comments.get(key) else {
        return;
    };
    let note = txn.and_then(|txn| app_ctx.ndb.get_note_by_id(txn, comment.id.bytes()).ok());
    if let Some(note) = note {
        comment_view_ui(ui, app_ctx, &note);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_SM;
        ui.label(
            egui::RichText::new(headway::fmt::short_author(&comment.author))
                .small()
                .strong()
                .color(theme.text_secondary),
        );
        ui.label(comment.body.as_str());
    });
}

/// Above the diff: the record's posted comments on the commit as a whole
/// (they have no lines to sit under), the composer for the picked lines or,
/// after a `c` with nothing picked, for the commit, and the record's drafts,
/// each with a ✕ to drop it. Also keeps the diff's comment rows current
/// ([`sync_notes`]). A pane with no record (a card found by its trailer) has
/// nothing to root a comment on, so it gets none of it.
pub(super) fn comments_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    loaded: &mut LoadedReview,
    record: Option<&ReviewView>,
    drafts: &mut ReviewDrafts,
) {
    let Some(record) = record else {
        drafts.focus = false;
        return;
    };
    sync_notes(loaded, record, drafts);
    commit_comments_ui(ui, theme, app_ctx, record);

    if let Some(picked) = loaded.patch_state.selection() {
        // Picking lines takes over a composer opened on the commit.
        drafts.whole = None;
        composer_ui(ui, theme, loaded, record.id, Some(picked), drafts);
        ui.add_space(SPACING_SM);
    } else if drafts.whole == Some(record.id) {
        composer_ui(ui, theme, loaded, record.id, None, drafts);
        ui.add_space(SPACING_SM);
    } else {
        // No composer, so `c` has nothing to focus.
        drafts.focus = false;
    }
    drafts_ui(ui, theme, record.id, drafts);
}

/// `record`'s posted comments on the commit as a whole, oldest first, each
/// drawn by the note renderer ([`posted_comment_ui`]). Replies to a comment
/// on lines carry its lines, so they draw in the diff instead.
fn commit_comments_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    app_ctx: &mut notedeck::AppContext,
    record: &ReviewView,
) {
    let mut whole = record
        .comments
        .iter()
        .enumerate()
        .filter(|(_, c)| c.location.is_none())
        .map(|(key, _)| key)
        .peekable();
    if whole.peek().is_none() {
        return;
    }
    let txn = nostrdb::Transaction::new(app_ctx.ndb).ok();
    for key in whole {
        posted_comment_ui(ui, theme, app_ctx, txn.as_ref(), record, key);
    }
    ui.add_space(SPACING_SM);
}

/// The composer: what it's on (the picked lines' place, or the commit), a
/// field for the comment, and "Add comment" (or Ctrl+Enter in the field),
/// which files it as a draft and closes the composer, or "Cancel", which
/// closes it. `picked` is `None` for a comment on the commit as a whole.
fn composer_ui(
    ui: &mut egui::Ui,
    theme: &ColorTheme,
    loaded: &mut LoadedReview,
    record: NoteId,
    picked: Option<PatchSelection>,
    drafts: &mut ReviewDrafts,
) {
    if let Some(picked) = &picked
        && drafts.pick_for.as_ref() != Some(picked)
    {
        drafts.pick_label = loaded
            .patch
            .files()
            .get(picked.file)
            .and_then(|f| Some(place(f.path(), f.line_span(picked.lines.clone())?)))
            .unwrap_or_default();
        drafts.pick_for = Some(picked.clone());
    }
    let (place_label, hint) = match picked {
        Some(_) => (
            drafts.pick_label.as_str(),
            "Comment on these lines (Ctrl+Enter adds it)",
        ),
        None => (WHOLE_COMMIT, "Comment on this commit (Ctrl+Enter adds it)"),
    };

    let id = composer_field_id();
    let focused = ui.memory(|m| m.has_focus(id));
    // Before the field sees it, so the Enter doesn't land as a newline.
    let submit =
        focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));

    let mut add = submit;
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_XS;
        ui.label(
            egui::RichText::new("Comment on")
                .small()
                .color(theme.text_muted),
        );
        ui.label(
            egui::RichText::new(place_label)
                .small()
                .monospace()
                .color(theme.text_secondary),
        );
    });
    ui.add_space(SPACING_XS);
    let field = ui.add(
        egui::TextEdit::multiline(&mut drafts.composer)
            .id(id)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text(hint),
    );
    if std::mem::take(&mut drafts.focus) {
        field.request_focus();
    }
    ui.add_space(SPACING_XS);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACING_SM;
        add |= secondary_action_button(ui, theme, "Add comment").clicked();
        cancel = ui
            .add(egui::Button::new(egui::RichText::new("Cancel").small()).frame(false))
            .clicked();
    });

    if cancel {
        loaded.patch_state.clear_selection();
        drafts.whole = None;
        drafts.composer.clear();
        ui.memory_mut(|m| m.surrender_focus(id));
        return;
    }
    let body = drafts.composer.trim();
    if !add || body.is_empty() {
        return;
    }
    let draft = match &picked {
        Some(picked) => {
            DraftComment::new(&loaded.patch, picked, &loaded.commit.sha, body.to_owned())
        }
        None => Some(DraftComment::on_commit(body.to_owned())),
    };
    let Some(draft) = draft else {
        return;
    };
    drafts.add(record, draft);
    drafts.composer.clear();
    drafts.whole = None;
    loaded.patch_state.clear_selection();
    ui.memory_mut(|m| m.surrender_focus(id));
}

/// The record's drafts, one small row each: its place (or
/// [`WHOLE_COMMIT`]), then its first line, then a ✕ that drops it.
fn drafts_ui(ui: &mut egui::Ui, theme: &ColorTheme, record: NoteId, drafts: &mut ReviewDrafts) {
    let mut dropped = None;
    for (i, draft) in drafts.of(record).iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = SPACING_SM;
            ui.label(
                egui::RichText::new(draft.place())
                    .small()
                    .monospace()
                    .color(theme.warning),
            );
            let first = draft.body.lines().next().unwrap_or_default();
            ui.add(
                egui::Label::new(
                    egui::RichText::new(first)
                        .small()
                        .color(theme.text_secondary),
                )
                .truncate(),
            );
            let remove = egui::Button::new(egui::RichText::new("✕").small()).frame(false);
            if ui.add(remove).on_hover_text("Drop this draft").clicked() {
                dropped = Some(i);
            }
        });
    }
    if let Some(i) = dropped {
        drafts.remove(record, i);
    }
    if !drafts.of(record).is_empty() {
        ui.add_space(SPACING_SM);
    }
}

impl BoardUiState {
    /// `c`: put the keyboard in the comment composer on `card`'s diff — on
    /// the picked lines, or, with none picked, on the shown record's commit
    /// as a whole. A pane with no record has nothing to root a comment on,
    /// and says so.
    pub(crate) fn focus_review_composer(&mut self, view: &BoardView, card: NoteId, now: f64) {
        let Some((_, found)) = find_card(view, card) else {
            return;
        };
        if self.review.has_picked_lines(found) {
            self.review.drafts.focus_composer();
            return;
        }
        match self.review.shown_record(found) {
            Some(record) => self.review.drafts.open_on_commit(record.id),
            None => self.set_notice(QueueNotice::NoRecordToComment, now),
        }
    }

    /// The record whose commit as a whole the composer is open on, if any.
    #[cfg(test)]
    pub(crate) fn commenting_on_commit(&self) -> Option<NoteId> {
        self.review.drafts.whole
    }

    /// File `draft` on `record`, as the composer's "Add comment" does.
    #[cfg(test)]
    pub(crate) fn add_review_draft(&mut self, record: NoteId, draft: DraftComment) {
        self.review.drafts.add(record, draft);
    }

    /// `C`, or the header's "Send N comments": publish the drafts on `card`'s
    /// shown record as review comments on it, and send them all, in one
    /// message, to the record's agentium session as a [`BoardEffect::Open`].
    /// A record with no session still gets its comments. With no drafts it
    /// only says so.
    pub(crate) fn send_review_comments(
        &mut self,
        view: &BoardView,
        card: NoteId,
        now: f64,
    ) -> Option<BoardAction> {
        let (_, found) = find_card(view, card)?;
        let record = self.acted_record(found)?;
        let drafts = self.review.drafts.take(record.id);
        if drafts.is_empty() {
            self.set_notice(QueueNotice::NoComments, now);
            return None;
        }
        let card_ref = headway::wordid::card_ref(&view.id, card.bytes());
        if let Some(open) = review_comments_open(&record.fields, &card_ref, &drafts) {
            self.raise(BoardEffect::Open(open));
        }
        let comments = drafts
            .into_iter()
            .map(|d| NewReviewComment {
                location: d.at.map(|at| at.location),
                body: d.body,
                reply_to: None,
            })
            .collect();
        Some(BoardAction::AddReviewComments {
            card,
            record: record.id,
            comments,
        })
    }
}
