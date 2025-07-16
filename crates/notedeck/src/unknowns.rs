use crate::{
    Result,
    note::NoteRef,
    notecache::{CachedNote, NoteCache},
};

use enostr::{Filter, NoteId, Pubkey};
use nostr::RelayUrl;
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::error;

#[must_use = "process_action should be used on this result"]
pub enum SingleUnkIdAction {
    NoAction,
    NeedsProcess(UnknownId),
}

#[must_use = "process_action should be used on this result"]
pub enum NoteRefsUnkIdAction {
    NoAction,
    NeedsProcess(Vec<NoteRef>),
}

impl NoteRefsUnkIdAction {
    pub fn new(refs: Vec<NoteRef>) -> Self {
        NoteRefsUnkIdAction::NeedsProcess(refs)
    }

    pub fn no_action() -> Self {
        Self::NoAction
    }

    pub fn process_action(
        &self,
        txn: &Transaction,
        ndb: &Ndb,
        unk_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
    ) {
        match self {
            Self::NoAction => {}
            Self::NeedsProcess(refs) => {
                UnknownIds::update_from_note_refs(txn, ndb, unk_ids, note_cache, refs);
            }
        }
    }
}

impl SingleUnkIdAction {
    pub fn new(id: UnknownId) -> Self {
        SingleUnkIdAction::NeedsProcess(id)
    }

    pub fn no_action() -> Self {
        Self::NoAction
    }

    pub fn pubkey(pubkey: Pubkey) -> Self {
        SingleUnkIdAction::new(UnknownId::Pubkey(pubkey))
    }

    pub fn note_id(note_id: NoteId) -> Self {
        SingleUnkIdAction::new(UnknownId::Id(note_id))
    }

    /// Some functions may return unknown id actions that need to be processed.
    /// For example, when we add a new account we need to make sure we have the
    /// profile for that account. This function ensures we add this to the
    /// unknown id tracker without adding side effects to functions.
    pub fn process_action(&self, ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        match self {
            Self::NeedsProcess(id) => {
                ids.add_unknown_id_if_missing(ndb, txn, id);
            }
            Self::NoAction => {}
        }
    }
}

/// Unknown Id searcher
#[derive(Default, Debug)]
pub struct UnknownIds {
    ids: HashMap<UnknownId, HashSet<RelayUrl>>,
    first_updated: Option<Instant>,
    last_updated: Option<Instant>,
}

impl UnknownIds {
    /// Simple debouncer
    pub fn ready_to_send(&self) -> bool {
        if self.ids.is_empty() {
            return false;
        }

        // we trigger on first set
        if self.first_updated == self.last_updated {
            return true;
        }

        let last_updated = if let Some(last) = self.last_updated {
            last
        } else {
            // if we've
            return true;
        };

        Instant::now() - last_updated >= Duration::from_secs(2)
    }

    pub fn ids_iter(&self) -> impl ExactSizeIterator<Item = &UnknownId> {
        self.ids.keys()
    }

    pub fn ids_mut(&mut self) -> &mut HashMap<UnknownId, HashSet<RelayUrl>> {
        &mut self.ids
    }

    pub fn clear(&mut self) {
        self.ids = HashMap::default();
    }

    pub fn filter(&self) -> Option<Vec<Filter>> {
        let ids: Vec<&UnknownId> = self.ids.keys().collect();
        get_unknown_ids_filter(&ids)
    }

    /// We've updated some unknown ids, update the last_updated time to now
    pub fn mark_updated(&mut self) {
        let now = Instant::now();
        if self.first_updated.is_none() {
            self.first_updated = Some(now);
        }
        self.last_updated = Some(now);
    }

    pub fn update_from_note_key(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        key: NoteKey,
    ) -> bool {
        let note = if let Ok(note) = ndb.get_note_by_key(txn, key) {
            note
        } else {
            return false;
        };

        UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note)
    }

    /// Should be called on freshly polled notes from subscriptions
    pub fn update_from_note_refs(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        note_refs: &[NoteRef],
    ) {
        for note_ref in note_refs {
            Self::update_from_note_key(txn, ndb, unknown_ids, note_cache, note_ref.key);
        }
    }

    pub fn update_from_note(
        txn: &Transaction,
        ndb: &Ndb,
        unknown_ids: &mut UnknownIds,
        note_cache: &mut NoteCache,
        note: &Note,
    ) -> bool {
        let before = unknown_ids.ids_iter().len();
        let key = note.key().expect("note key");
        //let cached_note = note_cache.cached_note_or_insert(key, note).clone();
        let cached_note = note_cache.cached_note_or_insert(key, note);
        if let Err(e) = get_unknown_note_ids(ndb, cached_note, txn, note, unknown_ids.ids_mut()) {
            error!("UnknownIds::update_from_note {e}");
        }
        let after = unknown_ids.ids_iter().len();

        if before != after {
            unknown_ids.mark_updated();
            true
        } else {
            false
        }
    }

    pub fn add_unknown_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, unk_id: &UnknownId) {
        match unk_id {
            UnknownId::Pubkey(pk) => self.add_pubkey_if_missing(ndb, txn, pk),
            UnknownId::Id(note_id) => self.add_note_id_if_missing(ndb, txn, note_id.bytes()),
        }
    }

    pub fn add_pubkey_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, pubkey: &Pubkey) {
        // we already have this profile, skip
        if ndb.get_profile_by_pubkey(txn, pubkey).is_ok() {
            return;
        }

        let unknown_id = UnknownId::Pubkey(*pubkey);
        if self.ids.contains_key(&unknown_id) {
            return;
        }
        self.ids.entry(unknown_id).or_default();
        self.mark_updated();
    }

    pub fn add_note_id_if_missing(&mut self, ndb: &Ndb, txn: &Transaction, note_id: &[u8; 32]) {
        // we already have this note, skip
        if ndb.get_note_by_id(txn, note_id).is_ok() {
            return;
        }

        let unknown_id = UnknownId::Id(NoteId::new(*note_id));
        if self.ids.contains_key(&unknown_id) {
            return;
        }
        self.ids.entry(unknown_id).or_default();
        self.mark_updated();
    }
}

