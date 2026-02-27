use std::cmp::Ordering;

use crate::{
    cache::{
        message_store::NotePkg,
        registry::{
            ConversationId, ConversationIdentifierUnowned, ConversationRegistry,
            ParticipantSetUnowned,
        },
    },
    convo_renderable::ConversationRenderable,
    nip17::get_participants,
    relay_ensure::DmListState,
};

use super::message_store::MessageStore;
use enostr::Pubkey;
use hashbrown::HashMap;
use nostrdb::{Ndb, Note, NoteKey, Subscription, Transaction};
use notedeck::{note::event_tag, NoteCache, NoteRef, UnknownIds};

pub struct ConversationCache {
    pub registry: ConversationRegistry,
    conversations: HashMap<ConversationId, Conversation>,
    order: Vec<ConversationOrder>,
    pub state: ConversationListState,
    dm_relay_list_ensure: DmListState,
    pub active: Option<ConversationId>,
}

impl ConversationCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.conversations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty()
    }

    pub fn get(&self, id: ConversationId) -> Option<&Conversation> {
        self.conversations.get(&id)
    }

    pub fn get_id_by_index(&self, i: usize) -> Option<&ConversationId> {
        Some(&self.order.get(i)?.id)
    }

    pub fn get_active(&self) -> Option<&Conversation> {
        self.conversations.get(&self.active?)
    }

    #[profiling::function]
    pub fn ingest_chatroom_msg(
        &mut self,
        note: Note,
        key: NoteKey,
        ndb: &Ndb,
        txn: &Transaction,
        note_cache: &mut NoteCache,
        unknown_ids: &mut UnknownIds,
    ) {
        let participants = get_participants(&note);

        let id = self
            .registry
            .get_or_insert(ConversationIdentifierUnowned::Nip17(
                ParticipantSetUnowned::new(participants.clone()),
            ));

        let conversation = self.conversations.entry(id).or_insert_with(|| {
            let participants: Vec<Pubkey> =
                participants.into_iter().map(|p| Pubkey::new(*p)).collect();

            Conversation::new(id, participants)
        });

        tracing::trace!("ingesting into conversation id {id}: {:?}", note.json());
        UnknownIds::update_from_note(txn, ndb, unknown_ids, note_cache, &note);
        if conversation.ingest_kind_14(note, key) {
            let latest = conversation.last_activity();
            refresh_order(&mut self.order, id, LatestMessage::Latest(latest));
        }
    }

    pub fn initialize_conversation(&mut self, id: ConversationId, participants: Vec<Pubkey>) {
        if self.conversations.contains_key(&id) {
            return;
        }

        self.conversations
            .insert(id, Conversation::new(id, participants));

        refresh_order(&mut self.order, id, LatestMessage::NoMessages);
    }

    pub fn first_convo_id(&self) -> Option<ConversationId> {
        Some(self.order.first()?.id)
    }

    /// Mutable access to the selected-account DM relay-list ensure state.
    pub fn dm_relay_list_ensure_mut(&mut self) -> &mut DmListState {
        &mut self.dm_relay_list_ensure
    }
}

fn refresh_order(order: &mut Vec<ConversationOrder>, id: ConversationId, latest: LatestMessage) {
    if let Some(pos) = order.iter().position(|entry| entry.id == id) {
        order.remove(pos);
    }

    let entry = ConversationOrder { id, latest };
    let idx = match order.binary_search(&entry) {
        Ok(idx) | Err(idx) => idx,
    };
    order.insert(idx, entry);
}

#[derive(Clone, Copy, Debug)]
struct ConversationOrder {
    id: ConversationId,
    latest: LatestMessage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LatestMessage {
    NoMessages,
    Latest(u64),
}

impl PartialOrd for LatestMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LatestMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (LatestMessage::Latest(a), LatestMessage::Latest(b)) => a.cmp(b),
            (LatestMessage::NoMessages, LatestMessage::NoMessages) => Ordering::Equal,
            (LatestMessage::NoMessages, _) => Ordering::Greater,
            (_, LatestMessage::NoMessages) => Ordering::Less,
        }
    }
}

impl PartialEq for ConversationOrder {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ConversationOrder {}

impl PartialOrd for ConversationOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ConversationOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        // newer first
        match other.latest.cmp(&self.latest) {
            Ordering::Equal => self.id.cmp(&other.id),
            non_eq => non_eq,
        }
    }
}

pub struct Conversation {
    pub id: ConversationId,
    pub messages: MessageStore,
    pub metadata: ConversationMetadata,
    pub renderable: ConversationRenderable,
}

impl Conversation {
    pub fn new(id: ConversationId, participants: Vec<Pubkey>) -> Self {
        Self {
            id,
            messages: MessageStore::default(),
            metadata: ConversationMetadata::new(participants),
            renderable: ConversationRenderable::new(&[]),
        }
    }

    fn last_activity(&self) -> u64 {
        self.messages.newest_timestamp().unwrap_or(0)
    }

    pub fn ingest_kind_14(&mut self, note: Note, key: NoteKey) -> bool {
        if note.kind() != 14 {
            tracing::error!("tried to ingest a non-kind 14 note...");
            return false;
        }

        if let Some(title) = event_tag(&note, "subject") {
            let created = note.created_at();

            if self
                .metadata
                .title
                .as_ref()
                .is_none_or(|cur| created > cur.last_modified)
            {
                self.metadata.title = Some(TitleMetadata {
                    title: title.to_string(),
                    last_modified: created,
                });
            }
        }

        let inserted = self.messages.insert(NotePkg {
            note_ref: NoteRef {
                key,
                created_at: note.created_at(),
            },
            author: Pubkey::new(*note.pubkey()),
        });

        if inserted {
            self.renderable = ConversationRenderable::new(&self.messages.messages_ordered);
        }

        inserted
    }
}

impl Default for ConversationCache {
    fn default() -> Self {
        Self {
            registry: ConversationRegistry::default(),
            conversations: HashMap::new(),
            order: Vec::new(),
            state: Default::default(),
            dm_relay_list_ensure: Default::default(),
            active: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConversationMetadata {
    pub title: Option<TitleMetadata>,
    pub participants: Vec<Pubkey>,
}

#[derive(Clone, Debug)]
pub struct TitleMetadata {
    pub title: String,
    pub last_modified: u64,
}

impl ConversationMetadata {
    pub fn new(participants: Vec<Pubkey>) -> Self {
        Self {
            title: None,
            participants,
        }
    }
}

/// Tracks the conversation list initialization and subscription lifecycle.
#[derive(Default)]
pub enum ConversationListState {
    /// No loader request has been issued yet.
    #[default]
    Initializing,
    /// Loader is streaming the initial conversation list.
    Loading {
        /// Optional live subscription for incoming conversation updates.
        subscription: Option<Subscription>,
    },
    /// Initial load completed; subscription remains active if available.
    Initialized(Option<Subscription>), // conversation list filter
}
