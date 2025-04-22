use enostr::{KeypairUnowned, NoteId};
use nostrdb::Transaction;
use notedeck::{note::NoteAction, MuteFun, NoteContext};
use notedeck_ui::{jobs::JobsCache, NoteOptions};

use crate::{
    timeline::{thread::Threads, TimelineTab, ViewFilter},
    ui::timeline::TimelineTabView,
};

pub struct ThreadView<'a, 'd> {
    threads: &'a mut Threads,
    selected_note_id: &'a [u8; 32],
    note_options: NoteOptions,
    col: usize,
    id_source: egui::Id,
    is_muted: &'a MuteFun,
    note_context: &'a mut NoteContext<'d>,
    cur_acc: &'a Option<KeypairUnowned<'a>>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> ThreadView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        threads: &'a mut Threads,
        selected_note_id: &'a [u8; 32],
        note_options: NoteOptions,
        is_muted: &'a MuteFun,
        note_context: &'a mut NoteContext<'d>,
        cur_acc: &'a Option<KeypairUnowned<'a>>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        let id_source = egui::Id::new("threadscroll_threadview");
        ThreadView {
            threads,
            selected_note_id,
            note_options,
            id_source,
            is_muted,
            note_context,
            cur_acc,
            jobs,
            col: 0,
        }
    }

    pub fn id_source(mut self, col: usize) -> Self {
        self.col = col;
        self.id_source = egui::Id::new(("threadscroll", col));
        self
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let txn = Transaction::new(self.note_context.ndb).expect("txn");

        let note_id = NoteId::new(*self.selected_note_id);

        if let Ok(note) = self.note_context.ndb.get_note_by_id(&txn, note_id.bytes()) {
            self.threads.update(
                &note,
                self.note_context.note_cache,
                self.note_context.ndb,
                &txn,
                self.note_context.unknown_ids,
                self.col,
            );
        }

        egui::ScrollArea::vertical()
            .id_salt(self.id_source)
            .animated(false)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                if let Some(thread_node) = self.threads.threads.get(&note_id) {
                    let notes: Vec<notedeck::NoteRef> =
                        thread_node.replies.iter().copied().collect();
                    let mut tab = TimelineTab::new(ViewFilter::NotesAndReplies);
                    tab.notes = notes;
                    TimelineTabView::new(
                        &tab,
                        true, // reversed for threads
                        self.note_options,
                        &txn,
                        self.is_muted,
                        self.note_context,
                        self.cur_acc,
                        self.jobs,
                    )
                    .show(ui)
                } else {
                    ui.label("Loading thread timeline...");
                    None
                }
            })
            .inner
    }
}
