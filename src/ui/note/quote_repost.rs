use enostr::{FilledKeypair, RelayPool};
use nostrdb::Ndb;

use crate::{
    draft::Drafts, imgcache::ImageCache, notecache::NoteCache,
    post_action_executor::PostActionExecutor, ui,
};

use super::PostResponse;

pub struct QuoteRepostView<'a> {
    ndb: &'a Ndb,
    poster: FilledKeypair<'a>,
    pool: &'a mut RelayPool,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    drafts: &'a mut Drafts,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
}

impl<'a> QuoteRepostView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        poster: FilledKeypair<'a>,
        pool: &'a mut RelayPool,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        drafts: &'a mut Drafts,
        quoting_note: &'a nostrdb::Note<'a>,
    ) -> Self {
        let id_source: Option<egui::Id> = None;
        QuoteRepostView {
            ndb,
            poster,
            pool,
            note_cache,
            img_cache,
            drafts,
            quoting_note,
            id_source,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> PostResponse {
        let id = self.id();
        let quoting_note_id = self.quoting_note.id();

        let post_response = {
            let draft = self.drafts.quote_mut(quoting_note_id);
            ui::PostView::new(
                self.ndb,
                draft,
                crate::draft::DraftSource::Quote(quoting_note_id),
                self.img_cache,
                self.note_cache,
                self.poster,
            )
            .id_source(id)
            .ui(self.quoting_note.txn().unwrap(), ui)
        };

        if let Some(action) = &post_response.action {
            PostActionExecutor::execute(
                &self.poster,
                action,
                self.pool,
                self.drafts,
                |np, seckey| np.to_quote(seckey, self.quoting_note),
                |drafts| {
                    drafts.quote_mut(quoting_note_id).clear();
                },
            );
        }

        post_response
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