mod action;
mod context;

pub use action::{NoteAction, ReactAction, ScrollInfo, ZapAction, ZapTargetAmount};
pub use context::{BroadcastContext, ContextSelection, NoteContextSelection};

use crate::Accounts;
use crate::GlobalWallet;
use crate::JobPool;
use crate::Localization;
use crate::UnknownIds;
use crate::{
    notecache::NoteCache,
    outbox::{dispatch_unknown_ids, OutboxManager},
    zaps::Zaps,
    Images,
};
use enostr::{NoteId, RelayPool};
use nostrdb::{Ndb, Note, NoteKey, QueryResult, Transaction};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;

/// Aggregates dependencies to reduce the number of parameters
/// passed to inner UI elements, minimizing prop drilling.
pub struct NoteContext<'d> {
    pub ndb: &'d Ndb,
    pub accounts: &'d Accounts,
    pub global_wallet: &'d GlobalWallet,
    pub i18n: &'d mut Localization,
    pub img_cache: &'d mut Images,
    pub note_cache: &'d mut NoteCache,
    pub zaps: &'d mut Zaps,
    pub pool: &'d mut RelayPool,
    pub job_pool: &'d mut JobPool,
    pub outbox: &'d mut OutboxManager,
    pub unknown_ids: &'d mut UnknownIds,
    pub clipboard: &'d mut egui_winit::clipboard::Clipboard,
}

impl<'d> NoteContext<'d> {
    /// Flush pending unknown-id lookups through the shared outbox pipeline. The
    /// helper returns whether a network request was dispatched so callers can
    /// decide if they should show additional loading state.
    pub fn drive_unknown_ids(&mut self, ctx: &egui::Context) -> bool {
        if !self.unknown_ids.ready_to_send() {
            return false;
        }

        let wakeup = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };

        dispatch_unknown_ids(
            self.unknown_ids,
            self.outbox,
            self.pool,
            self.ndb,
            wakeup,
        )
        .is_some()
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub struct NoteRef {
    pub key: NoteKey,
    pub created_at: u64,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct RootNoteIdBuf([u8; 32]);

impl fmt::Debug for RootNoteIdBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RootNoteIdBuf({})", self.hex())
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub struct RootNoteId<'a>(&'a [u8; 32]);

impl RootNoteIdBuf {
    pub fn to_note_id(self) -> NoteId {
        NoteId::new(self.0)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn new(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        id: &[u8; 32],
    ) -> Result<RootNoteIdBuf, RootIdError> {
        root_note_id_from_selected_id(ndb, note_cache, txn, id).map(|rnid| Self(*rnid.bytes()))
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn new_unsafe(id: [u8; 32]) -> Self {
        Self(id)
    }

    pub fn borrow(&self) -> RootNoteId<'_> {
        RootNoteId(self.bytes())
    }
}

impl<'a> RootNoteId<'a> {
    pub fn to_note_id(self) -> NoteId {
        NoteId::new(*self.0)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        self.0
    }

    pub fn hex(&self) -> String {
        hex::encode(self.bytes())
    }

    pub fn to_owned(&self) -> RootNoteIdBuf {
        RootNoteIdBuf::new_unsafe(*self.bytes())
    }

    pub fn new(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &'a Transaction,
        id: &'a [u8; 32],
    ) -> Result<RootNoteId<'a>, RootIdError> {
        root_note_id_from_selected_id(ndb, note_cache, txn, id)
    }

    pub fn new_unsafe(id: &'a [u8; 32]) -> Self {
        Self(id)
    }
}

impl Borrow<[u8; 32]> for RootNoteIdBuf {
    fn borrow(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Borrow<[u8; 32]> for RootNoteId<'_> {
    fn borrow(&self) -> &[u8; 32] {
        self.0
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

#[derive(Debug, Copy, Clone)]
pub enum RootIdError {
    NoteNotFound,
    NoRootId,
}

pub fn root_note_id_from_selected_id<'txn, 'a>(
    ndb: &Ndb,
    note_cache: &mut NoteCache,
    txn: &'txn Transaction,
    selected_note_id: &'a [u8; 32],
) -> Result<RootNoteId<'txn>, RootIdError>
where
    'a: 'txn,
{
    let selected_note_key = if let Ok(key) = ndb.get_notekey_by_id(txn, selected_note_id) {
        key
    } else {
        return Err(RootIdError::NoteNotFound);
    };

    let note = if let Ok(note) = ndb.get_note_by_key(txn, selected_note_key) {
        note
    } else {
        return Err(RootIdError::NoteNotFound);
    };

    note_cache
        .cached_note_or_insert(selected_note_key, &note)
        .reply
        .borrow(note.tags())
        .root()
        .map_or_else(
            || Ok(RootNoteId::new_unsafe(selected_note_id)),
            |rnid| Ok(RootNoteId::new_unsafe(rnid.id)),
        )
}

pub fn event_tag<'a>(ev: &nostrdb::Note<'a>, name: &str) -> Option<&'a str> {
    ev.tags().iter().find_map(|tag| {
        if tag.count() < 2 {
            return None;
        }

        let cur_name = tag.get_str(0)?;

        if cur_name != name {
            return None;
        }

        tag.get_str(1)
    })
}

/// Temporary way of checking whether a user has sent a reaction.
/// Should be replaced with nostrdb metadata
pub fn reaction_sent_id(sender_pk: &enostr::Pubkey, note_reacted_to: &[u8; 32]) -> egui::Id {
    egui::Id::new(("sent-reaction-id", note_reacted_to, sender_pk))
}
