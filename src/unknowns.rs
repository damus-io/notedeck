use crate::notecache::CachedNote;
use crate::timeline::ViewFilter;
use crate::{Damus, Result};
use enostr::{Filter, NoteId, Pubkey};
use nostrdb::{BlockType, Mention, Ndb, Note, NoteKey, Transaction};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tracing::error;

/// Unknown Id searcher
#[derive(Default)]
pub struct UnknownIds {
    ids: HashSet<UnknownId>,
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

    pub fn ids(&self) -> &HashSet<UnknownId> {
        &self.ids
    }

    pub fn ids_mut(&mut self) -> &mut HashSet<UnknownId> {
        &mut self.ids
    }

    pub fn clear(&mut self) {
        self.ids = HashSet::default();
    }

    pub fn filter(&self) -> Option<Vec<Filter>> {
        let ids: Vec<&UnknownId> = self.ids.iter().collect();
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

    pub fn update_from_note(txn: &Transaction, app: &mut Damus, note: &Note) -> bool {
        let before = app.unknown_ids.ids().len();
        let key = note.key().expect("note key");
        let cached_note = app
            .note_cache_mut()
            .cached_note_or_insert(key, note)
            .clone();
        if let Err(e) =
            get_unknown_note_ids(&app.ndb, &cached_note, txn, note, app.unknown_ids.ids_mut())
        {
            error!("UnknownIds::update_from_note {e}");
        }
        let after = app.unknown_ids.ids().len();

        if before != after {
            app.unknown_ids.mark_updated();
            true
        } else {
            false
        }
    }

    pub fn update(txn: &Transaction, app: &mut Damus) -> bool {
        let before = app.unknown_ids.ids().len();
        if let Err(e) = get_unknown_ids(txn, app) {
            error!("UnknownIds::update {e}");
        }
        let after = app.unknown_ids.ids().len();

        if before != after {
            app.unknown_ids.mark_updated();
            true
        } else {
            false
        }
    }
}

#[derive(Hash, Clone, Copy, PartialEq, Eq)]
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
pub fn get_unknown_note_ids<'a>(
    ndb: &Ndb,
    cached_note: &CachedNote,
    txn: &'a Transaction,
    note: &Note<'a>,
    ids: &mut HashSet<UnknownId>,
) -> Result<()> {
    // the author pubkey

    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
        ids.insert(UnknownId::Pubkey(Pubkey::new(*note.pubkey())));
    }

    // pull notes that notes are replying to
    if cached_note.reply.root.is_some() {
        let note_reply = cached_note.reply.borrow(note.tags());
        if let Some(root) = note_reply.root() {
            if ndb.get_note_by_id(txn, root.id).is_err() {
                ids.insert(UnknownId::Id(NoteId::new(*root.id)));
            }
        }

        if !note_reply.is_reply_to_root() {
            if let Some(reply) = note_reply.reply() {
                if ndb.get_note_by_id(txn, reply.id).is_err() {
                    ids.insert(UnknownId::Id(NoteId::new(*reply.id)));
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
                    ids.insert(UnknownId::Pubkey(Pubkey::new(*npub.pubkey())));
                }
            }
            Mention::Profile(nprofile) => {
                if ndb.get_profile_by_pubkey(txn, nprofile.pubkey()).is_err() {
                    ids.insert(UnknownId::Pubkey(Pubkey::new(*nprofile.pubkey())));
                }
            }
            Mention::Event(ev) => match ndb.get_note_by_id(txn, ev.id()) {
                Err(_) => {
                    ids.insert(UnknownId::Id(NoteId::new(*ev.id())));
                    if let Some(pk) = ev.pubkey() {
                        if ndb.get_profile_by_pubkey(txn, pk).is_err() {
                            ids.insert(UnknownId::Pubkey(Pubkey::new(*pk)));
                        }
                    }
                }
                Ok(note) => {
                    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                        ids.insert(UnknownId::Pubkey(Pubkey::new(*note.pubkey())));
                    }
                }
            },
            Mention::Note(note) => match ndb.get_note_by_id(txn, note.id()) {
                Err(_) => {
                    ids.insert(UnknownId::Id(NoteId::new(*note.id())));
                }
                Ok(note) => {
                    if ndb.get_profile_by_pubkey(txn, note.pubkey()).is_err() {
                        ids.insert(UnknownId::Pubkey(Pubkey::new(*note.pubkey())));
                    }
                }
            },
            _ => {}
        }
    }

    Ok(())
}

fn get_unknown_ids(txn: &Transaction, damus: &mut Damus) -> Result<()> {
    #[cfg(feature = "profiling")]
    puffin::profile_function!();

    let mut new_cached_notes: Vec<(NoteKey, CachedNote)> = vec![];

    for timeline in &damus.timelines {
        for noteref in timeline.notes(ViewFilter::NotesAndReplies) {
            let note = damus.ndb.get_note_by_key(txn, noteref.key)?;
            let note_key = note.key().unwrap();
            let cached_note = damus.note_cache().cached_note(noteref.key);
            let cached_note = if let Some(cn) = cached_note {
                cn.clone()
            } else {
                let new_cached_note = CachedNote::new(&note);
                new_cached_notes.push((note_key, new_cached_note.clone()));
                new_cached_note
            };

            let _ = get_unknown_note_ids(
                &damus.ndb,
                &cached_note,
                txn,
                &note,
                damus.unknown_ids.ids_mut(),
            );
        }
    }

    // This is mainly done to avoid the double mutable borrow that would happen
    // if we tried to update the note_cache mutably in the loop above
    for (note_key, note) in new_cached_notes {
        damus.note_cache_mut().cache_mut().insert(note_key, note);
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
