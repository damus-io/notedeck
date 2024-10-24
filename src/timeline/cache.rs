use crate::{note::NoteRef, timeline::TimelineTab};
use enostr::RelayPool;
use nostrdb::{Filter, Ndb};

#[derive(Default)]
pub struct TimelineCache {
    pub id_to_object: HashMap<TimelineCacheKey, CachedTimeline>,
}

pub struct CachedTimeline {
    pub cache_type: TimelineCacheType,
    views: Vec<TimelineTab>,
    pub subscription: Option<MultiSubscriber>,
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

#[derive(Hash)]
enum TimelineCacheKey {
    Profile(Pubkey),
    Thread(RootNoteId),
}

impl TimelineCacheKey {
    fn profile(pubkey: &[u8; 32]) -> Self {
        Self::Profile(*pubkey)
    }

    fn thread(root_id: &[u8; 32]) -> Self {
        Self::Thread(*root_id)
    }

    fn bytes(&self) -> &[u8; 32] {
        match self {
            Self::Profile(pk) => &pk,
            Self::Thread(root_id) => &root_id,
        }
    }
}

enum TimelineCacheType {
    Profile,
    Thread,
}

impl TimelineCacheType {
    /// The filters used to update our timeline cache
    pub fn filters_raw(&self, root: &[u8; 32]) -> Vec<FilterBuilder> {
        match self {
            TimelineCacheType::Thread => vec![
                nostrdb::Filter::new().kinds([1]).event(root),
                nostrdb::Filter::new().ids([root]).limit(1),
            ],

            TimelineCacheType::Profile => vec![Filter::new()
                .authors([root])
                .kinds([1])
                .limit(filter::default_limit())],
        }
    }

    pub fn filters_since(&self, root: &[u8; 32], since: u64) -> Vec<Filter> {
        self.filters_raw(root)
            .into_iter()
            .map(|fb| fb.since(since).build())
            .collect()
    }

    pub fn filters(&self, root: &[u8; 32]) -> Vec<Filter> {
        self.filters_raw(root)
            .into_iter()
            .map(|mut fb| fb.build())
            .collect()
    }
}

impl TimelineCache {
    pub fn get_mut(&mut self, id: &TimelineCacheKey) -> Option<&mut CachedTimeline> {
        self.id_to_object.get_mut(id)
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

        if self.id_to_object.contains_key(id) {
            return Vitality::Stale(self.notes_expected(id));
        }

        let notes = if let Ok(results) = ndb.query(txn, filters, 1000) {
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
            Self::new(txn, ndb, note_cache, id, self.cache_type.filters(id), notes),
        );
        Vitality::Fresh(self.id_to_object.get_mut(id).unwrap())
    }

    pub fn open(
        &mut self,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        pool: &mut RelayPool,
        id: &TimelineCacheKey,
    ) -> Option<BarResult> {
        let vitality = self.notes(ndb, note_cache, txn, id);

        let (cached_timeline, result) = match vitality {
            Vitality::Stale(cached_timeline) => {
                // The NotesHolder is stale, let's update it
                let notes =
                    CachedTimeline::new_notes(&cached_timeline.get_view().notes, id, txn, ndb);
                let cached_timeline_result = if notes.is_empty() {
                    None
                } else {
                    Some(BarResult::new_notes(notes, id.to_owned()))
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

        let multi_subscriber =
            if let Some(multi_subscriber) = cached_timeline.get_multi_subscriber() {
                multi_subscriber
            } else {
                let filters = cached_timeline.cache_type.filters(id);
                cached_timeline.set_multi_subscriber(MultiSubscriber::new(filters));
                cached_timeline.get_multi_subscriber().unwrap()
            };

        multi_subscriber.subscribe(ndb, pool);

        result
    }
}

impl CachedTimeline {
    pub fn new(notes: Vec<NoteRef>, view_filters: Vec<ViewFilter>) -> Self {
        let mut init_capacity = ((notes.len() as f32) * 1.5) as usize;
        if init_capacity == 0 {
            init_capacity = 25;
        }

        let mut views: Vec<TimelineTab> = Vec::new_with_capacity(view_filters.lne());
        for view_filter in view_filters {
            let mut view = TimelineTab::new_with_capacity(view_filter, init_capacity);
            view.notes = notes;
        }

        Self {
            view,
            multi_subscriber: None,
        }
    }

    pub fn subscriptions<'a>(&'a self) -> Option<Subscriptions<'a>> {
        self.multi_subscriber.and_then(|ms| ms.to_subscriptions())
    }
 
    #[must_use = "UnknownIds::update_from_note_refs should be used on this result"]
    fn poll_notes_into_view(&mut self, txn: &Transaction, ndb: &Ndb) -> Result<Vec<NoteRef>> {
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
