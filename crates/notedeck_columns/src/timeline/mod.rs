use crate::{
    error::Error,
    timeline::{
        kind::{
            hashtag_filter_state, people_list_note_filter, AlgoTimeline, ListKind, PeopleListRef,
        },
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
    FilterState, Localization, NoteCache, NoteRef, ScopedSubApi,
};

use egui_virtual_list::VirtualList;
use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Note, NoteKey, Transaction};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use tracing::{debug, error, info, warn};

const TIMELINE_LOCAL_INGEST_BUDGET: Duration = Duration::from_millis(2);
const TIMELINE_LOCAL_POLL_BATCH: u32 = 1;

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
pub use sub::RemoteSubscriptionPolicy;
pub(crate) use sub::{
    drop_timeline_remote_owner, ensure_remote_timeline_subscription,
    update_remote_timeline_subscription, update_remote_timeline_subscription_for_account,
};
pub use timeline_units::{MergeResponse, TimelineUnits};
pub use unit::{CompositeUnit, NoteUnit, ReactionUnit, RepostUnit, ZapUnit};

#[cfg(test)]
use crate::timeline::sub::{
    timeline_remote_sub_declaration, timeline_remote_sub_key, TimelineScopedSub,
};
#[cfg(test)]
use enostr::RelayRoutingPreference;
#[cfg(test)]
use notedeck::SubConfig;

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
    ) -> MergeResponse {
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
    /// Last remote filter set declared for this timeline's scoped subscription.
    remote_subscription_filters: Option<Vec<Filter>>,

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

    /// Create a hashtag timeline with the canonical hashtag filter state.
    pub fn hashtag(hashtag: Vec<String>) -> Self {
        let filter = hashtag_filter_state(&hashtag);

        Timeline::new(
            TimelineKind::Hashtag(hashtag),
            filter,
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
            remote_subscription_filters: None,
            contact_list_timestamp: None,
            initial_load: InitialLoadState::Pending,
        }
    }

    /// Remote filters for re-declaring the active scoped subscription.
    fn remote_filters_for_subscription_refresh(&self) -> Option<Vec<Filter>> {
        self.remote_subscription_filters.clone().or_else(|| {
            let FilterState::Ready(filter) = &self.filter else {
                return None;
            };

            Some(filter.remote().to_vec())
        })
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
        self.subscription.clear_pending_polled_note_keys();
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
    ) {
        let now = unix_time_secs();
        let mut payloads = Vec::with_capacity(notes.len());
        for note_ref in notes {
            if is_future_timestamp(note_ref.created_at, now) {
                continue;
            }

            let Ok(note) = ndb.get_note_by_key(txn, note_ref.key) else {
                continue;
            };
            payloads.push(NotePayload {
                note,
                key: note_ref.key,
            });
        }

        for view in &mut self.views {
            let should_include = view.filter.filter();
            let mut filtered_payloads = Vec::with_capacity(payloads.len());
            for payload in &payloads {
                let cached_note = note_cache.cached_note_or_insert_mut(payload.key, &payload.note);
                if should_include(cached_note, &payload.note) {
                    filtered_payloads.push(payload);
                }
            }

            view.units.merge_new_notes(filtered_payloads, ndb, txn);
        }
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
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<bool> {
        let note_payloads = self.note_payloads(new_note_ids, ndb, txn);
        for key in &note_payloads.missing_keys {
            error!(
                "hit race condition in poll_notes_into_view: https://github.com/damus-io/nostrdb/issues/35 note {:?} was not added to timeline",
                key
            );
        }
        self.insert_payloads(&note_payloads.payloads, ndb, txn, note_cache, reversed)
    }

    fn note_payloads<'txn>(
        &self,
        new_note_ids: &[NoteKey],
        ndb: &Ndb,
        txn: &'txn Transaction,
    ) -> NotePayloads<'txn> {
        let mut payloads: Vec<NotePayload> = Vec::with_capacity(new_note_ids.len());
        let mut missing_keys = Vec::new();
        let now = unix_time_secs();

        for key in new_note_ids {
            let note = if let Ok(note) = ndb.get_note_by_key(txn, *key) {
                note
            } else {
                missing_keys.push(*key);
                continue;
            };

            if is_future_timestamp(note.created_at(), now) {
                continue;
            }

            payloads.push(NotePayload { note, key: *key });
        }

        NotePayloads {
            payloads,
            missing_keys,
        }
    }

    fn insert_polled_note_keys(
        &mut self,
        new_note_ids: &[NoteKey],
        ndb: &Ndb,
        txn: &Transaction,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<PollInsertResult> {
        let note_payloads = self.note_payloads(new_note_ids, ndb, txn);

        let inserted_keys = note_payloads
            .payloads
            .iter()
            .map(|payload| payload.key)
            .collect();
        let any_front_insert =
            self.insert_payloads(&note_payloads.payloads, ndb, txn, note_cache, reversed)?;

        Ok(PollInsertResult {
            inserted_keys,
            missing_keys: note_payloads.missing_keys,
            any_front_insert,
        })
    }

    fn insert_payloads<'txn>(
        &mut self,
        payloads: &[NotePayload<'txn>],
        ndb: &Ndb,
        txn: &'txn Transaction,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<bool> {
        let mut any_front_insert = false;

        for view in &mut self.views {
            let should_include = view.filter.filter();
            let mut filtered_payloads = Vec::with_capacity(payloads.len());
            for payload in payloads {
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
        }

        Ok(any_front_insert)
    }

    #[profiling::function]
    /// Poll for new notes and insert them into the timeline.
    /// Returns the inserted [`NoteKey`]s (empty if nothing new arrived).
    pub fn poll_notes_into_view(
        &mut self,
        account_pk: &Pubkey,
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        reversed: bool,
    ) -> Result<Vec<NoteKey>> {
        if !self.kind.should_subscribe_locally() {
            // don't need to poll for timelines that don't have local subscriptions
            return Ok(vec![]);
        }

        if self.subscription.get_local(account_pk).is_none() {
            return Err(Error::App(notedeck::Error::no_active_sub()));
        }

        let start = Instant::now();
        let mut inserted = Vec::new();
        let mut any_front_insert = false;

        loop {
            if !inserted.is_empty() && start.elapsed() >= TIMELINE_LOCAL_INGEST_BUDGET {
                break;
            }

            let Some(new_note_ids) =
                self.subscription
                    .take_pending_or_poll(account_pk, ndb, TIMELINE_LOCAL_POLL_BATCH)
            else {
                return Err(Error::App(notedeck::Error::no_active_sub()));
            };
            if new_note_ids.is_empty() {
                break;
            }

            let txn = match Transaction::new(ndb) {
                Ok(txn) => txn,
                Err(err) => {
                    self.subscription
                        .push_pending_polled_note_keys(*account_pk, new_note_ids);
                    return Err(err.into());
                }
            };
            let result =
                self.insert_polled_note_keys(&new_note_ids, ndb, &txn, note_cache, reversed)?;
            any_front_insert |= result.any_front_insert;
            inserted.extend(result.inserted_keys);

            if !result.missing_keys.is_empty() {
                self.subscription
                    .push_pending_polled_note_keys(*account_pk, result.missing_keys);
                break;
            }
        }

        if any_front_insert {
            self.seen_latest_notes = false;
        }

        Ok(inserted)
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
        self.remote_subscription_filters = None;
        self.subscription.clear_pending_polled_note_keys();
        self.contact_list_timestamp = None;
        self.initial_load = InitialLoadState::Pending;
    }
}

struct NotePayloads<'txn> {
    payloads: Vec<NotePayload<'txn>>,
    missing_keys: Vec<NoteKey>,
}

