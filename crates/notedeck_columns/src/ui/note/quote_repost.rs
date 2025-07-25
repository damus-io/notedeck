use super::{PostResponse, PostType};
use crate::{
    draft::Draft,
    ui::{self},
};

use egui::ScrollArea;
use enostr::{FilledKeypair, NoteId};
use notedeck::NoteContext;
use notedeck_ui::{jobs::JobsCache, NoteOptions};

pub struct QuoteRepostView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    scroll_id: egui::Id,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
    jobs: &'a mut JobsCache,
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
        jobs: &'a mut JobsCache,
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

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        ScrollArea::vertical()
            .id_salt(self.scroll_id)
            .show(ui, |ui| self.show_internal(ui))
            .inner
    }

    fn show_internal(&mut self, ui: &mut egui::Ui) -> PostResponse {
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
