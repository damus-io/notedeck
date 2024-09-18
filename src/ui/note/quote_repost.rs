use enostr::FilledKeypair;
use nostrdb::Ndb;

use crate::{draft::Draft, imgcache::ImageCache, notecache::NoteCache, ui};

use super::PostResponse;

pub struct QuoteRepostView<'a> {
    ndb: &'a Ndb,
    poster: FilledKeypair<'a>,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
}

impl<'a> QuoteRepostView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        poster: FilledKeypair<'a>,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        draft: &'a mut Draft,
        quoting_note: &'a nostrdb::Note<'a>,
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
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        let id = self.id();
        let quoting_note_id = self.quoting_note.id();

        ui::PostView::new(
            self.ndb,
            self.draft,
            crate::draft::DraftSource::Quote(quoting_note_id),
            self.img_cache,
            self.note_cache,
            self.poster,
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