struct PollInsertResult {
    inserted_keys: Vec<NoteKey>,
    missing_keys: Vec<NoteKey>,
    any_front_insert: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
pub(crate) fn setup_new_timeline(
    timeline: &mut Timeline,
    ndb: &Ndb,
    txn: &Transaction,
    scoped_subs: &mut ScopedSubApi<'_>,
    since_optimize: bool,
    accounts: &Accounts,
    remote_policy: RemoteSubscriptionPolicy,
) {
    let account_pk = *accounts.selected_account_pubkey();

    // if we're ready, setup local subs
    if is_timeline_ready(ndb, scoped_subs, timeline, accounts, remote_policy) {
        if let Err(err) = setup_initial_timeline(ndb, timeline, account_pk) {
            error!("setup_new_timeline: {err}");
        }
    }

    send_initial_timeline_filter(
        since_optimize,
        ndb,
        txn,
        timeline,
        accounts,
        scoped_subs,
        remote_policy,
    );
    timeline.subscription.increment(account_pk);
}

pub(crate) fn send_initial_timeline_filter(
    can_since_optimize: bool,
    ndb: &Ndb,
    txn: &Transaction,
    timeline: &mut Timeline,
    accounts: &Accounts,
    scoped_subs: &mut ScopedSubApi<'_>,
    remote_policy: RemoteSubscriptionPolicy,
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

            update_remote_timeline_subscription(timeline, new_filters, scoped_subs, remote_policy);
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

pub(crate) fn fetch_people_list(ndb: &Ndb, txn: &Transaction, timeline: &mut Timeline) {
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
pub(crate) fn is_timeline_ready(
    ndb: &Ndb,
    scoped_subs: &mut ScopedSubApi<'_>,
    timeline: &mut Timeline,
    accounts: &Accounts,
    remote_policy: RemoteSubscriptionPolicy,
) -> bool {
    // TODO: we should debounce the filter states a bit to make sure we have
    // seen all of the different contact lists from each relay
    if let FilterState::Ready(filter) = &timeline.filter {
        let account_pk = *accounts.selected_account_pubkey();
        if timeline.subscription.dependers(&account_pk) > 0
            && !timeline.subscription.is_remote_registered(&account_pk)
        {
            let remote_filters = filter.remote().to_vec();
            ensure_remote_timeline_subscription(
                timeline,
                account_pk,
                remote_filters,
                scoped_subs,
                remote_policy,
            );
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

            update_remote_timeline_subscription(
                timeline,
                filter.remote().to_vec(),
                scoped_subs,
                remote_policy,
            );
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
    use enostr::{FullKeypair, NormRelayUrl};
    use nostrdb::{NoteBuilder, Transaction};
    use notedeck::{Accounts, Notedeck, ScopedSubApi};
    use serde_json::Value;
    use tempfile::TempDir;

    struct TimelineRemoteHarness {
        _tmp: TempDir,
        notedeck: Notedeck,
    }

    impl TimelineRemoteHarness {
        fn with_forced_relays(forced_relays: Vec<String>) -> Self {
            let tmp = TempDir::new().expect("tmp dir");
            let ui_ctx = egui::Context::default();
            let mut args = vec!["notedeck".to_owned(), "--testrunner".to_owned()];
            for relay in forced_relays {
                args.push("--relay".to_owned());
                args.push(relay);
            }
            let notedeck = Notedeck::init(&ui_ctx, tmp.path(), &args);

            Self {
                _tmp: tmp,
                notedeck,
            }
        }

        fn selected_account_pubkey(&mut self) -> Pubkey {
            *self
                .notedeck
                .app_context()
                .accounts
                .selected_account_pubkey()
        }

        fn with_scoped_subs<T>(
            &mut self,
            f: impl FnOnce(&mut ScopedSubApi<'_>, &Accounts, &Ndb) -> T,
        ) -> T {
            let mut app_ctx = self.notedeck.app_context();
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            f(&mut scoped_subs, app_ctx.accounts, app_ctx.ndb)
        }
    }

    fn expected_accounts_read_with_author_outbox_config(
        live_filters: Vec<Filter>,
        routing_preference: RelayRoutingPreference,
    ) -> SubConfig {
        SubConfig::builder(live_filters)
            .accounts_read_important_with_preference(routing_preference)
            .with_author_outbox_augmentation()
            .build()
    }

    fn expected_accounts_read_only_config(
        live_filters: Vec<Filter>,
        routing_preference: RelayRoutingPreference,
    ) -> SubConfig {
        SubConfig::builder(live_filters)
            .accounts_read_important_with_preference(routing_preference)
            .build()
    }

    fn tag_json(filter: &Filter, tag: &str) -> Vec<String> {
        let json = filter.json().expect("filter json");
        let value: Value = serde_json::from_str(&json).expect("filter value");
        value[tag]
            .as_array()
            .expect("tag array")
            .iter()
            .map(|entry| entry.as_str().expect("tag value").to_owned())
            .collect()
    }

    fn remote_policy(use_outbox_relays: bool) -> RemoteSubscriptionPolicy {
        RemoteSubscriptionPolicy::from_outbox_relays(use_outbox_relays)
    }

    fn new_test_ndb() -> (TempDir, Ndb) {
        let tmp = TempDir::new().expect("tmp dir");
        let ndb =
            Ndb::new(tmp.path().to_str().expect("path"), &nostrdb::Config::new()).expect("ndb");
        (tmp, ndb)
    }

    fn wait_for_note_ref(ndb: &Ndb, filter: &Filter) -> NoteRef {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(ndb).expect("txn");
            if let Ok(mut results) = ndb.query(&txn, std::slice::from_ref(filter), 1) {
                if let Some(result) = results.pop() {
                    return NoteRef::from_query_result(result);
                }
            }

            assert!(Instant::now() < deadline, "timed out waiting for test note");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn insert_new_does_not_queue_unknown_author_profiles() {
        let (_tmp, ndb) = new_test_ndb();
        let author = FullKeypair::generate();
        let filter = Filter::new()
            .authors([author.pubkey.bytes()])
            .kinds([1])
            .limit(20)
            .build();

        let note = NoteBuilder::new()
            .kind(1)
            .content("missing profile author")
            .created_at(1)
            .sign(&author.secret_key.secret_bytes())
            .build()
            .expect("note build");
        ndb.process_client_event(&note.json().expect("note json"))
            .expect("ingest note");
        let note_ref = wait_for_note_ref(&ndb, &filter);

        let txn = Transaction::new(&ndb).expect("txn");
        let mut timeline = Timeline::new(
            TimelineKind::profile(author.pubkey),
            FilterState::ready(vec![filter]),
            TimelineTab::only_notes_and_replies(),
        );
        let mut note_cache = NoteCache::default();

        timeline.insert_new(&txn, &ndb, &mut note_cache, &[note_ref]);

        assert_eq!(timeline.all_or_any_entries().len(), 1);
    }

    #[test]
    fn poll_notes_into_view_retries_key_missed_by_old_transaction() {
        let (_tmp, ndb) = new_test_ndb();
        let account_pk = Pubkey::new([0x42; 32]);
        let author = FullKeypair::generate();
        let filter = Filter::new()
            .authors([author.pubkey.bytes()])
            .kinds([1])
            .limit(20)
            .build();
        let mut timeline = Timeline::new(
            TimelineKind::profile(author.pubkey),
            FilterState::ready(vec![filter.clone()]),
            TimelineTab::only_notes_and_replies(),
        );
        if let FilterState::Ready(filter) = &timeline.filter {
            timeline
                .subscription
                .try_add_local(account_pk, &ndb, filter);
        }

        let stale_txn = Transaction::new(&ndb).expect("stale txn");
        let note = NoteBuilder::new()
            .kind(1)
            .content("committed after frame txn")
            .created_at(1)
            .sign(&author.secret_key.secret_bytes())
            .build()
            .expect("note build");
        ndb.process_client_event(&note.json().expect("note json"))
            .expect("ingest note");
        assert!(
            ndb.get_note_by_id(&stale_txn, note.id()).is_err(),
            "stale transaction should not see the newly committed note"
        );

        let mut note_cache = NoteCache::default();
        let sub = timeline
            .subscription
            .get_local(&account_pk)
            .expect("local subscription");
        let deadline = Instant::now() + Duration::from_secs(2);
        let polled_keys = loop {
            let keys = ndb.poll_for_notes(sub, TIMELINE_LOCAL_POLL_BATCH);
            if !keys.is_empty() {
                break keys;
            }
            assert!(Instant::now() < deadline, "timed out waiting for poll key");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(polled_keys.len(), 1);

        let first_insert = timeline
            .insert_polled_note_keys(&polled_keys, &ndb, &stale_txn, &mut note_cache, false)
            .expect("insert attempt");
        assert!(first_insert.inserted_keys.is_empty());
        assert_eq!(first_insert.missing_keys, polled_keys);
        timeline
            .subscription
            .push_pending_polled_note_keys(account_pk, first_insert.missing_keys);
        drop(stale_txn);

        let inserted = timeline
            .poll_notes_into_view(&account_pk, &ndb, &mut note_cache, false)
            .expect("retry pending polled key");

        assert_eq!(inserted, polled_keys);
        assert_eq!(timeline.all_or_any_entries().len(), 1);
    }

    #[test]
    fn contact_timelines_install_author_outbox_without_full_history() {
        let pk = Pubkey::new([0x11; 32]);
        let kind = TimelineKind::contact_list(pk);
        let live_filters = vec![Filter::new()
            .authors([pk.bytes()])
            .kinds([1])
            .limit(20)
            .build()];
        let (key, config) = timeline_remote_sub_declaration(
            &kind,
            live_filters.clone(),
            RelayRoutingPreference::PreferDedicated,
            remote_policy(true),
        );

        assert_eq!(
            key,
            timeline_remote_sub_key(&kind, TimelineScopedSub::RemoteBaselineByKind)
        );

        assert_eq!(
            config,
            expected_accounts_read_with_author_outbox_config(
                live_filters,
                RelayRoutingPreference::PreferDedicated,
            )
        );
    }

    #[test]
    fn last_per_pubkey_author_outbox_does_not_install_timeline_full_history() {
        let pk = Pubkey::new([0x12; 32]);
        let kind = TimelineKind::last_per_pubkey(ListKind::contact_list(pk));
        let live_filters = vec![Filter::new()
            .authors([pk.bytes()])
            .kinds([1])
            .limit(1)
            .build()];
        let (key, config) = timeline_remote_sub_declaration(
            &kind,
            live_filters.clone(),
            RelayRoutingPreference::PreferDedicated,
            remote_policy(true),
        );

        assert_eq!(
            key,
            timeline_remote_sub_key(&kind, TimelineScopedSub::RemoteBaselineByKind)
        );
        assert_eq!(
            config,
            expected_accounts_read_with_author_outbox_config(
                live_filters,
                RelayRoutingPreference::PreferDedicated,
            )
        );
    }

    #[test]
    fn author_remote_filters_install_author_outbox_remote_sub() {
        let pk = Pubkey::new([0x44; 32]);
        let people_list = PeopleListRef {
            author: pk,
            identifier: "team".to_owned(),
        };
        let cases = [
            (
                TimelineKind::profile(pk),
                vec![Filter::new()
                    .authors([pk.bytes()])
                    .kinds([1, 6, 0, 3])
                    .limit(20)
                    .build()],
            ),
            (
                TimelineKind::people_list(people_list.author, people_list.identifier.clone()),
                vec![people_list_note_filter(&people_list)],
            ),
            (
                TimelineKind::search("nostr".to_owned()),
                vec![Filter::new()
                    .authors([pk.bytes()])
                    .kinds([1])
                    .limit(20)
                    .build()],
            ),
        ];

        for (kind, remote_filters) in cases {
            let (_key, config) = timeline_remote_sub_declaration(
                &kind,
                remote_filters.clone(),
                RelayRoutingPreference::PreferDedicated,
                remote_policy(true),
            );

            assert_eq!(
                config,
                expected_accounts_read_with_author_outbox_config(
                    remote_filters,
                    RelayRoutingPreference::PreferDedicated,
                )
            );
        }
    }

    #[test]
    fn people_list_needs_remote_waits_for_local_list_note() {
        let relay = NormRelayUrl::new("ws://127.0.0.1:6557").expect("static relay url");
        let people_list = PeopleListRef {
            author: Pubkey::new([0x66; 32]),
            identifier: "team".to_owned(),
        };
        let kinds = [
            TimelineKind::people_list(people_list.author, people_list.identifier.clone()),
            TimelineKind::last_per_pubkey(ListKind::PeopleList(people_list.clone())),
        ];

        for kind in kinds {
            let mut h = TimelineRemoteHarness::with_forced_relays(vec![relay.to_string()]);
            let selected = h.selected_account_pubkey();
            let mut timeline = Timeline::new(
                kind.clone(),
                FilterState::NeedsRemote,
                TimelineTab::only_notes_and_replies(),
            );

            h.with_scoped_subs(|scoped_subs, accounts, ndb| {
                let txn = Transaction::new(ndb).expect("txn");
                setup_new_timeline(
                    &mut timeline,
                    ndb,
                    &txn,
                    scoped_subs,
                    false,
                    accounts,
                    remote_policy(true),
                );
            });

            assert!(matches!(timeline.filter, FilterState::FetchingRemote));
            assert!(!timeline.subscription.is_remote_registered(&selected));
            assert!(timeline.remote_subscription_filters.is_none());
        }
    }

    #[test]
    fn remote_filters_without_authors_use_accounts_read_only() {
        let pk = Pubkey::new([0x55; 32]);
        let kind = TimelineKind::contact_list(pk);
        let (_key, config) = timeline_remote_sub_declaration(
            &kind,
            vec![Filter::new().kinds([1]).limit(20).build()],
            RelayRoutingPreference::PreferDedicated,
            remote_policy(true),
        );

        assert_eq!(
            config,
            expected_accounts_read_only_config(
                vec![Filter::new().kinds([1]).limit(20).build()],
                RelayRoutingPreference::PreferDedicated,
            )
        );
    }

    #[test]
    fn empty_remote_filters_clear_timeline_remote_subscription() {
        let relay = NormRelayUrl::new("ws://127.0.0.1:6558").expect("static relay url");
        let mut h = TimelineRemoteHarness::with_forced_relays(vec![relay.to_string()]);
        let selected = h.selected_account_pubkey();
        let mut timeline = Timeline::new(
            TimelineKind::notifications(selected),
            FilterState::ready(vec![Filter::new().kinds(vec![1]).limit(20).build()]),
            TimelineTab::notifications(),
        );

        h.with_scoped_subs(|scoped_subs, _accounts, _ndb| {
            ensure_remote_timeline_subscription(
                &mut timeline,
                selected,
                vec![Filter::new().kinds(vec![1]).limit(20).build()],
                scoped_subs,
                remote_policy(true),
            );
        });
        assert!(timeline.subscription.is_remote_registered(&selected));

        h.with_scoped_subs(|scoped_subs, _accounts, _ndb| {
            update_remote_timeline_subscription(
                &mut timeline,
                Vec::new(),
                scoped_subs,
                remote_policy(true),
            );
        });

        assert!(!timeline.subscription.is_remote_registered(&selected));
        assert!(timeline.remote_subscription_filters.is_none());
    }

    #[test]
    fn fetching_people_list_has_no_remote_filters_for_refresh() {
        let relay = NormRelayUrl::new("ws://127.0.0.1:6557").expect("static relay url");
        let people_list = PeopleListRef {
            author: Pubkey::new([0x67; 32]),
            identifier: "team".to_owned(),
        };
        let kinds = [
            TimelineKind::people_list(people_list.author, people_list.identifier.clone()),
            TimelineKind::last_per_pubkey(ListKind::PeopleList(people_list.clone())),
        ];

        for kind in kinds {
            let mut h = TimelineRemoteHarness::with_forced_relays(vec![relay.to_string()]);
            let mut timeline = Timeline::new(
                kind,
                FilterState::NeedsRemote,
                TimelineTab::only_notes_and_replies(),
            );

            h.with_scoped_subs(|scoped_subs, accounts, ndb| {
                let txn = Transaction::new(ndb).expect("txn");
                setup_new_timeline(
                    &mut timeline,
                    ndb,
                    &txn,
                    scoped_subs,
                    false,
                    accounts,
                    remote_policy(false),
                );
            });

            assert!(matches!(timeline.filter, FilterState::FetchingRemote));
            assert!(timeline.remote_filters_for_subscription_refresh().is_none());
        }
    }

    #[test]
    fn contact_timelines_disable_outbox_relays_use_accounts_read_only() {
        let pk = Pubkey::new([0x33; 32]);
        let kinds = [
            TimelineKind::contact_list(pk),
            TimelineKind::last_per_pubkey(ListKind::contact_list(pk)),
        ];

        for kind in kinds {
            let (key, config) = timeline_remote_sub_declaration(
                &kind,
                vec![Filter::new()
                    .authors([pk.bytes()])
                    .kinds([1])
                    .limit(20)
                    .build()],
                RelayRoutingPreference::PreferDedicated,
                remote_policy(false),
            );

            assert_eq!(
                key,
                timeline_remote_sub_key(&kind, TimelineScopedSub::RemoteBaselineByKind)
            );
            assert_eq!(
                config,
                expected_accounts_read_only_config(
                    vec![Filter::new()
                        .authors([pk.bytes()])
                        .kinds([1])
                        .limit(20)
                        .build()],
                    RelayRoutingPreference::PreferDedicated,
                )
            );
        }
    }

    #[test]
    fn hashtag_timeline_uses_lowercase_tags() {
        let timeline = Timeline::hashtag(vec!["Nostr".to_owned(), "RUST".to_owned()]);
        let FilterState::Ready(filter) = timeline.filter else {
            panic!("hashtag timeline should have ready filters");
        };
        let tags = filter
            .remote()
            .iter()
            .map(|filter| tag_json(filter, "#t"))
            .collect::<Vec<_>>();

        assert_eq!(tags, vec![vec!["nostr"], vec!["rust"]]);
    }

    #[test]
    fn notifications_keep_accounts_read_selection() {
        let pk = Pubkey::new([0x22; 32]);
        let kind = TimelineKind::notifications(pk);
        let (key, config) = timeline_remote_sub_declaration(
            &kind,
            vec![kind::notifications_filter(&pk)],
            RelayRoutingPreference::RequireDedicated,
            remote_policy(true),
        );
        assert_eq!(
            key,
            timeline_remote_sub_key(&kind, TimelineScopedSub::RemoteBaselineByKind)
        );

        assert_eq!(
            config,
            SubConfig::builder(vec![kind::notifications_filter(&pk)])
                .accounts_read_critical_with_preference(RelayRoutingPreference::RequireDedicated)
                .build()
        );
    }
}
