use crate::{
    actionbar::BarResult, column::Columns, imgcache::ImageCache, notecache::NoteCache,
    thread::Threads, timeline::TimelineSource, ui, unknowns::UnknownIds,
};
use enostr::RelayPool;
use nostrdb::{Ndb, NoteKey, Transaction};
use tracing::{error, warn};

pub struct ThreadView<'a> {
    column: usize,
    columns: &'a mut Columns,
    threads: &'a mut Threads,
    ndb: &'a Ndb,
    pool: &'a mut RelayPool,
    note_cache: &'a mut NoteCache,
    img_cache: &'a mut ImageCache,
    unknown_ids: &'a mut UnknownIds,
    selected_note_id: &'a [u8; 32],
    textmode: bool,
}

impl<'a> ThreadView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        column: usize,
        columns: &'a mut Columns,
        threads: &'a mut Threads,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        img_cache: &'a mut ImageCache,
        unknown_ids: &'a mut UnknownIds,
        pool: &'a mut RelayPool,
        textmode: bool,
        selected_note_id: &'a [u8; 32],
    ) -> Self {
        ThreadView {
            column,
            columns,
            threads,
            ndb,
            note_cache,
            img_cache,
            textmode,
            selected_note_id,
            unknown_ids,
            pool,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<BarResult> {
        let txn = Transaction::new(self.ndb).expect("txn");
        let mut result: Option<BarResult> = None;

        let selected_note_key = if let Ok(key) = self
            .ndb
            .get_notekey_by_id(&txn, self.selected_note_id)
            .map(NoteKey::new)
        {
            key
        } else {
            // TODO: render 404 ?
            return None;
        };

        let scroll_id = {
            egui::Id::new((
                "threadscroll",
                self.columns.column(self.column).view_id(),
                selected_note_key,
            ))
        };

        ui.label(
            egui::RichText::new("Threads ALPHA! It's not done. Things will be broken.")
                .color(egui::Color32::RED),
        );

        egui::ScrollArea::vertical()
            .id_source(scroll_id)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let note = if let Ok(note) = self.ndb.get_note_by_key(&txn, selected_note_key) {
                    note
                } else {
                    return;
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

                // poll for new notes and insert them into our existing notes
                if let Err(e) = TimelineSource::Thread(root_id).poll_notes_into_view(
                    &txn,
                    self.ndb,
                    self.columns,
                    self.threads,
                    self.unknown_ids,
                    self.note_cache,
                ) {
                    error!("Thread::poll_notes_into_view: {e}");
                }

                let (len, list) = {
                    let thread = self.threads.thread_mut(self.ndb, &txn, root_id).get_ptr();

                    let len = thread.view.notes.len();
                    (len, &mut thread.view.list)
                };

                list.clone()
                    .borrow_mut()
                    .ui_custom_layout(ui, len, |ui, start_index| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().item_spacing.x = 4.0;

                        let ind = len - 1 - start_index;
                        let note_key = {
                            let thread = self.threads.thread_mut(self.ndb, &txn, root_id).get_ptr();
                            thread.view.notes[ind].key
                        };

                        let note = if let Ok(note) = self.ndb.get_note_by_key(&txn, note_key) {
                            note
                        } else {
                            warn!("failed to query note {:?}", note_key);
                            return 0;
                        };

                        ui::padding(8.0, ui, |ui| {
                            let resp =
                                ui::NoteView::new(self.ndb, self.note_cache, self.img_cache, &note)
                                    .note_previews(!self.textmode)
                                    .textmode(self.textmode)
                                    .show(ui);

                            if let Some(action) = resp.action {
                                let br = action.execute(
                                    self.ndb,
                                    self.columns.column_mut(self.column),
                                    self.threads,
                                    self.note_cache,
                                    self.pool,
                                    note.id(),
                                    &txn,
                                );
                                if br.is_some() {
                                    result = br;
                                }
                            }
                        });

                        ui::hline(ui);
                        //ui.add(egui::Separator::default().spacing(0.0));

                        1
                    });
            });

        result
    }
}
