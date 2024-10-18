use crate::{
    error::Error,
    filter::{self, FilterState},
    note::NoteRef,
    notecache::{CachedNote, NoteCache},
    unknowns::UnknownIds,
    subscriptions::Subscriptions,
    Result,
};

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use egui_virtual_list::VirtualList;
use enostr::RelayPool;
use nostrdb::{Ndb, Note, Subscription, Transaction};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::hash::Hash;
use std::rc::Rc;

use tracing::{debug, error, info, warn};

pub mod cache;
pub mod kind;
pub mod route;

pub use cache::{CachedTimeline, TimelineCache, TimelineCacheKey};
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
    pub filter: FilterState,
    pub views: Vec<TimelineTab>,
    pub selected_view: i32,

    /// Our nostrdb subscription
    pub subscription: Option<Subscription>
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
    pub fn unsubscribe(&mut self, subscriptions: &Subscriptions) {
        let sub_id = if let Some(sub_id) = self.subscription {
            sub_id
        } else {
            warn!("could not unsubscribe from timeline, no subscription id");
            return;
        };

        subscriptions.unsubscribe(sub_id);
        self.subscription = None;
    }

    /// Create a timeline from a contact list
    pub fn contact_list(contact_list: &Note, pk_src: PubkeySource) -> Result<Self> {
        let filter = filter::filter_from_tags(contact_list)?.into_follow_filter();

        Ok(Timeline::new(
            TimelineKind::contact_list(pk_src),
            FilterState::ready(filter),
        ))
    }

    pub fn make_view_id(id: TimelineId, selected_view: i32) -> egui::Id {
        egui::Id::new((id, selected_view))
    }

    pub fn view_id(&self) -> egui::Id {
        Timeline::make_view_id(self.id, self.selected_view)
    }

    pub fn new(kind: TimelineKind, filter: FilterState) -> Self {
        // global unique id for all new timelines
        static UIDS: AtomicU32 = AtomicU32::new(0);

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
        timeline_ind: usize,
        timelines: &mut Vec<Timeline>,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) -> Result<()> {
        let timeline = timelines
            .get_mut(timeline_ind)
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
