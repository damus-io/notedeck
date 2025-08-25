use crate::{
    error::Error,
    multi_subscriber::TimelineSub,
    subscriptions::{self, SubKind, Subscriptions},
    timeline::{kind::ListKind, note_units::InsertManyResponse, timeline_units::NotePayload},
    Result,
};

use notedeck::{
    contacts::hybrid_contacts_filter,
    debouncer::Debouncer,
    filter::{self, HybridFilter},
    tr, Accounts, CachedNote, ContactState, FilterError, FilterState, FilterStates, Localization,
    NoteCache, NoteRef, UnknownIds,
};

use egui_virtual_list::VirtualList;
use enostr::{PoolRelay, Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::{
    cell::RefCell,
    collections::HashSet,
    time::{Duration, UNIX_EPOCH},
};
use std::{rc::Rc, time::SystemTime};

use tracing::{debug, error, info, warn};

pub mod cache;
pub mod kind;
mod note_units;
pub mod route;
pub mod thread;
mod timeline_units;
mod unit;

pub use cache::TimelineCache;
pub use kind::{ColumnTitle, PubkeySource, ThreadSelection, TimelineKind};
pub use note_units::{InsertionResponse, NoteUnits};
pub use timeline_units::{TimelineUnits, UnknownPks};
pub use unit::{CompositeUnit, NoteUnit, ReactionUnit};

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ViewFilter {
    Notes,

    #[default]
    NotesAndReplies,
}

impl ViewFilter {
    pub fn name(&self, i18n: &mut Localization) -> String {
        match self {
            ViewFilter::Notes => tr!(i18n, "Notes", "Filter label for notes only view"),
            ViewFilter::NotesAndReplies => {
                tr!(
                    i18n,
                    "Notes & Replies",
                    "Filter label for notes and replies view"
                )
            }
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
    pub units: TimelineUnits,
    pub selection: i32,
    pub filter: ViewFilter,
    pub list: Rc<RefCell<VirtualList>>,
    pub freshness: NotesFreshness,
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

        TimelineTab {
            units: TimelineUnits::with_capacity(cap),
            selection,
            filter,
            list,
            freshness: NotesFreshness::default(),
        }
    }

    fn insert<'a>(
        &mut self,
        payloads: Vec<&'a NotePayload>,
        ndb: &Ndb,
        txn: &Transaction,
        reversed: bool,
    ) -> Option<UnknownPks<'a>> {
        if payloads.is_empty() {
            return None;
        }

        let num_refs = payloads.len();

        let resp = self.units.merge_new_notes(payloads, ndb, txn);

        let InsertManyResponse::Some {
            entries_merged,
            merge_kind,
        } = resp.insertion_response
        else {
            return resp.tl_response;
        };

        let mut list = self.list.borrow_mut();

        match merge_kind {
            // TODO: update egui_virtual_list to support spliced inserts
            MergeKind::Spliced => {
                debug!("spliced when inserting {num_refs} new notes, resetting virtual list",);
                list.reset();
            }
            MergeKind::FrontInsert => {
                // only run this logic if we're reverse-chronological
                // reversed in this case means chronological, since the
                // default is reverse-chronological. yeah it's confusing.
                if !reversed {
                    debug!("inserting {num_refs} new notes at start");
                    list.items_inserted_at_start(entries_merged);
                }
            }
        };

        resp.tl_response
    }

    pub fn select_down(&mut self) {
        debug!("select_down {}", self.selection + 1);
        if self.selection + 1 > self.units.len() as i32 {
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

impl<'a> UnknownPks<'a> {
    pub fn process(&self, unknown_ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        for pk in &self.unknown_pks {
            unknown_ids.add_pubkey_if_missing(ndb, txn, pk);
        }
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

    pub subscription: TimelineSub,
}

impl Timeline {
    /// Create a timeline from a contact list
    pub fn contact_list(contact_list: &Note, pubkey: &[u8; 32]) -> Result<Self> {
        let with_hashtags = false;
        let add_pk = Some(pubkey);
        let filter = hybrid_contacts_filter(contact_list, add_pk, with_hashtags)?;

        Ok(Timeline::new(
            TimelineKind::contact_list(Pubkey::new(*pubkey)),
            FilterState::ready_hybrid(filter),
            TimelineTab::full_tabs(),
        ))
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

    pub fn hashtag(hashtag: Vec<String>) -> Self {
        let filters = hashtag
            .iter()
            .filter(|tag| !tag.is_empty())
            .map(|tag| {
                Filter::new()
                    .kinds([1])
                    .limit(filter::default_limit())
                    .tags([tag.as_str()], 't')
                    .build()
            })
            .collect::<Vec<_>>();

        Timeline::new(
            TimelineKind::Hashtag(hashtag),
            FilterState::ready(filters),
            TimelineTab::only_notes_and_replies(),
        )
    }

    pub fn make_view_id(id: &TimelineKind, col: usize, selected_view: usize) -> egui::Id {
        egui::Id::new((id, selected_view, col))
    }

    pub fn view_id(&self, col: usize) -> egui::Id {
        Timeline::make_view_id(&self.kind, col, self.selected_view)
    }

    pub fn new(kind: TimelineKind, filter_state: FilterState, views: Vec<TimelineTab>) -> Self {
        let filter = FilterStates::new(filter_state);
        let subscription = TimelineSub::default();
        let selected_view = 0;

        Timeline {
            kind,
            filter,
            views,
            subscription,
            selected_view,
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
    pub fn all_or_any_entries(&self) -> &TimelineUnits {
        self.entries(ViewFilter::NotesAndReplies)
            .unwrap_or_else(|| {
                self.entries(ViewFilter::Notes)
                    .expect("should have at least notes")
            })
    }

    pub fn entries(&self, view: ViewFilter) -> Option<&TimelineUnits> {
        self.view(view).map(|v| &v.units)
    }

    pub fn latest_note(&self, view: ViewFilter) -> Option<&NoteRef> {
        self.view(view).and_then(|v| v.units.latest())
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
    ) -> Option<UnknownPksOwned> {
        let filters = {
            let views = &self.views;
            let filters: Vec<fn(&CachedNote, &Note) -> bool> =
                views.iter().map(|v| v.filter.filter()).collect();
            filters
        };

        let mut unknown_pks = HashSet::new();
        for note_ref in notes {
            for (view, filter) in filters.iter().enumerate() {
                if let Ok(note) = ndb.get_note_by_key(txn, note_ref.key) {
                    if filter(
                        note_cache.cached_note_or_insert_mut(note_ref.key, &note),
                        &note,
                    ) {
                        if let Some(resp) = self.views[view]
                            .units
                            .merge_new_notes(
                                vec![&NotePayload {
                                    note,
                                    key: note_ref.key,
                                }],
                                ndb,
                                txn,
                            )
                            .tl_response
                        {
                            let pks: HashSet<Pubkey> = resp
                                .unknown_pks
                                .into_iter()
                                .map(|r| Pubkey::new(*r))
                                .collect();

                            unknown_pks.extend(pks);
                        }
                    }
                }
            }
        }

        Some(UnknownPksOwned { pks: unknown_pks })
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
        let mut payloads: Vec<NotePayload> = Vec::with_capacity(new_note_ids.len());

        for key in new_note_ids {
            let note = if let Ok(note) = ndb.get_note_by_key(txn, *key) {
                note
            } else {
                error!(
                    "hit race condition in poll_notes_into_view: https://github.com/damus-io/nostrdb/issues/35 note {:?} was not added to timeline",
                    key
                );
                continue;
            };

            // Ensure that unknown ids are captured when inserting notes
            // into the timeline
            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

            payloads.push(NotePayload { note, key: *key });
        }

        for view in &mut self.views {
            match view.filter {
                ViewFilter::NotesAndReplies => {
                    let res: Vec<&NotePayload<'_>> = payloads.iter().collect();
                    if let Some(res) = view.insert(res, ndb, txn, reversed) {
                        res.process(unknown_ids, ndb, txn);
                    }
                }

                ViewFilter::Notes => {
                    let mut filtered_payloads = Vec::with_capacity(payloads.len());
                    for payload in &payloads {
                        let cached_note =
                            note_cache.cached_note_or_insert(payload.key, &payload.note);

                        if ViewFilter::filter_notes(cached_note, &payload.note) {
                            filtered_payloads.push(payload);
                        }
                    }

                    if let Some(res) = view.insert(filtered_payloads, ndb, txn, reversed) {
                        res.process(unknown_ids, ndb, txn);
                    }
                }
            }
        }

        Ok(())
    }

    pub fn poll_notes_into_view(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<()> {
        if !self.kind.should_subscribe_locally() {
            // don't need to poll for timelines that don't have local subscriptions
            return Ok(());
        }

        let sub = self
            .subscription
            .get_local()
            .ok_or(Error::App(notedeck::Error::no_active_sub()))?;

        let new_note_ids = ndb.poll_for_notes(sub, 500);
        if new_note_ids.is_empty() {
            return Ok(());
        } else {
            debug!("{} new notes! {:?}", new_note_ids.len(), new_note_ids);
        }

        self.insert(&new_note_ids, ndb, txn, unknown_ids, note_cache, reversed)
    }
}

pub struct UnknownPksOwned {
    pub pks: HashSet<Pubkey>,
}

impl UnknownPksOwned {
    pub fn process(&self, ndb: &Ndb, txn: &Transaction, unknown_ids: &mut UnknownIds) {
        self.pks
            .iter()
            .for_each(|p| unknown_ids.add_pubkey_if_missing(ndb, txn, p));
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
    txn: &Transaction,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    note_cache: &mut NoteCache,
    since_optimize: bool,
    accounts: &Accounts,
    unknown_ids: &mut UnknownIds,
) {
    // if we're ready, setup local subs
    if is_timeline_ready(ndb, pool, note_cache, timeline, accounts, unknown_ids) {
        if let Err(err) = setup_timeline_nostrdb_sub(ndb, txn, note_cache, timeline, unknown_ids) {
            error!("setup_new_timeline: {err}");
        }
    }

    for relay in &mut pool.relays {
        send_initial_timeline_filter(since_optimize, subs, relay, timeline, accounts);
    }
    timeline.subscription.increment();
}

/// Send initial filters for a specific relay. This typically gets called
/// when we first connect to a new relay for the first time. For
/// situations where you are adding a new timeline, use
/// setup_new_timeline.
pub fn send_initial_timeline_filters(
    since_optimize: bool,
    timeline_cache: &mut TimelineCache,
    subs: &mut Subscriptions,
    pool: &mut RelayPool,
    relay_id: &str,
    accounts: &Accounts,
) -> Option<()> {
    info!("Sending initial filters to {}", relay_id);
    let relay = &mut pool.relays.iter_mut().find(|r| r.url() == relay_id)?;

    for (_kind, timeline) in timeline_cache {
        send_initial_timeline_filter(since_optimize, subs, relay, timeline, accounts);
    }

    Some(())
}

pub fn send_initial_timeline_filter(
    can_since_optimize: bool,
    subs: &mut Subscriptions,
    relay: &mut PoolRelay,
    timeline: &mut Timeline,
    accounts: &Accounts,
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
            let new_filters: Vec<Filter> = filter.remote().to_owned().into_iter().map(|f| {
                // limit the size of remote filters
                let default_limit = filter::default_remote_limit();
                let mut lim = f.limit().unwrap_or(default_limit);
                let mut filter = f;
                if lim > default_limit {
                    lim = default_limit;
                    filter = filter.limit_mut(lim);
                }

                let entries = timeline.all_or_any_entries();

                // Should we since optimize? Not always. For example
                // if we only have a few notes locally. One way to
                // determine this is by looking at the current filter
                // and seeing what its limit is. If we have less
                // notes than the limit, we might want to backfill
                // older notes
                if can_since_optimize && filter::should_since_optimize(lim, entries.len()) {
                    filter = filter::since_optimize_filter(filter, entries.latest());
                } else {
                    warn!("Skipping since optimization for {:?}: number of local notes is less than limit, attempting to backfill.", &timeline.kind);
                }

                filter
            }).collect();

            //let sub_id = damus.gen_subid(&SubKind::Initial);
            let sub_id = subscriptions::new_sub_id();
            subs.subs.insert(sub_id.clone(), SubKind::Initial);

            if let Err(err) = relay.subscribe(sub_id.clone(), new_filters.clone()) {
                error!("error subscribing: {err}");
            } else {
                timeline.subscription.force_add_remote(sub_id);
            }
        }

        // we need some data first
        FilterState::NeedsRemote => fetch_contact_list(subs, timeline, accounts),
    }
}

pub fn fetch_contact_list(subs: &mut Subscriptions, timeline: &mut Timeline, accounts: &Accounts) {
    if timeline.filter.get_any_ready().is_some() {
        return;
    }

    let new_filter_state = match accounts.get_selected_account().data.contacts.get_state() {
        ContactState::Unreceived => {
            FilterState::FetchingRemote(filter::FetchingRemoteType::Contact)
        }
        ContactState::Received {
            contacts: _,
            note_key: _,
            timestamp: _,
        } => FilterState::GotRemote(filter::GotRemoteType::Contact),
    };

    timeline.filter.set_all_states(new_filter_state);

    let sub = &accounts.get_subs().contacts;
    if subs.subs.contains_key(&sub.remote) {
        return;
    }

    let sub_kind = SubKind::FetchingContactList(timeline.kind.clone());
    subs.subs.insert(sub.remote.clone(), sub_kind);
}

fn setup_initial_timeline(
    ndb: &Ndb,
    txn: &Transaction,
    timeline: &mut Timeline,
    note_cache: &mut NoteCache,
    unknown_ids: &mut UnknownIds,
    filters: &HybridFilter,
) -> Result<()> {
    // some timelines are one-shot and a refreshed, like last_per_pubkey algo feed
    if timeline.kind.should_subscribe_locally() {
        timeline.subscription.try_add_local(ndb, filters);
    }

    debug!(
        "querying nostrdb sub {:?} {:?}",
        timeline.subscription, timeline.filter
    );

    let mut lim = 0i32;
    for filter in filters.local() {
        lim += filter.limit().unwrap_or(1) as i32;
    }

    debug!("setup_initial_timeline: limit for local filter is {}", lim);

    let notes: Vec<NoteRef> = ndb
        .query(txn, filters.local(), lim)?
        .into_iter()
        .map(NoteRef::from_query_result)
        .collect();

    if let Some(pks) = timeline.insert_new(txn, ndb, note_cache, &notes) {
        pks.process(ndb, txn, unknown_ids);
    }

    Ok(())
}

pub fn setup_initial_nostrdb_subs(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    timeline_cache: &mut TimelineCache,
    unknown_ids: &mut UnknownIds,
) -> Result<()> {
    for (_kind, timeline) in timeline_cache {
        let txn = Transaction::new(ndb).expect("txn");
        if let Err(err) = setup_timeline_nostrdb_sub(ndb, &txn, note_cache, timeline, unknown_ids) {
            error!("setup_initial_nostrdb_subs: {err}");
        }
    }

    Ok(())
}

fn setup_timeline_nostrdb_sub(
    ndb: &Ndb,
    txn: &Transaction,
    note_cache: &mut NoteCache,
    timeline: &mut Timeline,
    unknown_ids: &mut UnknownIds,
) -> Result<()> {
    let filter_state = timeline
        .filter
        .get_any_ready()
        .ok_or(Error::App(notedeck::Error::empty_contact_list()))?
        .to_owned();

    setup_initial_timeline(ndb, txn, timeline, note_cache, unknown_ids, &filter_state)?;

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
    accounts: &Accounts,
    unknown_ids: &mut UnknownIds,
) -> bool {
    // TODO: we should debounce the filter states a bit to make sure we have
    // seen all of the different contact lists from each relay
    if let Some(_f) = timeline.filter.get_any_ready() {
        return true;
    }

    let Some(res) = timeline.filter.get_any_gotremote() else {
        return false;
    };

    let (relay_id, note_key) = match res {
        filter::GotRemoteResult::Normal { relay_id, sub_id } => {
            // We got at least one eose for our filter request. Let's see
            // if nostrdb is done processing it yet.
            let res = ndb.poll_for_notes(sub_id, 1);
            if res.is_empty() {
                debug!(
                    "check_timeline_filter_state: no notes found (yet?) for timeline {:?}",
                    timeline
                );
                return false;
            }

            info!("notes found for contact timeline after GotRemote!");

            (relay_id, res[0])
        }
        filter::GotRemoteResult::Contact { relay_id } => {
            let ContactState::Received {
                contacts: _,
                note_key,
                timestamp: _,
            } = accounts.get_selected_account().data.contacts.get_state()
            else {
                return false;
            };

            (relay_id, *note_key)
        }
    };

    let with_hashtags = false;

    let filter = {
        let txn = Transaction::new(ndb).expect("txn");
        let note = ndb.get_note_by_key(&txn, note_key).expect("note");
        let add_pk = timeline.kind.pubkey().map(|pk| pk.bytes());

        hybrid_contacts_filter(&note, add_pk, with_hashtags)
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
            let txn = Transaction::new(ndb).expect("txn");
            setup_initial_timeline(ndb, &txn, timeline, note_cache, unknown_ids, &filter)
                .expect("setup init");
            timeline
                .filter
                .set_relay_state(relay_id, FilterState::ready_hybrid(filter.clone()));

            //let ck = &timeline.kind;
            //let subid = damus.gen_subid(&SubKind::Column(ck.clone()));
            timeline.subscription.try_add_remote(pool, &filter);
            true
        }
    }
}

#[derive(Debug)]
pub struct NotesFreshness {
    debouncer: Debouncer,
    state: NotesFreshnessState,
}

#[derive(Debug)]
enum NotesFreshnessState {
    Fresh {
        timestamp_viewed: u64,
    },
    Stale {
        have_unseen: bool,
        timestamp_last_viewed: u64,
    },
}

impl Default for NotesFreshness {
    fn default() -> Self {
        Self {
            debouncer: Debouncer::new(Duration::from_secs(2)),
            state: NotesFreshnessState::Stale {
                have_unseen: true,
                timestamp_last_viewed: 0,
            },
        }
    }
}

impl NotesFreshness {
    pub fn set_fresh(&mut self) {
        if !self.debouncer.should_act() {
            return;
        }
        self.state = NotesFreshnessState::Fresh {
            timestamp_viewed: timestamp_now(),
        };
        self.debouncer.bounce();
    }

    pub fn update(&mut self, check_have_unseen: impl FnOnce(u64) -> bool) {
        if !self.debouncer.should_act() {
            return;
        }

        match &self.state {
            NotesFreshnessState::Fresh { timestamp_viewed } => {
                let Ok(dur) = SystemTime::now()
                    .duration_since(UNIX_EPOCH + Duration::from_secs(*timestamp_viewed))
                else {
                    return;
                };

                if dur > Duration::from_secs(2) {
                    self.state = NotesFreshnessState::Stale {
                        have_unseen: check_have_unseen(*timestamp_viewed),
                        timestamp_last_viewed: *timestamp_viewed,
                    };
                }
            }
            NotesFreshnessState::Stale {
                have_unseen,
                timestamp_last_viewed,
            } => {
                if *have_unseen {
                    return;
                }

                self.state = NotesFreshnessState::Stale {
                    have_unseen: check_have_unseen(*timestamp_last_viewed),
                    timestamp_last_viewed: *timestamp_last_viewed,
                };
            }
        }

        self.debouncer.bounce();
    }

    pub fn has_unseen(&self) -> bool {
        match &self.state {
            NotesFreshnessState::Fresh {
                timestamp_viewed: _,
            } => false,
            NotesFreshnessState::Stale {
                have_unseen,
                timestamp_last_viewed: _,
            } => *have_unseen,
        }
    }
}

fn timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}