#[derive(Hash, Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnknownId {
    Pubkey(Pubkey),
    Id(NoteId),
}

impl UnknownId {
    pub fn is_pubkey(&self) -> Option<&Pubkey> {
        match self {
            UnknownId::Pubkey(pk) => Some(pk),
            _ => None,
        }
    }

    pub fn is_id(&self) -> Option<&NoteId> {
        match self {
            UnknownId::Id(id) => Some(id),
            _ => None,
        }
    }
}

/// Look for missing notes in various parts of notes that we see:
///
/// - pubkeys and notes mentioned inside the note
/// - notes being replied to
///
/// We return all of this in a HashSet so that we can fetch these from
/// remote relays.
///
#[profiling::function]
pub fn get_unknown_note_ids<'a>(
    ndb: &Ndb,
    cached_note: &CachedNote,
    txn: &'a Transaction,
    note: &Note<'a>,
    ids: &mut HashMap<UnknownId, HashSet<RelayUrl>>,
) -> Result<()> {
    // the author pubkey
    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
        ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
            .or_default();
    }

    // pull notes that notes are replying to
    if cached_note.reply.root.is_some() {
        let note_reply = cached_note.reply.borrow(note.tags());
        if let Some(root) = note_reply.root() {
            if ndb.get_note_by_id(txn, root.id).is_err() {
                ids.entry(UnknownId::Id(NoteId::new(*root.id))).or_default();
            }
        }

        if !note_reply.is_reply_to_root() {
            if let Some(reply) = note_reply.reply() {
                if ndb.get_note_by_id(txn, reply.id).is_err() {
                    ids.entry(UnknownId::Id(NoteId::new(*reply.id)))
                        .or_default();
                }
            }
        }
    }

    let blocks = ndb.get_blocks_by_key(txn, note.key().expect("note key"))?;
    for block in blocks.iter(note) {
        if block.blocktype() != BlockType::MentionBech32 {
            continue;
        }

        match block.as_mention().unwrap() {
            Mention::Pubkey(npub) => {
                if ndb.get_profile_by_pubkey(txn, npub.pubkey()).is_err() {
                    ids.entry(UnknownId::Pubkey(Pubkey::new(*npub.pubkey())))
                        .or_default();
                }
            }
            Mention::Profile(nprofile) => {
                if ndb.get_profile_by_pubkey(txn, nprofile.pubkey()).is_err() {
                    let id = UnknownId::Pubkey(Pubkey::new(*nprofile.pubkey()));
                    let relays = nprofile
                        .relays_iter()
                        .filter_map(|s| RelayUrl::parse(s).ok())
                        .collect::<HashSet<RelayUrl>>();
                    ids.entry(id).or_default().extend(relays);
                }
            }
            Mention::Event(ev) => {
                let relays = ev
                    .relays_iter()
                    .filter_map(|s| RelayUrl::parse(s).ok())
                    .collect::<HashSet<RelayUrl>>();
                match ndb.get_note_by_id(txn, ev.id()) {
                    Err(_) => {
                        ids.entry(UnknownId::Id(NoteId::new(*ev.id())))
                            .or_default()
                            .extend(relays.clone());
                        if let Some(pk) = ev.pubkey() {
                            if ndb.get_profile_by_pubkey(txn, pk).is_err() {
                                ids.entry(UnknownId::Pubkey(Pubkey::new(*pk)))
                                    .or_default()
                                    .extend(relays);
                            }
                        }
                    }
                    Ok(note) => {
                        if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                            ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
                                .or_default()
                                .extend(relays);
                        }
                    }
                }
            }
            Mention::Note(note) => match ndb.get_note_by_id(txn, note.id()) {
                Err(_) => {
                    ids.entry(UnknownId::Id(NoteId::new(*note.id())))
                        .or_default();
                }
                Ok(note) => {
                    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                        ids.entry(UnknownId::Pubkey(Pubkey::new(*note.pubkey())))
                            .or_default();
                    }
                }
            },
            _ => {}
        }
    }

    Ok(())
}

fn get_unknown_ids_filter(ids: &[&UnknownId]) -> Option<Vec<Filter>> {
    if ids.is_empty() {
        return None;
    }

    let ids = &ids[0..500.min(ids.len())];
    let mut filters: Vec<Filter> = vec![];

    let pks: Vec<&[u8; 32]> = ids
        .iter()
        .flat_map(|id| id.is_pubkey().map(|pk| pk.bytes()))
        .collect();
    if !pks.is_empty() {
        let pk_filter = Filter::new().authors(pks).kinds([0]).build();
        filters.push(pk_filter);
    }

    let note_ids: Vec<&[u8; 32]> = ids
        .iter()
        .flat_map(|id| id.is_id().map(|id| id.bytes()))
        .collect();
    if !note_ids.is_empty() {
        filters.push(Filter::new().ids(note_ids).build());
    }

    Some(filters)
}
