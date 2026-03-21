use std::time::{Duration, Instant};

use enostr::Pubkey;
use nostrdb::{Ndb, Subscription, Transaction};
use notedeck::{
    Accounts, RelaySelection, RelayType, RemoteApi, ScopedSubEoseStatus, ScopedSubIdentity,
    SubConfig, SubKey, SubOwnerKey,
};

use crate::{
    list_ensure_owner_key, list_fetch_sub_key,
    nip17::{
        build_backdated_default_dm_relay_list_note, build_default_dm_relay_list_note,
        is_participant_dm_relay_list, participant_dm_relay_list_filter,
    },
};

/// Maximum time to wait for all relays to EOSE before publishing a backdated default list.
const ENSURE_EOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Result of the timeout fallback publish attempt.
enum TimeoutPublishResult {
    /// The user's real local relay list was found and republished. Ensure is complete.
    RealListRepublished,
    /// A synthetic backdated fallback was published. Keep watching for the real list.
    BackdatedFallbackPublished,
    /// Publishing failed entirely.
    Failed,
}

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
            started_at,
        } => handle_waiting(&mut ctx, state, *remote_sub_key, *local_sub, *started_at),
        ListFindingState::FallbackPublished { local_sub } => {
            handle_fallback_published(&mut ctx, *local_sub)
        }
    };

    if list_found {
        set_list_found(&mut ctx, ensure_state);
    }
}

type ListFound = bool;

/// Handles the `Idle` ensure phase for the selected account DM relay list.
#[profiling::function]
fn handle_idle(ctx: &mut EnsureListCtx<'_, '_>, ensure_state: &mut ListFindingState) -> ListFound {
    tracing::debug!(
        "entering dm relay-list ensure idle state for selected account {}",
        ctx.accounts.selected_account_pubkey()
    );
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

    tracing::info!(
        "waiting for selected account dm relay list ensure; selected_account={}, remote_sub_key={remote_sub_key:?}, local_sub_present={}",
        pk,
        local_sub.is_some()
    );
    *ensure_state = ListFindingState::Waiting {
        remote_sub_key,
        local_sub,
        started_at: Instant::now(),
    };

    false
}

/// Handles the `Waiting` ensure phase for the selected account DM relay list.
#[profiling::function]
fn handle_waiting(
    ctx: &mut EnsureListCtx<'_, '_>,
    state: &mut ListFindingState,
    remote_sub_key: SubKey,
    local_sub: Option<Subscription>,
    started_at: Instant,
) -> ListFound {
    let pk = ctx.accounts.selected_account_pubkey();
    if let Some(local_sub) = local_sub {
        if received_dm_relay_list_from_poll(ctx.ndb, local_sub, pk) {
            tracing::debug!(
                "found selected account dm relay list on ndb poll; still waiting for remote EOSE"
            );
        }
    }

    if all_eosed(ctx, remote_sub_key) {
        tracing::debug!(
            "selected account dm relay-list ensure reached all-EOSE; selected_account={}, remote_sub_key={remote_sub_key:?}",
            pk
        );
        return republish_existing_or_publish_default_list(ctx, pk);
    }

    if started_at.elapsed() >= ENSURE_EOSE_TIMEOUT {
        tracing::warn!(
            "dm relay-list ensure timed out after {:?} waiting for all-EOSE; selected_account={}",
            ENSURE_EOSE_TIMEOUT,
            pk
        );
        match republish_existing_or_publish_backdated_default_list(ctx, pk) {
            TimeoutPublishResult::RealListRepublished => return true,
            TimeoutPublishResult::BackdatedFallbackPublished => {
                *state = ListFindingState::FallbackPublished { local_sub };
            }
            TimeoutPublishResult::Failed => {}
        }
        return false;
    }

    false
}

