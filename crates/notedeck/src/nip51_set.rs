use enostr::Pubkey;
use indexmap::IndexMap;
use nostrdb::{Filter, Ndb, Note, Transaction};

use crate::{RelayPool, UnifiedSubscription, UnknownIds};

/// Keeps track of most recent NIP-51 sets
#[derive(Debug)]
pub struct Nip51SetCache {
    pub sub: UnifiedSubscription,
    cached_notes: IndexMap<PackId, Nip51Set>,
}

type PackId = String;

impl Nip51SetCache {
    pub fn new(
        pool: &mut RelayPool,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
        nip51_set_filter: Vec<Filter>,
    ) -> Option<Self> {
        let mut cached_notes = IndexMap::default();

        let notes: Option<Vec<Note>> = if let Ok(results) = ndb.query(txn, &nip51_set_filter, 500) {
            Some(results.into_iter().map(|r| r.note).collect())
        } else {
            None
        };

        if let Some(notes) = notes {
            add(notes, &mut cached_notes, ndb, txn, unknown_ids);
        }

        let sub = match ndb.subscribe(&nip51_set_filter) {
            Ok(sub) => sub,
            Err(e) => {
                tracing::error!("Could not ndb subscribe: {e}");
                return None;
            }
        };
        let remote = pool.subscribe(nip51_set_filter);

        Some(Self {
            sub: UnifiedSubscription { local: sub, remote },
            cached_notes,
        })
    }

    pub fn poll_for_notes(&mut self, ndb: &Ndb, unknown_ids: &mut UnknownIds) {
        let new_notes = ndb.poll_for_notes(self.sub.local, 5);

        if new_notes.is_empty() {
            return;
        }

        let txn = Transaction::new(ndb).expect("txn");
        let notes: Vec<Note> = new_notes
            .into_iter()
            .filter_map(|new_note_key| ndb.get_note_by_key(&txn, new_note_key).ok())
            .collect();

        add(notes, &mut self.cached_notes, ndb, &txn, unknown_ids);
    }

    pub fn iter(&self) -> impl IntoIterator<Item = &Nip51Set> {
        self.cached_notes.values()
    }

    pub fn len(&self) -> usize {
        self.cached_notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cached_notes.is_empty()
    }

    pub fn at_index(&self, index: usize) -> Option<&Nip51Set> {
        self.cached_notes.get_index(index).map(|(_, s)| s)
    }
}

fn add(
    notes: Vec<Note>,
    cache: &mut IndexMap<PackId, Nip51Set>,
    ndb: &Ndb,
    txn: &Transaction,
    unknown_ids: &mut UnknownIds,
) {
    for note in notes {
        let Some(new_pack) = create_nip51_set(note) else {
            continue;
        };

        if let Some(cur_cached) = cache.get(&new_pack.identifier) {
            if new_pack.created_at <= cur_cached.created_at {
                continue;
            }
        }

        for pk in &new_pack.pks {
            unknown_ids.add_pubkey_if_missing(ndb, txn, pk);
        }

        cache.insert(new_pack.identifier.clone(), new_pack);
    }
}

pub fn create_nip51_set(note: Note) -> Option<Nip51Set> {
    let mut identifier = None;
    let mut title = None;
    let mut image = None;
    let mut description = None;
    let mut pks = Vec::new();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(first) = tag.get_str(0) else {
            continue;
        };

        match first {
            "p" => {
                let Some(pk) = tag.get_id(1) else {
                    continue;
                };

                pks.push(Pubkey::new(*pk));
            }
            "d" => {
                let Some(id) = tag.get_str(1) else {
                    continue;
                };

                identifier = Some(id.to_owned());
            }
            "image" => {
                let Some(cur_img) = tag.get_str(1) else {
                    continue;
                };

                image = Some(cur_img.to_owned());
            }
            "title" => {
                let Some(cur_title) = tag.get_str(1) else {
                    continue;
                };

                title = Some(cur_title.to_owned());
            }
            "description" => {
                let Some(cur_desc) = tag.get_str(1) else {
                    continue;
                };

                description = Some(cur_desc.to_owned());
            }
            _ => {
                continue;
            }
        };
    }

    let identifier = identifier?;

    Some(Nip51Set {
        identifier,
        title,
        image,
        description,
        pks,
        created_at: note.created_at(),
    })
}

/// NIP-51 Set. Read only (do not use for writing)
pub struct Nip51Set {
    pub identifier: String, // 'd' tag
    pub title: Option<String>,
    pub image: Option<String>,
    pub description: Option<String>,
    pub pks: Vec<Pubkey>,
    created_at: u64,
}

impl std::fmt::Debug for Nip51Set {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nip51Set")
            .field("identifier", &self.identifier)
            .field("title", &self.title)
            .field("image", &self.image)
            .field("description", &self.description)
            .field("pks", &self.pks.len())
            .field("created_at", &self.created_at)
            .finish()
    }
}
