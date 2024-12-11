use crate::{
    column::Columns,
    notes_holder::{NotesHolder, NotesHolderStorage},
    profile::Profile,
    route::{Route, Router},
    thread::Thread,
};

use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};
use notedeck::{note::root_note_id_from_selected_id, MuteFun, NoteCache, NoteRef};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum NoteAction {
    Reply(NoteId),
    Quote(NoteId),
    OpenThread(NoteId),
    OpenProfile(Pubkey),
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
#[allow(clippy::too_many_arguments)]
fn open_thread(
    ndb: &Ndb,
    txn: &Transaction,
    router: &mut Router<Route>,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    threads: &mut NotesHolderStorage<Thread>,
    selected_note: &[u8; 32],
    is_muted: &MuteFun,
) -> Option<NotesHolderResult> {
    router.route_to(Route::thread(NoteId::new(selected_note.to_owned())));

    let root_id = root_note_id_from_selected_id(ndb, note_cache, txn, selected_note);
    Thread::open(ndb, note_cache, txn, pool, threads, root_id, is_muted)
}

impl NoteAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        threads: &mut NotesHolderStorage<Thread>,
        profiles: &mut NotesHolderStorage<Profile>,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
        is_muted: &MuteFun,
    ) -> Option<NotesHolderResult> {
        match self {
            NoteAction::Reply(note_id) => {
                router.route_to(Route::reply(note_id));
                None
            }

            NoteAction::OpenThread(note_id) => open_thread(
                ndb,
                txn,
                router,
                note_cache,
                pool,
                threads,
                note_id.bytes(),
                is_muted,
            ),

            NoteAction::OpenProfile(pubkey) => {
                router.route_to(Route::profile(pubkey));
                Profile::open(
                    ndb,
                    note_cache,
                    txn,
                    pool,
                    profiles,
                    pubkey.bytes(),
                    is_muted,
                )
            }

            NoteAction::Quote(note_id) => {
                router.route_to(Route::quote(note_id));
                None
            }
        }
    }

    /// Execute the NoteAction and process the NotesHolderResult
    #[allow(clippy::too_many_arguments)]
    pub fn execute_and_process_result(
        self,
        ndb: &Ndb,
        columns: &mut Columns,
        col: usize,
        threads: &mut NotesHolderStorage<Thread>,
        profiles: &mut NotesHolderStorage<Profile>,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
        is_muted: &MuteFun,
    ) {
        let router = columns.column_mut(col).router_mut();
        if let Some(br) = self.execute(
            ndb, router, threads, profiles, note_cache, pool, txn, is_muted,
        ) {
            br.process(ndb, note_cache, txn, threads, is_muted);
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
        is_muted: &MuteFun,
    ) {
        match self {
            // update the thread for next render if we have new notes
            NotesHolderResult::NewNotes(new_notes) => {
                let holder = storage
                    .notes_holder_mutated(ndb, note_cache, txn, &new_notes.id, is_muted)
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
