use crate::{
    actionbar::NoteAction,
    timeline::{ThreadSelection, TimelineCache, TimelineKind},
};

use nostrdb::Transaction;
use notedeck::{MuteFun, RootNoteId, UnknownIds};
use tracing::error;

use super::{note::contents::NoteContentsDriller, timeline::TimelineTabView};

pub struct ThreadView<'a, 'd> {
    timeline_cache: &'a mut TimelineCache,
    unknown_ids: &'a mut UnknownIds,
    selected_note_id: &'a [u8; 32],
    id_source: egui::Id,
    is_muted: &'a MuteFun,
    driller: &'a mut NoteContentsDriller<'d>,
}

impl<'a, 'd> ThreadView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_cache: &'a mut TimelineCache,
        unknown_ids: &'a mut UnknownIds,
        selected_note_id: &'a [u8; 32],
        is_muted: &'a MuteFun,
        driller: &'a mut NoteContentsDriller<'d>,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            timeline_cache,
            unknown_ids,
            selected_note_id,
            id_source,
            is_muted,
            driller,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.driller.ndb).expect("txn");

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
                let root_id = match RootNoteId::new(
                    self.driller.ndb,
                    self.driller.note_cache,
                    &txn,
                    self.selected_note_id,
                ) {
                    Ok(root_id) => root_id,

                    Err(err) => {
                        ui.label(format!("Error loading thread: {:?}", err));
                        return None;
                    }
                };

                let thread_timeline = self
                    .timeline_cache
                    .notes(
                        self.driller.ndb,
                        self.driller.note_cache,
                        &txn,
                        &TimelineKind::Thread(ThreadSelection::from_root_id(root_id.to_owned())),
                    )
                    .get_ptr();

                // TODO(jb55): skip poll if ThreadResult is fresh?

                let reversed = true;
                // poll for new notes and insert them into our existing notes
                if let Err(err) = thread_timeline.poll_notes_into_view(
                    self.driller.ndb,
                    &txn,
                    self.unknown_ids,
                    self.driller.note_cache,
                    reversed,
                ) {
                    error!("error polling notes into thread timeline: {err}");
                }

                TimelineTabView::new(
                    thread_timeline.current_view(),
                    true,
                    &txn,
                    self.is_muted,
                    self.driller,
                )
                .show(ui)
            })
            .inner
    }
}
