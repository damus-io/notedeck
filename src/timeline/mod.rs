use crate::{
    column::Columns,
    decks::DecksCache,
    error::{Error, FilterError},
    filter::{self, FilterState, FilterStates},
    muted::MuteFun,
    note::NoteRef,
    notecache::{CachedNote, NoteCache},
    subscriptions::{self, SubKind, Subscriptions},
    unknowns::UnknownIds,
    Result,
};

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use egui_virtual_list::VirtualList;
use enostr::{Relay, RelayPool};
use nostrdb::{Filter, Ndb, Note, Subscription, Transaction};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::hash::Hash;
use std::rc::Rc;

use tracing::{debug, error, info, warn};

pub mod kind;
pub mod route;

pub use kind::{PubkeySource, TimelineKind};
pub use route::TimelineRoute;

#[derive(Debug, Hash, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TimelineId(u32);

impl TimelineId {
    pub fn new(id: u32) -> Self {
        TimelineId(id)
    }
}

impl fmt::Display for TimelineId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TimelineId({})", self.0)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ViewFilter {
    Notes,

    #[default]
    NotesAndReplies,
}

impl ViewFilter {
    pub fn name(&self) -> &'static str {
        match self {
            ViewFilter::Notes => "Notes",
            ViewFilter::NotesAndReplies => "Notes & Replies",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            ViewFilter::Notes => 0,
            ViewFilter::NotesAndReplies => 1,
        }
    }

    pub fn filter_notes(cache: &CachedNote, note: &Note) -> bool {
        !cache.reply.borrow(note.tags()).is_reply()
    }

    fn identity(_cache: &CachedNote, _note: &Note) -> bool {
        true
    }

    pub fn filter(&self) -> fn(&CachedNote, &Note) -> bool {
        match self {
            ViewFilter::Notes => ViewFilter::filter_notes,
            ViewFilter::NotesAndReplies => ViewFilter::identity,
        }
    }
}

/// A timeline view is a filtered view of notes in a timeline. Two standard views
/// are "Notes" and "Notes & Replies". A timeline is associated with a Filter,
/// but a TimelineTab is a further filtered view of this Filter that can't
/// be captured by a Filter itself.
#[derive(Default, Debug)]
pub struct TimelineTab {
    pub notes: Vec<NoteRef>,
    pub selection: i32,
    pub filter: ViewFilter,
    pub list: Rc<RefCell<VirtualList>>,
}

impl TimelineTab {
    pub fn new(filter: ViewFilter) -> Self {
        TimelineTab::new_with_capacity(filter, 1000)
    }

    pub fn new_with_capacity(filter: ViewFilter, cap: usize) -> Self {
        let selection = 0i32;
        let mut list = VirtualList::new();
        list.hide_on_resize(None);
        list.over_scan(1000.0);
        let list = Rc::new(RefCell::new(list));
        let notes: Vec<NoteRef> = Vec::with_capacity(cap);

        TimelineTab {
            notes,
            selection,
            filter,
            list,
        }
    }

    pub fn insert(&mut self, new_refs: &[NoteRef], reversed: bool) {
        if new_refs.is_empty() {
            return;
        }
        let num_prev_items = self.notes.len();
        let (notes, merge_kind) = crate::timeline::merge_sorted_vecs(&self.notes, new_refs);

        self.notes = notes;
        let new_items = self.notes.len() - num_prev_items;

        // TODO: technically items could have been added inbetween
        if new_items > 0 {
            let mut list = self.list.borrow_mut();

            match merge_kind {
                // TODO: update egui_virtual_list to support spliced inserts
                MergeKind::Spliced => {
                    debug!(
                        "spliced when inserting {} new notes, resetting virtual list",
                        new_refs.len()
                    );
                    list.reset();
                }
                MergeKind::FrontInsert => {
                    // only run this logic if we're reverse-chronological
                    // reversed in this case means chronological, since the
                    // default is reverse-chronological. yeah it's confusing.
                    if !reversed {
                        list.items_inserted_at_start(new_items);
                    }
                }
            }
        }
    }

    pub fn select_down(&mut self) {
        debug!("select_down {}", self.selection + 1);
        if self.selection + 1 > self.notes.len() as i32 {
            return;
        }

        self.selection += 1;
    }

