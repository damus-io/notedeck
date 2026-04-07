use crate::{
    error::Error,
    scoped_sub_owner_keys::timeline_remote_owner_key,
    timeline::{
        kind::{people_list_note_filter, AlgoTimeline, ListKind, PeopleListRef},
        note_units::InsertManyResponse,
        sub::TimelineSub,
        timeline_units::NotePayload,
    },
    Result,
};

use notedeck::{
    contacts::{hybrid_contacts_filter, hybrid_last_per_pubkey_filter},
    filter::{self},
    is_future_timestamp, tr, unix_time_secs, Accounts, CachedNote, ContactState, FilterError,
    FilterState, Localization, NoteCache, NoteRef, RelaySelection, ScopedSubApi, ScopedSubIdentity,
    SubConfig, SubKey, UnknownIds,
};

use egui_virtual_list::VirtualList;
use enostr::{Pubkey, RelayRoutingPreference};
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::rc::Rc;
use std::{cell::RefCell, collections::HashSet};

use tracing::{debug, error, info, warn};

pub mod cache;
pub mod kind;
mod note_units;
pub mod route;
mod sub;
pub mod thread;
mod timeline_units;
mod unit;

pub use cache::TimelineCache;
pub use kind::{ColumnTitle, PubkeySource, ThreadSelection, TimelineKind};
pub use note_units::{CompositeType, InsertionResponse, NoteUnits};
pub use timeline_units::{MergeResponse, TimelineUnits, UnknownPks};
pub use unit::{CompositeUnit, NoteUnit, ReactionUnit, RepostUnit};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TimelineScopedSub {
    RemoteByKind,
}

fn timeline_remote_sub_key(kind: &TimelineKind) -> SubKey {
    SubKey::builder(TimelineScopedSub::RemoteByKind)
        .with(kind)
        .finish()
}

fn timeline_remote_sub_config(
    remote_filters: Vec<Filter>,
    routing_preference: RelayRoutingPreference,
) -> SubConfig {
    SubConfig {
        relays: RelaySelection::AccountsRead,
        filters: remote_filters,
        routing_preference,
    }
}

pub(crate) fn ensure_remote_timeline_subscription(
    timeline: &mut Timeline,
    account_pk: Pubkey,
    remote_filters: Vec<Filter>,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
) {
    let owner = timeline_remote_owner_key(account_pk, &timeline.kind);
    let identity = ScopedSubIdentity::account(owner, timeline_remote_sub_key(&timeline.kind));
    let config = timeline_remote_sub_config(
        remote_filters,
        if matches!(&timeline.kind, TimelineKind::Notifications(_)) {
            RelayRoutingPreference::RequireDedicated
        } else {
            RelayRoutingPreference::default()
        },
    );
    let _ = scoped_subs.ensure_sub(identity, config);
    timeline.subscription.mark_remote_seeded(account_pk);
}

pub(crate) fn update_remote_timeline_subscription(
    timeline: &mut Timeline,
    remote_filters: Vec<Filter>,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
) {
    let owner = timeline_remote_owner_key(scoped_subs.selected_account_pubkey(), &timeline.kind);
    let identity = ScopedSubIdentity::account(owner, timeline_remote_sub_key(&timeline.kind));
    let config = timeline_remote_sub_config(
        remote_filters,
        if matches!(&timeline.kind, TimelineKind::Notifications(_)) {
            RelayRoutingPreference::RequireDedicated
        } else {
            RelayRoutingPreference::default()
        },
    );
    let _ = scoped_subs.set_sub(identity, config);
    timeline
        .subscription
        .mark_remote_seeded(scoped_subs.selected_account_pubkey());
}

pub fn drop_timeline_remote_owner(
    timeline: &Timeline,
    account_pk: Pubkey,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
) {
    let owner = timeline_remote_owner_key(account_pk, &timeline.kind);
    let _ = scoped_subs.drop_owner(owner);
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default, PartialOrd, Ord)]
pub enum ViewFilter {
    MentionsOnly,
    Notes,

