use crate::{
    column::Column,
    note::NoteRef,
    notecache::NoteCache,
    route::Route,
    thread::{Thread, ThreadResult, Threads},
};
use enostr::{NoteId, RelayPool};
use nostrdb::{Ndb, Transaction};
use tracing::{error, info};
use uuid::Uuid;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum BarAction {
    Reply,
    OpenThread,
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
    column: &mut Column,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    threads: &mut Threads,
    selected_note: &[u8; 32],
) -> Option<BarResult> {
    {
        column
            .routes_mut()
            .push(Route::Thread(NoteId::new(selected_note.to_owned())));
        column.navigating = true;
    }

    let root_id = crate::note::root_note_id_from_selected_id(ndb, note_cache, txn, selected_note);
    let thread_res = threads.thread_mut(ndb, txn, root_id);

    let (thread, result) = match thread_res {
        ThreadResult::Stale(thread) => {
            // The thread is stale, let's update it
            let notes = Thread::new_notes(&thread.view.notes, root_id, txn, ndb);
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

    // only start a subscription on nav and if we don't have
    // an active subscription for this thread.
    if thread.subscription().is_none() {
        let filters = Thread::filters(root_id);
        *thread.subscription_mut() = ndb.subscribe(&filters).ok();

        if thread.remote_subscription().is_some() {
            error!("Found active remote subscription when it was not expected");
        } else {
            let subid = Uuid::new_v4().to_string();
            *thread.remote_subscription_mut() = Some(subid.clone());
            pool.subscribe(subid, filters);
        }

        match thread.subscription() {
            Some(_sub) => {
                thread.subscribers += 1;
                info!(
                    "Locally/remotely subscribing to thread. {} total active subscriptions, {} on this thread",
                    ndb.subscription_count(),
                    thread.subscribers,
                );
            }
            None => error!(
                "Error subscribing locally to selected note '{}''s thread",
                hex::encode(selected_note)
            ),
        }
    } else {
        thread.subscribers += 1;
        info!(
            "Re-using existing thread subscription. {} total active subscriptions, {} on this thread",
            ndb.subscription_count(),
            thread.subscribers,
        )
    }

    result
}

impl BarAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        self,
        ndb: &Ndb,
        column: &mut Column,
        threads: &mut Threads,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        replying_to: &[u8; 32],
        txn: &Transaction,
    ) -> Option<BarResult> {
        match self {
            BarAction::Reply => {
                column
                    .routes_mut()
                    .push(Route::Reply(NoteId::new(replying_to.to_owned())));
                column.navigating = true;
                None
            }

            BarAction::OpenThread => {
                open_thread(ndb, txn, column, note_cache, pool, threads, replying_to)
            }
        }
    }
}

impl BarResult {
    pub fn new_thread_notes(notes: Vec<NoteRef>, root_id: NoteId) -> Self {
        BarResult::NewThreadNotes(NewThreadNotes::new(notes, root_id))
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
        thread.view.insert(&self.notes, reversed);
    }
}
