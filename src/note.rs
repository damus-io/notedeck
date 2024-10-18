use crate::notecache::NoteCache;
use nostrdb::{Ndb, Note, NoteKey, QueryResult, Transaction};
use enostr::NoteId;
use std::cmp::Ordering;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct NoteRef {
    pub key: NoteKey,
    pub created_at: u64,
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub struct RootNoteId([u8; 32]);

impl RootNoteId {
    pub fn to_note_id(self) -> NoteId {
        NoteId::new(self.0)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn new(ndb: &Ndb, note_cache: &mut NoteCache, txn: &Transaction, id: &[u8; 32]) -> Self {
        RootNoteId(*root_note_id_from_selected_id(ndb, note_cache, txn, id))
    }

    pub fn new_unsafe(id: &[u8; 32]) -> Self {
        RootNoteId(*id)
    }
}

impl NoteRef {
    pub fn new(key: NoteKey, created_at: u64) -> Self {
        NoteRef { key, created_at }
    }

    pub fn from_note(note: &Note<'_>) -> Self {
        let created_at = note.created_at();
        let key = note.key().expect("todo: implement NoteBuf");
        NoteRef::new(key, created_at)
    }

    pub fn from_query_result(qr: QueryResult<'_>) -> Self {
        NoteRef {
            key: qr.note_key,
            created_at: qr.note.created_at(),
        }
    }
}

impl Ord for NoteRef {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.created_at.cmp(&other.created_at) {
            Ordering::Equal => self.key.cmp(&other.key),
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        }
    }
}

impl PartialOrd for NoteRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn root_note_id_from_selected_id<'a>(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    txn: &'a Transaction,
    selected_note_id: &'a [u8; 32],
) -> &'a [u8; 32] {
    let selected_note_key = if let Ok(key) = ndb
        .get_notekey_by_id(txn, selected_note_id)
        .map(NoteKey::new)
    {
        key
    } else {
        return selected_note_id;
    };

    let note = if let Ok(note) = ndb.get_note_by_key(txn, selected_note_key) {
        note
    } else {
        return selected_note_id;
    };

    note_cache
        .cached_note_or_insert(selected_note_key, &note)
        .reply
        .borrow(note.tags())
        .root()
        .map_or_else(|| selected_note_id, |nr| nr.id)
}
