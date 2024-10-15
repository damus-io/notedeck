use crate::{
    actionbar::NoteActionResponse,
    imgcache::ImageCache,
    notecache::NoteCache,
    notes_holder::{NotesHolder, NotesHolderStorage},
    thread::Thread,
};
use nostrdb::{Ndb, NoteKey, Transaction};
use tracing::error;

use super::timeline::TimelineTabView;

pub struct ThreadView<'a> {
    threads: &'a mut NotesHolderStorage<Thread>,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    selected_note_id: &'a [u8; 32],
    textmode: bool,
    id_source: egui::Id,
}

impl<'a> ThreadView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        threads: &'a mut NotesHolderStorage<Thread>,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        selected_note_id: &'a [u8; 32],
        textmode: bool,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            threads,
            ndb,
            note_cache,
            img_cache,
            selected_note_id,
            textmode,
            id_source,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> NoteActionResponse {
        let txn = Transaction::new(self.ndb).expect("txn");

        let selected_note_key = if let Ok(key) = self
            .ndb
            .get_notekey_by_id(&txn, self.selected_note_id)
            .map(NoteKey::new)
        {
            key
        } else {
            // TODO: render 404 ?
            return NoteActionResponse::default();
        };

        ui.label(
            egui::RichText::new("Threads ALPHA! It's not done. Things will be broken.")
                .color(egui::Color32::RED),
        );

        egui::ScrollArea::vertical()
            .id_source(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let note = if let Ok(note) = self.ndb.get_note_by_key(&txn, selected_note_key) {
                    note
                } else {
                    return NoteActionResponse::default();
                };

                let root_id = {
                    let cached_note = self
                        .note_cache
                        .cached_note_or_insert(selected_note_key, &note);

                    cached_note
                        .reply
                        .borrow(note.tags())
                        .root()
                        .map_or_else(|| self.selected_note_id, |nr| nr.id)
                };

                let thread = self
                    .threads
                    .notes_holder_mutated(self.ndb, self.note_cache, &txn, root_id)
                    .get_ptr();

                // TODO(jb55): skip poll if ThreadResult is fresh?

                // poll for new notes and insert them into our existing notes
                if let Err(e) = thread.poll_notes_into_view(&txn, self.ndb) {
                    error!("Thread::poll_notes_into_view: {e}");
                }

                TimelineTabView::new(
                    thread.view(),
                    true,
                    self.textmode,
                    &txn,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                )
                .show(ui)
            })
            .inner
    }
}
