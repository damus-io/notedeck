use crate::{
    actionbar::TimelineOpenResult,
    multi_subscriber::MultiSubscriber,
    profile::Profile,
    thread::Thread,
    //subscriptions::SubRefs,
    timeline::{PubkeySource, Timeline},
};

use notedeck::{NoteCache, NoteRef, RootNoteId, RootNoteIdBuf};

use enostr::{Pubkey, PubkeyRef, RelayPool};
use nostrdb::{Filter, FilterBuilder, Ndb, Transaction};
use std::collections::HashMap;
use tracing::{debug, info, warn};

#[derive(Default)]
pub struct TimelineCache {
    pub threads: HashMap<RootNoteIdBuf, Thread>,
    pub profiles: HashMap<Pubkey, Profile>,
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

#[derive(Hash, Debug, Copy, Clone)]
pub enum TimelineCacheKey<'a> {
    Profile(PubkeyRef<'a>),
    Thread(RootNoteId<'a>),
}

impl<'a> TimelineCacheKey<'a> {
    pub fn profile(pubkey: PubkeyRef<'a>) -> Self {
        Self::Profile(pubkey)
    }

    pub fn thread(root_id: RootNoteId<'a>) -> Self {
        Self::Thread(root_id)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::Profile(pk) => pk.bytes(),
            Self::Thread(root_id) => root_id.bytes(),
        }
    }

    /// The filters used to update our timeline cache
    pub fn filters_raw(&self) -> Vec<FilterBuilder> {
        match self {
            TimelineCacheKey::Thread(root_id) => Thread::filters_raw(*root_id),

            TimelineCacheKey::Profile(pubkey) => vec![Filter::new()
                .authors([pubkey.bytes()])
                .kinds([1])
                .limit(notedeck::filter::default_limit())],
        }
    }

    pub fn filters_since(&self, since: u64) -> Vec<Filter> {
        self.filters_raw()
            .into_iter()
            .map(|fb| fb.since(since).build())
            .collect()
    }

    pub fn filters(&self) -> Vec<Filter> {
        self.filters_raw()
            .into_iter()
            .map(|mut fb| fb.build())
            .collect()
    }
}

impl TimelineCache {
    fn contains_key(&self, key: TimelineCacheKey<'_>) -> bool {
        match key {
            TimelineCacheKey::Profile(pubkey) => self.profiles.contains_key(pubkey.bytes()),
            TimelineCacheKey::Thread(root_id) => self.threads.contains_key(root_id.bytes()),
        }
    }

    fn get_expected_mut(&mut self, key: TimelineCacheKey<'_>) -> &mut Timeline {
        match key {
            TimelineCacheKey::Profile(pubkey) => self
                .profiles
                .get_mut(pubkey.bytes())
                .map(|p| &mut p.timeline),
            TimelineCacheKey::Thread(root_id) => self
                .threads
                .get_mut(root_id.bytes())
                .map(|t| &mut t.timeline),
        }
        .expect("expected notes in timline cache")
    }

    /// Insert a new profile or thread into the cache, based on the TimelineCacheKey
    #[allow(clippy::too_many_arguments)]
    fn insert_new(
        &mut self,
        id: TimelineCacheKey<'_>,
        txn: &Transaction,
        ndb: &Ndb,
        notes: &[NoteRef],
        note_cache: &mut NoteCache,
        filters: Vec<Filter>,
    ) {
        match id {
            TimelineCacheKey::Profile(pubkey) => {
                let mut profile = Profile::new(PubkeySource::Explicit(pubkey.to_owned()), filters);
                // insert initial notes into timeline
                profile.timeline.insert_new(txn, ndb, note_cache, notes);
                self.profiles.insert(pubkey.to_owned(), profile);
            }

            TimelineCacheKey::Thread(root_id) => {
                let mut thread = Thread::new(root_id.to_owned());
                thread.timeline.insert_new(txn, ndb, note_cache, notes);
                self.threads.insert(root_id.to_owned(), thread);
            }
        }
    }

    /// Get and/or update the notes associated with this timeline
    pub fn notes<'a>(
        &'a mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: TimelineCacheKey<'a>,
    ) -> Vitality<'a, Timeline> {
        // we can't use the naive hashmap entry API here because lookups
        // require a copy, wait until we have a raw entry api. We could
        // also use hashbrown?

        if self.contains_key(id) {
            return Vitality::Stale(self.get_expected_mut(id));
        }

        let filters = id.filters();
        let notes = if let Ok(results) = ndb.query(txn, &filters, 1000) {
            results
                .into_iter()
                .map(NoteRef::from_query_result)
                .collect()
        } else {
            debug!("got no results from TimelineCache lookup for {:?}", id);
            vec![]
        };

        if notes.is_empty() {
            warn!("NotesHolder query returned 0 notes? ")
        } else {
            info!("found NotesHolder with {} notes", notes.len());
        }

        self.insert_new(id, txn, ndb, &notes, note_cache, filters);

        Vitality::Fresh(self.get_expected_mut(id))
    }

    pub fn subscription(
        &mut self,
        id: TimelineCacheKey<'_>,
    ) -> Option<&mut Option<MultiSubscriber>> {
        match id {
            TimelineCacheKey::Profile(pubkey) => self
                .profiles
                .get_mut(pubkey.bytes())
                .map(|p| &mut p.subscription),
            TimelineCacheKey::Thread(root_id) => self
                .threads
                .get_mut(root_id.bytes())
                .map(|t| &mut t.subscription),
        }
    }

    pub fn open<'a>(
        &mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        pool: &mut RelayPool,
        id: TimelineCacheKey<'a>,
    ) -> Option<TimelineOpenResult<'a>> {
        let result = match self.notes(ndb, note_cache, txn, id) {
            Vitality::Stale(timeline) => {
                // The timeline cache is stale, let's update it
                let notes = find_new_notes(timeline.all_or_any_notes(), id, txn, ndb);
                let cached_timeline_result = if notes.is_empty() {
                    None
                } else {
                    let new_notes = notes.iter().map(|n| n.key).collect();
                    Some(TimelineOpenResult::new_notes(new_notes, id))
                };

                // we can't insert and update the VirtualList now, because we
                // are already borrowing it mutably. Let's pass it as a
                // result instead
                //
                // holder.get_view().insert(&notes); <-- no
                cached_timeline_result
            }

            Vitality::Fresh(_timeline) => None,
        };

        let sub_id = if let Some(sub) = self.subscription(id) {
            if let Some(multi_subscriber) = sub {
                multi_subscriber.subscribe(ndb, pool);
                multi_subscriber.sub.as_ref().map(|s| s.local)
            } else {
                let mut multi_sub = MultiSubscriber::new(id.filters());
                multi_sub.subscribe(ndb, pool);
                let sub_id = multi_sub.sub.as_ref().map(|s| s.local);
                *sub = Some(multi_sub);
                sub_id
            }
        } else {
            None
        };

        let timeline = self.get_expected_mut(id);
        if let Some(sub_id) = sub_id {
            timeline.subscription = Some(sub_id);
        }

        // TODO: We have subscription ids tracked in different places. Fix this

        result
    }
}

/// Look for new thread notes since our last fetch
fn find_new_notes(
    notes: &[NoteRef],
    id: TimelineCacheKey<'_>,
    txn: &Transaction,
    ndb: &Ndb,
) -> Vec<NoteRef> {
    if notes.is_empty() {
        return vec![];
    }

    let last_note = notes[0];
    let filters = id.filters_since(last_note.created_at + 1);

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
