use nostrdb::Transaction;
use notedeck::enostr::RelayId;
use notedeck::AppContext;

use crate::cache::{ConversationCache, ConversationId};
use crate::nip17::query_participant_dm_relays;
use crate::nip17::{build_rumor_json, giftwrap_message, OsRng};

pub(crate) enum SendMessageResult {
    Sent,
    NotSent { content: String },
}

pub(crate) fn send_conversation_message(
    conversation_id: ConversationId,
    content: String,
    cache: &ConversationCache,
    ctx: &mut AppContext<'_>,
) -> SendMessageResult {
    if content.trim().is_empty() {
        return SendMessageResult::Sent;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        tracing::warn!("missing conversation {conversation_id} for send action");
        return SendMessageResult::NotSent { content };
    };

    let Some(selected_kp) = ctx.accounts.selected_filled() else {
        tracing::warn!("cannot send message without a full keypair");
        return SendMessageResult::NotSent { content };
    };
    let selected_pubkey = *selected_kp.pubkey;
    let sender_secret = selected_kp.secret_key.clone();

    let txn = Transaction::new(ctx.ndb).expect("txn");
    let mut participant_routes = Vec::new();
    for participant in &conversation.metadata.participants {
        let relays = query_participant_dm_relays(ctx.ndb, &txn, participant);
        if relays.is_empty() {
            tracing::warn!(
                participant = %participant,
                "cannot send message until participant dm relay list is available"
            );
            return SendMessageResult::NotSent { content };
        }

        participant_routes.push((*participant, relays));
    }
    drop(txn);

    let participants = participant_routes
        .iter()
        .map(|(participant, _)| *participant)
        .collect::<Vec<_>>();
    let Some(rumor_json) = build_rumor_json(&content, &participants, &selected_pubkey) else {
        tracing::error!("failed to build rumor for conversation {conversation_id}");
        return SendMessageResult::NotSent { content };
    };

    let mut rng = OsRng;
    for (participant, relays) in participant_routes {
        let Some(giftwrap_note) =
            giftwrap_message(&mut rng, &sender_secret, &participant, &rumor_json)
        else {
            continue;
        };

        if participant == selected_pubkey {
            let Some(giftwrap_json) = giftwrap_note.json().ok() else {
                continue;
            };

            if let Err(e) = ctx.ndb.process_client_event(&giftwrap_json) {
                tracing::error!("Could not ingest event: {e:?}");
            }
        }

        let relays = relays
            .into_iter()
            .map(RelayId::Websocket)
            .collect::<Vec<_>>();
        let mut publisher = ctx.remote.publisher_explicit();
        publisher.publish_note(&giftwrap_note, relays);
    }

    SendMessageResult::Sent
}
