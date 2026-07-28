use enostr::Pubkey;
use notedeck::{
    Accounts, FullHistoryConfig, RemoteApi, ScopedSubApi, ScopedSubIdentity, SubConfig, SubOwnerKey,
};

use crate::{
    cache::{ConversationCache, ConversationId},
    list_prefetch_owner_key, list_prefetch_sub_key,
    nip17::participant_dm_relay_list_prefetch_filter,
};

/// Pure builder for the scoped-sub spec used to prefetch participant relay lists.
fn participant_relay_prefetch_spec(participants: &[Pubkey]) -> SubConfig {
    let filter = participant_dm_relay_list_prefetch_filter(participants);

    SubConfig::live(vec![filter.clone()])
        .full_history(FullHistoryConfig::new(vec![filter]))
        .build()
}

/// Ensures remote prefetch subscriptions for one conversation's participants.
#[profiling::function]
pub(crate) fn ensure_conversation_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &mut ConversationCache,
    conversation_id: ConversationId,
) {
    let Some(participants) = cache
        .get(conversation_id)
        .map(|conversation| conversation.metadata.participants.clone())
    else {
        tracing::debug!(
            "skipping participant relay-list prefetch for missing conversation {conversation_id}"
        );
        return;
    };

    tracing::debug!(
        "ensuring participant relay-list prefetch for conversation {conversation_id} with {} participant(s)",
        participants.len()
    );
    ensure_participant_prefetch(remote, accounts, cache, &participants);
}

/// Ensures remote prefetch subscriptions for all provided participants.
#[profiling::function]
pub(crate) fn ensure_participant_prefetch(
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    cache: &mut ConversationCache,
    participants: &[Pubkey],
) {
    let participants =
        participant_prefetch_candidates(accounts.selected_account_pubkey(), participants);

    if participants.is_empty() {
        tracing::debug!("skipping participant relay-list prefetch for empty participant set");
        return;
    }

    if !cache.extend_dm_relay_list_prefetch_participants(&participants) {
        tracing::trace!(
            "skipping unchanged participant relay-list prefetch for selected_account={} participant_count={}",
            accounts.selected_account_pubkey(),
            cache.dm_relay_list_prefetch_participants().len()
        );
        return;
    }

    let participants = cache.dm_relay_list_prefetch_participants();
    let account_pk = *accounts.selected_account_pubkey();
    let owner = list_prefetch_owner_key(account_pk);
    tracing::debug!(
        "ensuring participant relay-list prefetch for selected_account={} participant_count={}",
        account_pk,
        participants.len()
    );
    let mut scoped_subs = remote.scoped_subs(accounts);
    set_participant_prefetch_sub(&mut scoped_subs, owner, participants);
}

/// Returns relay-list prefetch candidates, excluding the selected account itself.
fn participant_prefetch_candidates(
    selected_account: &Pubkey,
    participants: &[Pubkey],
) -> Vec<Pubkey> {
    let mut candidates: Vec<Pubkey> = participants
        .iter()
        .copied()
        .filter(|participant| participant != selected_account)
        .collect();

    candidates.sort_unstable_by(|a, b| a.bytes().cmp(b.bytes()));
    candidates.dedup();
    candidates
}

/// Sets the single account-scoped relay-list prefetch declaration.
#[profiling::function]
fn set_participant_prefetch_sub(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    owner: SubOwnerKey,
    participants: &[Pubkey],
) {
    let key = list_prefetch_sub_key();
    let spec = participant_relay_prefetch_spec(participants);
    let identity = ScopedSubIdentity::account(owner, key);
    tracing::trace!(
        "setting participant relay-list prefetch sub owner={owner:?} participant_count={} sub_key={key:?}",
        participants.len()
    );
    let _ = scoped_subs.set_sub(identity, spec);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies participant relay-list prefetch uses one account-level key.
    #[test]
    fn participant_relay_prefetch_sub_key_is_stable_account_level_key() {
        let participant_a = Pubkey::new([0x22; 32]);
        let participant_b = Pubkey::new([0x33; 32]);

        assert_eq!(list_prefetch_sub_key(), list_prefetch_sub_key());
        assert_ne!(
            list_prefetch_sub_key(),
            crate::list_fetch_sub_key(&participant_a)
        );
        assert_ne!(
            list_prefetch_sub_key(),
            crate::list_fetch_sub_key(&participant_b)
        );
    }

    /// Verifies relay-list prefetch does not request the selected account's own kind `10050`.
    #[test]
    fn participant_prefetch_candidates_exclude_selected_account() {
        let selected = Pubkey::new([0x11; 32]);
        let participant_a = Pubkey::new([0x33; 32]);
        let participant_b = Pubkey::new([0x22; 32]);

        let candidates = participant_prefetch_candidates(
            &selected,
            &[participant_a, selected, participant_b, participant_a],
        );

        assert_eq!(candidates, vec![participant_b, participant_a]);
    }
}
