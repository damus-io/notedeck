use crate::{
    error::Error,
    multi_subscriber::MultiSubscriber,
    subscriptions::{self, SubKind, Subscriptions},
    timeline::kind::ListKind,
    Result,
};

use notedeck::{
    filter, CachedNote, FilterError, FilterState, FilterStates, NoteCache, NoteRef, UnknownIds,
};

use egui_virtual_list::VirtualList;
use enostr::{PoolRelay, Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashSet;

use tracing::{debug, error, info, warn};

pub mod cache;
pub mod kind;
pub mod route;

pub use cache::TimelineCache;
pub use kind::{ColumnTitle, PubkeySource, ThreadSelection, TimelineKind};

//#[derive(Debug, Hash, Clone, Eq, PartialEq)]
//pub type TimelineId = TimelineKind;

/*

impl TimelineId {
    pub fn kind(&self) -> &TimelineKind {
        &self.kind
    }

    pub fn new(id: TimelineKind) -> Self {
        TimelineId(id)
    }

    pub fn profile(pubkey: Pubkey) -> Self {
        TimelineId::new(TimelineKind::Profile(PubkeySource::pubkey(pubkey)))
    }
}

impl fmt::Display for TimelineId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "TimelineId({})", self.0)
    }
}
*/

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

    pub fn only_notes_and_replies() -> Vec<Self> {
        vec![TimelineTab::new(ViewFilter::NotesAndReplies)]
    }

    pub fn no_replies() -> Vec<Self> {
        vec![TimelineTab::new(ViewFilter::Notes)]
    }

    pub fn full_tabs() -> Vec<Self> {
        vec![
            TimelineTab::new(ViewFilter::Notes),
            TimelineTab::new(ViewFilter::NotesAndReplies),
        ]
    }

    pub fn new_with_capacity(filter: ViewFilter, cap: usize) -> Self {
        let selection = 0i32;
        let mut list = VirtualList::new();
        list.hide_on_resize(None);
        list.over_scan(50.0);
        let list = Rc::new(RefCell::new(list));
        let notes: Vec<NoteRef> = Vec::with_capacity(cap);

        TimelineTab {
            notes,
            selection,
            filter,
            list,
        }
    }

    fn insert(&mut self, new_refs: &[NoteRef], reversed: bool) {
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
                        debug!("inserting {} new notes at start", new_refs.len());
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
    pub kind: TimelineKind,
    // We may not have the filter loaded yet, so let's make it an option so
    // that codepaths have to explicitly handle it
    pub filter: FilterStates,
    pub views: Vec<TimelineTab>,
    pub selected_view: usize,
    /// Notes polled from the database but not yet shown in the UI.
    pub pending_notes: Vec<NoteRef>,

    pub subscription: Option<MultiSubscriber>,
}

impl Timeline {
    /// Create a timeline from a contact list
    pub fn contact_list(contact_list: &Note, pubkey: &[u8; 32]) -> Result<Self> {
        let with_hashtags = false;
        let filter = filter::filter_from_tags(contact_list, Some(pubkey), with_hashtags)?
            .into_follow_filter();

        Ok(Timeline::new(
            TimelineKind::contact_list(Pubkey::new(*pubkey)),
            FilterState::ready(filter),
            TimelineTab::full_tabs(),
        ))
    }

    pub fn thread(selection: ThreadSelection) -> Self {
        let filter = vec![
            nostrdb::Filter::new()
                .kinds([1])
                .event(selection.root_id.bytes())
                .build(),
            nostrdb::Filter::new()
                .ids([selection.root_id.bytes()])
                .limit(1)
                .build(),
        ];
        Timeline::new(
            TimelineKind::Thread(selection),
            FilterState::ready(filter),
            TimelineTab::only_notes_and_replies(),
        )
    }

    pub fn last_per_pubkey(list: &Note, list_kind: &ListKind) -> Result<Self> {
        let kind = 1;
        let notes_per_pk = 1;
        let filter = filter::last_n_per_pubkey_from_tags(list, kind, notes_per_pk)?;

        Ok(Timeline::new(
            TimelineKind::last_per_pubkey(*list_kind),
            FilterState::ready(filter),
            TimelineTab::only_notes_and_replies(),
        ))
    }

    pub fn hashtag(hashtag: String) -> Self {
        let hashtag = hashtag.to_lowercase();
        let htag: &str = &hashtag;
        let filter = Filter::new()
            .kinds([1])
            .limit(filter::default_limit())
            .tags([htag], 't')
            .build();

        Timeline::new(
            TimelineKind::Hashtag(hashtag),
            FilterState::ready(vec![filter]),
            TimelineTab::only_notes_and_replies(),
        )
    }

    pub fn make_view_id(id: &TimelineKind, selected_view: usize) -> egui::Id {
        egui::Id::new((id, selected_view))
    }

    pub fn view_id(&self) -> egui::Id {
        Timeline::make_view_id(&self.kind, self.selected_view)
    }

    pub fn new(kind: TimelineKind, filter_state: FilterState, views: Vec<TimelineTab>) -> Self {
        Self {
            kind,
            filter: FilterStates::new(filter_state),
            views,
            selected_view: 0,
            pending_notes: Vec::new(),
            subscription: None,
        }
    }

    pub fn current_view(&self) -> &TimelineTab {
        &self.views[self.selected_view]
    }

    pub fn current_view_mut(&mut self) -> &mut TimelineTab {
        &mut self.views[self.selected_view]
    }

    /// Get the note refs for NotesAndReplies. If we only have Notes, then
    /// just return that instead
    pub fn all_or_any_notes(&self) -> &[NoteRef] {
        self.notes(ViewFilter::NotesAndReplies).unwrap_or_else(|| {
            self.notes(ViewFilter::Notes)
                .expect("should have at least notes")
        })
    }

    pub fn notes(&self, view: ViewFilter) -> Option<&[NoteRef]> {
        self.view(view).map(|v| &*v.notes)
    }

    pub fn view(&self, view: ViewFilter) -> Option<&TimelineTab> {
        self.views.iter().find(|tab| tab.filter == view)
    }

    pub fn view_mut(&mut self, view: ViewFilter) -> Option<&mut TimelineTab> {
        self.views.iter_mut().find(|tab| tab.filter == view)
    }

    /// Initial insert of notes into a timeline. Subsequent inserts should
    /// just use the insert function
    pub fn insert_new(
        &mut self,
        txn: &Transaction,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        notes: &[NoteRef],
    ) {
        let filters = {
            let views = &self.views;
            let filters: Vec<fn(&CachedNote, &Note) -> bool> =
                views.iter().map(|v| v.filter.filter()).collect();
            filters
        };

        for note_ref in notes {
            for (view, filter) in filters.iter().enumerate() {
                if let Ok(note) = ndb.get_note_by_key(txn, note_ref.key) {
                    if filter(
                        note_cache.cached_note_or_insert_mut(note_ref.key, &note),
                        &note,
                    ) {
                        self.views[view].notes.push(*note_ref)
                    }
                }
            }
        }
    }

    /// The main function used for inserting notes into timelines. Handles
    /// inserting into multiple views if we have them. All timeline note
    /// insertions should use this function.
    pub fn insert(
        &mut self,
        new_note_ids: &[NoteKey],
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<()> {
        let mut new_refs: Vec<(Note, NoteRef)> = Vec::with_capacity(new_note_ids.len());

        for key in new_note_ids {
            let note = if let Ok(note) = ndb.get_note_by_key(txn, *key) {
                note
            } else {
                error!("hit race condition in poll_notes_into_view: https://github.com/damus-io/nostrdb/issues/35 note {:?} was not added to timeline", key);
                continue;
            };

            // Ensure that unknown ids are captured when inserting notes
            // into the timeline
            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

            let created_at = note.created_at();
            new_refs.push((
                note,
                NoteRef {
                    key: *key,
                    created_at,
                },
            ));
        }

        for view in &mut self.views {
            match view.filter {
                ViewFilter::NotesAndReplies => {
                    let refs: Vec<NoteRef> = new_refs.iter().map(|(_note, nr)| *nr).collect();

                    view.insert(&refs, reversed);
                }

                ViewFilter::Notes => {
                    let mut filtered_refs = Vec::with_capacity(new_refs.len());
                    for (note, nr) in &new_refs {
                        let cached_note = note_cache.cached_note_or_insert(nr.key, note);

                        if ViewFilter::filter_notes(cached_note, note) {
                            filtered_refs.push(*nr);
                        }
                    }

                    view.insert(&filtered_refs, reversed);
                }
            }
        }

        Ok(())
    }

    /// Adds newly polled notes to the `pending_notes` list.
    /// Returns true if new notes were added.
    pub fn poll_notes_into_pending(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) -> Result<bool> {
        if !self.kind.should_subscribe_locally() {
            // don't need to poll for timelines that don't have local subscriptions
            return Ok(false);
        }

        let sub = self
            .subscription
            .as_ref()
            .and_then(|s| s.local_subid)
            .ok_or(Error::App(notedeck::Error::no_active_sub()))?;

        let new_note_ids = ndb.poll_for_notes(sub, 500);
        if new_note_ids.is_empty() {
            return Ok(false);
        } else {
            debug!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
        }

        // Fetch NoteRefs for the new NoteKeys and add to pending_notes
        let mut new_refs: Vec<NoteRef> = Vec::with_capacity(new_note_ids.len());
        for key in new_note_ids {
            let note = match ndb.get_note_by_key(txn, key) {
                Ok(note) => note,
                Err(_) => {
                    error!(
                        "hit race condition in poll_notes_into_pending: note {:?} not found",
                        key
                    );
                    continue;
                }
            };

            // Ensure that unknown ids are captured (needed for profile info etc.)
            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

            let created_at = note.created_at();
            new_refs.push(NoteRef {
                key,
                created_at,
            });
        }

        // Add to pending, ensuring no duplicates and maintaining order (newest first)
        // We assume poll_for_notes returns newest first.
        let mut existing_pending_keys: HashSet<_> = self.pending_notes.iter().map(|nr| nr.key).collect();
        let mut added = false;
        for new_ref in new_refs.into_iter().rev() { // Iterate reversed to prepend correctly
            if existing_pending_keys.insert(new_ref.key) {
                self.pending_notes.insert(0, new_ref); // Prepend to keep newest first
                added = true;
            }
        }

        Ok(added)
    }

    /// Applies the notes currently in `pending_notes` to the visible timeline views.
    pub fn apply_pending_notes(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) -> Result<()> {
        if self.pending_notes.is_empty() {
            return Ok(());
        }

        let notes_to_apply = std::mem::take(&mut self.pending_notes);
        let reversed = matches!(self.kind, TimelineKind::Thread(_)); // Apply reversed logic if needed

        // Fetch full notes for applying filters (slightly inefficient but necessary for Notes filter)
        let mut fetched_notes: Vec<(Note, NoteRef)> = Vec::with_capacity(notes_to_apply.len());
        for key_ref in &notes_to_apply {
             let note = match ndb.get_note_by_key(txn, key_ref.key) {
                Ok(note) => note,
                Err(_) => {
                    // Note might have been deleted between polling and applying
                    error!(
                        "Failed to fetch note {:?} while applying pending notes",
                        key_ref.key
                    );
                    continue;
                }
            };
            // Re-check unknown IDs just in case?
            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);
            fetched_notes.push((note, *key_ref));
        }


        for view in &mut self.views {
            match view.filter {
                ViewFilter::NotesAndReplies => {
                    // For this view, we just need the NoteRefs
                    let refs_to_insert: Vec<NoteRef> = notes_to_apply.iter().copied().collect();
                     if !refs_to_insert.is_empty() {
                        view.insert(&refs_to_insert, reversed);
                    }
                }
                ViewFilter::Notes => {
                    let mut filtered_refs = Vec::with_capacity(fetched_notes.len());
                    for (note, nr) in &fetched_notes {
                        let cached_note = note_cache.cached_note_or_insert(nr.key, note);
                        if ViewFilter::filter_notes(cached_note, note) {
                            filtered_refs.push(*nr);
                        }
                    }
                     if !filtered_refs.is_empty() {
                        view.insert(&filtered_refs, reversed);
                    }
                }
            }
        }

        Ok(())
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
#[allow(clippy::too_many_arguments)]
pub fn setup_new_timeline(
    timeline: &mut Timeline,
    ndb: &Ndb,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    since_optimize: bool,
) {
    // if we're ready, setup local subs
    if is_timeline_ready(ndb, pool, note_cache, timeline) {
        if let Err(err) = setup_timeline_nostrdb_sub(ndb, note_cache, timeline) {
            error!("setup_new_timeline: {err}");
        }
    }

    for relay in &mut pool.relays {
        send_initial_timeline_filter(ndb, since_optimize, subs, relay, timeline);
    }
}

/// Send initial filters for a specific relay. This typically gets called
/// when we first connect to a new relay for the first time. For
/// situations where you are adding a new timeline, use
/// setup_new_timeline.
pub fn send_initial_timeline_filters(
    ndb: &Ndb,
    since_optimize: bool,
    timeline_cache: &mut TimelineCache,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    relay_id: &str,
) -> Option<()> {
    info!("Sending initial filters to {}", relay_id);
    let relay = &mut pool.relays.iter_mut().find(|r| r.url() == relay_id)?;

    for (_kind, timeline) in timeline_cache.timelines.iter_mut() {
        send_initial_timeline_filter(ndb, since_optimize, subs, relay, timeline);
    }

    Some(())
}

pub fn send_initial_timeline_filter(
    ndb: &Ndb,
    can_since_optimize: bool,
    subs: &mut Subscriptions,
    relay: &mut PoolRelay,
    timeline: &mut Timeline,
) {
    let filter_state = timeline.filter.get_mut(relay.url());

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

                let notes = timeline.all_or_any_notes();

                // Should we since optimize? Not always. For example
                // if we only have a few notes locally. One way to
                // determine this is by looking at the current filter
                // and seeing what its limit is. If we have less
                // notes than the limit, we might want to backfill
                // older notes
                if can_since_optimize && filter::should_since_optimize(lim, notes.len()) {
                    filter = filter::since_optimize_filter(filter, notes);
                } else {
                    warn!("Skipping since optimization for {:?}: number of local notes is less than limit, attempting to backfill.", &timeline.kind);
                }

                filter
            }).collect();

            //let sub_id = damus.gen_subid(&SubKind::Initial);
            let sub_id = subscriptions::new_sub_id();
            subs.subs.insert(sub_id.clone(), SubKind::Initial);

            if let Err(err) = relay.subscribe(sub_id, new_filters) {
                error!("error subscribing: {err}");
            }
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
    relay: &mut PoolRelay,
    timeline: &mut Timeline,
) {
    let sub_kind = SubKind::FetchingContactList(timeline.kind.clone());
    let sub_id = subscriptions::new_sub_id();
    let local_sub = ndb.subscribe(&filter).expect("sub");

    timeline.filter.set_relay_state(
        relay.url().to_string(),
        FilterState::fetching_remote(sub_id.clone(), local_sub),
    );

    subs.subs.insert(sub_id.clone(), sub_kind);

    info!("fetching contact list from {}", relay.url());
    if let Err(err) = relay.subscribe(sub_id, filter) {
        error!("error subscribing: {err}");
    }
}

fn setup_initial_timeline(
    ndb: &Ndb,
    timeline: &mut Timeline,
    note_cache: &mut NoteCache,
    filters: &[Filter],
) -> Result<()> {
    // some timelines are one-shot and a refreshed, like last_per_pubkey algo feed
    if timeline.kind.should_subscribe_locally() {
        let local_sub = ndb.subscribe(filters)?;
        match &mut timeline.subscription {
            None => {
                timeline.subscription = Some(MultiSubscriber::with_initial_local_sub(
                    local_sub,
                    filters.to_vec(),
                ));
            }

            Some(msub) => {
                msub.local_subid = Some(local_sub);
            }
        };
    }

    debug!(
        "querying nostrdb sub {:?} {:?}",
        timeline.subscription, timeline.filter
    );

    let mut lim = 0i32;
    for filter in filters {
        lim += filter.limit().unwrap_or(1) as i32;
    }

    let txn = Transaction::new(ndb)?;
    let notes: Vec<NoteRef> = ndb
        .query(&txn, filters, lim)?
        .into_iter()
        .map(NoteRef::from_query_result)
        .collect();

    timeline.insert_new(&txn, ndb, note_cache, &notes);

    Ok(())
}

pub fn setup_initial_nostrdb_subs(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    timeline_cache: &mut TimelineCache,
) -> Result<()> {
    for (_kind, timeline) in timeline_cache.timelines.iter_mut() {
        if let Err(err) = setup_timeline_nostrdb_sub(ndb, note_cache, timeline) {
            error!("setup_initial_nostrdb_subs: {err}");
        }
    }

    Ok(())
}

fn setup_timeline_nostrdb_sub(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    timeline: &mut Timeline,
) -> Result<()> {
    let filter_state = timeline
        .filter
        .get_any_ready()
        .ok_or(Error::App(notedeck::Error::empty_contact_list()))?
        .to_owned();

    setup_initial_timeline(ndb, timeline, note_cache, &filter_state)?;

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
    let with_hashtags = false;

    let filter = {
        let txn = Transaction::new(ndb).expect("txn");
        let note = ndb.get_note_by_key(&txn, note_key).expect("note");
        let add_pk = timeline.kind.pubkey().map(|pk| pk.bytes());
        filter::filter_from_tags(&note, add_pk, with_hashtags).map(|f| f.into_follow_filter())
    };

    // TODO: into_follow_filter is hardcoded to contact lists, let's generalize
    match filter {
        Err(notedeck::Error::Filter(e)) => {
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
            setup_initial_timeline(ndb, timeline, note_cache, &filter).expect("setup init");
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
