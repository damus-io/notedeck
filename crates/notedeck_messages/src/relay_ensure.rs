use enostr::Pubkey;
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{
    Accounts, RelaySelection, RelayType, RemoteApi, ScopedSubEoseStatus, ScopedSubIdentity,
    SubConfig, SubKey, SubOwnerKey,
};

use crate::{
    list_ensure_owner_key, list_fetch_sub_key,
    nip17::{
        build_default_dm_relay_list_note, is_participant_dm_relay_list,
        participant_dm_relay_list_filter,
    },
};

/// Local view over the dependencies used by the DM relay-list ensure state machine.
struct EnsureListCtx<'a, 'remote> {
    ndb: &'a mut Ndb,
    remote: &'a mut RemoteApi<'remote>,
    accounts: &'a Accounts,
    owner_key: SubOwnerKey,
}

/// Pure builder for the selected account's own DM relay-list ensure scoped-sub spec.
fn dm_relay_list_spec(selected_account: &Pubkey) -> SubConfig {
    SubConfig {
        relays: RelaySelection::AccountsRead,
        filters: vec![participant_dm_relay_list_filter(selected_account)],
        use_transparent: false,
    }
}

#[profiling::function]
pub(crate) fn ensure_selected_account_dm_list(
    ndb: &mut Ndb,
    remote: &mut RemoteApi<'_>,
    accounts: &Accounts,
    ensure_state: &mut DmListState,
) {
    let DmListState::Finding(state) = ensure_state else {
        return;
    };

    let selected_account = *accounts.selected_account_pubkey();
    let mut ctx = EnsureListCtx {
        ndb,
        remote,
        accounts,
        owner_key: list_ensure_owner_key(selected_account),
    };

    let list_found = match &state {
        ListFindingState::Idle => handle_idle(&mut ctx, state),
        ListFindingState::Waiting {
            remote_sub_key,
            local_sub,
        } => handle_waiting(&mut ctx, *remote_sub_key, *local_sub),
    };

    if list_found {
        set_list_found(&mut ctx, ensure_state);
    }
}

type ListFound = bool;

/// Handles the `Idle` ensure phase for the selected account DM relay list.
fn handle_idle(ctx: &mut EnsureListCtx<'_, '_>, ensure_state: &mut ListFindingState) -> ListFound {
    tracing::debug!("In idle state");
    let pk = ctx.accounts.selected_account_pubkey();
    let filter = participant_dm_relay_list_filter(pk);
    let local_sub = match ctx.ndb.subscribe(std::slice::from_ref(&filter)) {
        Ok(sub) => Some(sub),
        Err(err) => {
            tracing::error!("failed to subscribe to local dm relay list: {err}");
            None
        }
    };

    let remote_sub_key = list_fetch_sub_key(pk);
    let spec = dm_relay_list_spec(pk);
    let identity = ScopedSubIdentity::account(ctx.owner_key, remote_sub_key);
    let _ = ctx
        .remote
        .scoped_subs(ctx.accounts)
        .ensure_sub(identity, spec);

    tracing::info!("waiting for selected account dm relay list ensure");
    *ensure_state = ListFindingState::Waiting {
        remote_sub_key,
        local_sub,
    };

    false
}

/// Handles the `Waiting` ensure phase for the selected account DM relay list.
fn handle_waiting(
    ctx: &mut EnsureListCtx<'_, '_>,
    remote_sub_key: SubKey,
    local_sub: Option<Subscription>,
) -> ListFound {
    let pk = ctx.accounts.selected_account_pubkey();
    if let Some(local_sub) = local_sub {
        if received_dm_relay_list_from_poll(ctx.ndb, local_sub, pk) {
            tracing::debug!(
                "found selected account dm relay list on ndb poll; still waiting for remote EOSE"
            );
        }
    }

    if !all_eosed(ctx, remote_sub_key) {
        return false;
    }

    republish_existing_or_publish_default_list(ctx, pk)
}

