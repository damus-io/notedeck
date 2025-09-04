use egui_nav::ReturnType;
use egui_virtual_list::VirtualList;
use enostr::{NoteId, RelayPool};
use hashbrown::{hash_map::RawEntryMut, HashMap};
use nostrdb::{Filter, Ndb, Note, NoteKey, NoteReplyBuf, Transaction};
use notedeck::{NoteCache, NoteRef, UnknownIds};

use crate::{
    actionbar::{process_thread_notes, NewThreadNotes},
    multi_subscriber::ThreadSubs,
    timeline::{
        note_units::{NoteUnits, UnitKey},
        unit::NoteUnit,
        InsertionResponse,
    },
};

use super::ThreadSelection;

pub struct ThreadNode {
    pub replies: SingleNoteUnits,
    pub prev: ParentState,
    pub have_all_ancestors: bool,
    pub list: VirtualList,
    pub set_scroll_offset: Option<f32>,
}

#[derive(Clone)]
pub enum ParentState {
    Unknown,
    None,
    Parent(NoteId),
}

impl ThreadNode {
    pub fn new(parent: ParentState) -> Self {
        Self {
            replies: SingleNoteUnits::new(true),
            prev: parent,
            have_all_ancestors: false,
            list: VirtualList::new(),
            set_scroll_offset: None,
        }
    }

    pub fn with_offset(mut self, offset: f32) -> Self {
        self.set_scroll_offset = Some(offset);
        self
    }
}

#[derive(Default)]
pub struct Threads {
    pub threads: HashMap<NoteId, ThreadNode>,
    pub subs: ThreadSubs,

    pub seen_flags: NoteSeenFlags,
}

