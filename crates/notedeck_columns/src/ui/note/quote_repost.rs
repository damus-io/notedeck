use enostr::{FilledKeypair, NoteId};

use crate::{
    draft::Draft,
    ui::{self},
};

use super::{contents::NoteContentsDriller, PostResponse, PostType};

pub struct QuoteRepostView<'a, 'd> {
    driller: &'a mut NoteContentsDriller<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
}

impl<'a, 'd> QuoteRepostView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        driller: &'a mut NoteContentsDriller<'d>,
        poster: FilledKeypair<'a>,
        draft: &'a mut Draft,
        quoting_note: &'a nostrdb::Note<'a>,
        inner_rect: egui::Rect,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        QuoteRepostView {
            driller,
            poster,
            draft,
            quoting_note,
            id_source,
            inner_rect,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        let id = self.id();
        let quoting_note_id = self.quoting_note.id();

        ui::PostView::new(
            self.driller,
            self.draft,
            PostType::Quote(NoteId::new(quoting_note_id.to_owned())),
            self.poster,
            self.inner_rect,
        )
        .id_source(id)
        .ui(self.quoting_note.txn().unwrap(), ui)
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
