use super::{PostResponse, PostType};
use crate::{
    draft::Draft,
    nav::BodyResponse,
    ui::{self},
};

use egui::ScrollArea;
use enostr::{FilledKeypair, NoteId};
use notedeck::{JobsCacheOld, NoteContext};
use notedeck_ui::NoteOptions;

pub struct QuoteRepostView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    scroll_id: egui::Id,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
    jobs: &'a mut JobsCacheOld,
}

impl<'a, 'd> QuoteRepostView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        note_context: &'a mut NoteContext<'d>,
        poster: FilledKeypair<'a>,
        draft: &'a mut Draft,
        quoting_note: &'a nostrdb::Note<'a>,
        inner_rect: egui::Rect,
        note_options: NoteOptions,
        jobs: &'a mut JobsCacheOld,
        col: usize,
    ) -> Self {
        QuoteRepostView {
            note_context,
            poster,
            draft,
            quoting_note,
            scroll_id: QuoteRepostView::scroll_id(col, quoting_note.id()),
            inner_rect,
            note_options,
            jobs,
        }
    }

    fn id(col: usize, note_id: &[u8; 32]) -> egui::Id {
        egui::Id::new(("quote_repost", col, note_id))
    }

    pub fn scroll_id(col: usize, note_id: &[u8; 32]) -> egui::Id {
        QuoteRepostView::id(col, note_id).with("scroll")
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> BodyResponse<PostResponse> {
        let scroll_out = ScrollArea::vertical()
            .id_salt(self.scroll_id)
            .show(ui, |ui| Some(self.show_internal(ui)));

        let scroll_id = scroll_out.id;

        if let Some(inner) = scroll_out.inner {
            inner
        } else {
            BodyResponse::none()
        }
        .scroll_raw(scroll_id)
    }

    fn show_internal(&mut self, ui: &mut egui::Ui) -> BodyResponse<PostResponse> {
        let quoting_note_id = self.quoting_note.id();

        let post_resp = ui::PostView::new(
            self.note_context,
            self.draft,
            PostType::Quote(NoteId::new(quoting_note_id.to_owned())),
            self.poster,
            self.inner_rect,
            self.note_options,
            self.jobs,
        )
        .ui_no_scroll(self.quoting_note.txn().unwrap(), ui);
        post_resp
    }
}
