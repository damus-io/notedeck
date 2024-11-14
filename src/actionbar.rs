use crate::{
    note::{NoteRef, RootNoteId},
    notecache::NoteCache,
    route::{Route, Router},
    timeline::{CachedTimeline, TimelineCache, TimelineCacheKey},
};
use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply(NoteId),
    Quote(NoteId),
    OpenThread(NoteId),
}

#[derive(Default)]
pub struct NoteActionResponse {
    pub bar_action: Option<BarAction>,
    pub open_profile: Option<Pubkey>,
}

pub struct NewNotes {
    pub id: TimelineCacheKey,
    pub notes: Vec<NoteRef>,
}

pub enum BarResult {
    NewNotes(NewNotes),
}

/// open_thread is called when a note is selected and we need to navigate
/// to a thread It is responsible for managing the subscription and
/// making sure the thread is up to date. In a sense, it's a model for
/// the thread view. We don't have a concept of model/view/controller etc
/// in egui, but this is the closest thing to that.
fn open_thread(
    ndb: &Ndb,
    txn: &Transaction,
    router: &mut Router<Route>,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    timeline_cache: &mut TimelineCache,
    selected_note: &[u8; 32],
) -> Option<BarResult> {
    let root_id_raw =
        crate::note::root_note_id_from_selected_id(ndb, note_cache, txn, selected_note);
    let root_id = RootNoteId::new_unsafe(root_id_raw);

    router.route_to(Route::thread(root_id.clone()));

    timeline_cache.open(
        ndb,
        note_cache,
        txn,
        pool,
        &TimelineCacheKey::thread(root_id),
    )
}

impl BarAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
    ) -> Option<BarResult> {
        match self {
            BarAction::Reply(note_id) => {
                router.route_to(Route::reply(note_id));
                router.navigating = true;
                None
            }

            BarAction::OpenThread(note_id) => open_thread(
                ndb,
                txn,
                router,
                note_cache,
                pool,
                timeline_cache,
                note_id.bytes(),
            ),

            BarAction::Quote(note_id) => {
                router.route_to(Route::quote(note_id));
                router.navigating = true;
                None
            }
        }
    }

    /// Execute the BarAction and process the BarResult
    pub fn execute_and_process_result(
        self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
    ) {
        if let Some(br) = self.execute(ndb, router, timeline_cache, note_cache, pool, txn) {
            br.process(ndb, note_cache, txn, timeline_cache);
        }
    }
}

impl BarResult {
    pub fn new_notes(notes: Vec<NoteRef>, id: TimelineCacheKey) -> Self {
        Self::NewNotes(NewNotes::new(notes, id))
    }

    pub fn process(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        timeline_cache: &mut TimelineCache,
    ) {
        match self {
            // update the thread for next render if we have new notes
            Self::NewNotes(new_notes) => {
                let notes = timeline_cache
                    .notes(ndb, note_cache, txn, &new_notes.id)
                    .get_ptr();
                new_notes.process(notes);
            }
        }
    }
}

impl NewNotes {
    pub fn new(notes: Vec<NoteRef>, id: TimelineCacheKey) -> Self {
        NewNotes { notes, id }
    }

    /// Simple helper for processing a NewThreadNotes result. It simply
    /// inserts/merges the notes into the thread cache
    pub fn process(&self, thread: &mut CachedTimeline) {
        // threads are chronological, ie reversed from reverse-chronological, the default.
        let reversed = true;
        thread
            .timeline()
            .get_current_view_mut()
            .insert(&self.notes, reversed);
    }
}
