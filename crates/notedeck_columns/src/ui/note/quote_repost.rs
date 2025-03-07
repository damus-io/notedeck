use enostr::{FilledKeypair, NoteId};

use crate::{
    draft::Draft,
    ui::{self},
};

use super::{contents::NoteContext, NoteOptions, PostResponse, PostType};

pub struct QuoteRepostView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
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
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        QuoteRepostView {
            note_context,
            poster,
            draft,
            quoting_note,
            id_source,
            inner_rect,
            note_options,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        let id = self.id();
        let quoting_note_id = self.quoting_note.id();

        let post_resp = ui::PostView::new(
            self.note_context,
            self.draft,
            PostType::Quote(NoteId::new(quoting_note_id.to_owned())),
            self.poster,
            self.inner_rect,
            self.note_options,
        )
        .id_source(id)
        .ui(self.quoting_note.txn().unwrap(), ui);
        post_resp
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = Some(id);
        self
    }

    pub fn id(&self) -> egui::Id {
        self.id_source
            .unwrap_or_else(|| egui::Id::new("quote-repost-view"))
    }
}
