use std::collections::HashSet;

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Note, NoteKey, Subscription, Transaction};

#[derive(Clone)]
pub struct Contacts {
    pub filter: Filter,
    pub(super) state: ContactState,
}

pub struct ContactSnapshot<'a> {
    pub contacts: &'a HashSet<Pubkey>,
    pub timestamp: u64,
}

#[derive(Clone)]
pub enum ContactState {
    Unreceived,
    Received {
        contacts: HashSet<Pubkey>,
        note_key: NoteKey,
        timestamp: u64,
    },
}

#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum IsFollowing {
    /// We don't have the contact list, so we don't know
    Unknown,

    /// We are follow
    Yes,

    No,
}

impl Contacts {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        let filter = Filter::new().authors([pubkey]).kinds([3]).limit(1).build();

        Self {
            filter,
            state: ContactState::Unreceived,
        }
    }

    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        let binding = ndb
            .query(txn, std::slice::from_ref(&self.filter), 1)
            .expect("query user relays results");

        let Some(res) = binding.first() else {
            return;
        };

        update_state(&mut self.state, &res.note, res.note_key);
    }

    pub fn is_following(&self, other_pubkey: &[u8; 32]) -> IsFollowing {
        match &self.state {
            ContactState::Unreceived => IsFollowing::Unknown,
            ContactState::Received {
                contacts,
                note_key: _,
                timestamp: _,
            } => {
                if contacts.contains(other_pubkey) {
                    IsFollowing::Yes
                } else {
                    IsFollowing::No
                }
            }
        }
    }

    pub(super) fn poll_for_updates(&mut self, ndb: &Ndb, txn: &Transaction, sub: Subscription) {
        let nks = ndb.poll_for_notes(sub, 1);

        let Some(key) = nks.first() else {
            return;
        };

        let note = match ndb.get_note_by_key(txn, *key) {
            Ok(note) => note,
            Err(e) => {
                tracing::error!("Could not find note at key {:?}: {e}", key);
                return;
            }
        };

        if let ContactState::Received {
            contacts: _,
            note_key: _,
            timestamp,
        } = self.get_state()
        {
            if *timestamp > note.created_at() {
                // the current contact list is more up to date than the one we just received. ignore it.
                return;
            }
        }

        update_state(&mut self.state, &note, *key);
    }

    pub fn get_state(&self) -> &ContactState {
        &self.state
    }

    pub fn snapshot(&self) -> Option<ContactSnapshot<'_>> {
        match &self.state {
            ContactState::Received {
                contacts,
                timestamp,
                ..
            } => Some(ContactSnapshot {
                contacts,
                timestamp: *timestamp,
            }),
            ContactState::Unreceived => None,
        }
    }
}

fn update_state(state: &mut ContactState, note: &Note, key: NoteKey) {
    match state {
        ContactState::Unreceived => {
            *state = ContactState::Received {
                contacts: get_contacts_owned(note),
                note_key: key,
                timestamp: note.created_at(),
            };
        }
        ContactState::Received {
            contacts,
            note_key,
            timestamp,
        } => {
            update_contacts(contacts, note);
            *note_key = key;
            *timestamp = note.created_at();
        }
    };
}

fn get_contacts<'a>(note: &Note<'a>) -> HashSet<&'a [u8; 32]> {
    let mut contacts = HashSet::with_capacity(note.tags().count().into());

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some("p") = tag.get_str(0) else {
            continue;
        };

        let Some(cur_id) = tag.get_id(1) else {
            continue;
        };

        contacts.insert(cur_id);
    }

    contacts
}

fn get_contacts_owned(note: &Note<'_>) -> HashSet<Pubkey> {
    get_contacts(note)
        .iter()
        .map(|p| Pubkey::new(**p))
        .collect()
}

fn update_contacts(cur: &mut HashSet<Pubkey>, new: &Note<'_>) {
    let new_contacts = get_contacts(new);

    cur.retain(|pk| new_contacts.contains(pk.bytes()));

    new_contacts.iter().for_each(|c| {
        if !cur.contains(*c) {
            cur.insert(Pubkey::new(**c));
        }
    });
}
