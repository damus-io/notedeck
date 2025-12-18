mod message_store;
mod registry;

pub use message_store::{MessageStore, NotePkg};
pub use registry::{
    ConversationId, ConversationIdentifier, ConversationIdentifierUnowned, ParticipantSetUnowned,
};
