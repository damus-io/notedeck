//! The Outputs card: what this session produced. Its commits (those whose
//! `Agentium:` trailer names it), each a click away from its hash on the
//! clipboard; the headway cards its transcript mentions, drawn as the same
//! chips the chat draws so they show each card's live title and column and
//! open it on a click; and the pages it published.
//!
//! State lives in [`OutputsState`]: `git log` runs off the UI thread, and the
//! transcript is rescanned only when its text has grown. A remote session's
//! repo is on its host, so only its cards and pages are listed.

use crate::inspector::{CommitList, Mention, MentionGroup, OutputLabels, OutputsKey, OutputsState};
use nostrdb::Transaction;
use notedeck::{tr, Localization, NoteContext};
use std::time::Instant;

use super::{heading_ui, weak_ui, SessionFacts, MONO_SIZE};

/// The Outputs card's body. `session_ref` is the `agentium:` ref the
/// commits' trailer names; `txn` reads the notes the card chips resolve to,
/// and without one they are drawn as their ref text.
pub fn outputs_ui(
    state: &mut OutputsState,
    session_ref: Option<&str>,
    facts: &SessionFacts<'_>,
    note: &mut NoteContext<'_>,
    txn: Option<&Transaction>,
    ui: &mut egui::Ui,
) {
    let now = Instant::now();
    let chat_rows = facts.chat.len();
    let source = commits_source(facts, session_ref);
    if let Some(source) = &source {
        state
            .commits
            .poll(source.cwd, source.session_ref, chat_rows, now);
        if state.commits.pending(chat_rows) {
            ui.ctx()
                .request_repaint_after(crate::inspector::CommitsState::refresh_after());
        }
    }
    state.mentions.refresh(facts.chat, now);
    if state.mentions.stale(facts.chat) {
        ui.ctx()
            .request_repaint_after(crate::inspector::MentionsMemo::rescan_after());
    }
    let commits = source.map(|_| state.commits.list());
    let mentions = state.mentions.mentions();

    let commit_count = match commits {
        Some(Some(CommitList::Commits(list))) => list.len(),
        _ => 0,
    };
    let key = OutputsKey {
        commits: state.commits.generation(),
        mentions: state.mentions.generation(),
    };
    let counts = Counts {
        commits: commit_count,
        cards: mentions.cards.total,
        pages: mentions.pages.total,
        cards_listed: mentions.cards.list.len(),
        pages_listed: mentions.pages.list.len(),
    };
    let labels = state
        .labels
        .get(key, note.i18n, |_, i18n| build_labels(counts, i18n));

    heading_ui(ui, &labels.commits);
    match commits {
        None => commits_unavailable_ui(facts, note.i18n, ui),
        Some(None) => weak_ui(ui, "…"),
        Some(Some(CommitList::NotARepo)) => weak_ui(
            ui,
            &tr!(
                note.i18n,
                "Not a git repository",
                "Dave session inspector outputs: the session's directory has no repo to list commits from"
            ),
        ),
        Some(Some(CommitList::Failed(err))) => weak_ui(ui, err),
        Some(Some(CommitList::Commits(list))) if list.is_empty() => weak_ui(
            ui,
            &tr!(
                note.i18n,
                "No commits carry this session's Agentium trailer yet",
                "Dave session inspector outputs: no commit names this session"
            ),
        ),
        Some(Some(CommitList::Commits(list))) => {
            for commit in list {
                if commit_row_ui(&commit.short, &commit.subject, ui).clicked() {
                    ui.ctx().copy_text(commit.hash.clone());
                }
            }
        }
    }

    heading_ui(ui, &labels.cards);
    cards_ui(&mentions.cards, labels, note, txn, ui);

    heading_ui(ui, &labels.pages);
    pages_ui(&mentions.pages, labels, ui);
}

/// Where a session's commits are read from.
struct CommitsSource<'a> {
    cwd: &'a std::path::Path,
    session_ref: &'a str,
}

/// The repo and ref to list commits for: a local session with a cwd and a
/// ref. `None` when the card can't ask git.
fn commits_source<'a>(
    facts: &SessionFacts<'a>,
    session_ref: Option<&'a str>,
) -> Option<CommitsSource<'a>> {
    if facts.is_remote {
        return None;
    }
    Some(CommitsSource {
        cwd: facts.details?.cwd.as_deref()?,
        session_ref: session_ref?,
    })
}

/// Why there are no commits to list: the repo is on another host, or the
/// session has no directory or ref yet.
fn commits_unavailable_ui(facts: &SessionFacts<'_>, i18n: &mut Localization, ui: &mut egui::Ui) {
    if facts.is_remote {
        let host = facts.details.map_or("", |d| d.hostname.as_str());
        weak_ui(
            ui,
            &tr!(
                i18n,
                "Commits live in the repo on {host}",
                "Dave session inspector outputs: a remote session's commits are in a repo on the host running it",
                host = host
            ),
        );
        return;
    }
    weak_ui(
        ui,
        &tr!(
            i18n,
            "No repository for this session",
            "Dave session inspector outputs: the session has no working directory or agentium ref to find commits by"
        ),
    );
}

