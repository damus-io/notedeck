use enostr::{FilledKeypair, NoteId};
use nostrdb::Ndb;
use notedeck::{MediaCache, NoteCache};

use crate::{
    draft::Draft,
    ui::{self, note::NoteOptions},
};

use super::{PostResponse, PostType};

pub struct QuoteRepostView<'a> {
    ndb: &'a Ndb,
    poster: FilledKeypair<'a>,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut MediaCache,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
    inner_rect: egui::Rect,
    note_options: NoteOptions,
}

impl<'a> QuoteRepostView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ndb: &'a Ndb,
        poster: FilledKeypair<'a>,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut MediaCache,
        draft: &'a mut Draft,
        quoting_note: &'a nostrdb::Note<'a>,
        inner_rect: egui::Rect,
        note_options: NoteOptions,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        QuoteRepostView {
            ndb,
            poster,
            note_cache,
            img_cache,
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

        ui::PostView::new(
            self.ndb,
            self.draft,
            PostType::Quote(NoteId::new(quoting_note_id.to_owned())),
            self.img_cache,
            self.note_cache,
            self.poster,
            self.inner_rect,
            self.note_options,
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
