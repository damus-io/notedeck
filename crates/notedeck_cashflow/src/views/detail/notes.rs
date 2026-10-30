//! The detail's Notes section: the thread of notes left on the record,
//! oldest first, each with who wrote it and when, then a box to write
//! another and its "Post note".
//!
//! The thread comes from the app's [`crate::notes::NoteSource`], read
//! again only when a note arrives or another record opens, and is laid out
//! here once per read and width ([`NotesGalleys`]), so a quiet frame lays
//! out nothing. A post goes on [`crate::notes::NotesCx::outbox`], which the
//! app posts after the frame ([`cashflow::notes::post`]); "Saving…" shows
//! until the note comes back through the subscription.

use std::sync::Arc;

use egui::{Galley, RichText};

use super::model::ago;
use super::{Layouter, heading, text};
use crate::notes::{NotePost, Thread};
use crate::ui::palette::Palette;

/// The gap between two notes.
const NOTE_GAP: f32 = 10.0;

/// What the section keeps between frames.
#[derive(Default)]
pub(crate) struct NotesState {
    /// The note being written.
    draft: String,
    /// The thread laid out; `None` until the first frame draws it.
    galleys: Option<NotesGalleys>,
    /// Why the last post failed.
    error: Option<String>,
}

impl NotesState {
    /// A post this section queued failed with `err`.
    pub(crate) fn post_failed(&mut self, err: String) {
        self.error = Some(err);
    }
}

/// The thread's texts, laid out for one read of it, width and scale.
struct NotesGalleys {
    /// The [`Thread::generation`] they were laid out from.
    generation: u64,
    width: f32,
    pixels_per_point: f32,
    /// "Notes" or "Notes (N)".
    heading: Arc<Galley>,
    /// "No notes yet.", with none.
    empty: Option<Arc<Galley>>,
    notes: Vec<NoteGalleys>,
    post: Arc<Galley>,
}

/// One note's galleys.
struct NoteGalleys {
    /// "you · 3 h ago".
    meta: Arc<Galley>,
    text: Arc<Galley>,
}

impl NotesGalleys {
    /// `thread` laid out `width` wide.
    fn lay_out(l: &Layouter<'_>, thread: &Thread, width: f32) -> Self {
        let now = super::unix_now();
        let count = thread.notes.len();
        let heading = if count == 0 {
            l.bold("Notes", 15.0)
        } else {
            l.bold(&format!("Notes ({count})"), 15.0)
        };
        NotesGalleys {
            generation: thread.generation,
            width,
            pixels_per_point: l.painter.pixels_per_point(),
            heading,
            empty: (count == 0).then(|| l.line("No notes yet.", 13.0)),
            notes: thread
                .notes
                .iter()
                .map(|note| {
                    let who = if thread.me.as_ref() == Some(&note.author) {
                        "you".to_owned()
                    } else {
                        note.author.hex()[..8].to_owned()
                    };
                    NoteGalleys {
                        meta: l.line(&format!("{who} · {}", ago(note.created_at, now)), 12.0),
                        text: l.wrapped(&note.text, 14.0, width),
                    }
                })
                .collect(),
            post: l.line("Post note", 14.0),
        }
    }

    /// Whether these were laid out from `thread`'s read at `width`, `ppp`.
    fn fits(&self, thread: &Thread, width: f32, ppp: f32) -> bool {
        self.generation == thread.generation && self.width == width && self.pixels_per_point == ppp
    }
}

/// What the section draws with this frame.
pub(super) struct Section<'a> {
    /// Its own state: the draft and the layout.
    pub(super) state: &'a mut NotesState,
    /// The open record's thread.
    pub(super) thread: &'a Thread,
    /// Whether a post hasn't come back yet.
    pub(super) saving: bool,
}

/// Something the section asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Post the draft.
    Post,
}

/// Draw the thread and the box to add to it, `width` wide.
pub(super) fn section_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    section: Section<'_>,
    width: f32,
) -> Option<Event> {
    let Section {
        state,
        thread,
        saving,
    } = section;
    let ppp = ui.ctx().pixels_per_point();
    if !state
        .galleys
        .as_ref()
        .is_some_and(|g| g.fits(thread, width, ppp))
    {
        let l = Layouter {
            painter: ui.painter(),
            bold: palette.faces.bold.clone(),
        };
        state.galleys = Some(NotesGalleys::lay_out(&l, thread, width));
    }
    let g = state.galleys.as_ref()?;

    heading(ui, palette, &g.heading);
    if let Some(empty) = &g.empty {
        text(ui, empty, palette.text_muted);
    }
    for (i, note) in g.notes.iter().enumerate() {
        if i > 0 {
            ui.add_space(NOTE_GAP);
        }
        text(ui, &note.meta, palette.text_muted);
        text(ui, &note.text, palette.text);
    }
    ui.add_space(NOTE_GAP);
    ui.add(
        egui::TextEdit::multiline(&mut state.draft)
            .hint_text("Add a note…")
            .desired_rows(2)
            .desired_width(width),
    );
    let mut event = None;
    ui.horizontal(|ui| {
        let ready = !state.draft.trim().is_empty();
        if ui
            .add_enabled(ready, egui::Button::new(g.post.clone()))
            .clicked()
        {
            event = Some(Event::Post);
        }
        if saving {
            ui.label(RichText::new("Saving…").color(palette.text_muted));
        }
    });
    if let Some(error) = &state.error {
        ui.label(RichText::new(error.as_str()).color(palette.warning));
    }
    event
}

/// Do what `event` asked for on `record`'s thread, queuing a post on
/// `outbox`.
pub(super) fn act(state: &mut NotesState, event: Event, record: &str, outbox: &mut Vec<NotePost>) {
    match event {
        Event::Post => {
            let text = state.draft.trim();
            if text.is_empty() {
                return;
            }
            outbox.push(NotePost {
                record: record.to_owned(),
                text: text.to_owned(),
            });
            state.draft.clear();
            state.error = None;
        }
    }
}
