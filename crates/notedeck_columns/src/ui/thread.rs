use crate::{
    actionbar::NoteAction,
    timeline::{ThreadSelection, TimelineCache, TimelineKind},
    ui::note::NoteOptions,
};

use nostrdb::{Ndb, Transaction};
use notedeck::{MediaCache, MuteFun, NoteCache, RootNoteId, UnknownIds};
use tracing::error;

use super::timeline::TimelineTabView;

pub struct ThreadView<'a> {
    timeline_cache: &'a mut TimelineCache,
    ndb: &'a Ndb,
    note_cache: &'a mut NoteCache,
    unknown_ids: &'a mut UnknownIds,
    img_cache: &'a mut MediaCache,
    selected_note_id: &'a [u8; 32],
    note_options: NoteOptions,
    id_source: egui::Id,
    is_muted: &'a MuteFun,
}

impl<'a> ThreadView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_cache: &'a mut TimelineCache,
        ndb: &'a Ndb,
        note_cache: &'a mut NoteCache,
        unknown_ids: &'a mut UnknownIds,
        img_cache: &'a mut MediaCache,
        selected_note_id: &'a [u8; 32],
        note_options: NoteOptions,
        is_muted: &'a MuteFun,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            timeline_cache,
            ndb,
            note_cache,
            unknown_ids,
            img_cache,
            selected_note_id,
            note_options,
            id_source,
            is_muted,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.ndb).expect("txn");

        ui.label(
            egui::RichText::new("Threads ALPHA! It's not done. Things will be broken.")
                .color(egui::Color32::RED),
        );

        egui::ScrollArea::vertical()
            .id_salt(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let root_id =
                    match RootNoteId::new(self.ndb, self.note_cache, &txn, self.selected_note_id) {
                        Ok(root_id) => root_id,

                        Err(err) => {
                            ui.label(format!("Error loading thread: {:?}", err));
                            return None;
                        }
                    };

                let thread_timeline = self
                    .timeline_cache
                    .notes(
                        self.ndb,
                        self.note_cache,
                        &txn,
                        &TimelineKind::Thread(ThreadSelection::from_root_id(root_id.to_owned())),
                    )
                    .get_ptr();

                // TODO(jb55): skip poll if ThreadResult is fresh?

                let reversed = true;
                // poll for new notes and insert them into our existing notes
                if let Err(err) = thread_timeline.poll_notes_into_view(
                    self.ndb,
                    &txn,
                    self.unknown_ids,
                    self.note_cache,
                    reversed,
                ) {
                    error!("error polling notes into thread timeline: {err}");
                }

                TimelineTabView::new(
                    thread_timeline.current_view(),
                    true,
                    self.note_options,
                    &txn,
                    self.ndb,
                    self.note_cache,
                    self.img_cache,
                    self.is_muted,
                )
                .show(ui)
            })
            .inner
    }
}