    pub fn select_up(&mut self) {
        debug!("select_up {}", self.selection - 1);
        if self.selection - 1 < 0 {
            return;
        }

        self.selection -= 1;
    }
}

/// A column in a deck. Holds navigation state, loaded notes, column kind, etc.
#[derive(Debug)]
pub struct Timeline {
    pub id: TimelineId,
    pub kind: TimelineKind,
    // We may not have the filter loaded yet, so let's make it an option so
    // that codepaths have to explicitly handle it
    pub filter: FilterStates,
    pub views: Vec<TimelineTab>,
    pub selected_view: i32,

    /// Our nostrdb subscription
    pub subscription: Option<Subscription>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SerializableTimeline {
    pub id: TimelineId,
    pub kind: TimelineKind,
}

impl SerializableTimeline {
    pub fn into_timeline(self, ndb: &Ndb, deck_user_pubkey: Option<&[u8; 32]>) -> Option<Timeline> {
        self.kind.into_timeline(ndb, deck_user_pubkey)
    }
}

impl Timeline {
    /// Create a timeline from a contact list
    pub fn contact_list(contact_list: &Note, pk_src: PubkeySource) -> Result<Self> {
        let filter = filter::filter_from_tags(contact_list)?.into_follow_filter();

        Ok(Timeline::new(
            TimelineKind::contact_list(pk_src),
            FilterState::ready(filter),
        ))
    }

    pub fn hashtag(hashtag: String) -> Self {
        let filter = Filter::new()
            .kinds([1])
            .limit(filter::default_limit())
            .tags([hashtag.clone()], 't')
            .build();

        Timeline::new(
            TimelineKind::Hashtag(hashtag),
            FilterState::ready(vec![filter]),
        )
    }

    pub fn make_view_id(id: TimelineId, selected_view: i32) -> egui::Id {
        egui::Id::new((id, selected_view))
    }

    pub fn view_id(&self) -> egui::Id {
        Timeline::make_view_id(self.id, self.selected_view)
    }

    pub fn new(kind: TimelineKind, filter_state: FilterState) -> Self {
        // global unique id for all new timelines
        static UIDS: AtomicU32 = AtomicU32::new(0);

        let filter = FilterStates::new(filter_state);
        let subscription: Option<Subscription> = None;
        let notes = TimelineTab::new(ViewFilter::Notes);
        let replies = TimelineTab::new(ViewFilter::NotesAndReplies);
        let views = vec![notes, replies];
        let selected_view = 0;
        let id = TimelineId::new(UIDS.fetch_add(1, Ordering::Relaxed));

        Timeline {
            id,
            kind,
            filter,
            views,
            subscription,
            selected_view,
        }
    }

    pub fn current_view(&self) -> &TimelineTab {
        &self.views[self.selected_view as usize]
    }

    pub fn current_view_mut(&mut self) -> &mut TimelineTab {
        &mut self.views[self.selected_view as usize]
    }

    pub fn notes(&self, view: ViewFilter) -> &[NoteRef] {
        &self.views[view.index()].notes
    }

    pub fn view(&self, view: ViewFilter) -> &TimelineTab {
        &self.views[view.index()]
    }

    pub fn view_mut(&mut self, view: ViewFilter) -> &mut TimelineTab {
        &mut self.views[view.index()]
    }

