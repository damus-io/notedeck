//! Inline renderer for plain nostr notes (kind 1), contributed by the columns
//! app to notedeck's [`KindRenderer`] registry so surfaces like the notebook can
//! draw a referenced note using the same `NoteView` widget the timeline uses,
//! and a list (the Cmd-K bar's search hits) can draw it as one compact row.

use egui::text::{LayoutJob, TextFormat};
use egui::{FontFamily, FontId, Sense};
use notedeck::name::get_display_name;
use notedeck::tokens::SPACING_SM;
use notedeck::{
    get_profile_url, ColorTheme, KindRenderRequest, KindRenderResponse, KindRenderer,
    NamedFontFamily, NoteContext, RenderContext,
};
use notedeck_ui::{NoteOptions, NoteView, ProfilePic};

/// The picture's size in a note drawn as a list row.
const ROW_PFP: f32 = 24.0;

/// The most lines of a note's text a list row shows.
const ROW_LINES: usize = 2;

/// The most of a note's text a list row lays out, in bytes: two lines'
/// worth and then some, so a long note isn't laid out whole to show its start.
const ROW_TEXT_BYTES: usize = 400;

/// The size of a row's author line and text.
const ROW_TEXT_SIZE: f32 = 13.0;

/// The size of a row's timestamp.
const ROW_SMALL_SIZE: f32 = 12.0;

/// Renders a kind-1 nostr note inline via notedeck_ui's [`NoteView`], in a
/// compact preview style (framed, small pfp, no action bar). Registered into
/// [`notedeck::KindRendererRegistry`] at app startup.
pub struct NoteKindRenderer;

impl KindRenderer for NoteKindRenderer {
    fn id(&self) -> &'static str {
        "columns.note"
    }

    fn name(&self) -> &'static str {
        "Note"
    }

    fn kinds(&self) -> &'static [u32] {
        &[1]
    }

    fn render(
        &self,
        ui: &mut egui::Ui,
        note_context: &mut NoteContext,
        req: &KindRenderRequest,
    ) -> KindRenderResponse {
        if req.context == RenderContext::Row {
            let response = note_row_ui(ui, note_context, req);
            return notedeck::open_on_click(ui, response, req);
        }
        let response = NoteView::new(note_context, req.note, NoteOptions::default())
            .preview_style()
            .show(ui)
            .response;
        KindRenderResponse::new(response)
    }
}

/// A note as one row of a list ([`RenderContext::Row`]): its author's
/// picture, then their name and how long ago it was posted, over the first
/// [`ROW_LINES`] lines of its text. Painted rather than built from labels, so
/// nothing in it takes the row's hover or click; it senses hover only.
fn note_row_ui(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    req: &KindRenderRequest,
) -> egui::Response {
    let note = req.note;
    let theme = ColorTheme::current(ui.ctx());
    let profile = note_context
        .ndb
        .get_profile_by_pubkey(req.txn, note.pubkey())
        .ok();
    let name = get_display_name(profile.as_ref()).name();
    let url = get_profile_url(profile.as_ref());
    let ago = note_context
        .time_cache
        .time_ago_since(note_context.i18n, note.created_at());

    let width = ui.available_width();
    let text_width = (width - ROW_PFP - SPACING_SM).max(0.0);
    let bold = FontFamily::Name(NamedFontFamily::Bold.as_str().into());
    let mut header = LayoutJob::default();
    header.append(
        name,
        0.0,
        TextFormat::simple(FontId::new(ROW_TEXT_SIZE, bold), theme.text_primary),
    );
    header.append(
        ago,
        SPACING_SM,
        TextFormat::simple(FontId::proportional(ROW_SMALL_SIZE), theme.text_muted),
    );
    header.wrap.max_width = text_width;
    header.wrap.max_rows = 1;
    header.wrap.break_anywhere = true;

    let mut body = LayoutJob::single_section(
        one_line(note.content()),
        TextFormat::simple(FontId::proportional(ROW_TEXT_SIZE), theme.text_primary),
    );
    body.wrap.max_width = text_width;
    body.wrap.max_rows = ROW_LINES;

    let header = ui.fonts_mut(|f| f.layout_job(header));
    let body = ui.fonts_mut(|f| f.layout_job(body));
    let height = (header.size().y + body.size().y).max(ROW_PFP);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());

    let pfp = egui::Rect::from_min_size(rect.min, egui::vec2(ROW_PFP, ROW_PFP));
    ui.put(
        pfp,
        &mut ProfilePic::new(note_context.img_cache, note_context.jobs, url).size(ROW_PFP),
    );
    let text = rect.min + egui::vec2(ROW_PFP + SPACING_SM, 0.0);
    let header_height = header.size().y;
    ui.painter().galley(text, header, theme.text_primary);
    ui.painter().galley(
        text + egui::vec2(0.0, header_height),
        body,
        theme.text_primary,
    );
    response
}

/// `content`'s words with every run of whitespace as one space, up to
/// about [`ROW_TEXT_BYTES`]: a note's text as a row wraps it.
fn one_line(content: &str) -> String {
    let mut line = String::new();
    for word in content.split_whitespace() {
        if line.len() > ROW_TEXT_BYTES {
            break;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rows_text_is_one_line_cut_after_its_budget() {
        assert_eq!(one_line("  a\n\nb\tc "), "a b c");
        let long = one_line(&"word ".repeat(400));
        assert!(long.len() <= ROW_TEXT_BYTES + "word ".len());
    }
}
