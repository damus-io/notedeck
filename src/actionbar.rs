use crate::{
    multi_subscriber::MultiSubscriber,
    note::NoteRef,
    notecache::NoteCache,
    route::{Route, Router},
    thread::{Thread, ThreadResult, Threads},
};
use enostr::{NoteId, RelayPool};
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply(NoteId),
    Quote(NoteId),
    OpenThread(NoteId),
}

pub struct NewThreadNotes {
    pub root_id: NoteId,
    pub notes: Vec<NoteRef>,
}

pub enum BarResult {
    NewThreadNotes(NewThreadNotes),
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
    threads: &mut Threads,
    selected_note: &[u8; 32],
) -> Option<BarResult> {
    router.route_to(Route::thread(NoteId::new(selected_note.to_owned())));

    let root_id = crate::note::root_note_id_from_selected_id(ndb, note_cache, txn, selected_note);
    let thread_res = threads.thread_mut(ndb, txn, root_id);

    let (thread, result) = match thread_res {
        ThreadResult::Stale(thread) => {
            // The thread is stale, let's update it
            let notes = Thread::new_notes(&thread.view().notes, root_id, txn, ndb);
            let bar_result = if notes.is_empty() {
                None
            } else {
                Some(BarResult::new_thread_notes(
                    notes,
                    NoteId::new(root_id.to_owned()),
                ))
            };

            //
            // we can't insert and update the VirtualList now, because we
            // are already borrowing it mutably. Let's pass it as a
            // result instead
            //
            // thread.view.insert(&notes); <-- no
            //
            (thread, bar_result)
        }

        ThreadResult::Fresh(thread) => (thread, None),
    };

    let multi_subscriber = if let Some(multi_subscriber) = &mut thread.multi_subscriber {
        multi_subscriber
    } else {
        let filters = Thread::filters(root_id);
        thread.multi_subscriber = Some(MultiSubscriber::new(filters));
        thread.multi_subscriber.as_mut().unwrap()
    };

    multi_subscriber.subscribe(ndb, pool);

    result
}

impl BarAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        threads: &mut Threads,
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
        threads: &mut Threads,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
    ) {
        if let Some(br) = self.execute(ndb, router, threads, note_cache, pool, txn) {
            br.process(ndb, txn, threads);
        }
    }
}

impl BarResult {
    pub fn new_thread_notes(notes: Vec<NoteRef>, root_id: NoteId) -> Self {
        BarResult::NewThreadNotes(NewThreadNotes::new(notes, root_id))
    }

    pub fn process(&self, ndb: &Ndb, txn: &Transaction, threads: &mut Threads) {
        match self {
            // update the thread for next render if we have new notes
            BarResult::NewThreadNotes(new_notes) => {
                let thread = threads
                    .thread_mut(ndb, txn, new_notes.root_id.bytes())
                    .get_ptr();
                new_notes.process(thread);
            }
        }
    }
}

impl NewThreadNotes {
    pub fn new(notes: Vec<NoteRef>, root_id: NoteId) -> Self {
        NewThreadNotes { notes, root_id }
    }

    /// Simple helper for processing a NewThreadNotes result. It simply
    /// inserts/merges the notes into the thread cache
    pub fn process(&self, thread: &mut Thread) {
        // threads are chronological, ie reversed from reverse-chronological, the default.
        let reversed = true;
        thread.view_mut().insert(&self.notes, reversed);
    }
}