    pub fn poll_notes_into_view(
        timeline_idx: usize,
        mut timelines: Vec<&mut Timeline>,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        is_muted: &MuteFun,
    ) -> Result<()> {
        let timeline = timelines
            .get_mut(timeline_idx)
            .ok_or(Error::TimelineNotFound)?;
        let sub = timeline.subscription.ok_or(Error::no_active_sub())?;

        let new_note_ids = ndb.poll_for_notes(sub, 500);
        if new_note_ids.is_empty() {
            return Ok(());
        } else {
            debug!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
        }

        let mut new_refs: Vec<(Note, NoteRef)> = Vec::with_capacity(new_note_ids.len());

        for key in new_note_ids {
            let note = if let Ok(note) = ndb.get_note_by_key(txn, key) {
                note
            } else {
                error!("hit race condition in poll_notes_into_view: https://github.com/damus-io/nostrdb/issues/35 note {:?} was not added to timeline", key);
                continue;
            };
            if is_muted(&note) {
                continue;
            }

            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

            let created_at = note.created_at();
            new_refs.push((note, NoteRef { key, created_at }));
        }

        // We're assuming reverse-chronological here (timelines). This
        // flag ensures we trigger the items_inserted_at_start
        // optimization in VirtualList. We need this flag because we can
        // insert notes into chronological order sometimes, and this
        // optimization doesn't make sense in those situations.
        let reversed = false;

        // ViewFilter::NotesAndReplies
        {
            let refs: Vec<NoteRef> = new_refs.iter().map(|(_note, nr)| *nr).collect();

            let reversed = false;
            timeline
                .view_mut(ViewFilter::NotesAndReplies)
                .insert(&refs, reversed);
        }

        //
        // handle the filtered case (ViewFilter::Notes, no replies)
        //
        // TODO(jb55): this is mostly just copied from above, let's just use a loop
        //             I initially tried this but ran into borrow checker issues
        {
            let mut filtered_refs = Vec::with_capacity(new_refs.len());
            for (note, nr) in &new_refs {
                let cached_note = note_cache.cached_note_or_insert(nr.key, note);

                if ViewFilter::filter_notes(cached_note, note) {
                    filtered_refs.push(*nr);
                }
            }

            timeline
                .view_mut(ViewFilter::Notes)
                .insert(&filtered_refs, reversed);
        }

        Ok(())
    }