fn publish_default_list(ctx: &mut EnsureListCtx<'_, '_>) -> ListFound {
    let Some(secret_key) = ctx.accounts.get_selected_account().key.secret_key.as_ref() else {
        return false;
    };

    let Some(note) = build_default_dm_relay_list_note(secret_key) else {
        return false;
    };

    let Ok(note_json) = note.json() else {
        return false;
    };

    if let Err(err) = ctx.ndb.process_client_event(&note_json) {
        tracing::error!("failed to ingest default dm relay list: {err}");
        return false;
    }

    let mut publisher = ctx.remote.publisher(ctx.accounts);
    publisher.publish_note(&note, RelayType::AccountsWrite);

    true
}

/// After all-EOSE, republish the latest local selected-account kind `10050` if present.
///
/// Falls back to publishing a default kind `10050` when no local list exists.
fn republish_existing_or_publish_default_list(
    ctx: &mut EnsureListCtx<'_, '_>,
    selected_account: &Pubkey,
) -> ListFound {
    let filter = participant_dm_relay_list_filter(selected_account);
    let txn = Transaction::new(ctx.ndb).expect("txn");

    let Ok(results) = ctx.ndb.query(&txn, std::slice::from_ref(&filter), 1) else {
        tracing::error!("failed to query selected account dm relay list during ensure");
        return false;
    };

    match results.first() {
        Some(result) => {
            tracing::info!("all relays eosed; republishing existing local dm relay list note");
            let mut publisher = ctx.remote.publisher(ctx.accounts);
            publisher.publish_note(&result.note, RelayType::AccountsWrite);
            true
        }
        None => {
            tracing::info!(
                "all relays eosed; no local dm relay list note found, publishing default list"
            );
            publish_default_list(ctx)
        }
    }
}

/// Returns true when the selected-account DM relay-list ensure scoped sub reached all-EOSE.
fn all_eosed(ctx: &mut EnsureListCtx<'_, '_>, remote_sub_key: SubKey) -> bool {
    let scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
    let identity = ScopedSubIdentity::account(ctx.owner_key, remote_sub_key);
    matches!(
        scoped_subs.sub_eose_status(identity),
        ScopedSubEoseStatus::Live(live) if live.all_eosed
    )
}

/// Returns true when the ensure local subscription delivers a selected-account kind `10050` note.
fn received_dm_relay_list_from_poll(
    ndb: &Ndb,
    local_sub: Subscription,
    selected_account: &Pubkey,
) -> bool {
    let note_keys = ndb.poll_for_notes(local_sub, 1);

    let Some(key) = note_keys.first() else {
        return false;
    };

    let txn = Transaction::new(ndb).expect("txn");
    let Ok(note) = ndb.get_note_by_key(&txn, *key) else {
        return false;
    };

    is_participant_dm_relay_list(&note, selected_account)
}

/// Moves DM relay-list ensure state to `Done` and tears down the local ensure subscription.
///
/// The remote scoped sub is intentionally left declared so it stays alive for the account session
/// and can be shared with later conversation prefetch activity.
fn set_list_found(ctx: &mut EnsureListCtx<'_, '_>, list_state: &mut DmListState) {
    let prior = std::mem::replace(list_state, DmListState::Found);
    let DmListState::Finding(ListFindingState::Waiting {
        remote_sub_key: _,
        local_sub,
    }) = prior
    else {
        return;
    };

    let Some(local_sub) = local_sub else {
        return;
    };

    if let Err(err) = ctx.ndb.unsubscribe(local_sub) {
        tracing::error!("failed to unsubscribe dm relay-list local sub: {err}");
    }
}

/// Active (non-terminal) phases for selected-account DM relay-list ensure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListFindingState {
    #[default]
    Idle,
    Waiting {
        remote_sub_key: SubKey,
        local_sub: Option<Subscription>,
    },
}

/// Ensure-state for the selected account's kind `10050` DM relay list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmListState {
    Finding(ListFindingState),
    Found,
}

impl Default for DmListState {
    fn default() -> Self {
        Self::Finding(ListFindingState::Idle)
    }
}
