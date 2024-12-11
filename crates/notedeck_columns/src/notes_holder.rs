use std::collections::HashMap;

use enostr::{Filter, RelayPool};
use nostrdb::{Ndb, Transaction};
use notedeck::{MuteFun, NoteCache, NoteRef, NoteRefsUnkIdAction};
use tracing::{debug, info, warn};

use crate::{
    actionbar::NotesHolderResult, multi_subscriber::MultiSubscriber, timeline::TimelineTab, Error,
    Result,
};

pub struct NotesHolderStorage<M: NotesHolder> {
    pub id_to_object: HashMap<[u8; 32], M>,
}

impl<M: NotesHolder> Default for NotesHolderStorage<M> {
    fn default() -> Self {
        NotesHolderStorage {
            id_to_object: HashMap::new(),
        }
    }
}

pub enum Vitality<'a, M> {
    Fresh(&'a mut M),
    Stale(&'a mut M),
}

impl<'a, M> Vitality<'a, M> {
    pub fn get_ptr(self) -> &'a mut M {
        match self {
            Self::Fresh(ptr) => ptr,
            Self::Stale(ptr) => ptr,
        }
    }

    pub fn is_stale(&self) -> bool {
        match self {
            Self::Fresh(_ptr) => false,
            Self::Stale(_ptr) => true,
        }
    }
}

impl<M: NotesHolder> NotesHolderStorage<M> {
    pub fn notes_holder_expected_mut(&mut self, id: &[u8; 32]) -> &mut M {
        self.id_to_object
            .get_mut(id)
            .expect("notes_holder_expected_mut used but there was no NotesHolder")
    }

    pub fn notes_holder_mutated<'a>(
        &'a mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &[u8; 32],
        is_muted: &MuteFun,
    ) -> Vitality<'a, M> {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.id_to_object.contains_key(id) {
            return Vitality::Stale(self.notes_holder_expected_mut(id));
        }

        // we don't have the note holder, query for it!
        let filters = M::filters(id);

        let notes = if let Ok(results) = ndb.query(txn, &filters, 1000) {
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!(
                "got no results from NotesHolder lookup for {}",
                hex::encode(id)
            );
            vec![]
        };

        if notes.is_empty() {
            warn!("NotesHolder query returned 0 notes? ")
        } else {
            info!("found NotesHolder with {} notes", notes.len());
        }

        self.id_to_object.insert(
            id.to_owned(),
            M::new_notes_holder(txn, ndb, note_cache, id, M::filters(id), notes, is_muted),
        );
        Vitality::Fresh(self.id_to_object.get_mut(id).unwrap())
    }
}

pub trait NotesHolder {
    fn get_multi_subscriber(&mut self) -> Option<&mut MultiSubscriber>;
    fn set_multi_subscriber(&mut self, subscriber: MultiSubscriber);
    fn get_view(&mut self) -> &mut TimelineTab;
    fn filters(for_id: &[u8; 32]) -> Vec<Filter>;
    fn filters_since(for_id: &[u8; 32], since: u64) -> Vec<Filter>;
    fn new_notes_holder(
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        id: &[u8; 32],
        filters: Vec<Filter>,
        notes: Vec<NoteRef>,
        is_muted: &MuteFun,
    ) -> Self;

    #[must_use = "process_action must be handled in the Ok(action) case"]
    fn poll_notes_into_view(
        &mut self,
        txn: &Transaction,
        ndb: &Ndb,
        is_muted: &MuteFun,
    ) -> Result<NoteRefsUnkIdAction> {
        if let Some(multi_subscriber) = self.get_multi_subscriber() {
            let reversed = true;
            let note_refs: Vec<NoteRef> = multi_subscriber.poll_for_notes(ndb, txn, is_muted)?;
            self.get_view().insert(&note_refs, reversed);
            Ok(NoteRefsUnkIdAction::new(note_refs))
        } else {
            Err(Error::Generic(
                "NotesHolder unexpectedly has no MultiSubscriber".to_owned(),
            ))
        }
    }

    /// Look for new thread notes since our last fetch
    fn new_notes(notes: &[NoteRef], id: &[u8; 32], txn: &Transaction, ndb: &Ndb) -> Vec<NoteRef> {
        if notes.is_empty() {
            return vec![];
        }

        let last_note = notes[0];
        let filters = Self::filters_since(id, last_note.created_at + 1);

        if let Ok(results) = ndb.query(txn, &filters, 1000) {
            debug!("got {} results from NotesHolder update", results.len());
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!("got no results from NotesHolder update",);
            vec![]
        }
    }

    /// Local NotesHolder unsubscribe
    fn unsubscribe_locally<M: NotesHolder>(
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        notes_holder_storage: &mut NotesHolderStorage<M>,
        pool: &mut RelayPool,
        id: &[u8; 32],
        is_muted: &MuteFun,
    ) {
        let notes_holder = notes_holder_storage
            .notes_holder_mutated(ndb, note_cache, txn, id, is_muted)
            .get_ptr();

        if let Some(multi_subscriber) = notes_holder.get_multi_subscriber() {
            multi_subscriber.unsubscribe(ndb, pool);
        }
    }

    fn open<M: NotesHolder>(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        pool: &mut RelayPool,
        storage: &mut NotesHolderStorage<M>,
        id: &[u8; 32],
        is_muted: &MuteFun,
    ) -> Option<NotesHolderResult> {
        let vitality = storage.notes_holder_mutated(ndb, note_cache, txn, id, is_muted);

        let (holder, result) = match vitality {
            Vitality::Stale(holder) => {
                // The NotesHolder is stale, let's update it
                let notes = M::new_notes(&holder.get_view().notes, id, txn, ndb);
                let holder_result = if notes.is_empty() {
                    None
                } else {
                    Some(NotesHolderResult::new_notes(notes, id.to_owned()))
                };

                //
                // we can't insert and update the VirtualList now, because we
                // are already borrowing it mutably. Let's pass it as a
                // result instead
                //
                // holder.get_view().insert(&notes); <-- no
                //
                (holder, holder_result)
            }

            Vitality::Fresh(thread) => (thread, None),
        };

        let multi_subscriber = if let Some(multi_subscriber) = holder.get_multi_subscriber() {
            multi_subscriber
        } else {
            let filters = M::filters(id);
            holder.set_multi_subscriber(MultiSubscriber::new(filters));
            holder.get_multi_subscriber().unwrap()
        };

        multi_subscriber.subscribe(ndb, pool);

        result
    }
}