    #[default]
    NotesAndReplies,

    All,
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
            ViewFilter::All => tr!(i18n, "All", "Filter label for all notes view"),
            ViewFilter::MentionsOnly => {
                tr!(i18n, "Mentions", "Filter label for mentions only view")
            }
        }
    }

    pub fn filter_notes(cache: &CachedNote, note: &Note) -> bool {
        note.kind() == 6 || !cache.reply.borrow(note.tags()).is_reply()
    }

    fn identity(_cache: &CachedNote, _note: &Note) -> bool {
        true
    }

    fn notes_and_replies(_cache: &CachedNote, note: &Note) -> bool {
        note.kind() == 1 || note.kind() == 6
    }

    fn mentions_only(cache: &CachedNote, note: &Note) -> bool {
        if note.kind() != 1 {
            return false;
        }

        let note_reply = cache.reply.borrow(note.tags());

        note_reply.is_reply() || note_reply.mention().is_some()
    }

    pub fn filter(&self) -> fn(&CachedNote, &Note) -> bool {
        match self {
            ViewFilter::Notes => ViewFilter::filter_notes,
            ViewFilter::NotesAndReplies => ViewFilter::notes_and_replies,
            ViewFilter::All => ViewFilter::identity,
            ViewFilter::MentionsOnly => ViewFilter::mentions_only,
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

    pub fn notifications() -> Vec<Self> {
        vec![
            TimelineTab::new(ViewFilter::All),
            TimelineTab::new(ViewFilter::MentionsOnly),
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
        }
    }

    /// Reset the tab to an empty state, clearing all cached notes.
    ///
    /// Used when the contact list changes and we need to rebuild
    /// the timeline with a new filter.
    pub fn reset(&mut self) {
        self.units = TimelineUnits::with_capacity(1000);
        self.selection = 0;
        self.list.borrow_mut().reset();
    }

    #[profiling::function]
    fn insert<'a>(
        &mut self,
        payloads: Vec<&'a NotePayload>,
        ndb: &Ndb,
        txn: &Transaction,
        reversed: bool,
        use_front_insert: bool,
    ) -> MergeResponse<'a> {
        if payloads.is_empty() {
            return MergeResponse::empty();
        }

        let num_refs = payloads.len();

        let resp = self.units.merge_new_notes(payloads, ndb, txn);

        let InsertManyResponse::Some {
            entries_merged,
            merge_kind,
        } = &resp.insertion_response
        else {
            return resp;
        };

        let mut list = self.list.borrow_mut();

        match merge_kind {
            // TODO: update egui_virtual_list to support spliced inserts
            MergeKind::Spliced => {
                tracing::trace!(
                    "spliced when inserting {num_refs} new notes, resetting virtual list",
                );
                list.reset();
            }
            MergeKind::FrontInsert => 's: {
                if !use_front_insert {
                    break 's;
                }

                // only run this logic if we're reverse-chronological
                // reversed in this case means chronological, since the
                // default is reverse-chronological. yeah it's confusing.
                if !reversed {
                    debug!("inserting {num_refs} new notes at start");
                    list.items_inserted_at_start(*entries_merged);
                }
            }
        };

        resp
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
    pub fn process_unknown_pks(&self, unknown_ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
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
    pub filter: FilterState,
    pub views: Vec<TimelineTab>,
    pub selected_view: usize,
    pub seen_latest_notes: bool,

    pub subscription: TimelineSub,
    pub enable_front_insert: bool,

    /// Timestamp (`created_at`) of the contact list note used to build
    /// the current filter. Used to detect when the contact list has
    /// changed (e.g., after follow/unfollow) so the filter can be rebuilt.
    pub contact_list_timestamp: Option<u64>,

    /// Whether the initial async load has been completed for this timeline.
    pub initial_load: InitialLoadState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InitialLoadState {
    /// Not yet scheduled for loading.
    #[default]
    Pending,
    /// Currently loading initial notes.
    Loading,
    /// Initial load is complete.
    Complete,
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
        let notes_per_pk = 1;
        let filter = hybrid_last_per_pubkey_filter(list, notes_per_pk)?;

        Ok(Timeline::new(
            TimelineKind::last_per_pubkey(list_kind.clone()),
            FilterState::ready_hybrid(filter),
            TimelineTab::only_notes_and_replies(),
        ))
    }

    /// Create a hashtag timeline with ready filters.
    pub fn hashtag(hashtag: Vec<String>) -> Self {
        debug!("timeline.hashtag -> pushing filter");

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

        if filters.is_empty() {
            warn!(?hashtag, "hashtag timeline created with no usable tags");
        }

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
        let subscription = TimelineSub::default();
        let selected_view = 0;

        // by default, disabled for profiles since they contain widgets above the list items
        let enable_front_insert = !matches!(kind, TimelineKind::Profile(_));

        Timeline {
            kind,
            filter: filter_state,
            views,
            subscription,
            selected_view,
            enable_front_insert,
            seen_latest_notes: false,
            contact_list_timestamp: None,
            initial_load: InitialLoadState::Pending,
        }
    }

    pub fn current_view(&self) -> &TimelineTab {
        &self.views[self.selected_view]
    }

    pub fn current_view_mut(&mut self) -> &mut TimelineTab {
        &mut self.views[self.selected_view]
    }

    /// Get the note refs for the filter with the widest scope
    pub fn all_or_any_entries(&self) -> &TimelineUnits {
        let widest_filter = self
            .views
            .iter()
            .map(|view| view.filter)
            .max()
            .expect("at least one filter exists");

        self.entries(widest_filter)
            .expect("should have at least notes")
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

    /// Reset all views to an empty state, clearing all cached notes.
    ///
    /// Used when the contact list changes and we need to rebuild
    /// the timeline with a new filter.
    pub fn reset_views(&mut self) {
        for view in &mut self.views {
            view.reset();
        }
    }

    /// Initial insert of notes into a timeline. Subsequent inserts should
    /// just use the insert function
    #[profiling::function]
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

        let now = unix_time_secs();
        let mut unknown_pks = HashSet::new();
        for note_ref in notes {
            profiling::scope!("inserting notes");
            if is_future_timestamp(note_ref.created_at, now) {
                continue;
            }

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
    #[profiling::function]
    pub fn insert<'txn>(
        &mut self,
        new_note_ids: &[NoteKey],
        ndb: &Ndb,
        txn: &'txn Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<bool> {
        let mut payloads: Vec<NotePayload> = Vec::with_capacity(new_note_ids.len());
        let now = unix_time_secs();
        let mut any_front_insert = false;

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

            if is_future_timestamp(note.created_at(), now) {
                continue;
            }

            // Ensure that unknown ids are captured when inserting notes
            // into the timeline
            UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);

            payloads.push(NotePayload { note, key: *key });
        }

        for view in &mut self.views {
            let should_include = view.filter.filter();
            let mut filtered_payloads = Vec::with_capacity(payloads.len());
            for payload in &payloads {
                let cached_note = note_cache.cached_note_or_insert(payload.key, &payload.note);

                if should_include(cached_note, &payload.note) {
                    filtered_payloads.push(payload);
                }
            }

            let res = view.insert(
                filtered_payloads,
                ndb,
                txn,
                reversed,
                self.enable_front_insert,
            );

            any_front_insert = any_front_insert || res.insertion_response.is_front_insert();

            if let Some(unknown_pks) = res.tl_response {
                unknown_pks.process_unknown_pks(unknown_ids, ndb, txn);
            }
        }

        Ok(any_front_insert)
    }

    #[profiling::function]
    /// Poll for new notes and insert them into the timeline.
    /// Returns `Ok(true)` if new notes were found, `Ok(false)` otherwise.
    pub fn poll_notes_into_view(
        &mut self,
        account_pk: &Pubkey,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<bool> {
        if !self.kind.should_subscribe_locally() {
            // don't need to poll for timelines that don't have local subscriptions
            return Ok(false);
        }

        let sub = self
            .subscription
            .get_local(account_pk)
            .ok_or(Error::App(notedeck::Error::no_active_sub()))?;

        let new_note_ids = {
            profiling::scope!("big ndb poll");
            ndb.poll_for_notes(sub, 500)
        };

        if new_note_ids.is_empty() {
            return Ok(false);
        };

        let any_front_insert =
            self.insert(&new_note_ids, ndb, txn, unknown_ids, note_cache, reversed)?;

        if any_front_insert {
            // front inserts (not merged insert) typically mean we have something new to notify on,
            // otherwise its likely just an old note that slid into the notification timeline
            // somewhere
            //
            // While this isn't perfect, since we might have a notification that slid in just
            // behind the latest, it is a pragmatic heuristic for now.
            self.seen_latest_notes = false;
        }

        Ok(true)
    }

    /// Invalidate the timeline, forcing a rebuild on the next check.
    ///
    /// This resets all relay states to [`FilterState::NeedsRemote`] and
    /// clears the contact list timestamp, which will trigger the filter
    /// rebuild flow when the timeline is next polled.
    ///
    /// Note: We reset states rather than clearing them so that
    /// [`Self::set_all_states`] can update them during the rebuild.
    pub fn invalidate(&mut self) {
        self.filter = FilterState::NeedsRemote;
        self.contact_list_timestamp = None;
        self.initial_load = InitialLoadState::Pending;
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
pub fn setup_new_timeline(
    timeline: &mut Timeline,
    ndb: &Ndb,
    txn: &Transaction,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    since_optimize: bool,
    accounts: &Accounts,
) {
    let account_pk = *accounts.selected_account_pubkey();

    // if we're ready, setup local subs
    if is_timeline_ready(ndb, scoped_subs, timeline, accounts) {
        if let Err(err) = setup_initial_timeline(ndb, timeline, account_pk) {
            error!("setup_new_timeline: {err}");
        }
    }

    send_initial_timeline_filter(since_optimize, ndb, txn, timeline, accounts, scoped_subs);
    timeline.subscription.increment(account_pk);
}

pub fn send_initial_timeline_filter(
    can_since_optimize: bool,
    ndb: &Ndb,
    txn: &Transaction,
    timeline: &mut Timeline,
    accounts: &Accounts,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
) {
    match &timeline.filter {
        FilterState::Broken(err) => {
            error!(
                "FetchingRemote state in broken state when sending initial timeline filter? {err}"
            );
        }

        FilterState::FetchingRemote => {
            error!("FetchingRemote state when sending initial timeline filter?");
        }

        FilterState::GotRemote => {
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

            update_remote_timeline_subscription(timeline, new_filters, scoped_subs);
        }

        // we need some data first
        FilterState::NeedsRemote => match &timeline.kind {
            TimelineKind::List(ListKind::PeopleList(_))
            | TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::PeopleList(_))) => {
                fetch_people_list(ndb, txn, timeline);
            }
            _ => fetch_contact_list(timeline, accounts),
        },
    }
}

