use crate::{
    actionbar::BarAction, imgcache::ImageCache, note_options::process_note_selection,
    notecache::NoteCache, thread::Threads, ui,
};
use nostrdb::{Ndb, NoteKey, Transaction};
use tracing::{error, warn};

pub struct ThreadView<'a> {
    threads: &'a mut Threads,
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
        threads: &'a mut Threads,
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

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<BarAction> {
        let txn = Transaction::new(self.ndb).expect("txn");
        let mut action: Option<BarAction> = None;

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

                let thread = self.threads.thread_mut(self.ndb, &txn, root_id).get_ptr();

                // TODO(jb55): skip poll if ThreadResult is fresh?

                // poll for new notes and insert them into our existing notes
                if let Err(e) = thread.poll_notes_into_view(&txn, self.ndb) {
                    error!("Thread::poll_notes_into_view: {e}");
                }

                let len = thread.view().notes.len();

                thread.view().list.clone().borrow_mut().ui_custom_layout(
                    ui,
                    len,
                    |ui, start_index| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().item_spacing.x = 4.0;

                        let ind = len - 1 - start_index;

                        let note_key = thread.view().notes[ind].key;

                        let note = if let Ok(note) = self.ndb.get_note_by_key(&txn, note_key) {
                            note
                        } else {
                            warn!("failed to query note {:?}", note_key);
                            return 0;
                        };

                        ui::padding(8.0, ui, |ui| {
                            let note_response =
                                ui::NoteView::new(self.ndb, self.note_cache, self.img_cache, &note)
                                    .note_previews(!self.textmode)
                                    .textmode(self.textmode)
                                    .use_more_options_button(!self.textmode)
                                    .show(ui);
                            if let Some(bar_action) = note_response.action {
                                action = Some(bar_action);
                            }

                            process_note_selection(ui, note_response.option_selection, &note);
                        });

                        ui::hline(ui);
                        //ui.add(egui::Separator::default().spacing(0.0));

                        1
                    },
                );
            });

        action
    }
}
