use crate::{
    column::Columns,
    route::{Route, Router},
    timeline::{TimelineCache, TimelineCacheKey},
};

use enostr::{NoteId, Pubkey, RelayPool};
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::{note::root_note_id_from_selected_id, NoteCache, RootIdError, UnknownIds};
use tracing::error;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum NoteAction {
    Reply(NoteId),
    Quote(NoteId),
    OpenThread(NoteId),
    OpenProfile(Pubkey),
}

pub struct NewNotes<'a> {
    pub id: TimelineCacheKey<'a>,
    pub notes: Vec<NoteKey>,
}

pub enum TimelineOpenResult<'a> {
    NewNotes(NewNotes<'a>),
}

/// open_thread is called when a note is selected and we need to navigate
/// to a thread It is responsible for managing the subscription and
/// making sure the thread is up to date. In a sense, it's a model for
/// the thread view. We don't have a concept of model/view/controller etc
/// in egui, but this is the closest thing to that.
#[allow(clippy::too_many_arguments)]
fn open_thread<'txn>(
    ndb: &Ndb,
    txn: &'txn Transaction,
    router: &mut Router<Route>,
    note_cache: &mut NoteCache,
    pool: &mut RelayPool,
    timeline_cache: &mut TimelineCache,
    selected_note: &'txn [u8; 32],
) -> Option<TimelineOpenResult<'txn>> {
    router.route_to(Route::thread(NoteId::new(selected_note.to_owned())));

    match root_note_id_from_selected_id(ndb, note_cache, txn, selected_note) {
        Ok(root_id) => timeline_cache.open(
            ndb,
            note_cache,
            txn,
            pool,
            TimelineCacheKey::thread(root_id),
        ),

        Err(RootIdError::NoteNotFound) => {
            error!(
                "open_thread: note not found: {}",
                hex::encode(selected_note)
            );
            None
        }

        Err(RootIdError::NoRootId) => {
            error!(
                "open_thread: note has no root id: {}",
                hex::encode(selected_note)
            );
            None
        }
    }
}

impl NoteAction {
    #[allow(clippy::too_many_arguments)]
    pub fn execute<'txn, 'a>(
        &'a self,
        ndb: &Ndb,
        router: &mut Router<Route>,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &'txn Transaction,
    ) -> Option<TimelineOpenResult<'txn>>
    where
        'a: 'txn,
    {
        match self {
            NoteAction::Reply(note_id) => {
                router.route_to(Route::reply(*note_id));
                None
            }

            NoteAction::OpenThread(note_id) => open_thread(
                ndb,
                txn,
                router,
                note_cache,
                pool,
                timeline_cache,
                note_id.bytes(),
            ),

            NoteAction::OpenProfile(pubkey) => {
                router.route_to(Route::profile(*pubkey));
                timeline_cache.open(
                    ndb,
                    note_cache,
                    txn,
                    pool,
                    TimelineCacheKey::profile(pubkey.as_ref()),
                )
            }

            NoteAction::Quote(note_id) => {
                router.route_to(Route::quote(*note_id));
                None
            }
        }
    }

    /// Execute the NoteAction and process the TimelineOpenResult
    #[allow(clippy::too_many_arguments)]
    pub fn execute_and_process_result(
        self,
        ndb: &Ndb,
        columns: &mut Columns,
        col: usize,
        timeline_cache: &mut TimelineCache,
        note_cache: &mut NoteCache,
        pool: &mut RelayPool,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
    ) {
        let router = columns.column_mut(col).router_mut();
        if let Some(br) = self.execute(ndb, router, timeline_cache, note_cache, pool, txn) {
            br.process(ndb, note_cache, txn, timeline_cache, unknown_ids);
        }
    }
}

impl<'a> TimelineOpenResult<'a> {
    pub fn new_notes(notes: Vec<NoteKey>, id: TimelineCacheKey<'a>) -> Self {
        Self::NewNotes(NewNotes::new(notes, id))
    }

    pub fn process(
        &self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        storage: &mut TimelineCache,
        unknown_ids: &mut UnknownIds,
    ) {
        match self {
            // update the thread for next render if we have new notes
            TimelineOpenResult::NewNotes(new_notes) => {
                new_notes.process(storage, ndb, txn, unknown_ids, note_cache);
            }
        }
    }
}

impl<'a> NewNotes<'a> {
    pub fn new(notes: Vec<NoteKey>, id: TimelineCacheKey<'a>) -> Self {
        NewNotes { notes, id }
    }

    /// Simple helper for processing a NewThreadNotes result. It simply
    /// inserts/merges the notes into the corresponding timeline cache
    pub fn process(
        &self,
        timeline_cache: &mut TimelineCache,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) {
        match self.id {
            TimelineCacheKey::Profile(pubkey) => {
                let profile = if let Some(profile) = timeline_cache.profiles.get_mut(pubkey.bytes())
                {
                    profile
                } else {
                    return;
                };

                let reversed = false;

                if let Err(err) = profile.timeline.insert(
                    &self.notes,
                    ndb,
                    txn,
                    unknown_ids,
                    note_cache,
                    reversed,
                ) {
                    error!("error inserting notes into profile timeline: {err}")
                }
            }

            TimelineCacheKey::Thread(root_id) => {
                // threads are chronological, ie reversed from reverse-chronological, the default.
                let reversed = true;
                let thread = if let Some(thread) = timeline_cache.threads.get_mut(root_id.bytes()) {
                    thread
                } else {
                    return;
                };

                if let Err(err) =
                    thread
                        .timeline
                        .insert(&self.notes, ndb, txn, unknown_ids, note_cache, reversed)
                {
                    error!("error inserting notes into thread timeline: {err}")
                }
            }
        }
    }
}
