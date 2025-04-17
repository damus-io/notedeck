use enostr::KeypairUnowned;
use nostrdb::Transaction;
use notedeck::{MuteFun, NoteAction, NoteContext, RootNoteId, UnknownIds};
use notedeck_ui::NoteOptions;
use tracing::error;

use crate::timeline::{ThreadSelection, TimelineCache, TimelineKind};
use crate::ui::timeline::TimelineTabView;

pub struct ThreadView<'a, 'd> {
    timeline_cache: &'a mut TimelineCache,
    unknown_ids: &'a mut UnknownIds,
    selected_note_id: &'a [u8; 32],
    note_options: NoteOptions,
    id_source: egui::Id,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
}

impl<'a, 'd> ThreadView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_cache: &'a mut TimelineCache,
        unknown_ids: &'a mut UnknownIds,
        selected_note_id: &'a [u8; 32],
        note_options: NoteOptions,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            timeline_cache,
            unknown_ids,
            selected_note_id,
            note_options,
            id_source,
            is_muted,
            note_context,
            cur_acc,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.note_context.ndb).expect("txn");

        egui::ScrollArea::vertical()
            .id_salt(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let root_id = match RootNoteId::new(
                    self.note_context.ndb,
                    self.note_context.note_cache,
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
                        self.note_context.ndb,
                        self.note_context.note_cache,
                        &txn,
                        &TimelineKind::Thread(ThreadSelection::from_root_id(root_id.to_owned())),
                    )
                    .get_ptr();

                // TODO(jb55): skip poll if ThreadResult is fresh?

                let reversed = true;
                // poll for new notes and insert them into our existing notes
                if let Err(err) = thread_timeline.poll_notes_into_view(
                    self.note_context.ndb,
                    &txn,
                    self.unknown_ids,
                    self.note_context.note_cache,
                    reversed,
                ) {
                    error!("error polling notes into thread timeline: {err}");
                }

                TimelineTabView::new(
                    thread_timeline.current_view(),
                    true,
                    self.note_options,
                    &txn,
                    self.is_muted,
                    self.note_context,
                    self.cur_acc,
                )
                .show(ui)
            })
            .inner
    }
}
