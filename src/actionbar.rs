use crate::{
    note::NoteRef,
    route::Route,
    thread::{Thread, ThreadResult},
    Damus,
};
use enostr::NoteId;
use nostrdb::Transaction;
use tracing::{info, warn};

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
    app: &mut Damus,
    txn: &Transaction,
    timeline: usize,
    selected_note: &[u8; 32],
) -> Option<BarResult> {
    {
        let timeline = &mut app.timelines[timeline];
        timeline
            .routes
            .push(Route::Thread(NoteId::new(selected_note.to_owned())));
        timeline.navigating = true;
    }

    let root_id = crate::note::root_note_id_from_selected_id(app, txn, selected_note);
    let thread_res = app.threads.thread_mut(&app.ndb, txn, root_id);

    // The thread is stale, let's update it
    let (thread, result) = match thread_res {
        ThreadResult::Stale(thread) => {
            let notes = Thread::new_notes(&thread.view.notes, root_id, txn, &app.ndb);
            let br = if notes.is_empty() {
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
            // thread.view.insert(&notes);
            (thread, br)
        }

        ThreadResult::Fresh(thread) => (thread, None),
    };

    // only start a subscription on nav and if we don't have
    // an active subscription for this thread.
    if thread.subscription().is_none() {
        *thread.subscription_mut() = app.ndb.subscribe(Thread::filters(root_id)).ok();

        match thread.subscription() {
            Some(_sub) => {
                thread.subscribers += 1;
                info!(
                    "Locally subscribing to thread. {} total active subscriptions, {} on this thread",
                    app.ndb.subscription_count(),
                    thread.subscribers,
                );
            }
            None => warn!(
                "Error subscribing locally to selected note '{}''s thread",
                hex::encode(selected_note)
            ),
        }
    } else {
        thread.subscribers += 1;
        info!(
            "Re-using existing thread subscription. {} total active subscriptions, {} on this thread",
            app.ndb.subscription_count(),
            thread.subscribers,
        )
    }

    result
}

impl BarAction {
    pub fn execute(
        self,
        app: &mut Damus,
        timeline: usize,
        replying_to: &[u8; 32],
        txn: &Transaction,
    ) -> Option<BarResult> {
        match self {
            BarAction::Reply => {
                let timeline = &mut app.timelines[timeline];
                timeline
                    .routes
                    .push(Route::Reply(NoteId::new(replying_to.to_owned())));
                timeline.navigating = true;
                None
            }

            BarAction::OpenThread => open_thread(app, txn, timeline, replying_to),
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
        thread.view.insert(&self.notes);
    }
}
