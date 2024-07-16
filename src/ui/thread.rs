use crate::{ui, Damus};
use nostrdb::{Note, NoteReply};
use tracing::warn;

pub struct ThreadView<'a> {
    app: &'a mut Damus,
    timeline: usize,
    selected_note: &'a Note<'a>,
}

impl<'a> ThreadView<'a> {
    pub fn new(app: &'a mut Damus, timeline: usize, selected_note: &'a Note<'a>) -> Self {
        ThreadView {
            app,
            timeline,
            selected_note,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let txn = self.selected_note.txn().unwrap();
        let key = self.selected_note.key().unwrap();
        let scroll_id = egui::Id::new((
            "threadscroll",
            self.app.timelines[self.timeline].selected_view,
            self.timeline,
            key,
        ));
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
                let root_id = NoteReply::new(self.selected_note.tags())
                    .root()
                    .map_or_else(|| self.selected_note.id(), |nr| nr.id);

                let (len, list) = {
                    let thread = self.app.threads.thread_mut(&self.app.ndb, txn, root_id);
                    let len = thread.view.notes.len();
                    (len, &mut thread.view.list)
                };

                list.clone()
                    .borrow_mut()
                    .ui_custom_layout(ui, len, |ui, start_index| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().item_spacing.x = 4.0;

                        let note_key = {
                            let thread = self.app.threads.thread_mut(&self.app.ndb, txn, root_id);
                            thread.view.notes[start_index].key
                        };

                        let note = if let Ok(note) = self.app.ndb.get_note_by_key(txn, note_key) {
                            note
                        } else {
                            warn!("failed to query note {:?}", note_key);
                            return 0;
                        };

                        ui::padding(8.0, ui, |ui| {
                            let textmode = self.app.textmode;
                            let resp = ui::NoteView::new(self.app, &note)
                                .note_previews(!textmode)
                                .show(ui);

                            if let Some(action) = resp.action {
                                action.execute(self.app, self.timeline, note.id());
                            }
                        });

                        ui::hline(ui);
                        //ui.add(egui::Separator::default().spacing(0.0));

                        1
                    });
            });
    }
}
