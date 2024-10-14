use crate::{
    note::NoteRef,
    notecache::NoteCache,
    notes_holder::{NotesHolder, NotesHolderStorage},
    route::{Route, Router},
    thread::Thread,
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
pub struct TimelineResponse {
    pub bar_action: Option<BarAction>,
    pub open_profile: Option<Pubkey>,
}

pub struct NewNotes {
    pub id: [u8; 32],
    pub notes: Vec<NoteRef>,
}

pub enum NotesHolderResult {
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
    threads: &mut NotesHolderStorage<Thread>,
    selected_note: &[u8; 32],
) -> Option<NotesHolderResult> {
    router.route_to(Route::thread(NoteId::new(selected_note.to_owned())));

    let root_id = crate::note::root_note_id_from_selected_id(ndb, note_cache, txn, selected_note);
    Thread::open(ndb, note_cache, txn, pool, threads, root_id)
}

impl BarAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        threads: &mut NotesHolderStorage<Thread>,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
    ) -> Option<NotesHolderResult> {
        match self {
            BarAction::Reply(note_id) => {
                router.route_to(Route::reply(note_id));
                router.navigating = true;
                None
            }

            BarAction::OpenThread(note_id) => {
                open_thread(ndb, txn, router, note_cache, pool, threads, note_id.bytes())
            }

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
        threads: &mut NotesHolderStorage<Thread>,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
    ) {
        if let Some(br) = self.execute(ndb, router, threads, note_cache, pool, txn) {
            br.process(ndb, note_cache, txn, threads);
        }
    }
}

impl NotesHolderResult {
    pub fn new_notes(notes: Vec<NoteRef>, id: [u8; 32]) -> Self {
        NotesHolderResult::NewNotes(NewNotes::new(notes, id))
    }

    pub fn process<N: NotesHolder>(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        storage: &mut NotesHolderStorage<N>,
    ) {
        match self {
            // update the thread for next render if we have new notes
            NotesHolderResult::NewNotes(new_notes) => {
                let holder = storage
                    .notes_holder_mutated(ndb, note_cache, txn, &new_notes.id)
                    .get_ptr();
                new_notes.process(holder);
            }
        }
    }
}

impl NewNotes {
    pub fn new(notes: Vec<NoteRef>, id: [u8; 32]) -> Self {
        NewNotes { notes, id }
    }

    /// Simple helper for processing a NewThreadNotes result. It simply
    /// inserts/merges the notes into the thread cache
    pub fn process<N: NotesHolder>(&self, thread: &mut N) {
        // threads are chronological, ie reversed from reverse-chronological, the default.
        let reversed = true;
        thread.get_view().insert(&self.notes, reversed);
    }
}