/// One commit: its short hash in mono, then its subject, cut to the card.
/// The whole row is the click target.
fn commit_row_ui(short: &str, subject: &str, ui: &mut egui::Ui) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(short)
                .font(egui::FontId::monospace(MONO_SIZE + 1.0))
                .color(ui.visuals().hyperlink_color),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(subject).font(egui::FontId::proportional(super::VALUE_SIZE)),
            )
            .truncate(),
        );
    });
    // Its own id, so the click sense registers above the labels (see the
    // Files card's edited rows).
    let resp = ui
        .interact(
            inner.response.rect,
            ui.make_persistent_id(("dave_output_commit", short)),
            egui::Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::Copy);
    if resp.hovered() {
        ui.painter().rect_stroke(
            resp.rect.expand(2.0),
            notedeck::tokens::RADIUS_SM,
            ui.visuals().widgets.hovered.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    resp
}

/// The mentioned cards as their inline chips, wrapped. A chip's click raises
/// its open action. A ref no card resolves to (not in the local db, or no
/// headway app to resolve it) is its text, which opens it the same way.
fn cards_ui(
    cards: &MentionGroup,
    labels: &OutputLabels,
    note: &mut NoteContext<'_>,
    txn: Option<&Transaction>,
    ui: &mut egui::Ui,
) {
    if cards.list.is_empty() {
        weak_ui(ui, &labels.no_cards);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for card in &cards.list {
            card_ui(card, note, txn, ui);
        }
    });
    if let Some(more) = &labels.more_cards {
        weak_ui(ui, more);
    }
}

/// One card: its chip, or its ref text when it doesn't resolve.
fn card_ui(
    card: &Mention,
    note: &mut NoteContext<'_>,
    txn: Option<&Transaction>,
    ui: &mut egui::Ui,
) {
    // The chip pushes its own open action when clicked.
    let drawn = txn.is_some_and(|txn| {
        notedeck_ui::markdown::render_reference(
            ui,
            note,
            txn,
            &card.text,
            notedeck::RenderContext::Inline,
        )
    });
    if drawn {
        return;
    }
    let resp = ui
        .add(
            egui::Label::new(
                egui::RichText::new(&card.text)
                    .font(egui::FontId::monospace(MONO_SIZE + 1.0))
                    .color(ui.visuals().hyperlink_color),
            )
            .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        note.app_actions
            .push(notedeck::AppAction::Open(notedeck::OpenUri::new(
                card.text.clone(),
            )));
    }
}

/// The published pages, each its URL cut to the card, opened in the browser
/// on a click.
fn pages_ui(pages: &MentionGroup, labels: &OutputLabels, ui: &mut egui::Ui) {
    if pages.list.is_empty() {
        weak_ui(ui, &labels.no_pages);
        return;
    }
    for page in &pages.list {
        let shown = page.text.strip_prefix("https://").unwrap_or(&page.text);
        let resp = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(shown)
                        .font(egui::FontId::monospace(MONO_SIZE + 1.0))
                        .color(ui.visuals().hyperlink_color),
                )
                .truncate()
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&page.text));
        }
    }
    if let Some(more) = &labels.more_pages {
        weak_ui(ui, more);
    }
}

/// How many of each output there are, for the headings.
#[derive(Clone, Copy)]
struct Counts {
    commits: usize,
    cards: usize,
    pages: usize,
    cards_listed: usize,
    pages_listed: usize,
}

/// The card's strings for `counts`.
fn build_labels(counts: Counts, i18n: &mut Localization) -> OutputLabels {
    let more = |total: usize, listed: usize, i18n: &mut Localization| {
        (total > listed).then(|| {
            tr!(
                i18n,
                "{count} more",
                "Dave session inspector outputs: how many more cards or pages there are than the card lists",
                count = total - listed
            )
        })
    };
    OutputLabels {
        commits: tr!(
            i18n,
            "Commits · {count}",
            "Dave session inspector outputs: heading over the commits that name this session",
            count = counts.commits
        ),
        cards: tr!(
            i18n,
            "Cards · {count}",
            "Dave session inspector outputs: heading over the headway cards the transcript mentions",
            count = counts.cards
        ),
        pages: tr!(
            i18n,
            "Pages · {count}",
            "Dave session inspector outputs: heading over the pages the session published",
            count = counts.pages
        ),
        no_cards: tr!(
            i18n,
            "No cards mentioned",
            "Dave session inspector outputs: the transcript mentions no headway card"
        ),
        no_pages: tr!(
            i18n,
            "No pages published",
            "Dave session inspector outputs: the transcript holds no published page"
        ),
        more_cards: more(counts.cards, counts.cards_listed, i18n),
        more_pages: more(counts.pages, counts.pages_listed, i18n),
    }
}
