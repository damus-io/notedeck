pub mod message;

use enostr::{FullKeypair, NormRelayUrl, Pubkey, SecretKey};
use hashbrown::HashSet;
pub use message::send_conversation_message;
pub use nostr::secp256k1::rand::rngs::OsRng;
use nostr::secp256k1::rand::Rng;
use nostr::{
    event::{EventBuilder, Kind, Tag},
    key::PublicKey,
    nips::nip44,
    util::JsonUtil,
};
use nostrdb::{Filter, FilterBuilder, Ndb, Note, NoteBuilder, Transaction};
use notedeck::get_p_tags;

fn build_rumor_json(
    message: &str,
    participants: &[Pubkey],
    sender_pubkey: &Pubkey,
) -> Option<String> {
    let sender = nostrcrate_pk(sender_pubkey)?;
    let mut tags = Vec::new();
    for participant in participants {
        if let Some(pk) = nostrcrate_pk(participant) {
            tags.push(Tag::public_key(pk));
        } else {
            tracing::warn!("invalid participant {}", participant);
        }
    }

    let builder = EventBuilder::new(Kind::PrivateDirectMessage, message).tags(tags);
    Some(builder.build(sender).as_json())
}

pub fn giftwrap_message(
    rng: &mut OsRng,
    sender_secret: &SecretKey,
    recipient: &Pubkey,
    rumor_json: &str,
) -> Option<Note<'static>> {
    let Some(recipient_pk) = nostrcrate_pk(recipient) else {
        tracing::warn!("failed to convert recipient pubkey {}", recipient);
        return None;
    };

    let encrypted_rumor = match nip44::encrypt_with_rng(
        rng,
        sender_secret,
        &recipient_pk,
        rumor_json,
        nip44::Version::V2,
    ) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!("failed to encrypt rumor for {recipient}: {err}");
            return None;
        }
    };

    let seal_created = randomized_timestamp(rng);
    let Some(seal_json) = build_seal_json(&encrypted_rumor, sender_secret, seal_created) else {
        tracing::error!("failed to build seal for recipient {}", recipient);
        return None;
    };

    let wrap_keys = FullKeypair::generate();
    let encrypted_seal = match nip44::encrypt_with_rng(
        rng,
        &wrap_keys.secret_key,
        &recipient_pk,
        &seal_json,
        nip44::Version::V2,
    ) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!("failed to encrypt seal for wrap: {err}");
            return None;
        }
    };

    let wrap_created = randomized_timestamp(rng);
    build_giftwrap_note(&encrypted_seal, &wrap_keys, recipient, wrap_created)
}

fn build_seal_json(
    content_ciphertext: &str,
    sender_secret: &SecretKey,
    created_at: u64,
) -> Option<String> {
    let builder = NoteBuilder::new()
        .kind(13)
        .content(content_ciphertext)
        .created_at(created_at);

    builder
        .sign(&sender_secret.secret_bytes())
        .build()?
        .json()
        .ok()
}

fn build_giftwrap_note(
    content: &str,
    wrap_keys: &FullKeypair,
    recipient: &Pubkey,
    created_at: u64,
) -> Option<Note<'static>> {
    let builder = NoteBuilder::new()
        .kind(1059)
        .content(content)
        .created_at(created_at)
        .start_tag()
        .tag_str("p")
        .tag_str(&recipient.hex());

    builder.sign(&wrap_keys.secret_key.secret_bytes()).build()
}

fn nostrcrate_pk(pk: &Pubkey) -> Option<PublicKey> {
    PublicKey::from_slice(pk.bytes()).ok()
}

fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn randomized_timestamp(rng: &mut OsRng) -> u64 {
    const MAX_SKEW_SECS: u64 = 2 * 24 * 60 * 60;
    let now = current_timestamp();
    let tweak = rng.gen_range(0..=MAX_SKEW_SECS);
    now.saturating_sub(tweak)
}

#[profiling::function]
pub fn get_participants<'a>(note: &Note<'a>) -> Vec<&'a [u8; 32]> {
    let mut participants = get_p_tags(note);
    let chat_message_sender = note.pubkey();
    if !participants.contains(&chat_message_sender) {
        // the chat message sender must be in the participants set
        participants.push(chat_message_sender);
    }
    participants
}

pub fn conversation_filter(cur_acc: &Pubkey) -> Vec<Filter> {
    vec![
        FilterBuilder::new()
            .kinds([14])
            .pubkey([cur_acc.bytes()])
            .build(),
        FilterBuilder::new()
            .kinds([14])
            .authors([cur_acc.bytes()])
            .build(),
    ]
}