pub fn fetch_contact_list(timeline: &mut Timeline, accounts: &Accounts) {
    if matches!(&timeline.filter, FilterState::Ready(_)) {
        return;
    }

    let new_filter_state = match accounts.get_selected_account().data.contacts.get_state() {
        ContactState::Unreceived => FilterState::FetchingRemote,
        ContactState::Received {
            contacts: _,
            note_key: _,
            timestamp: _,
        } => FilterState::GotRemote,
    };

    timeline.filter = new_filter_state;
}

pub fn fetch_people_list(ndb: &Ndb, txn: &Transaction, timeline: &mut Timeline) {
    if matches!(&timeline.filter, FilterState::Ready(_)) {
        return;
    }

    let Some(plr) = people_list_ref(&timeline.kind) else {
        error!("fetch_people_list called for non-people-list timeline");
        timeline.filter = FilterState::broken(FilterError::EmptyList);
        return;
    };

    let filter = people_list_note_filter(plr);

    let results = match ndb.query(txn, std::slice::from_ref(&filter), 1) {
        Ok(results) => results,
        Err(err) => {
            error!("people list query failed in fetch_people_list: {err}");
            timeline.filter = FilterState::broken(FilterError::EmptyList);
            return;
        }
    };

    if results.is_empty() {
        timeline.filter = FilterState::FetchingRemote;
        return;
    }

    timeline.filter = FilterState::GotRemote;
}

