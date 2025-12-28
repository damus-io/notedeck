use enostr::Pubkey;
use hashbrown::HashSet;
use nostrdb::NoteKey;
use notedeck::NoteRef;
use std::cmp::Ordering;

/// Maintains a strictly ordered list of message references for a single
/// conversation. It mirrors the lightweight ordering guarantees that
/// `TimelineCache` and `Threads` rely on so UI code can assume the
/// backing data is already sorted from newest to oldest.
#[derive(Default)]
pub struct MessageStore {
    pub messages_ordered: Vec<NotePkg>,
    seen: HashSet<NoteKey>,
}

impl MessageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a new `NoteRef` while keeping the store sorted. Returns
    /// `true` when the reference was new to the conversation.
    pub fn insert(&mut self, note: NotePkg) -> bool {
        if !self.seen.insert(note.note_ref.key) {
            return false;
        }

        match self.messages_ordered.binary_search(&note) {
            Ok(_) => {
                debug_assert!(
                    false,
                    "MessageStore::insert was asked to insert a duplicate NoteRef"
                );
                false
            }
            Err(idx) => {
                self.messages_ordered.insert(idx, note);
                true
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages_ordered.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages_ordered.len()
    }

    pub fn latest(&self) -> Option<&NoteRef> {
        self.messages_ordered.first().map(|p| &p.note_ref)
    }

    pub fn newest_timestamp(&self) -> Option<u64> {
        self.latest().map(|n| n.created_at)
    }
}

pub struct NotePkg {
    pub note_ref: NoteRef,
    pub author: Pubkey,
}

impl Ord for NotePkg {
    fn cmp(&self, other: &Self) -> Ordering {
        self.note_ref
            .cmp(&other.note_ref)
            .then_with(|| self.author.cmp(&other.author))
    }
}

impl PartialOrd for NotePkg {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for NotePkg {
    fn eq(&self, other: &Self) -> bool {
        self.note_ref == other.note_ref && self.author == other.author
    }
}

impl Eq for NotePkg {}
