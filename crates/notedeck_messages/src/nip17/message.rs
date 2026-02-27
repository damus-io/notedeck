use nostrdb::Transaction;
use notedeck::enostr::RelayId;
use notedeck::{AppContext, RelayType};

use crate::cache::{ConversationCache, ConversationId};
use crate::nip17::{build_rumor_json, giftwrap_message, query_participant_dm_relays, OsRng};

pub fn send_conversation_message(
    conversation_id: ConversationId,
    content: String,
    cache: &ConversationCache,
    ctx: &mut AppContext<'_>,
) {
    if content.trim().is_empty() {
        return;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        tracing::warn!("missing conversation {conversation_id} for send action");
        return;
    };

    let Some(selected_kp) = ctx.accounts.selected_filled() else {
        tracing::warn!("cannot send message without a full keypair");
        return;
    };

    let Some(rumor_json) = build_rumor_json(
        &content,
        &conversation.metadata.participants,
        selected_kp.pubkey,
    ) else {
        tracing::error!("failed to build rumor for conversation {conversation_id}");
        return;
    };

    let Some(sender_secret) = ctx.accounts.selected_filled().map(|f| f.secret_key) else {
        return;
    };

    let txn = Transaction::new(ctx.ndb).expect("txn");
    let mut rng = OsRng;
    for participant in &conversation.metadata.participants {
        let Some(gifrwrap_note) =
            giftwrap_message(&mut rng, sender_secret, participant, &rumor_json)
        else {
            continue;
        };
        if participant == selected_kp.pubkey {
            let Some(giftwrap_json) = gifrwrap_note.json().ok() else {
                continue;
            };

            if let Err(e) = ctx.ndb.process_client_event(&giftwrap_json) {
                tracing::error!("Could not ingest event: {e:?}");
            }
        }

        let participant_relays = query_participant_dm_relays(ctx.ndb, &txn, participant);
        let relay_type = if participant_relays.is_empty() {
            RelayType::AccountsWrite
        } else {
            RelayType::Explicit(
                participant_relays
                    .into_iter()
                    .map(RelayId::Websocket)
                    .collect(),
            )
        };

        let mut publisher = ctx.remote.publisher(ctx.accounts);
        publisher.publish_note(&gifrwrap_note, relay_type);
    }
}