    pub fn as_serializable_timeline(&self) -> SerializableTimeline {
        SerializableTimeline {
            id: self.id,
            kind: self.kind.clone(),
        }
    }
}

pub enum MergeKind {
    FrontInsert,
    Spliced,
}

pub fn merge_sorted_vecs<T: Ord + Copy>(vec1: &[T], vec2: &[T]) -> (Vec<T>, MergeKind) {
    let mut merged = Vec::with_capacity(vec1.len() + vec2.len());
    let mut i = 0;
    let mut j = 0;
    let mut result: Option<MergeKind> = None;

    while i < vec1.len() && j < vec2.len() {
        if vec1[i] <= vec2[j] {
            if result.is_none() && j < vec2.len() {
                // if we're pushing from our large list and still have
                // some left in vec2, then this is a splice
                result = Some(MergeKind::Spliced);
            }
            merged.push(vec1[i]);
            i += 1;
        } else {
            merged.push(vec2[j]);
            j += 1;
        }
    }

    // Append any remaining elements from either vector
    if i < vec1.len() {
        merged.extend_from_slice(&vec1[i..]);
    }
    if j < vec2.len() {
        merged.extend_from_slice(&vec2[j..]);
    }

    (merged, result.unwrap_or(MergeKind::FrontInsert))
}

/// When adding a new timeline, we may have a situation where the
/// FilterState is NeedsRemote. This can happen if we don't yet have the
/// contact list, etc. For these situations, we query all of the relays
/// with the same sub_id. We keep track of this sub_id and update the
/// filter with the latest version of the returned filter (ie contact
/// list) when they arrive.
///
/// We do this by maintaining this sub_id in the filter state, even when
/// in the ready state. See: [`FilterReady`]
pub fn setup_new_timeline(
    timeline: &mut Timeline,
    ndb: &Ndb,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    since_optimize: bool,
    is_muted: &MuteFun,
) {
    // if we're ready, setup local subs
    if is_timeline_ready(ndb, pool, note_cache, timeline, is_muted) {
        if let Err(err) = setup_timeline_nostrdb_sub(ndb, note_cache, timeline, is_muted) {
            error!("setup_new_timeline: {err}");
        }
    }

    for relay in &mut pool.relays {
        send_initial_timeline_filter(ndb, since_optimize, subs, &mut relay.relay, timeline);
    }
}

/// Send initial filters for a specific relay. This typically gets called
/// when we first connect to a new relay for the first time. For
/// situations where you are adding a new timeline, use
/// setup_new_timeline.
pub fn send_initial_timeline_filters(
    ndb: &Ndb,
    since_optimize: bool,
    columns: &mut Columns,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    relay_id: &str,
) -> Option<()> {
    info!("Sending initial filters to {}", relay_id);
    let relay = &mut pool
        .relays
        .iter_mut()
        .find(|r| r.relay.url == relay_id)?
        .relay;

    for timeline in columns.timelines_mut() {
        send_initial_timeline_filter(ndb, since_optimize, subs, relay, timeline);
    }

    Some(())
}

pub fn send_initial_timeline_filter(
    ndb: &Ndb,
    can_since_optimize: bool,
    subs: &mut Subscriptions,
    relay: &mut Relay,
    timeline: &mut Timeline,
) {
    let filter_state = timeline.filter.get(&relay.url);

    match filter_state {
        FilterState::Broken(err) => {
            error!(
                "FetchingRemote state in broken state when sending initial timeline filter? {err}"
            );
        }

        FilterState::FetchingRemote(_unisub) => {
            error!("FetchingRemote state when sending initial timeline filter?");
        }

        FilterState::GotRemote(_sub) => {
            error!("GotRemote state when sending initial timeline filter?");
        }

        FilterState::Ready(filter) => {
            let filter = filter.to_owned();
            let new_filters = filter.into_iter().map(|f| {
                // limit the size of remote filters
                let default_limit = filter::default_remote_limit();
                let mut lim = f.limit().unwrap_or(default_limit);
                let mut filter = f;
                if lim > default_limit {
                    lim = default_limit;
                    filter = filter.limit_mut(lim);
                }

                let notes = timeline.notes(ViewFilter::NotesAndReplies);

                // Should we since optimize? Not always. For example
                // if we only have a few notes locally. One way to
                // determine this is by looking at the current filter
                // and seeing what its limit is. If we have less
                // notes than the limit, we might want to backfill
                // older notes
                if can_since_optimize && filter::should_since_optimize(lim, notes.len()) {
                    filter = filter::since_optimize_filter(filter, notes);
                } else {
                    warn!("Skipping since optimization for {:?}: number of local notes is less than limit, attempting to backfill.", filter);
                }

                filter
            }).collect();

            //let sub_id = damus.gen_subid(&SubKind::Initial);
            let sub_id = subscriptions::new_sub_id();
            subs.subs.insert(sub_id.clone(), SubKind::Initial);

            relay.subscribe(sub_id, new_filters);
        }

        // we need some data first
        FilterState::NeedsRemote(filter) => {
            fetch_contact_list(filter.to_owned(), ndb, subs, relay, timeline)
        }
    }
}

fn fetch_contact_list(
    filter: Vec<Filter>,
    ndb: &Ndb,
    subs: &mut Subscriptions,
    relay: &mut Relay,
    timeline: &mut Timeline,
) {
    let sub_kind = SubKind::FetchingContactList(timeline.id);
    let sub_id = subscriptions::new_sub_id();
    let local_sub = ndb.subscribe(&filter).expect("sub");

    timeline.filter.set_relay_state(
        relay.url.clone(),
        FilterState::fetching_remote(sub_id.clone(), local_sub),
    );

    subs.subs.insert(sub_id.clone(), sub_kind);

    info!("fetching contact list from {}", &relay.url);
    relay.subscribe(sub_id, filter);
}

fn setup_initial_timeline(
    ndb: &Ndb,
    timeline: &mut Timeline,
    note_cache: &mut NoteCache,
    filters: &[Filter],
    is_muted: &MuteFun,
) -> Result<()> {
    timeline.subscription = Some(ndb.subscribe(filters)?);
    let txn = Transaction::new(ndb)?;
    debug!(
        "querying nostrdb sub {:?} {:?}",
        timeline.subscription, timeline.filter
    );
    let lim = filters[0].limit().unwrap_or(crate::filter::default_limit()) as i32;
    let notes = ndb
        .query(&txn, filters, lim)?
        .into_iter()
        .map(NoteRef::from_query_result)
        .collect();

    copy_notes_into_timeline(timeline, &txn, ndb, note_cache, notes, is_muted);

    Ok(())
}

pub fn copy_notes_into_timeline(
    timeline: &mut Timeline,
    txn: &Transaction,
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    notes: Vec<NoteRef>,
    is_muted: &MuteFun,
) {
    let filters = {
        let views = &timeline.views;
        let filters: Vec<fn(&CachedNote, &Note) -> bool> =
            views.iter().map(|v| v.filter.filter()).collect();
        filters
    };

    for note_ref in notes {
        for (view, filter) in filters.iter().enumerate() {
            if let Ok(note) = ndb.get_note_by_key(txn, note_ref.key) {
                if is_muted(&note) {
                    continue;
                }
                if filter(
                    note_cache.cached_note_or_insert_mut(note_ref.key, &note),
                    &note,
                ) {
                    timeline.views[view].notes.push(note_ref)
                }
            }
        }
    }
}

pub fn setup_initial_nostrdb_subs(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    decks_cache: &mut DecksCache,
    is_muted: &MuteFun,
) -> Result<()> {
    for decks in decks_cache.get_all_decks_mut() {
        for deck in decks.decks_mut() {
            for timeline in deck.columns_mut().timelines_mut() {
                if let Err(err) = setup_timeline_nostrdb_sub(ndb, note_cache, timeline, is_muted) {
                    error!("setup_initial_nostrdb_subs: {err}");
                }
            }
        }
    }

    Ok(())
}

fn setup_timeline_nostrdb_sub(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    timeline: &mut Timeline,
    is_muted: &MuteFun,
) -> Result<()> {
    let filter_state = timeline
        .filter
        .get_any_ready()
        .ok_or(Error::empty_contact_list())?
        .to_owned();

    setup_initial_timeline(ndb, timeline, note_cache, &filter_state, is_muted)?;

    Ok(())
}

/// Check our timeline filter and see if we have any filter data ready.
/// Our timelines may require additional data before it is functional. For
/// example, when we have to fetch a contact list before we do the actual
/// following list query.
pub fn is_timeline_ready(
    ndb: &Ndb,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    timeline: &mut Timeline,
    is_muted: &MuteFun,
) -> bool {
    // TODO: we should debounce the filter states a bit to make sure we have
    // seen all of the different contact lists from each relay
    if let Some(_f) = timeline.filter.get_any_ready() {
        return true;
    }

    let (relay_id, sub) = if let Some((relay_id, sub)) = timeline.filter.get_any_gotremote() {
        (relay_id.to_string(), sub)
    } else {
        return false;
    };

    // We got at least one eose for our filter request. Let's see
    // if nostrdb is done processing it yet.
    let res = ndb.poll_for_notes(sub, 1);
    if res.is_empty() {
        debug!(
            "check_timeline_filter_state: no notes found (yet?) for timeline {:?}",
            timeline
        );
        return false;
    }

    info!("notes found for contact timeline after GotRemote!");

    let note_key = res[0];

    let filter = {
        let txn = Transaction::new(ndb).expect("txn");
        let note = ndb.get_note_by_key(&txn, note_key).expect("note");
        filter::filter_from_tags(&note).map(|f| f.into_follow_filter())
    };

    // TODO: into_follow_filter is hardcoded to contact lists, let's generalize
    match filter {
        Err(Error::Filter(e)) => {
            error!("got broken when building filter {e}");
            timeline
                .filter
                .set_relay_state(relay_id, FilterState::broken(e));
            false
        }
        Err(err) => {
            error!("got broken when building filter {err}");
            timeline
                .filter
                .set_relay_state(relay_id, FilterState::broken(FilterError::EmptyContactList));
            false
        }
        Ok(filter) => {
            // we just switched to the ready state, we should send initial
            // queries and setup the local subscription
            info!("Found contact list! Setting up local and remote contact list query");
            setup_initial_timeline(ndb, timeline, note_cache, &filter, is_muted)
                .expect("setup init");
            timeline
                .filter
                .set_relay_state(relay_id, FilterState::ready(filter.clone()));

            //let ck = &timeline.kind;
            //let subid = damus.gen_subid(&SubKind::Column(ck.clone()));
            let subid = subscriptions::new_sub_id();
            pool.subscribe(subid, filter);
            true
        }
    }
}
