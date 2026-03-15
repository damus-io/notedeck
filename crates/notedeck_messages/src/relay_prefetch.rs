use enostr::{Pubkey, RelayRoutingPreference};
use notedeck::{
    Accounts, RelaySelection, RemoteApi, ScopedSubApi, ScopedSubIdentity, SubConfig, SubOwnerKey,
};

use crate::{
    cache::{ConversationCache, ConversationId},
    list_fetch_sub_key, list_prefetch_owner_key,
    nip17::participant_dm_relay_list_filter,
};

/// Pure builder for the scoped-sub spec used to prefetch one participant relay list.
fn participant_relay_prefetch_spec(participant: &Pubkey) -> SubConfig {
    SubConfig {
        relays: RelaySelection::AccountsRead,
        filters: vec![participant_dm_relay_list_filter(participant)],
        routing_preference: RelayRoutingPreference::default(),
    }
}

/// Ensures remote prefetch subscriptions for one conversation's participants.
#[profiling::function]
pub(crate) fn ensure_conversation_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &ConversationCache,
    conversation_id: ConversationId,
) {
    let Some(conversation) = cache.get(conversation_id) else {
        tracing::debug!(
            "skipping participant relay-list prefetch for missing conversation {conversation_id}"
        );
        return;
    };

    tracing::debug!(
        "ensuring participant relay-list prefetch for conversation {conversation_id} with {} participant(s)",
        conversation.metadata.participants.len()
    );
    ensure_participant_prefetch(remote, accounts, &conversation.metadata.participants);
}

/// Ensures remote prefetch subscriptions for all provided participants.
#[profiling::function]
pub(crate) fn ensure_participant_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    participants: &[Pubkey],
) {
    if participants.is_empty() {
        tracing::debug!("skipping participant relay-list prefetch for empty participant set");
        return;
    }

    let account_pk = *accounts.selected_account_pubkey();
    let owner = list_prefetch_owner_key(account_pk);
    tracing::debug!(
        "ensuring participant relay-list prefetch for selected_account={} participant_count={}",
        account_pk,
        participants.len()
    );
    let mut scoped_subs = remote.scoped_subs(accounts);
    ensure_participant_subs(&mut scoped_subs, owner, participants);
}

#[profiling::function]
fn ensure_participant_subs(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    owner: SubOwnerKey,
    participants: &[Pubkey],
) {
    for participant in participants {
        let key = list_fetch_sub_key(participant);
        let spec = participant_relay_prefetch_spec(participant);
        let identity = ScopedSubIdentity::account(owner, key);
        tracing::trace!(
            "ensuring participant relay-list prefetch sub owner={owner:?} participant={} sub_key={key:?}",
            participant
        );
        let _ = scoped_subs.ensure_sub(identity, spec);
    }
}
