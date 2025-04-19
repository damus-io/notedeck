use super::{PostResponse, PostType};
use crate::{
    draft::Draft,
    ui::{self},
};

use enostr::{FilledKeypair, NoteId};
use notedeck::NoteContext;
use notedeck_ui::{jobs::JobsCache, NoteOptions};

pub struct QuoteRepostView<'a, 'd> {
    note_context: &'a mut NoteContext<'d>,
    poster: FilledKeypair<'a>,
    draft: &'a mut Draft,
    quoting_note: &'a nostrdb::Note<'a>,
    id_source: Option<egui::Id>,
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
            jobs,
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
            self.jobs,
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
