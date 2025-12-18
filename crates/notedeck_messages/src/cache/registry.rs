use enostr::Pubkey;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use std::{
    fmt::Debug,
    hash::{BuildHasher, Hash},
};

pub type ConversationId = u32;

#[derive(Default)]
pub struct ConversationRegistry {
    next_id: ConversationId,
    conversation_ids: HashMap<ConversationIdentifier, ConversationId>,
}

impl ConversationRegistry {
    pub fn get(&self, id: ConversationIdentifierUnowned) -> Option<ConversationId> {
        let hash = id.hash(self.conversation_ids.hasher());
        self.conversation_ids
            .raw_entry()
            .from_hash(hash, |existing| id.matches(existing))
            .map(|(_, v)| *v)
    }

    pub fn get_or_insert(&mut self, id: ConversationIdentifierUnowned) -> ConversationId {
        let hash = id.hash(self.conversation_ids.hasher());
        let id_c = id.clone();

        let uid = match self
            .conversation_ids
            .raw_entry_mut()
            .from_hash(hash, |existing| id.matches(existing))
        {
            RawEntryMut::Occupied(entry) => *entry.get(),
            RawEntryMut::Vacant(entry) => {
                let owned = id.into_owned();
                let uid = self.next_id;
                entry.insert(owned, uid);
                self.next_id = self.next_id.wrapping_add(1);
                uid
            }
        };
        tracing::info!("normalized conversation id: {id_c:?} | uid: {uid}");
        uid
    }

    pub fn insert(&mut self, id: ConversationIdentifier) -> ConversationId {
        let uid = self.next_id;
        self.conversation_ids.insert(id, uid);
        self.next_id = self.next_id.wrapping_add(1);

        uid
    }
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub enum ConversationIdentifier {
    Nip17(ParticipantSet),
}

#[derive(Debug, Clone)]
pub enum ConversationIdentifierUnowned<'a> {
    Nip17(ParticipantSetUnowned<'a>),
}

// Set of Pubkeys, sorted and deduplicated
#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct ParticipantSet(Vec<[u8; 32]>);

impl ParticipantSet {
    pub fn new(mut items: Vec<[u8; 32]>) -> Self {
        items.sort();
        items.dedup();
        Self(items)
    }
}

#[derive(Clone)]
pub struct ParticipantSetUnowned<'a>(Vec<&'a [u8; 32]>);

impl<'a> ParticipantSetUnowned<'a> {
    pub fn new(mut items: Vec<&'a [u8; 32]>) -> Self {
        items.sort();
        items.dedup();
        Self(items)
    }

    pub fn normalize(&mut self) {
        self.0.sort_unstable();
        self.0.dedup();
    }

    fn hash_with<S: BuildHasher>(&self, build_hasher: &S) -> u64 {
        build_hasher.hash_one(&self.0)
    }

    fn matches(&self, owned: &ParticipantSet) -> bool {
        if self.0.len() != owned.0.len() {
            return false;
        }

        self.0
            .iter()
            .zip(&owned.0)
            .all(|(left, right)| *left == right)
    }

    fn into_owned(self) -> ParticipantSet {
        let owned = self.0.into_iter().copied().collect();
        ParticipantSet::new(owned)
    }
}

impl<'a> Debug for ParticipantSetUnowned<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hexes: Vec<String> = self
            .0
            .iter()
            .map(|bytes| Pubkey::new(**bytes).hex())
            .collect();

        f.debug_tuple("ConversationParticipantsUnowned")
            .field(&hexes)
            .finish()
    }
}

impl<'a> ConversationIdentifierUnowned<'a> {
    fn hash<S: BuildHasher>(&self, build_hasher: &S) -> u64 {
        match self {
            Self::Nip17(participants) => participants.hash_with(build_hasher),
        }
    }

    fn matches(&self, owned: &ConversationIdentifier) -> bool {
        match (self, owned) {
            (Self::Nip17(left), ConversationIdentifier::Nip17(right)) => left.matches(right),
        }
    }

    fn into_owned(self) -> ConversationIdentifier {
        match self {
            Self::Nip17(participants) => ConversationIdentifier::Nip17(participants.into_owned()),
        }
    }
}