/// Unfortunately this gives an OR across participants
pub fn chatroom_filter(participants: Vec<&[u8; 32]>, me: &[u8; 32]) -> Vec<Filter> {
    vec![FilterBuilder::new()
        .kinds([14])
        .authors(participants.clone())
        .pubkey([me])
        .build()]
}

/// Builds a filter for one participant's kind `10050` DM relay list.
pub fn participant_dm_relay_list_filter(participant: &Pubkey) -> Filter {
    FilterBuilder::new()
        .kinds([10050])
        .authors([participant.bytes()])
        .limit(1)
        .build()
}

/// Returns `true` when `note` is a kind `10050` DM relay-list authored by `participant`.
pub fn is_participant_dm_relay_list(note: &Note<'_>, participant: &Pubkey) -> bool {
    note.kind() == 10050 && note.pubkey() == participant.bytes()
}

/// Queries NDB for presence of one participant's kind `10050` DM relay list.
pub fn has_participant_dm_relay_list(ndb: &Ndb, txn: &Transaction, participant: &Pubkey) -> bool {
    let filter = participant_dm_relay_list_filter(participant);
    let Ok(results) = ndb.query(txn, std::slice::from_ref(&filter), 1) else {
        return false;
    };

    !results.is_empty()
}

/// Default relay URLs used when creating a new kind `10050` DM relay-list note.
pub fn default_dm_relay_urls() -> &'static [&'static str] {
    &["wss://relay.damus.io", "wss://nos.lol"]
}

/// Builds a signed kind `10050` DM relay-list note using default relay URLs.
pub fn build_default_dm_relay_list_note(sender_secret: &SecretKey) -> Option<Note<'static>> {
    let mut builder = NoteBuilder::new().kind(10050).content("");

    for relay in default_dm_relay_urls() {
        builder = builder.start_tag().tag_str("relay").tag_str(relay);
    }

    builder.sign(&sender_secret.secret_bytes()).build()
}

/// Parses a kind `10050` note into unique websocket relay URLs.
pub fn parse_dm_relay_list_relays(note: &Note<'_>) -> Vec<NormRelayUrl> {
    if note.kind() != 10050 {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut relays = Vec::new();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some("relay") = tag.get_str(0) else {
            continue;
        };

        let Some(url) = tag.get_str(1) else {
            continue;
        };

        let Ok(norm_url) = NormRelayUrl::new(url) else {
            continue;
        };

        if !seen.insert(norm_url.clone()) {
            continue;
        }

        relays.push(norm_url);
    }

    relays
}

/// Queries NDB for one participant's latest kind `10050` relay list.
///
/// Returns explicit websocket relay URLs when available, else an empty vec.
pub fn query_participant_dm_relays(
    ndb: &Ndb,
    txn: &Transaction,
    participant: &Pubkey,
) -> Vec<NormRelayUrl> {
    let filter = participant_dm_relay_list_filter(participant);
    let Ok(results) = ndb.query(txn, std::slice::from_ref(&filter), 1) else {
        return Vec::new();
    };

    let Some(result) = results.first() else {
        return Vec::new();
    };

    parse_dm_relay_list_relays(&result.note)
}

// easily retrievable from Note<'a>
pub struct Nip17ChatMessage<'a> {
    pub sender: &'a [u8; 32],
    pub p_tags: Vec<&'a [u8; 32]>,
    pub subject: Option<&'a str>,
    pub reply_to: Option<&'a [u8; 32]>, // NoteId
    pub message: &'a str,
    pub created_at: u64,
}

pub fn parse_chat_message<'a>(note: &Note<'a>) -> Option<Nip17ChatMessage<'a>> {
    if note.kind() != 14 {
        return None;
    }

    let mut p_tags = Vec::new();
    let mut subject = None;
    let mut reply_to = None;

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }
        let Some(first) = tag.get_str(0) else {
            continue;
        };

        if first == "p" {
            if let Some(id) = tag.get_id(1) {
                p_tags.push(id);
            }
        } else if first == "subject" {
            subject = tag.get_str(1);
        } else if first == "e" {
            reply_to = tag.get_id(1);
        }
    }

    Some(Nip17ChatMessage {
        sender: note.pubkey(),
        p_tags,
        subject,
        reply_to,
        message: note.content(),
        created_at: note.created_at(),
    })
}