/// Set up the local NDB subscription for a timeline without running
/// blocking queries. The actual note loading is handled by the async
/// timeline loader.
#[profiling::function]
fn setup_initial_timeline(ndb: &Ndb, timeline: &mut Timeline, account_pk: Pubkey) -> Result<()> {
    let FilterState::Ready(filters) = &timeline.filter else {
        return Err(Error::App(notedeck::Error::empty_contact_list()));
    };

    // some timelines are one-shot and refreshed, like last_per_pubkey algo feed
    if timeline.kind.should_subscribe_locally() {
        timeline
            .subscription
            .try_add_local(account_pk, ndb, filters);
    }

    Ok(())
}

#[profiling::function]
pub fn setup_initial_nostrdb_subs(
    ndb: &Ndb,
    timeline_cache: &mut TimelineCache,
    account_pk: Pubkey,
) -> Result<()> {
    for (_kind, timeline) in timeline_cache {
        if timeline.subscription.dependers(&account_pk) == 0 {
            continue;
        }

        if let Err(err) = setup_initial_timeline(ndb, timeline, account_pk) {
            error!("setup_initial_nostrdb_subs: {err}");
        }
    }

    Ok(())
}

/// Check our timeline filter and see if we have any filter data ready.
/// Our timelines may require additional data before it is functional. For
/// example, when we have to fetch a contact list before we do the actual
/// following list query.
#[profiling::function]
pub fn is_timeline_ready(
    ndb: &Ndb,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    timeline: &mut Timeline,
    accounts: &Accounts,
) -> bool {
    // TODO: we should debounce the filter states a bit to make sure we have
    // seen all of the different contact lists from each relay
    if let FilterState::Ready(filter) = &timeline.filter {
        let account_pk = *accounts.selected_account_pubkey();
        if timeline.subscription.dependers(&account_pk) > 0
            && !timeline.subscription.remote_seeded(&account_pk)
        {
            let remote_filters = filter.remote().to_vec();
            ensure_remote_timeline_subscription(timeline, account_pk, remote_filters, scoped_subs);
        }
        return true;
    }

    if !matches!(&timeline.filter, FilterState::GotRemote) {
        return false;
    }

    let note_key = match &timeline.kind {
        TimelineKind::List(ListKind::Contact(_))
        | TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::Contact(_))) => {
            let ContactState::Received {
                contacts: _,
                note_key,
                timestamp: _,
            } = accounts.get_selected_account().data.contacts.get_state()
            else {
                return false;
            };

            *note_key
        }
        TimelineKind::List(ListKind::PeopleList(plr))
        | TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::PeopleList(plr))) => {
            let list_filter = people_list_note_filter(plr);
            let txn = Transaction::new(ndb).expect("txn");
            let results = match ndb.query(&txn, std::slice::from_ref(&list_filter), 1) {
                Ok(results) => results,
                Err(err) => {
                    error!("people list query failed in is_timeline_ready: {err}");
                    return false;
                }
            };

            if results.is_empty() {
                debug!("people list note not yet in ndb for {:?}", plr);
                return false;
            }

            info!("found people list note after GotRemote!");
            results[0].note_key
        }
        _ => return false,
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
            timeline.filter = FilterState::broken(e);
            false
        }
        Err(err) => {
            error!("got broken when building filter {err}");
            let reason = match &timeline.kind {
                TimelineKind::List(ListKind::PeopleList(_))
                | TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::PeopleList(_))) => {
                    FilterError::EmptyList
                }
                _ => FilterError::EmptyContactList,
            };
            timeline.filter = FilterState::broken(reason);
            false
        }
        Ok(filter) => {
            // We just switched to the ready state; remote subscriptions can start now.
            info!("Found list note! Setting up remote timeline query");
            timeline.filter = FilterState::ready_hybrid(filter.clone());

            update_remote_timeline_subscription(timeline, filter.remote().to_vec(), scoped_subs);
            true
        }
    }
}

