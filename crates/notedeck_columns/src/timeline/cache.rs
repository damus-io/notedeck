use crate::{
    actionbar::TimelineOpenResult,
    error::Error,
    timeline::{Timeline, TimelineKind},
};

use notedeck::{filter, FilterState, NoteCache, NoteRef};

use enostr::RelayPool;
use nostrdb::{Filter, Ndb, Transaction};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

#[derive(Default)]
pub struct TimelineCache {
    timelines: HashMap<TimelineKind, Timeline>,
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

impl<'a> IntoIterator for &'a mut TimelineCache {
    type Item = (&'a TimelineKind, &'a mut Timeline);
    type IntoIter = std::collections::hash_map::IterMut<'a, TimelineKind, Timeline>;

    fn into_iter(self) -> Self::IntoIter {
        self.timelines.iter_mut()
    }
}

impl TimelineCache {
    /// Pop a timeline from the timeline cache. This only removes the timeline
    /// if it has reached 0 subscribers, meaning it was the last one to be
    /// removed
    pub fn pop(
        &mut self,
        id: &TimelineKind,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
    ) -> Result<(), Error> {
        let timeline = if let Some(timeline) = self.timelines.get_mut(id) {
            timeline
        } else {
            return Err(Error::TimelineNotFound);
        };

        timeline.subscription.unsubscribe_or_decrement(ndb, pool);

        if timeline.subscription.no_sub() {
            debug!(
                "popped last timeline {:?}, removing from timeline cache",
                id
            );
            self.timelines.remove(id);
        }

        Ok(())
    }

    fn get_expected_mut(&mut self, key: &TimelineKind) -> &mut Timeline {
        self.timelines
            .get_mut(key)
            .expect("expected notes in timline cache")
    }

    /// Insert a new timeline into the cache, based on the TimelineKind
    #[allow(clippy::too_many_arguments)]
    fn insert_new(
        &mut self,
        id: TimelineKind,
        txn: &Transaction,
        ndb: &Ndb,
        notes: &[NoteRef],
        note_cache: &mut NoteCache,
    ) {
        let mut timeline = if let Some(timeline) = id.clone().into_timeline(txn, ndb) {
            timeline
        } else {
            error!("Error creating timeline from {:?}", &id);
            return;
        };

        // insert initial notes into timeline
        timeline.insert_new(txn, ndb, note_cache, notes);
        self.timelines.insert(id, timeline);
    }

    pub fn insert(&mut self, id: TimelineKind, timeline: Timeline) {
        if let Some(cur_timeline) = self.timelines.get_mut(&id) {
            cur_timeline.subscription.increment();
            return;
        };

        self.timelines.insert(id, timeline);
    }

    /// Get and/or update the notes associated with this timeline
    pub fn notes<'a>(
        &'a mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &TimelineKind,
    ) -> Vitality<'a, Timeline> {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.timelines.contains_key(id) {
            return Vitality::Stale(self.get_expected_mut(id));
        }

        let notes = if let FilterState::Ready(filters) = id.filters(txn, ndb) {
            if let Ok(results) = ndb.query(txn, filters.local(), 1000) {
                results
                    .into_iter()
                    .map(NoteRef::from_query_result)
                    .collect()
            } else {
                debug!("got no results from TimelineCache lookup for {:?}", id);
                vec![]
            }
        } else {
            // filter is not ready yet
            vec![]
        };

        if notes.is_empty() {
            warn!("NotesHolder query returned 0 notes? ")
        } else {
            info!("found NotesHolder with {} notes", notes.len());
        }

        self.insert_new(id.to_owned(), txn, ndb, &notes, note_cache);

        Vitality::Fresh(self.get_expected_mut(id))
    }

    /// Open a timeline, this is another way of saying insert a timeline
    /// into the timeline cache. If there exists a timeline already, we
    /// bump its subscription reference count. If it's new we start a new
    /// subscription
    pub fn open(
        &mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        pool: &mut RelayPool,
        id: &TimelineKind,
    ) -> Option<TimelineOpenResult> {
        let (open_result, timeline) = match self.notes(ndb, note_cache, txn, id) {
            Vitality::Stale(timeline) => {
                // The timeline cache is stale, let's update it
                let notes = find_new_notes(
                    timeline.all_or_any_notes(),
                    timeline.subscription.get_filter()?.local(),
                    txn,
                    ndb,
                );
                let open_result = if notes.is_empty() {
                    None
                } else {
                    let new_notes = notes.iter().map(|n| n.key).collect();
                    Some(TimelineOpenResult::new_notes(new_notes, id.clone()))
                };

                // we can't insert and update the VirtualList now, because we
                // are already borrowing it mutably. Let's pass it as a
                // result instead
                //
                // holder.get_view().insert(&notes); <-- no
                (open_result, timeline)
            }

            Vitality::Fresh(timeline) => (None, timeline),
        };

        if let Some(filter) = timeline.filter.get_any_ready() {
            debug!("got open with *new* subscription for {:?}", &timeline.kind);
            timeline.subscription.try_add_local(ndb, filter);
            timeline.subscription.try_add_remote(pool, filter);
        } else {
            // This should never happen reasoning, self.notes would have
            // failed above if the filter wasn't ready
            error!(
                "open: filter not ready, so could not setup subscription. this should never happen"
            );
        };

        timeline.subscription.increment();

        open_result
    }

    pub fn get(&self, id: &TimelineKind) -> Option<&Timeline> {
        self.timelines.get(id)
    }

    pub fn get_mut(&mut self, id: &TimelineKind) -> Option<&mut Timeline> {
        self.timelines.get_mut(id)
    }

    pub fn num_timelines(&self) -> usize {
        self.timelines.len()
    }
}

/// Look for new thread notes since our last fetch
fn find_new_notes(
    notes: &[NoteRef],
    filters: &[Filter],
    txn: &Transaction,
    ndb: &Ndb,
) -> Vec<NoteRef> {
    if notes.is_empty() {
        return vec![];
    }

    let last_note = notes[0];
    let filters = filter::make_filters_since(filters, last_note.created_at + 1);

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
