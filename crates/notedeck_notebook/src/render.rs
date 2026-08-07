//! A [`KindRenderer`] for longform notes (kind 30023), contributed by the
//! notebook to notedeck's registry so a `nostr:naddr…` reference — inline in a
//! text node, or as a note-embed node — draws a preview of the referenced note.
//!
//! Kind-1 (`nevent`/`note`) references are already covered by the columns app's
//! `NoteKindRenderer`; longform had no renderer anywhere, and it's the
//! notebook's primary document type, so this fills that gap.

use nostrdb::Note;
use notedeck::{ColorTheme, KindRenderRequest, KindRenderResponse, KindRenderer, NoteContext};

use crate::editor::document_icon;
use crate::event::KIND_LONGFORM;

/// How many bytes of content to preview when a longform note has no `summary`
/// tag. A borrowed prefix (cut back to a char boundary) — never an owned
/// truncation — so this stays allocation-free on the per-frame render path.
const CONTENT_PREVIEW_BYTES: usize = 280;

/// Title shown for a longform note that carries no `title` tag.
const UNTITLED: &str = "Untitled";

/// Renders a kind-30023 longform note referenced from elsewhere. Registered into
/// [`notedeck::KindRendererRegistry`] at app startup via
/// [`Notebook::kind_renderers`](crate::Notebook).
///
/// Honors [`req.context`](KindRenderRequest::context): a compact one-line **chip**
/// (a document glyph + title) when the reference flows inline in a text node's
/// prose, and a fuller **block** (title heading + body preview) when a surface
/// gives the reference its own box — a note-embed node, say.
pub struct LongformKindRenderer;

impl KindRenderer for LongformKindRenderer {
    fn id(&self) -> &'static str {
        "notebook.longform"
    }

    fn name(&self) -> &'static str {
        "Longform"
    }

    fn kinds(&self) -> &'static [u32] {
        &[KIND_LONGFORM]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
    ) -> KindRenderResponse {
        let theme = ColorTheme::current(ui.ctx());
        let response = match req.context {
            notedeck::RenderContext::Inline => longform_chip_ui(ui, &theme, req.note),
            _ => longform_block_ui(ui, &theme, note_context, req),
        };
        KindRenderResponse::new(response)
    }
}

/// A compact, single-line inline reference to a longform note: a document glyph
/// followed by the note's title, in a small rounded pill — the in-prose shape
/// ([`notedeck::RenderContext::Inline`]), versus the full [`longform_block_ui`].
fn longform_chip_ui(ui: &mut egui::Ui, theme: &ColorTheme, note: &Note) -> egui::Response {
    let title = tag_value(note, "title").unwrap_or(UNTITLED);
    notedeck_ui::inline_chip(ui, theme, title, |ui, size| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        document_icon(ui.painter(), rect.center(), size, theme.text_muted);
    })
}

/// The block embed of a longform note ([`notedeck::RenderContext::Embed`], and
/// the default for any non-inline surface): its title heading and a short body
/// preview — the `summary` tag when present, otherwise the head of the content.
fn longform_block_ui(
    ui: &mut egui::Ui,
    _theme: &ColorTheme,
    note_context: &mut NoteContext,
    req: &KindRenderRequest,
) -> egui::Response {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(tag_value(req.note, "title").unwrap_or(UNTITLED))
                .heading()
                .strong(),
        );
        // Prefer the note's own summary; fall back to the head of the body. Both
        // are borrowed from the note, so no per-frame allocation.
        let (preview, truncated) = match tag_value(req.note, "summary") {
            Some(summary) => (summary, false),
            None => head(req.note.content(), CONTENT_PREVIEW_BYTES),
        };
        notedeck_ui::markdown::render_markdown_with_refs(ui, note_context, req.txn, preview);
        if truncated {
            ui.weak("…");
        }
    })
    .response
}

/// The value of the first tag named `name` (e.g. `title`, `summary`), borrowed
/// from the note and non-empty, or `None`. Allocation-free — scans the note's
/// tags each call rather than folding a [`LongformNote`](crate::event::LongformNote).
fn tag_value<'a>(note: &'a Note, name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.get_str(0) == Some(name)
            && let Some(value) = tag.get_str(1)
            && !value.is_empty()
        {
            return Some(value);
        }
    }
    None
}

/// `s` cut to at most `max` bytes on a char boundary, paired with whether it was
/// actually cut. Borrows `s` (no owned truncation), keeping the render path
/// allocation-free.
fn head(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_returns_whole_short_string_untruncated() {
        assert_eq!(head("short", 280), ("short", false));
    }

    #[test]
    fn head_cuts_long_ascii_at_the_limit() {
        let s = "a".repeat(300);
        let (preview, truncated) = head(&s, 280);
        assert_eq!(preview.len(), 280);
        assert!(truncated);
    }

    #[test]
    fn head_backs_off_to_a_char_boundary() {
        // "é" is two bytes; a cut landing mid-char must back off, never panic.
        let s = "é".repeat(200); // 400 bytes
        let (preview, truncated) = head(&s, 281);
        assert!(truncated);
        assert!(s.is_char_boundary(preview.len()));
        assert!(preview.len() <= 281);
    }
}
