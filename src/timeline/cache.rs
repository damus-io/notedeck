use crate::{
    actionbar::BarResult,
    error::Error,
    note::{NoteRef, RootNoteId},
    notecache::NoteCache,
    subscriptions::SubRefs,
    timeline::{Timeline, TimelineTab, ViewFilter},
};
use enostr::{Pubkey, RelayPool};
use nostrdb::{Filter, FilterBuilder, Ndb, Transaction};
use std::collections::HashMap;
use tracing::{debug, info, warn};

#[derive(Default)]
pub struct TimelineCache {
    pub columns: Vec<CachedTimeline>,
    pub threads: HashMap<RootNoteId, CachedTimeline>,
    pub profiles: HashMap<Pubkey, CachedTimeline>,
}

pub struct CachedTimeline {
    timeline: Timeline,
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

#[derive(Hash, Debug)]
pub enum TimelineCacheKey {
    Profile(Pubkey),
    Thread(RootNoteId),
}

impl TimelineCacheKey {
    pub fn profile(pubkey: &[u8; 32]) -> Self {
        Self::Profile(Pubkey::new(*pubkey))
    }

    pub fn thread(root_id: RootNoteId) -> Self {
        Self::Thread(RootNoteId::new(root_id))
    }

    pub fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::Profile(pk) => pk.bytes(),
            Self::Thread(root_id) => root_id.bytes(),
        }
    }
}

impl TimelineCacheKey {
    /// The filters used to update our timeline cache
    pub fn filters_raw(&self) -> Vec<FilterBuilder> {
        match self {
            TimelineCacheKey::Thread(root_id) => vec![
                nostrdb::Filter::new().kinds([1]).event(root_id.bytes()),
                nostrdb::Filter::new().ids([root_id.bytes()]).limit(1),
            ],

            TimelineCacheKey::Profile(pubkey) => vec![Filter::new()
                .authors([pubkey.bytes()])
                .kinds([1])
                .limit(filter::default_limit())],
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
    fn contains_key(&self, key: &TimelineCacheKey) -> bool {
        match key {
            TimelineCacheKey::Profile(pubkey) => self.profiles.contains_key(pubkey),
            TimelineCacheKey::Thread(root_id) => self.threads.contains_key(root_id),
        }
    }

    fn insert(
        &mut self,
        key: &TimelineCacheKey,
        timeline: CachedTimeline,
    ) -> Option<CachedTimeline> {
        match key {
            TimelineCacheKey::Profile(pubkey) => self.profiles.insert(pubkey.to_owned(), timeline),
            TimelineCacheKey::Thread(root_id) => self.threads.insert(root_id.to_owned(), timeline),
        }
    }

    fn get_expected_mut(&mut self, key: &TimelineCacheKey) -> &mut CachedTimeline {
        match key {
            TimelineCacheKey::Profile(pubkey) => self.profiles.get_mut(pubkey),
            TimelineCacheKey::Thread(root_id) => self.threads.get_mut(root_id),
        }
        .expect("expected notes in timline cache")
    }

    /// Get and/or update the notes associated with this timeline
    pub fn notes<'a>(
        &'a mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &TimelineCacheKey,
    ) -> Vitality<'a, CachedTimeline> {
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
            debug!("got no results from NotesHolder lookup for {?:}", id);
            vec![]
        };

        if notes.is_empty() {
            warn!("NotesHolder query returned 0 notes? ")
        } else {
            info!("found NotesHolder with {} notes", notes.len());
        }

        self.insert(
            id.to_owned(),
            Self::new(txn, ndb, note_cache, id, filters, notes),
        );
        Vitality::Fresh(self.get_expected_mut(id))
    }

    pub fn open(
        &mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        pool: &mut RelayPool,
        id: &TimelineCacheKey,
        subscriptions: &mut Subscriptions,
    ) -> Option<BarResult> {
        let vitality = self.notes(ndb, note_cache, txn, id);

        let (cached_timeline, result) = match vitality {
            Vitality::Stale(cached_timeline) => {
                // The timeline cache is stale, let's update it
                let notes =
                    CachedTimeline::new_notes(&cached_timeline.get_view().notes, id, txn, ndb);
                let cached_timeline_result = if notes.is_empty() {
                    None
                } else {
                    Some(BarResult::new_notes(notes, *id))
                };

                //
                // we can't insert and update the VirtualList now, because we
                // are already borrowing it mutably. Let's pass it as a
                // result instead
                //
                // holder.get_view().insert(&notes); <-- no
                //
                (cached_timeline, cached_timeline_result)
            }

            Vitality::Fresh(thread) => (thread, None),
        };

        if let Some(sub_id) = cached_timeline.timeline().subscription {
            // if we have an active local subscription, we increase
            // the subscription count

        } else {
            // otherwise we start a new local sub and request a remote sub
            // for these filters
        }
        let subscription =
            if let Some(sub_id) =  {
                sub_id
            } else {
                cached_timeline.timeline_mut().subscription = MultiSubscriber::new(id.filters());
                cached_timeline.timeline().subscription
            };

        subscription.subscribe(ndb, pool);

        result
    }
}

impl CachedTimeline {
    pub fn new(notes: Vec<NoteRef>, timeline: Timeline) -> Self {
        Self { timeline }
    }

    pub fn subscriptions<'a>(&'a self) -> Option<SubRefs<'a>> {
        self.subscription.and_then(|ms| ms.to_subscriptions())
    }

    #[must_use = "UnknownIds::update_from_note_refs should be used on this result"]
    pub fn poll_notes_into_view(&mut self, txn: &Transaction, ndb: &Ndb) -> Result<Vec<NoteRef>> {
        if let Some(multi_subscriber) = self.get_multi_subscriber() {
            let reversed = true;
            let note_refs: Vec<NoteRef> = multi_subscriber.poll_for_notes(ndb, txn)?;
            self.get_view().insert(&note_refs, reversed);

            Ok(note_refs)
        } else {
            Err(Error::Generic(
                "NotesHolder unexpectedly has no MultiSubscriber".to_owned(),
            ))
        }
    }

    /// Look for new thread notes since our last fetch
    fn new_notes(
        notes: &[NoteRef],
        id: &TimelineCacheKey,
        txn: &Transaction,
        ndb: &Ndb,
    ) -> Vec<NoteRef> {
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
}