fn people_list_ref(kind: &TimelineKind) -> Option<&PeopleListRef> {
    match kind {
        TimelineKind::List(ListKind::PeopleList(plr))
        | TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::PeopleList(plr))) => Some(plr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::{
        Nip11ApplyOutcome, Nip11LimitationsRaw, NormRelayUrl, OutboxPool, OutboxSessionHandler,
        RelayUrlPkgs,
    };
    use hashbrown::HashSet;
    use nostr_relay_builder::{LocalRelay, RelayBuilder};
    use nostrdb::{Config, Transaction};
    use notedeck::{EguiWakeup, ScopedSubEoseStatus, ScopedSubsState, FALLBACK_PUBKEY};
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::TempDir;

    struct TimelineRemoteHarness {
        _tmp: TempDir,
        _ndb: Ndb,
        accounts: Accounts,
        _unknown_ids: UnknownIds,
        scoped_sub_state: ScopedSubsState,
        pool: OutboxPool,
    }

    impl TimelineRemoteHarness {
        fn with_forced_relays(forced_relays: Vec<String>) -> Self {
            let tmp = TempDir::new().expect("tmp dir");
            let mut ndb =
                Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
            let txn = Transaction::new(&ndb).expect("txn");
            let mut unknown_ids = UnknownIds::default();
            let accounts = Accounts::new(
                None,
                forced_relays,
                FALLBACK_PUBKEY(),
                &mut ndb,
                &txn,
                &mut unknown_ids,
            );

            Self {
                _tmp: tmp,
                _ndb: ndb,
                accounts,
                _unknown_ids: unknown_ids,
                scoped_sub_state: ScopedSubsState::default(),
                pool: OutboxPool::default(),
            }
        }
    }

    async fn pump_pool_until<F>(
        pool: &mut OutboxPool,
        max_attempts: usize,
        mut predicate: F,
    ) -> bool
    where
        F: FnMut(&mut OutboxPool) -> bool,
    {
        for _ in 0..max_attempts {
            pool.try_recv(10, |_| {});
            if predicate(pool) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        predicate(pool)
    }

    /// Saturates one relay to `max_subscriptions = 1`, then promotes a
    /// `NoPreference` subscription into the live compaction lane by first
    /// occupying the dedicated slot with a `PreferDedicated` request and then
    /// unsubscribing it.
    async fn install_active_compaction_lane(
        pool: &mut OutboxPool,
        relay: &NormRelayUrl,
    ) -> enostr::OutboxSubId {
        let relay_pkgs = |routing_preference| {
            RelayUrlPkgs::with_preference(HashSet::from([relay.clone()]), routing_preference)
        };

        let preferred_id = {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.subscribe(
                vec![Filter::new().kinds(vec![1]).limit(10).build()],
                relay_pkgs(RelayRoutingPreference::PreferDedicated),
            )
        };
        let applied = pool.apply_nip11_limits(
            relay,
            Nip11LimitationsRaw {
                max_subscriptions: Some(1),
                ..Default::default()
            },
            UNIX_EPOCH + Duration::from_secs(1_700_000_410),
        );
        assert!(matches!(
            applied,
            Nip11ApplyOutcome::Applied | Nip11ApplyOutcome::Unchanged
        ));

        let compaction_id = {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.subscribe(
                vec![Filter::new().kinds(vec![2]).limit(10).build()],
                relay_pkgs(RelayRoutingPreference::NoPreference),
            )
        };

        let preferred_ready = pump_pool_until(pool, 100, |pool| pool.has_eose(&preferred_id)).await;
        assert!(
            preferred_ready,
            "preferred baseline subscription should stay active while the fallback request waits"
        );
        assert!(
            !pool.has_eose(&compaction_id),
            "fallback request should stay queued until the preferred dedicated slot is released"
        );

        {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.unsubscribe(preferred_id);
        }

        let compaction_ready =
            pump_pool_until(pool, 100, |pool| pool.has_eose(&compaction_id)).await;
        assert!(
            compaction_ready,
            "fallback request should become the active compaction route once the preferred slot is released"
        );
        assert!(
            !pool.status(&compaction_id).is_empty(),
            "active compaction route should expose one routed relay leg before notifications subscribe"
        );

        compaction_id
    }

    /// Verifies notifications timelines keep `RequireDedicated` routing on both
    /// create and update by revoking an existing non-preferred compaction leg
    /// rather than being absorbed into that shared fallback route.
    #[tokio::test]
    async fn notifications_remote_sub_keeps_require_dedicated_on_create_and_update() {
        let relay_task = LocalRelay::run(RelayBuilder::default())
            .await
            .expect("start local relay");
        let relay = NormRelayUrl::new(&relay_task.url()).expect("relay url");
        let mut h = TimelineRemoteHarness::with_forced_relays(vec![relay.to_string()]);
        let compaction_id = install_active_compaction_lane(&mut h.pool, &relay).await;

        let selected = *h.accounts.selected_account_pubkey();
        let mut timeline = Timeline::new(
            TimelineKind::notifications(selected),
            FilterState::ready(vec![Filter::new().kinds(vec![1]).limit(20).build()]),
            TimelineTab::notifications(),
        );
        let identity = ScopedSubIdentity::account(
            timeline_remote_owner_key(selected, &timeline.kind),
            timeline_remote_sub_key(&timeline.kind),
        );

        {
            let mut outbox =
                OutboxSessionHandler::new(&mut h.pool, EguiWakeup::new(egui::Context::default()));
            let mut scoped_subs = h.scoped_sub_state.api(&mut outbox, &h.accounts);
            ensure_remote_timeline_subscription(
                &mut timeline,
                selected,
                vec![Filter::new().kinds(vec![1]).limit(20).build()],
                &mut scoped_subs,
            );
        }
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut h.pool, EguiWakeup::new(egui::Context::default()));
            let scoped_subs = h.scoped_sub_state.api(&mut outbox, &h.accounts);
            assert_eq!(
                scoped_subs.sub_eose_status(identity),
                ScopedSubEoseStatus::Live(notedeck::ScopedSubLiveEoseStatus {
                    tracked_relays: 1,
                    any_eose: false,
                    all_eosed: false,
                })
            );
        }
        assert!(
            h.pool.status(&compaction_id).is_empty(),
            "required-dedicated notifications should evict the existing non-preferred compaction leg"
        );

        {
            let mut outbox =
                OutboxSessionHandler::new(&mut h.pool, EguiWakeup::new(egui::Context::default()));
            let mut scoped_subs = h.scoped_sub_state.api(&mut outbox, &h.accounts);
            update_remote_timeline_subscription(
                &mut timeline,
                vec![Filter::new().kinds(vec![1]).limit(5).build()],
                &mut scoped_subs,
            );
        }
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut h.pool, EguiWakeup::new(egui::Context::default()));
            let scoped_subs = h.scoped_sub_state.api(&mut outbox, &h.accounts);
            assert_eq!(
                scoped_subs.sub_eose_status(identity),
                ScopedSubEoseStatus::Live(notedeck::ScopedSubLiveEoseStatus {
                    tracked_relays: 1,
                    any_eose: false,
                    all_eosed: false,
                })
            );
        }
        assert!(
            h.pool.status(&compaction_id).is_empty(),
            "updating notifications should keep the dedicated route and leave the old compaction leg revoked"
        );

        relay_task.shutdown();
    }
}
