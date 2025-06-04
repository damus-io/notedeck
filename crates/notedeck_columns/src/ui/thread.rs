use enostr::KeypairUnowned;
use nostrdb::Transaction;
use notedeck::{MuteFun, NoteAction, NoteContext, RootNoteId, UnknownIds};
use notedeck_ui::jobs::JobsCache;
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
    jobs: &'a mut JobsCache,
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
        jobs: &'a mut JobsCache,
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
            jobs,
        }
    }

    pub fn id_source(mut self, id: egui::Id) -> Self {
        self.id_source = id;
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.note_context.ndb).expect("txn");

        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);

        let offset_id = self.id_source.with("scroll_offset");

        if let Some(offset) = ui.data(|i| i.get_temp::<f32>(offset_id)) {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }

        let output = scroll_area.show(ui, |ui| {
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
                self.jobs,
            )
            .show(ui)
        });

        ui.data_mut(|d| d.insert_temp(offset_id, output.state.offset.y));

        output.inner
    }
}