impl Threads {
    /// Opening a thread.
    /// Similar to [[super::cache::TimelineCache::open]]
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut RelayPool,
        thread: &ThreadSelection,
        new_scope: bool,
        col: usize,
        scroll_offset: f32,
    ) -> Option<NewThreadNotes> {
        tracing::info!("Opening thread: {:?}", thread);
        let local_sub_filter = if let Some(selected) = &thread.selected_note {
            vec![direct_replies_filter_non_root(
                selected.bytes(),
                thread.root_id.bytes(),
            )]
        } else {
            vec![direct_replies_filter_root(thread.root_id.bytes())]
        };

        let selected_note_id = thread.selected_or_root();
        self.seen_flags.mark_seen(selected_note_id);

        let filter = match self.threads.raw_entry_mut().from_key(&selected_note_id) {
            RawEntryMut::Occupied(_entry) => {
                // TODO(kernelkind): reenable this once the panic is fixed
                //
                // let node = entry.into_mut();
                // if let Some(first) = node.replies.first() {
                //     &filter::make_filters_since(&local_sub_filter, first.created_at + 1)
                // } else {
                //     &local_sub_filter
                // }
                &local_sub_filter
            }
            RawEntryMut::Vacant(entry) => {
                let id = NoteId::new(*selected_note_id);

                let node = ThreadNode::new(ParentState::Unknown).with_offset(scroll_offset);
                entry.insert(id, node);

                &local_sub_filter
            }
        };

        let new_notes = ndb.query(txn, filter, 500).ok().map(|r| {
            r.into_iter()
                .map(NoteRef::from_query_result)
                .collect::<Vec<_>>()
        });

        self.subs
            .subscribe(ndb, pool, col, thread, local_sub_filter, new_scope, || {
                replies_filter_remote(thread)
            });

        new_notes.map(|notes| NewThreadNotes {
            selected_note_id: NoteId::new(*selected_note_id),
            notes: notes.into_iter().map(|f| f.key).collect(),
        })
    }

    pub fn close(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        thread: &ThreadSelection,
        return_type: ReturnType,
        id: usize,
    ) {
        tracing::info!("Closing thread: {:?}", thread);
        self.subs.unsubscribe(ndb, pool, id, thread, return_type);
    }

    /// Responsible for making sure the chain and the direct replies are up to date
    pub fn update(
        &mut self,
        selected: &Note<'_>,
        note_cache: &mut NoteCache,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        col: usize,
    ) {
        let Some(selected_key) = selected.key() else {
            tracing::error!("Selected note did not have a key");
            return;
        };

        let reply = note_cache
            .cached_note_or_insert_mut(selected_key, selected)
            .reply;

        self.fill_reply_chain_recursive(selected, &reply, note_cache, ndb, txn, unknown_ids);
        let node = self
            .threads
            .get_mut(&selected.id())
            .expect("should be guarenteed to exist from `Self::fill_reply_chain_recursive`");

        let Some(sub) = self.subs.get_local(col) else {
            tracing::error!("Was expecting to find local sub");
            return;
        };

        let keys = ndb.poll_for_notes(sub.sub, 10);

        if keys.is_empty() {
            return;
        }

        tracing::info!("Got {} new notes", keys.len());

        process_thread_notes(
            &keys,
            node,
            &mut self.seen_flags,
            ndb,
            txn,
            unknown_ids,
            note_cache,
        );
    }

    fn fill_reply_chain_recursive(
        &mut self,
        cur_note: &Note<'_>,
        cur_reply: &NoteReplyBuf,
        note_cache: &mut NoteCache,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
    ) -> bool {
        let (unknown_parent_state, mut have_all_ancestors) = self
            .threads
            .get(&cur_note.id())
            .map(|t| (matches!(t.prev, ParentState::Unknown), t.have_all_ancestors))
            .unwrap_or((true, false));

        if have_all_ancestors {
            return true;
        }

        let mut new_parent = None;

        let note_reply = cur_reply.borrow(cur_note.tags());

        let next_link = 's: {
            let Some(parent) = note_reply.reply() else {
                break 's NextLink::None;
            };

            if unknown_parent_state {
                new_parent = Some(ParentState::Parent(NoteId::new(*parent.id)));
            }

            let Ok(reply_note) = ndb.get_note_by_id(txn, parent.id) else {
                break 's NextLink::Unknown(parent.id);
            };

            let Some(notekey) = reply_note.key() else {
                break 's NextLink::Unknown(parent.id);
            };

            NextLink::Next(reply_note, notekey)
        };

        match next_link {
            NextLink::Unknown(parent) => {
                unknown_ids.add_note_id_if_missing(ndb, txn, parent);
            }
            NextLink::Next(next_note, note_key) => {
                UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &next_note);

                let cached_note = note_cache.cached_note_or_insert_mut(note_key, &next_note);

                let next_reply = cached_note.reply;
                if self.fill_reply_chain_recursive(
                    &next_note,
                    &next_reply,
                    note_cache,
                    ndb,
                    txn,
                    unknown_ids,
                ) {
                    have_all_ancestors = true;
                }

                if !self.seen_flags.contains(next_note.id()) {
                    self.seen_flags.mark_replies(
                        next_note.id(),
                        selected_has_at_least_n_replies(ndb, txn, None, next_note.id(), 2),
                    );
                }
            }
            NextLink::None => {
                have_all_ancestors = true;
                new_parent = Some(ParentState::None);
            }
        }

        match self.threads.raw_entry_mut().from_key(&cur_note.id()) {
            RawEntryMut::Occupied(entry) => {
                let node = entry.into_mut();
                if let Some(parent) = new_parent {
                    node.prev = parent;
                }

                if have_all_ancestors {
                    node.have_all_ancestors = true;
                }
            }
            RawEntryMut::Vacant(entry) => {
                let id = NoteId::new(*cur_note.id());
                let parent = new_parent.unwrap_or(ParentState::Unknown);
                let (_, res) = entry.insert(id, ThreadNode::new(parent));

                if have_all_ancestors {
                    res.have_all_ancestors = true;
                }
            }
        }

        have_all_ancestors
    }
}