/// Handles the `FallbackPublished` phase: polls for the real relay list after a backdated
/// fallback was published.
///
/// We only enter this state when a synthetic backdated default (created_at=1) was published.
/// If the user's real relay list arrives from a slow relay, it will have a much higher
/// created_at and will replace the backdated note in NDB. We detect that, republish the
/// real list to AccountsWrite, and transition to Found.
#[profiling::function]
fn handle_fallback_published(
    ctx: &mut EnsureListCtx<'_, '_>,
    local_sub: Option<Subscription>,
) -> ListFound {
    let pk = ctx.accounts.selected_account_pubkey();

    let Some(local_sub) = local_sub else {
        return true;
    };

    if !received_dm_relay_list_from_poll(ctx.ndb, local_sub, pk) {
        return false;
    }

    let filter = participant_dm_relay_list_filter(pk);
    let txn = Transaction::new(ctx.ndb).expect("txn");

    let Ok(results) = ctx.ndb.query(&txn, std::slice::from_ref(&filter), 1) else {
        return false;
    };

    let Some(result) = results.first() else {
        return false;
    };

    // The backdated fallback has created_at=1. Any real list will be newer.
    if result.note.created_at() <= 1 {
        tracing::trace!(
            "ignoring polled dm relay list with created_at={} (still backdated fallback) for selected_account={}",
            result.note.created_at(),
            pk
        );
        return false;
    }

    tracing::info!(
        "real dm relay list arrived after timeout fallback for selected_account={} at created_at={}; republishing",
        pk,
        result.note.created_at()
    );

    let mut publisher = ctx.remote.publisher(ctx.accounts);
    publisher.publish_note(&result.note, RelayType::AccountsWrite);

    true
}

/// Publishes a backdated (`created_at = 1`) default DM relay list as a timeout fallback.
///
/// The extremely old timestamp means any real list the user has on any relay will supersede it.
#[profiling::function]
fn publish_backdated_default_list(ctx: &mut EnsureListCtx<'_, '_>) -> ListFound {
    let Some(secret_key) = ctx.accounts.get_selected_account().key.secret_key.as_ref() else {
        tracing::warn!(
            "cannot publish backdated default dm relay list without a selected-account secret key"
        );
        return false;
    };

    let Some(note) = build_backdated_default_dm_relay_list_note(secret_key) else {
        tracing::error!("failed to build backdated default dm relay list note");
        return false;
    };

    let Ok(note_json) = note.json() else {
        tracing::error!("failed to serialize backdated default dm relay list note");
        return false;
    };

    if let Err(err) = ctx.ndb.process_client_event(&note_json) {
        tracing::error!("failed to ingest backdated default dm relay list: {err}");
        return false;
    }

    let mut publisher = ctx.remote.publisher(ctx.accounts);
    publisher.publish_note(&note, RelayType::AccountsWrite);

    true
}

