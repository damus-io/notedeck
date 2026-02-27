use enostr::Pubkey;
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
        use_transparent: false,
    }
}

/// Ensures remote prefetch subscriptions for one conversation's participants.
pub(crate) fn ensure_conversation_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &ConversationCache,
    conversation_id: ConversationId,
) {
    let Some(conversation) = cache.get(conversation_id) else {
        return;
    };

    ensure_participant_prefetch(remote, accounts, &conversation.metadata.participants);
}

/// Ensures remote prefetch subscriptions for all provided participants.
pub(crate) fn ensure_participant_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    participants: &[Pubkey],
) {
    if participants.is_empty() {
        return;
    }

    let account_pk = *accounts.selected_account_pubkey();
    let owner = list_prefetch_owner_key(account_pk);
    let mut scoped_subs = remote.scoped_subs(accounts);
    ensure_participant_subs(&mut scoped_subs, owner, participants);
}

fn ensure_participant_subs(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    owner: SubOwnerKey,
    participants: &[Pubkey],
) {
    for participant in participants {
        let key = list_fetch_sub_key(participant);
        let spec = participant_relay_prefetch_spec(participant);
        let identity = ScopedSubIdentity::account(owner, key);
        let _ = scoped_subs.ensure_sub(identity, spec);
    }
}