enum NextLink<'a> {
    Unknown(&'a [u8; 32]),
    Next(Note<'a>, NoteKey),
    None,
}

pub fn selected_has_at_least_n_replies(
    ndb: &Ndb,
    txn: &Transaction,
    selected: Option<&[u8; 32]>,
    root: &[u8; 32],
    n: u8,
) -> bool {
    let filter = if let Some(selected) = selected {
        &vec![direct_replies_filter_non_root(selected, root)]
    } else {
        &vec![direct_replies_filter_root(root)]
    };

    let Ok(res) = ndb.query(txn, filter, n as i32) else {
        return false;
    };

    res.len() >= n.into()
}

fn direct_replies_filter_non_root(
    selected_note_id: &[u8; 32],
    root_id: &[u8; 32],
) -> nostrdb::Filter {
    let tmp_selected = *selected_note_id;
    nostrdb::Filter::new()
        .kinds([1])
        .custom(move |note: nostrdb::Note<'_>| {
            let reply = nostrdb::NoteReply::new(note.tags());
            if reply.is_reply_to_root() {
                return false;
            }

            reply.reply().is_some_and(|r| r.id == &tmp_selected)
        })
        .event(root_id)
        .build()
}

/// Custom filter requirements:
/// - Do NOT capture references (e.g. `*root_id`) inside the closure
/// - Instead, copy values outside and capture them with `move`
///
/// Incorrect:
///     .custom(|_| { *root_id })       // ❌
/// Also Incorrect:
///     .custom(move |_| { *root_id })  // ❌
/// Correct:
///     let tmp = *root_id;
///     .custom(move |_| { tmp })       // ✅
fn direct_replies_filter_root(root_id: &[u8; 32]) -> nostrdb::Filter {
    let moved_root_id = *root_id;
    nostrdb::Filter::new()
        .kinds([1])
        .custom(move |note: nostrdb::Note<'_>| {
            nostrdb::NoteReply::new(note.tags())
                .reply_to_root()
                .is_some_and(|r| r.id == &moved_root_id)
        })
        .event(root_id)
        .build()
}

fn replies_filter_remote(selection: &ThreadSelection) -> Vec<Filter> {
    vec![
        nostrdb::Filter::new()
            .kinds([1])
            .event(selection.root_id.bytes())
            .build(),
        nostrdb::Filter::new()
            .ids([selection.root_id.bytes()])
            .limit(1)
            .build(),
    ]
}

/// Represents indicators that there is more content in the note to view
#[derive(Default)]
pub struct NoteSeenFlags {
    // true indicates the note has replies AND it has not been read
    pub flags: HashMap<NoteId, bool>,
}

impl NoteSeenFlags {
    pub fn mark_seen(&mut self, note_id: &[u8; 32]) {
        self.flags.insert(NoteId::new(*note_id), false);
    }

    pub fn mark_replies(&mut self, note_id: &[u8; 32], has_replies: bool) {
        self.flags.insert(NoteId::new(*note_id), has_replies);
    }

    pub fn get(&self, note_id: &[u8; 32]) -> Option<&bool> {
        self.flags.get(&note_id)
    }

    pub fn contains(&self, note_id: &[u8; 32]) -> bool {
        self.flags.contains_key(&note_id)
    }
}

#[derive(Default)]
pub struct SingleNoteUnits {
    units: NoteUnits,
}

impl SingleNoteUnits {
    pub fn new(reversed: bool) -> Self {
        Self {
            units: NoteUnits::new_with_cap(0, reversed),
        }
    }

    pub fn insert(&mut self, note_ref: NoteRef) -> InsertionResponse {
        self.units.merge_single_unit(note_ref)
    }

    pub fn values(&self) -> impl Iterator<Item = &NoteRef> {
        self.units.values().filter_map(|entry| {
            if let NoteUnit::Single(note_ref) = entry {
                Some(note_ref)
            } else {
                None
            }
        })
    }

    pub fn contains_key(&self, k: &NoteKey) -> bool {
        self.units.contains_key(&UnitKey::Single(*k))
    }
}