#[profiling::function]
fn publish_default_list(ctx: &mut EnsureListCtx<'_, '_>) -> ListFound {
    let Some(secret_key) = ctx.accounts.get_selected_account().key.secret_key.as_ref() else {
        tracing::warn!(
            "cannot publish default dm relay list without a selected-account secret key"
        );
        return false;
    };

    let Some(note) = build_default_dm_relay_list_note(secret_key) else {
        tracing::error!("failed to build default dm relay list note");
        return false;
    };

    let Ok(note_json) = note.json() else {
        tracing::error!("failed to serialize default dm relay list note");
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

/// On EOSE timeout, republish the latest local kind `10050` if one arrived during the wait.
///
/// Falls back to publishing a backdated (`created_at = 1`) default list so the account
/// can immediately send/receive DMs without waiting forever for a stalled relay.
#[profiling::function]
fn republish_existing_or_publish_backdated_default_list(
    ctx: &mut EnsureListCtx<'_, '_>,
    selected_account: &Pubkey,
) -> TimeoutPublishResult {
    let filter = participant_dm_relay_list_filter(selected_account);
    let txn = Transaction::new(ctx.ndb).expect("txn");

    let Ok(results) = ctx.ndb.query(&txn, std::slice::from_ref(&filter), 1) else {
        tracing::error!("failed to query selected account dm relay list during timeout fallback");
        return if publish_backdated_default_list(ctx) {
            TimeoutPublishResult::BackdatedFallbackPublished
        } else {
            TimeoutPublishResult::Failed
        };
    };

    match results.first() {
        Some(result) => {
            let created_at = result.note.created_at();
            tracing::info!(
                "timeout fallback: republishing existing local dm relay list for selected_account={} at created_at={}",
                selected_account,
                created_at
            );
            let mut publisher = ctx.remote.publisher(ctx.accounts);
            publisher.publish_note(&result.note, RelayType::AccountsWrite);
            // A previously published backdated sentinel (created_at=1) is not
            // a real list — keep watching for the real one via FallbackPublished.
            if created_at <= 1 {
                TimeoutPublishResult::BackdatedFallbackPublished
            } else {
                TimeoutPublishResult::RealListRepublished
            }
        }
        None => {
            tracing::info!(
                "timeout fallback: no local dm relay list found, publishing backdated default list"
            );
            if publish_backdated_default_list(ctx) {
                TimeoutPublishResult::BackdatedFallbackPublished
            } else {
                TimeoutPublishResult::Failed
            }
        }
    }
}

/// After all-EOSE, republish the latest local selected-account kind `10050` if present.
///
/// Falls back to publishing a default kind `10050` when no local list exists.
#[profiling::function]
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
            tracing::info!(
                "all relays eosed; republishing existing local dm relay list note for selected_account={} at created_at={}",
                selected_account,
                result.note.created_at()
            );
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
#[profiling::function]
fn all_eosed(ctx: &mut EnsureListCtx<'_, '_>, remote_sub_key: SubKey) -> bool {
    let scoped_subs = ctx.remote.scoped_subs(ctx.accounts);
    let identity = ScopedSubIdentity::account(ctx.owner_key, remote_sub_key);
    let all_eosed = matches!(
        scoped_subs.sub_eose_status(identity),
        ScopedSubEoseStatus::Live(live) if live.all_eosed
    );
    tracing::trace!(
        "dm relay-list ensure all_eosed check for selected_account={} remote_sub_key={remote_sub_key:?} => {}",
        ctx.accounts.selected_account_pubkey(),
        all_eosed
    );
    all_eosed
}

/// Returns true when the ensure local subscription delivers a selected-account kind `10050` note.
#[profiling::function]
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

    let matched = is_participant_dm_relay_list(&note, selected_account);
    tracing::trace!(
        "polled local dm relay-list note for selected_account={} created_at={} matched={matched}",
        selected_account,
        note.created_at()
    );
    matched
}

/// Moves DM relay-list ensure state to `Done` and tears down the local ensure subscription.
///
/// The remote scoped sub is intentionally left declared so it stays alive for the account session
/// and can be shared with later conversation prefetch activity.
#[profiling::function]
fn set_list_found(ctx: &mut EnsureListCtx<'_, '_>, list_state: &mut DmListState) {
    let prior = std::mem::replace(list_state, DmListState::Found);
    let local_sub = match prior {
        DmListState::Finding(ListFindingState::Waiting { local_sub, .. }) => local_sub,
        DmListState::Finding(ListFindingState::FallbackPublished { local_sub }) => local_sub,
        _ => return,
    };

    let Some(local_sub) = local_sub else {
        return;
    };

    tracing::debug!(
        "marking dm relay-list ensure complete for selected account {}",
        ctx.accounts.selected_account_pubkey()
    );
    if let Err(err) = ctx.ndb.unsubscribe(local_sub) {
        tracing::error!("failed to unsubscribe dm relay-list local sub: {err}");
    }
}

/// Active (non-terminal) phases for selected-account DM relay-list ensure.
#[derive(Clone, Copy, Debug, Default)]
pub enum ListFindingState {
    #[default]
    Idle,
    Waiting {
        remote_sub_key: SubKey,
        local_sub: Option<Subscription>,
        started_at: Instant,
    },
    /// Backdated fallback was published; still watching for the real list from slow relays.
    FallbackPublished { local_sub: Option<Subscription> },
}

/// Ensure-state for the selected account's kind `10050` DM relay list.
#[derive(Clone, Copy, Debug)]
pub enum DmListState {
    Finding(ListFindingState),
    Found,
}

impl Default for DmListState {
    fn default() -> Self {
        Self::Finding(ListFindingState::Idle)
    }
}
