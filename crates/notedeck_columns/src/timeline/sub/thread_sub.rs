use egui_nav::ReturnType;
use enostr::{Filter, NormRelayUrl, NoteId, Pubkey};
use hashbrown::{HashMap, HashSet};
use nostrdb::{Ndb, NoteReply, Subscription, Transaction};
use notedeck::{Accounts, FullHistoryConfig, ScopedSubApi, SubConfig, SubKey};

use crate::column::ColumnId;
use crate::scoped_sub_owner_keys::thread_scope_owner_key;
use crate::timeline::{
    sub::{ndb_sub, ndb_unsub},
    RemoteSubscriptionPolicy, ThreadSelection,
};

type RootNoteId = NoteId;

// column id
type MetaId = ColumnId;

/// Outcome of removing local thread subscriptions for a close action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsubscribeOutcome {
    /// Local NDB sub(s) were removed, but the scope still has stack entries so the
    /// remote scoped-sub owner should remain.
    KeepOwner,
    /// The thread scope was fully removed and the remote scoped-sub owner should
    /// be released using the returned root note id plus the caller's stack depth.
    DropOwner(RootNoteId),
}

/// Thread subscription manager keyed by account and column scope.
///
/// This intentionally follows master's stack model: thread scopes are removed
/// from the top. The scope depth is therefore stable for the owner lifetime and
/// is the owner identity component. Do not remove an arbitrary lower scope to
/// "fix cleanup"; that invents a route lifecycle master did not have and forces
/// extra identity state.
#[derive(Default)]
pub struct ThreadSubs {
    /// Per-account thread subscription bookkeeping.
    by_account: HashMap<Pubkey, AccountThreadSubs>,
}

#[derive(Default)]
struct AccountThreadSubs {
    scopes: HashMap<MetaId, Vec<Scope>>,
}

struct Scope {
    root_id: NoteId,
    /// Selected notes opened inside this thread scope.
    ///
    /// The remote filter shape is root-only. The stack is local NDB subscription
    /// state and must not enter the remote `SubKey`.
    stack: Vec<Sub>,
}

#[cfg(test)]
struct ThreadRemoteSnapshot {
    /// Every local overlay/scope demanding this root request.
    owners: Vec<(MetaId, usize)>,
    /// Union of current observed relay coverage from every owner scope.
    observed_relays: HashSet<NormRelayUrl>,
    /// Logging/debug only; this is not part of the remote identity.
    stack_len: usize,
}

struct Sub {
    /// Selected note represented by this local subscription.
    selected_id: NoteId,
    sub: Subscription,
    // Keep local filters alive for the full subscription lifetime. Thread
    // filters use custom callbacks and can crash if dropped early.
    _filters: Vec<Filter>,
}

impl ThreadSubs {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn subscribe(
        &mut self,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
        meta_id: MetaId,
        id: &ThreadSelection,
        local_sub_filter: Vec<Filter>,
        new_scope: bool,
        remote_policy: RemoteSubscriptionPolicy,
    ) {
        let account_pk = scoped_subs.selected_account_pubkey();
        let (remote_update, num_locals) = {
            let account_subs = self.by_account.entry(account_pk).or_default();
            let cur_scopes = account_subs.scopes.entry(meta_id).or_default();
            let needs_new_scope = new_scope || cur_scopes.is_empty();
            let remote_update = if needs_new_scope {
                let scope_depth = cur_scopes.len();
                local_sub_new_scope(
                    ndb,
                    account_pk,
                    meta_id,
                    id,
                    scope_depth,
                    local_sub_filter,
                    cur_scopes,
                )
                .map(|root_id| (meta_id, scope_depth, root_id))
            } else {
                let cur_scope = cur_scopes.last_mut().expect("checked non-empty above");
                // Master only installed the remote owner when a thread scope opened.
                // Same-scope pushes are local NDB stack entries; do not turn them
                // into remote config churn or owner identity.
                let _ = sub_current_scope(ndb, id, local_sub_filter, cur_scope);
                None
            };

            (remote_update, account_subs.scopes.len())
        };

        if let Some((meta_id, scope_depth, root_id)) = remote_update {
            self.set_remote_for_scope(
                ndb,
                scoped_subs,
                account_pk,
                meta_id,
                scope_depth,
                &root_id,
                remote_policy,
            );
            tracing::debug!(
                "Sub stats: account={:?}, num locals: {}",
                account_pk,
                num_locals,
            );
        }
    }

    pub(crate) fn unsubscribe(
        &mut self,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
        meta_id: MetaId,
        id: &ThreadSelection,
        return_type: ReturnType,
    ) {
        let account_pk = scoped_subs.selected_account_pubkey();
        let (owner_to_drop, remove_account_entry) = {
            let Some(account_subs) = self.by_account.get_mut(&account_pk) else {
                return;
            };

            let (unsub_outcome, removed_scope_depth, remove_meta_entry) = {
                let Some(scopes) = account_subs.scopes.get_mut(&meta_id) else {
                    return;
                };
                let removed_scope_depth = scopes.len().saturating_sub(1);

                let Some(unsub_outcome) = (match return_type {
                    ReturnType::Drag => unsubscribe_drag(scopes, ndb, id),
                    ReturnType::Click => unsubscribe_click(scopes, ndb, id),
                }) else {
                    return;
                };

                (unsub_outcome, removed_scope_depth, scopes.is_empty())
            };

            if remove_meta_entry {
                account_subs.scopes.remove(&meta_id);
            }

            tracing::debug!(
                "unsub stats: account={:?}, num locals: {}, released owner: {}",
                account_pk,
                account_subs.scopes.len(),
                matches!(unsub_outcome, UnsubscribeOutcome::DropOwner(_)),
            );

            (
                match unsub_outcome {
                    UnsubscribeOutcome::KeepOwner => None,
                    UnsubscribeOutcome::DropOwner(root_id) => Some(thread_scope_owner_key(
                        account_pk,
                        meta_id,
                        &root_id,
                        removed_scope_depth as u64,
                    )),
                },
                account_subs.scopes.is_empty(),
            )
        };

        if remove_account_entry {
            self.by_account.remove(&account_pk);
        }

        if let Some(owner) = owner_to_drop {
            let _ = scoped_subs.drop_owner(owner);
        }
    }

    /// Remove the top thread scope for one removed route.
    ///
    /// This is intentionally top-only. Thread route ownership is stack-shaped:
    /// normal nav returns pop the top route, account-switch disposal pops the
    /// source column's top route, and column deletion disposes visible routes in
    /// reverse order. Searching for and removing a lower matching scope would
    /// mutate owner depths for scopes above it and recreate the bug-prone
    /// non-master model that required allocated scope ids.
    pub(crate) fn dispose_route_for_account(
        &mut self,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
        account_pk: Pubkey,
        meta_id: MetaId,
        selection: &ThreadSelection,
    ) {
        let Some(account_subs) = self.by_account.get_mut(&account_pk) else {
            return;
        };

        let Some(scopes) = account_subs.scopes.get_mut(&meta_id) else {
            return;
        };

        let scope_depth = scopes.len().saturating_sub(1);
        let Some(scope) = scopes.last() else {
            return;
        };
        if !scope_matches_selection(scope, selection) {
            tracing::error!(
                "disposed thread route did not match top thread scope: account={account_pk:?}, col={meta_id:?}, route_root={:?}, scope_root={:?}",
                selection.root_id.bytes(),
                scope.root_id.bytes()
            );
            return;
        }

        let owner_to_drop =
            thread_scope_owner_key(account_pk, meta_id, &scope.root_id, scope_depth as u64);
        let scope = scopes.pop().expect("checked non-empty above");
        dispose_thread_scope(ndb, scope);
        if scopes.is_empty() {
            account_subs.scopes.remove(&meta_id);
        }
        let remove_account_entry = account_subs.scopes.is_empty();

        if remove_account_entry {
            self.by_account.remove(&account_pk);
        }

        let _ = scoped_subs.drop_owner(owner_to_drop);
    }

    pub(crate) fn get_local(&self, account_pk: &Pubkey, meta_id: MetaId) -> Option<&Subscription> {
        self.by_account
            .get(account_pk)?
            .scopes
            .get(&meta_id)
            .and_then(|scopes| scopes.last())
            .and_then(|s| s.stack.last())
            .map(|s| &s.sub)
    }

    pub(crate) fn get_local_for_selected<'a>(
        &'a self,
        accounts: &Accounts,
        meta_id: MetaId,
    ) -> Option<&'a Subscription> {
        self.get_local(accounts.selected_account_pubkey(), meta_id)
    }

    pub(crate) fn refresh_remote_subscriptions(
        &mut self,
        ndb: &Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
        remote_policy: RemoteSubscriptionPolicy,
    ) {
        let mut scopes = Vec::new();
        for (account_pk, account_subs) in &self.by_account {
            for (meta_id, scope_stack) in &account_subs.scopes {
                for (scope_depth, scope) in scope_stack.iter().enumerate() {
                    scopes.push((*account_pk, *meta_id, scope_depth, scope.root_id));
                }
            }
        }

        for (account_pk, meta_id, scope_depth, root_id) in scopes {
            self.set_remote_for_scope(
                ndb,
                scoped_subs,
                account_pk,
                meta_id,
                scope_depth,
                &root_id,
                remote_policy,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn set_remote_for_scope(
        &self,
        ndb: &Ndb,
        scoped_subs: &mut ScopedSubApi<'_>,
        account_pk: Pubkey,
        meta_id: MetaId,
        scope_depth: usize,
        root_id: &RootNoteId,
        remote_policy: RemoteSubscriptionPolicy,
    ) {
        let Some(scope) = self.thread_scope(account_pk, meta_id, scope_depth, root_id) else {
            return;
        };
        let observed_relays = observed_thread_relays_for_thread_scope(
            ndb,
            root_id,
            scope_initial_selected_ids(scope),
        );
        set_scope_remote(
            scoped_subs,
            account_pk,
            meta_id,
            scope_depth,
            root_id,
            observed_relays,
            scope.stack.len(),
            remote_policy,
        );
    }

    #[cfg(test)]
    fn remote_snapshot_for_root(
        &self,
        ndb: &Ndb,
        account_pk: Pubkey,
        root_id: &RootNoteId,
    ) -> Option<ThreadRemoteSnapshot> {
        let account_subs = self.by_account.get(&account_pk)?;
        let scopes = account_subs
            .scopes
            .iter()
            .flat_map(|(meta_id, scopes)| {
                scopes
                    .iter()
                    .enumerate()
                    .filter(|(_, scope)| scope.root_id.bytes() == root_id.bytes())
                    .map(|(scope_depth, scope)| (*meta_id, scope_depth, scope))
            })
            .collect::<Vec<_>>();

        if scopes.is_empty() {
            return None;
        }

        let owners = scopes
            .iter()
            .map(|(meta_id, scope_depth, _scope)| (*meta_id, *scope_depth))
            .collect::<Vec<_>>();
        let stack_len = scopes.iter().map(|(_, _, scope)| scope.stack.len()).sum();
        let observed_relays = scopes
            .iter()
            .flat_map(|(_, _, scope)| observed_thread_relays(ndb, scope))
            .collect::<HashSet<_>>();

        Some(ThreadRemoteSnapshot {
            owners,
            observed_relays,
            stack_len,
        })
    }

    fn thread_scope(
        &self,
        account_pk: Pubkey,
        meta_id: MetaId,
        scope_depth: usize,
        root_id: &RootNoteId,
    ) -> Option<&Scope> {
        self.by_account
            .get(&account_pk)?
            .scopes
            .get(&meta_id)?
            .get(scope_depth)
            .filter(|scope| scope.root_id.bytes() == root_id.bytes())
    }
}

fn unsubscribe_drag(
    scopes: &mut Vec<Scope>,
    ndb: &mut Ndb,
    id: &ThreadSelection,
) -> Option<UnsubscribeOutcome> {
    let Some(scope) = scopes.last_mut() else {
        tracing::error!("called drag unsubscribe but there aren't any scopes left");
        return None;
    };

    let Some(cur_sub) = scope.stack.pop() else {
        tracing::error!("expected a scope to be left");
        return None;
    };

    log_scope_root_mismatch(scope, id);

    if !ndb_unsub(ndb, cur_sub.sub, id) {
        // Keep local bookkeeping aligned with NDB when unsubscribe fails.
        scope.stack.push(cur_sub);
        return None;
    }

    if scope.stack.is_empty() {
        let removed_scope = scopes.pop().expect("checked empty above");
        return Some(UnsubscribeOutcome::DropOwner(removed_scope.root_id));
    }

    Some(UnsubscribeOutcome::KeepOwner)
}

fn unsubscribe_click(
    scopes: &mut Vec<Scope>,
    ndb: &mut Ndb,
    id: &ThreadSelection,
) -> Option<UnsubscribeOutcome> {
    let Some(mut scope) = scopes.pop() else {
        tracing::error!("called unsubscribe but there aren't any scopes left");
        return None;
    };

    log_scope_root_mismatch(&scope, id);
    while let Some(sub) = scope.stack.pop() {
        if ndb_unsub(ndb, sub.sub, id) {
            continue;
        }

        // Partial rollback: restore the failed local sub (and any remaining ones)
        // to thread bookkeeping and keep the remote owner alive.
        scope.stack.push(sub);
        scopes.push(scope);
        return None;
    }
    Some(UnsubscribeOutcome::DropOwner(scope.root_id))
}

fn dispose_thread_scope(ndb: &mut Ndb, scope: Scope) {
    let root_id = scope.root_id;
    for sub in scope.stack {
        let _ = ndb_unsub(ndb, sub.sub, (&root_id, sub.selected_id));
    }
}

fn scope_matches_selection(scope: &Scope, selection: &ThreadSelection) -> bool {
    scope.root_id.bytes() == selection.root_id.bytes()
        && scope
            .stack
            .iter()
            .any(|sub| sub.selected_id.bytes() == selection.selected_or_root())
}

fn log_scope_root_mismatch(scope: &Scope, id: &ThreadSelection) {
    if scope.root_id.bytes() != id.root_id.bytes() {
        tracing::error!(
            "Somehow the current scope's root is not equal to the selected note's root. scope's root: {:?}, thread's root: {:?}",
            scope.root_id.hex(),
            id.root_id.bytes()
        );
    }
}

fn sub_current_scope(
    ndb: &mut Ndb,
    selection: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    cur_scope: &mut Scope,
) -> bool {
    if selection.root_id.bytes() != cur_scope.root_id.bytes() {
        tracing::error!(
            "Somehow the current scope's root is not equal to the selected note's root"
        );
    }

    let Some(sub) = ndb_sub(ndb, &local_sub_filter, selection) else {
        return false;
    };

    cur_scope.stack.push(Sub {
        selected_id: NoteId::new(*selection.selected_or_root()),
        sub,
        _filters: local_sub_filter,
    });

    true
}

#[allow(clippy::too_many_arguments)]
fn local_sub_new_scope(
    ndb: &mut Ndb,
    account_pk: Pubkey,
    meta_id: MetaId,
    id: &ThreadSelection,
    scope_depth: usize,
    local_sub_filter: Vec<Filter>,
    scopes: &mut Vec<Scope>,
) -> Option<RootNoteId> {
    let root_id = id.root_id.to_note_id();
    tracing::info!(
        "thread sub with owner: pk: {account_pk:?}, col: {meta_id:?}, rootid: {root_id:?}, depth: {scope_depth}"
    );

    let sub = ndb_sub(ndb, &local_sub_filter, id)?;

    scopes.push(Scope {
        root_id,
        stack: vec![Sub {
            selected_id: NoteId::new(*id.selected_or_root()),
            sub,
            _filters: local_sub_filter,
        }],
    });

    Some(root_id)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ThreadScopedSub {
    RepliesByRootBaseline,
}

fn thread_remote_sub_key(root_id: &RootNoteId, sub: ThreadScopedSub) -> SubKey {
    // This key must track `scope_remote_thread_filters`: root-shaped filters share
    // root-shaped scoped-sub state. Column and thread scope belong in the owner key.
    //
    // Cleanup is handled by `SubOwnerKey`; adding owner identity here turns
    // duplicate demand into duplicate outbox requests.
    SubKey::builder(sub).with(*root_id.bytes()).finish()
}

#[allow(clippy::too_many_arguments)]
fn set_scope_remote(
    scoped_subs: &mut ScopedSubApi<'_>,
    account_pk: Pubkey,
    meta_id: MetaId,
    scope_depth: usize,
    root_id: &RootNoteId,
    observed_relays: HashSet<NormRelayUrl>,
    stack_len: usize,
    remote_policy: RemoteSubscriptionPolicy,
) {
    // The scoped-sub key is the remote request shape: root replies plus the root
    // note by exact id. Column and thread scope are owner identity only. Including
    // them in the key creates duplicate live outbox demand for identical root
    // requests.
    //
    // Each owner declares the same `SubKey`. Scoped-subs counts demand by owner,
    // merges compatible additive relay coverage, and removes the outbox request
    // only after the last owner drops.
    tracing::debug!(
        "Remote subscribe for thread root {:?} with {} selected notes",
        root_id.hex(),
        stack_len
    );

    let filters = scope_remote_thread_filters(root_id);
    let history_filters = scope_remote_thread_history_filters(root_id);
    let (key, config) = thread_remote_sub_declaration(
        root_id,
        observed_relays,
        filters,
        history_filters,
        remote_policy,
    );
    let owner = thread_scope_owner_key(account_pk, meta_id, root_id, scope_depth as u64);
    let _ = scoped_subs.set_sub_for_account(account_pk, owner, key, config);
}

fn scope_remote_thread_filters(root_id: &RootNoteId) -> Vec<Filter> {
    vec![
        Filter::new()
            .kinds([1])
            .event(root_id.bytes())
            .limit(500)
            .build(),
        Filter::new().ids([root_id.bytes()]).limit(1).build(),
    ]
}

/// Build full-history thread filters without live subscription result limits.
fn scope_remote_thread_history_filters(root_id: &RootNoteId) -> Vec<Filter> {
    vec![
        Filter::new().kinds([1]).event(root_id.bytes()).build(),
        Filter::new().ids([root_id.bytes()]).build(),
    ]
}

fn observed_thread_relays_for_thread_scope(
    ndb: &Ndb,
    root_id: &RootNoteId,
    selected_ids: impl IntoIterator<Item = [u8; 32]>,
) -> HashSet<NormRelayUrl> {
    let Ok(txn) = Transaction::new(ndb) else {
        return HashSet::new();
    };
    let note_ids = known_thread_note_ids_with_txn(ndb, &txn, root_id.bytes(), selected_ids);
    observed_thread_relays_for_note_ids(ndb, &txn, &note_ids)
}

/// Return the root id plus each selected note and known ancestor by note id.
///
/// These ids are only used to inspect local observed-relay metadata. The remote
/// thread subscription still asks relays for `#e=root` replies plus `ids=[root]`,
/// matching the pre-outbox thread query shape.
fn known_thread_note_ids_with_txn(
    ndb: &Ndb,
    txn: &Transaction,
    root_id: &[u8; 32],
    selected_ids: impl IntoIterator<Item = [u8; 32]>,
) -> Vec<[u8; 32]> {
    let mut note_ids = vec![*root_id];
    let mut seen: HashSet<[u8; 32]> = note_ids.iter().copied().collect();

    for mut current_id in selected_ids {
        while seen.insert(current_id) {
            note_ids.push(current_id);

            if current_id == *root_id {
                break;
            }

            let Ok(note) = ndb.get_note_by_id(txn, &current_id) else {
                break;
            };
            let Some(parent) = NoteReply::new(note.tags()).reply() else {
                break;
            };
            current_id = *parent.id;
        }
    }

    note_ids
}

/// Build the remote thread declaration from the dynamic account-read baseline plus
/// observed relay coverage for the scope-opening thread note.
fn thread_remote_sub_declaration(
    root_id: &RootNoteId,
    observed_relays: HashSet<NormRelayUrl>,
    filters: Vec<Filter>,
    history_filters: Vec<Filter>,
    remote_policy: RemoteSubscriptionPolicy,
) -> (SubKey, SubConfig) {
    let key = thread_remote_sub_key(root_id, ThreadScopedSub::RepliesByRootBaseline);

    let full_history = FullHistoryConfig::new(history_filters);
    let builder = SubConfig::builder(filters)
        .full_history(full_history)
        .accounts_read_important();
    let config = if remote_policy.uses_observed_relay_coverage(!observed_relays.is_empty()) {
        builder.with_observed_relays(observed_relays).build()
    } else {
        builder.build()
    };

    (key, config)
}

fn observed_thread_relays_for_note_ids(
    ndb: &Ndb,
    txn: &Transaction,
    note_ids: &[[u8; 32]],
) -> HashSet<NormRelayUrl> {
    let mut relays = HashSet::new();
    for note_id in note_ids {
        collect_note_relays(ndb, txn, note_id, &mut relays);
    }
    relays
}

fn collect_note_relays(
    ndb: &Ndb,
    txn: &Transaction,
    note_id: &[u8; 32],
    relays: &mut HashSet<NormRelayUrl>,
) {
    let Ok(note) = ndb.get_note_by_id(txn, note_id) else {
        return;
    };

    relays.extend(
        note.relays(txn)
            .filter_map(|relay| NormRelayUrl::new(relay).ok()),
    );
}

fn scope_initial_selected_ids(scope: &Scope) -> impl Iterator<Item = [u8; 32]> + '_ {
    scope
        .stack
        .first()
        .map(|sub| *sub.selected_id.bytes())
        .into_iter()
}

#[cfg(test)]
fn observed_thread_relays(ndb: &Ndb, scope: &Scope) -> HashSet<NormRelayUrl> {
    observed_thread_relays_for_thread_scope(ndb, &scope.root_id, scope_initial_selected_ids(scope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post::NewPost;
    use egui::Context;
    use notedeck::{
        AppContext, Notedeck, RelayAction, RootNoteIdBuf, ScopedSubIdentity, ScopedSubReadiness,
    };
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    use crate::timeline::{thread::Threads, ThreadSelection};

    struct ThreadHostHarness {
        _tmp: TempDir,
        ui_ctx: Context,
        notedeck: Notedeck,
        threads: Threads,
    }

    impl ThreadHostHarness {
        fn new() -> Self {
            let tmp = TempDir::new().expect("tmp dir");
            let ui_ctx = Context::default();
            let notedeck = Notedeck::init(
                &ui_ctx,
                tmp.path(),
                &["notedeck".to_owned(), "--testrunner".to_owned()],
            );

            Self {
                _tmp: tmp,
                ui_ctx,
                notedeck,
                threads: Threads::default(),
            }
        }
    }

    fn thread_selection(tag: u8) -> ThreadSelection {
        ThreadSelection::from_root_id(RootNoteIdBuf::new_unsafe([tag; 32]))
    }

    fn thread_identity(
        account_pk: Pubkey,
        col: ColumnId,
        root_id: &NoteId,
        scope_depth: u64,
    ) -> ScopedSubIdentity {
        ScopedSubIdentity::account(
            thread_scope_owner_key(account_pk, col, root_id, scope_depth),
            thread_remote_sub_key(root_id, ThreadScopedSub::RepliesByRootBaseline),
        )
    }

    fn secret_bytes(account: &enostr::FullKeypair) -> [u8; 32] {
        account
            .clone()
            .to_keypair()
            .secret_key
            .expect("secret key")
            .to_secret_bytes()
    }

    fn wait_for_note_import(ndb: &Ndb, note_id: &[u8; 32]) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let txn = Transaction::new(ndb).expect("txn");
            if ndb.get_note_by_id(&txn, note_id).is_ok() {
                return;
            }

            assert!(
                Instant::now() < deadline,
                "timed out waiting for note import"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn ingest_note_from_relay(ndb: &Ndb, note: &nostrdb::Note<'_>, relay: &NormRelayUrl) {
        let json = note.json().expect("note json");
        let relay_url = relay.to_string();
        ndb.process_event_with(
            &json,
            nostrdb::IngestMetadata::new()
                .client(true)
                .relay(&relay_url),
        )
        .expect("ingest note");
        wait_for_note_import(ndb, note.id());
    }

    fn note_observed_relays(ndb: &Ndb, note_id: &[u8; 32]) -> HashSet<NormRelayUrl> {
        let txn = Transaction::new(ndb).expect("txn");
        let note = ndb.get_note_by_id(&txn, note_id).expect("note");
        note.relays(&txn)
            .filter_map(|relay| NormRelayUrl::new(relay).ok())
            .collect()
    }

    fn tracked_sub(ndb: &Ndb, selected_id: &[u8; 32]) -> Sub {
        let filters = vec![Filter::new().ids([selected_id]).build()];
        let sub = ndb_sub(ndb, &filters, "test scope sub").expect("local sub");
        Sub {
            selected_id: NoteId::new(*selected_id),
            sub,
            _filters: filters,
        }
    }

    fn filter_values(filters: &[Filter]) -> Vec<serde_json::Value> {
        filters
            .iter()
            .map(Filter::json)
            .collect::<Result<Vec<_>, _>>()
            .expect("filter json")
            .into_iter()
            .map(|json| serde_json::from_str(&json).expect("valid filter json"))
            .collect()
    }

    fn filter_ids(filters: &[Filter]) -> HashSet<String> {
        filter_values(filters)
            .into_iter()
            .filter_map(|value| {
                value
                    .get("ids")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
            })
            .flatten()
            .map(|id| id.as_str().expect("id string").to_owned())
            .collect()
    }

    fn exact_id_filter(note_id: &[u8; 32]) -> Vec<Filter> {
        vec![Filter::new().ids([note_id]).build()]
    }

    fn remote_policy(use_outbox_relays: bool) -> RemoteSubscriptionPolicy {
        RemoteSubscriptionPolicy::from_outbox_relays(use_outbox_relays)
    }

    fn add_selected_account_read_relay(app_ctx: &mut AppContext<'_>, relay_url: &str) {
        let relay = NormRelayUrl::new(relay_url).expect("relay");
        app_ctx.process_relay_action(RelayAction::Add(relay.to_string()));
        assert!(app_ctx
            .accounts
            .selected_account_read_relays()
            .contains(&relay));
    }

    #[track_caller]
    fn assert_readiness(
        scoped_subs: &ScopedSubApi<'_>,
        identity: ScopedSubIdentity,
        expected: ScopedSubReadiness,
    ) {
        assert_eq!(scoped_subs.sub_readiness(identity), expected);
    }

    #[track_caller]
    fn assert_live(scoped_subs: &ScopedSubApi<'_>, identity: ScopedSubIdentity) {
        let readiness = scoped_subs.sub_readiness(identity);
        assert!(
            matches!(readiness, ScopedSubReadiness::Live(_)),
            "expected live scoped sub readiness, got {readiness:?}"
        );
    }

    #[track_caller]
    fn wait_for_scoped_live(h: &mut ThreadHostHarness, identity: ScopedSubIdentity) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            {
                let mut app_ctx = h.notedeck.app_context();
                app_ctx.accounts.update(app_ctx.ndb, &mut app_ctx.remote);
            }
            h.notedeck.tick(&h.ui_ctx);
            let mut app_ctx = h.notedeck.app_context();
            let scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            let readiness = scoped_subs.sub_readiness(identity);
            drop(scoped_subs);
            drop(app_ctx);

            if matches!(readiness, ScopedSubReadiness::Live(_)) {
                return;
            }

            assert!(
                Instant::now() < deadline,
                "expected live scoped sub readiness, got {readiness:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn expected_thread_remote_config(
        observed_relays: HashSet<NormRelayUrl>,
        filters: Vec<Filter>,
        history_filters: Vec<Filter>,
        use_outbox_relays: bool,
    ) -> SubConfig {
        let policy = remote_policy(use_outbox_relays);
        let builder = SubConfig::builder(filters)
            .full_history(FullHistoryConfig::new(history_filters))
            .accounts_read_important();

        if policy.uses_observed_relay_coverage(!observed_relays.is_empty()) {
            return builder.with_observed_relays(observed_relays).build();
        }

        builder.build()
    }

    #[test]
    fn scope_remote_filters_keep_master_root_shape() {
        let root_id = NoteId::new([0x11; 32]);
        let selected_id = [0x22; 32];

        let ids = filter_ids(&scope_remote_thread_filters(&root_id));

        assert_eq!(ids, HashSet::from([hex::encode(root_id.bytes())]));
        assert!(!ids.contains(&hex::encode(selected_id)));
    }

    #[test]
    fn drag_pop_keeps_thread_remote_owner() {
        let mut h = ThreadHostHarness::new();
        let alice = enostr::FullKeypair::generate();
        let bob = enostr::FullKeypair::generate();
        let carol = enostr::FullKeypair::generate();
        let col = ColumnId::for_test(12);

        let root_post = NewPost::new("root".to_owned(), alice.clone(), vec![], vec![]);
        let root = root_post.to_note(&secret_bytes(&alice));
        let bob_post = NewPost::new("bob".to_owned(), bob.clone(), vec![], vec![]);
        let bob_reply = bob_post.to_reply(&secret_bytes(&bob), &root);
        let carol_post = NewPost::new("carol".to_owned(), carol.clone(), vec![], vec![]);
        let carol_reply = carol_post.to_reply(&secret_bytes(&carol), &root);

        let mut app_ctx = h.notedeck.app_context();
        app_ctx
            .ndb
            .process_client_event(&root.json().expect("root json"))
            .expect("ingest root");
        app_ctx
            .ndb
            .process_client_event(&bob_reply.json().expect("bob json"))
            .expect("ingest bob");
        app_ctx
            .ndb
            .process_client_event(&carol_reply.json().expect("carol json"))
            .expect("ingest carol");
        wait_for_note_import(app_ctx.ndb, root.id());
        wait_for_note_import(app_ctx.ndb, bob_reply.id());
        wait_for_note_import(app_ctx.ndb, carol_reply.id());

        let root_id = NoteId::new(*root.id());
        let bob_selection = ThreadSelection {
            root_id: RootNoteIdBuf::new_unsafe(*root.id()),
            selected_note: Some(NoteId::new(*bob_reply.id())),
        };
        let carol_selection = ThreadSelection {
            root_id: RootNoteIdBuf::new_unsafe(*root.id()),
            selected_note: Some(NoteId::new(*carol_reply.id())),
        };
        let account_pk = *app_ctx.accounts.selected_account_pubkey();
        let baseline_identity = thread_identity(account_pk, col, &root_id, 0);
        let mut subs = ThreadSubs::default();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &bob_selection,
            exact_id_filter(bob_reply.id()),
            true,
            remote_policy(true),
        );
        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &carol_selection,
            exact_id_filter(carol_reply.id()),
            false,
            remote_policy(true),
        );

        assert_readiness(
            &scoped_subs,
            baseline_identity,
            ScopedSubReadiness::Inactive,
        );

        subs.unsubscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &carol_selection,
            ReturnType::Drag,
        );

        let snapshot = subs
            .remote_snapshot_for_root(app_ctx.ndb, account_pk, &root_id)
            .expect("root snapshot");
        assert_eq!(snapshot.stack_len, 1);
        assert_readiness(
            &scoped_subs,
            baseline_identity,
            ScopedSubReadiness::Inactive,
        );
    }

    #[test]
    fn thread_remote_sub_retargets_from_inactive_to_live_when_account_read_relays_appear() {
        let mut h = ThreadHostHarness::new();
        let selection = thread_selection(0x71);
        let root_id = selection.root_id.to_note_id();
        let col = ColumnId::for_test(71);

        {
            let mut app_ctx = h.notedeck.app_context();
            let account_pk = *app_ctx.accounts.selected_account_pubkey();
            let identity = thread_identity(account_pk, col, &root_id, 0);
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            h.threads.subs.subscribe(
                app_ctx.ndb,
                &mut scoped_subs,
                col,
                &selection,
                exact_id_filter(selection.selected_or_root()),
                true,
                remote_policy(true),
            );

            assert_readiness(&scoped_subs, identity, ScopedSubReadiness::Inactive);
            drop(scoped_subs);

            add_selected_account_read_relay(&mut app_ctx, "wss://thread-retarget.example.com");
        }

        let account_pk = *h.notedeck.app_context().accounts.selected_account_pubkey();
        wait_for_scoped_live(&mut h, thread_identity(account_pk, col, &root_id, 0));
    }

    #[test]
    fn dispose_routes_drop_thread_scopes_and_remote_owners() {
        let mut h = ThreadHostHarness::new();
        let selection_a = thread_selection(0x41);
        let selection_b = thread_selection(0x42);
        let root_a = selection_a.root_id.to_note_id();
        let root_b = selection_b.root_id.to_note_id();
        let col = ColumnId::for_test(16);

        let mut app_ctx = h.notedeck.app_context();
        let account_pk = *app_ctx.accounts.selected_account_pubkey();
        let identity_a = thread_identity(account_pk, col, &root_a, 0);
        let identity_b = thread_identity(account_pk, col, &root_b, 1);
        let mut subs = ThreadSubs::default();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &selection_a,
            exact_id_filter(selection_a.selected_or_root()),
            true,
            remote_policy(true),
        );
        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &selection_b,
            exact_id_filter(selection_b.selected_or_root()),
            true,
            remote_policy(true),
        );

        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Inactive);
        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Inactive);

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col,
            &selection_b,
        );
        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col,
            &selection_a,
        );

        assert!(subs.get_local(&account_pk, col).is_none());
        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Missing);
        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Missing);
    }

    #[test]
    fn dispose_route_ignores_non_top_thread_scope() {
        let mut h = ThreadHostHarness::new();
        let selection_a = thread_selection(0x61);
        let selection_b = thread_selection(0x62);
        let root_a = selection_a.root_id.to_note_id();
        let root_b = selection_b.root_id.to_note_id();
        let col = ColumnId::for_test(61);

        let mut app_ctx = h.notedeck.app_context();
        let account_pk = *app_ctx.accounts.selected_account_pubkey();
        let identity_a = thread_identity(account_pk, col, &root_a, 0);
        let identity_b = thread_identity(account_pk, col, &root_b, 1);
        let mut subs = ThreadSubs::default();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &selection_a,
            exact_id_filter(selection_a.selected_or_root()),
            true,
            remote_policy(true),
        );
        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col,
            &selection_b,
            exact_id_filter(selection_b.selected_or_root()),
            true,
            remote_policy(true),
        );

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col,
            &selection_a,
        );

        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Inactive);
        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Inactive);

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col,
            &selection_b,
        );

        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Missing);
        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Inactive);

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col,
            &selection_a,
        );

        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Missing);
    }

    #[test]
    fn dispose_route_uses_column_id_not_display_index() {
        let mut h = ThreadHostHarness::new();
        let selection = thread_selection(0x51);
        let root_id = selection.root_id.to_note_id();
        let col_a = ColumnId::for_test(51);
        let col_b = ColumnId::for_test(52);

        let mut app_ctx = h.notedeck.app_context();
        let account_pk = *app_ctx.accounts.selected_account_pubkey();
        let identity_a = thread_identity(account_pk, col_a, &root_id, 0);
        let identity_b = thread_identity(account_pk, col_b, &root_id, 0);
        let mut subs = ThreadSubs::default();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col_a,
            &selection,
            exact_id_filter(selection.selected_or_root()),
            true,
            remote_policy(true),
        );
        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col_b,
            &selection,
            exact_id_filter(selection.selected_or_root()),
            true,
            remote_policy(true),
        );

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col_a,
            &selection,
        );

        assert_readiness(&scoped_subs, identity_a, ScopedSubReadiness::Missing);
        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Inactive);
        assert!(subs.get_local(&account_pk, col_b).is_some());
    }

    #[test]
    fn same_root_thread_scopes_share_unioned_remote_request() {
        let mut h = ThreadHostHarness::new();
        let alice = enostr::FullKeypair::generate();
        let bob = enostr::FullKeypair::generate();
        let carol = enostr::FullKeypair::generate();
        let bob_relay = NormRelayUrl::new("wss://bob-thread.example.com").expect("relay");
        let carol_relay = NormRelayUrl::new("wss://carol-thread.example.com").expect("relay");
        let col_a = ColumnId::for_test(81);
        let col_b = ColumnId::for_test(82);

        let root_post = NewPost::new("root".to_owned(), alice.clone(), vec![], vec![]);
        let root = root_post.to_note(&secret_bytes(&alice));
        let bob_post = NewPost::new("bob".to_owned(), bob.clone(), vec![], vec![]);
        let bob_reply = bob_post.to_reply(&secret_bytes(&bob), &root);
        let carol_post = NewPost::new("carol".to_owned(), carol.clone(), vec![], vec![]);
        let carol_reply = carol_post.to_reply(&secret_bytes(&carol), &root);

        let mut app_ctx = h.notedeck.app_context();
        app_ctx
            .ndb
            .process_client_event(&root.json().expect("root json"))
            .expect("ingest root");
        ingest_note_from_relay(app_ctx.ndb, &bob_reply, &bob_relay);
        ingest_note_from_relay(app_ctx.ndb, &carol_reply, &carol_relay);
        wait_for_note_import(app_ctx.ndb, root.id());

        let root_id = NoteId::new(*root.id());
        let bob_selection = ThreadSelection {
            root_id: RootNoteIdBuf::new_unsafe(*root.id()),
            selected_note: Some(NoteId::new(*bob_reply.id())),
        };
        let carol_selection = ThreadSelection {
            root_id: RootNoteIdBuf::new_unsafe(*root.id()),
            selected_note: Some(NoteId::new(*carol_reply.id())),
        };
        let account_pk = *app_ctx.accounts.selected_account_pubkey();
        let identity_a = thread_identity(account_pk, col_a, &root_id, 0);
        let identity_b = thread_identity(account_pk, col_b, &root_id, 0);
        let mut subs = ThreadSubs::default();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col_a,
            &bob_selection,
            exact_id_filter(bob_reply.id()),
            true,
            remote_policy(true),
        );
        subs.subscribe(
            app_ctx.ndb,
            &mut scoped_subs,
            col_b,
            &carol_selection,
            exact_id_filter(carol_reply.id()),
            true,
            remote_policy(true),
        );

        let snapshot = subs
            .remote_snapshot_for_root(app_ctx.ndb, account_pk, &root_id)
            .expect("root snapshot");
        assert_eq!(snapshot.owners.len(), 2);
        assert!(snapshot.observed_relays.contains(&bob_relay));
        assert!(snapshot.observed_relays.contains(&carol_relay));
        drop(scoped_subs);
        drop(app_ctx);
        wait_for_scoped_live(&mut h, identity_a);
        wait_for_scoped_live(&mut h, identity_b);

        let mut app_ctx = h.notedeck.app_context();
        let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);

        subs.dispose_route_for_account(
            app_ctx.ndb,
            &mut scoped_subs,
            account_pk,
            col_b,
            &carol_selection,
        );

        let snapshot = subs
            .remote_snapshot_for_root(app_ctx.ndb, account_pk, &root_id)
            .expect("remaining root snapshot");
        assert_eq!(snapshot.owners.len(), 1);
        assert!(snapshot.observed_relays.contains(&bob_relay));
        assert!(!snapshot.observed_relays.contains(&carol_relay));
        assert_live(&scoped_subs, identity_a);
        assert_readiness(&scoped_subs, identity_b, ScopedSubReadiness::Missing);
    }

    #[test]
    fn thread_remote_sub_key_is_remote_request_specific() {
        let root_id = NoteId::new([0x88; 32]);
        let other_root_id = NoteId::new([0x99; 32]);
        let account_pk = Pubkey::new([0x01; 32]);
        let col_a = ColumnId::for_test(7);
        let col_b = ColumnId::for_test(8);

        let root_key = thread_remote_sub_key(&root_id, ThreadScopedSub::RepliesByRootBaseline);
        let same_root_key = thread_remote_sub_key(&root_id, ThreadScopedSub::RepliesByRootBaseline);
        let other_root_key =
            thread_remote_sub_key(&other_root_id, ThreadScopedSub::RepliesByRootBaseline);

        assert_eq!(root_key, same_root_key);
        assert_ne!(root_key, other_root_key);
        assert_ne!(
            thread_scope_owner_key(account_pk, col_a, &root_id, 0),
            thread_scope_owner_key(account_pk, col_a, &root_id, 1)
        );
        assert_ne!(
            thread_scope_owner_key(account_pk, col_a, &root_id, 0),
            thread_scope_owner_key(account_pk, col_b, &root_id, 0)
        );
    }

    #[test]
    fn open_thread_remote_sub_restores_after_account_switch() {
        let mut h = ThreadHostHarness::new();
        let selection = thread_selection(0x33);
        let root_id = selection.root_id.to_note_id();
        let col = ColumnId::for_test(9);

        let mut app_ctx = h.notedeck.app_context();
        // Use a full-key account so the relay-list edit is signed, ingested into
        // NDB, and restored by the normal account-selection query path.
        let account_a_keypair = enostr::FullKeypair::generate().to_keypair();
        let account_a = account_a_keypair.pubkey;
        let add_account_a = app_ctx
            .accounts
            .add_account(account_a_keypair)
            .expect("new account A");
        assert_eq!(add_account_a.switch_to, account_a);
        app_ctx.select_account(&account_a);
        add_selected_account_read_relay(
            &mut app_ctx,
            "wss://thread-account-switch-read.example.com",
        );
        let account_b = enostr::FullKeypair::generate().to_keypair();
        let account_b_pk = account_b.pubkey;
        let add_response = app_ctx
            .accounts
            .add_account(account_b)
            .expect("new account");
        assert_eq!(add_response.switch_to, account_b_pk);
        assert_eq!(app_ctx.accounts.selected_account_pubkey(), &account_a);
        assert!(!app_ctx.accounts.selected_account_read_relays().is_empty());
        let identity = thread_identity(account_a, col, &root_id, 0);

        {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            let mut scoped_subs = app_ctx.remote.scoped_subs(app_ctx.accounts);
            let _ = h.threads.open(
                app_ctx.ndb,
                &txn,
                &mut scoped_subs,
                &selection,
                true,
                col,
                0.0,
                remote_policy(true),
            );
        }
        drop(app_ctx);
        wait_for_scoped_live(&mut h, identity);

        let mut app_ctx = h.notedeck.app_context();

        app_ctx.select_account(&account_b_pk);
        assert!(h
            .threads
            .subs
            .get_local_for_selected(app_ctx.accounts, col)
            .is_none());

        app_ctx.select_account(&account_a);
        assert!(!app_ctx.accounts.selected_account_read_relays().is_empty());
        assert!(h
            .threads
            .subs
            .get_local_for_selected(app_ctx.accounts, col)
            .is_some());

        drop(app_ctx);
        wait_for_scoped_live(&mut h, identity);
    }

    #[test]
    fn observed_thread_relays_include_known_ancestor_relays() {
        let mut h = ThreadHostHarness::new();
        let alice = enostr::FullKeypair::generate();
        let bob = enostr::FullKeypair::generate();
        let carol = enostr::FullKeypair::generate();
        let root_relay = NormRelayUrl::new("wss://root.example.com").expect("relay");
        let parent_relay = NormRelayUrl::new("wss://parent.example.com").expect("relay");
        let selected_relay = NormRelayUrl::new("wss://selected.example.com").expect("relay");

        let root_post = NewPost::new("root".to_owned(), alice.clone(), vec![], vec![]);
        let root = root_post.to_note(&secret_bytes(&alice));
        let bob_post = NewPost::new("bob".to_owned(), bob.clone(), vec![], vec![]);
        let bob_reply = bob_post.to_reply(&secret_bytes(&bob), &root);
        let carol_post = NewPost::new("carol".to_owned(), carol.clone(), vec![], vec![]);
        let carol_reply = carol_post.to_reply(&secret_bytes(&carol), &bob_reply);

        let app_ctx = h.notedeck.app_context();
        ingest_note_from_relay(app_ctx.ndb, &root, &root_relay);
        ingest_note_from_relay(app_ctx.ndb, &bob_reply, &parent_relay);
        ingest_note_from_relay(app_ctx.ndb, &carol_reply, &selected_relay);

        assert!(note_observed_relays(app_ctx.ndb, root.id()).contains(&root_relay));
        assert!(note_observed_relays(app_ctx.ndb, bob_reply.id()).contains(&parent_relay));
        assert!(note_observed_relays(app_ctx.ndb, carol_reply.id()).contains(&selected_relay));

        let scope = Scope {
            root_id: NoteId::new(*root.id()),
            stack: vec![tracked_sub(app_ctx.ndb, carol_reply.id())],
        };

        let relays = observed_thread_relays(app_ctx.ndb, &scope);

        assert!(relays.contains(&root_relay));
        assert!(relays.contains(&parent_relay));
        assert!(relays.contains(&selected_relay));
    }

    #[test]
    fn thread_remote_sub_declaration_retains_observed_relays_with_baseline() {
        let root_id = NoteId::new([0x44; 32]);
        let overlap_relay = NormRelayUrl::new("wss://overlap.example.com").expect("relay");
        let observed_extra = NormRelayUrl::new("wss://thread.example.com").expect("relay");
        let live_filters = vec![Filter::new().kinds([1]).limit(10).build()];
        let history_filters = vec![Filter::new().kinds([1]).build()];
        let (key, config) = thread_remote_sub_declaration(
            &root_id,
            HashSet::from_iter([overlap_relay.clone(), observed_extra.clone()]),
            live_filters.clone(),
            history_filters.clone(),
            remote_policy(true),
        );

        assert_eq!(
            key,
            thread_remote_sub_key(&root_id, ThreadScopedSub::RepliesByRootBaseline)
        );
        assert_eq!(
            config,
            expected_thread_remote_config(
                HashSet::from_iter([overlap_relay, observed_extra]),
                live_filters,
                history_filters,
                true,
            )
        );
    }

    #[test]
    fn thread_remote_sub_declaration_falls_back_to_accounts_read_when_thread_relays_missing() {
        let root_id = NoteId::new([0x66; 32]);
        let live_filters = vec![Filter::new().kinds([1]).limit(10).build()];
        let history_filters = vec![Filter::new().kinds([1]).build()];
        let (_key, config) = thread_remote_sub_declaration(
            &root_id,
            HashSet::new(),
            live_filters.clone(),
            history_filters.clone(),
            remote_policy(true),
        );

        assert_eq!(
            config,
            expected_thread_remote_config(HashSet::new(), live_filters, history_filters, true)
        );
    }

    #[test]
    fn thread_remote_sub_declaration_attaches_full_history_without_live_reply_limit() {
        let root_id = NoteId::new([0x11; 32]);
        let live_filters = scope_remote_thread_filters(&root_id);
        let history_filters = scope_remote_thread_history_filters(&root_id);
        let (_key, config) = thread_remote_sub_declaration(
            &root_id,
            HashSet::new(),
            live_filters.clone(),
            history_filters.clone(),
            remote_policy(true),
        );

        let live_values = filter_values(&live_filters);
        assert_eq!(live_values.len(), 2);
        assert_eq!(live_values[0]["limit"], serde_json::json!(500));
        assert_eq!(
            live_values[0]["#e"],
            serde_json::json!([hex::encode(root_id.bytes())])
        );
        assert_eq!(
            live_values[1]["ids"],
            serde_json::json!([hex::encode(root_id.bytes())])
        );
        assert!(live_values[1].get("kinds").is_none());
        assert_eq!(live_values[1]["limit"], serde_json::json!(1));

        let history_values = filter_values(&history_filters);
        assert_eq!(history_values.len(), 2);
        assert!(history_values
            .iter()
            .all(|filter| filter.get("limit").is_none()));
        assert_eq!(
            history_values[0]["#e"],
            serde_json::json!([hex::encode(root_id.bytes())])
        );
        assert_eq!(
            history_values[1]["ids"],
            serde_json::json!([hex::encode(root_id.bytes())])
        );
        assert!(history_values[1].get("kinds").is_none());
        assert_eq!(
            config,
            expected_thread_remote_config(HashSet::new(), live_filters, history_filters, true)
        );
    }

    #[test]
    fn thread_remote_sub_declaration_disable_outbox_relays_ignore_observed_relays() {
        let root_id = NoteId::new([0x77; 32]);
        let observed_relay = NormRelayUrl::new("wss://thread.example.com").expect("relay");
        let live_filters = vec![Filter::new().kinds([1]).limit(10).build()];
        let history_filters = vec![Filter::new().kinds([1]).build()];
        let (key, config) = thread_remote_sub_declaration(
            &root_id,
            HashSet::from_iter([observed_relay]),
            live_filters.clone(),
            history_filters.clone(),
            remote_policy(false),
        );

        assert_eq!(
            key,
            thread_remote_sub_key(&root_id, ThreadScopedSub::RepliesByRootBaseline)
        );
        assert_eq!(
            config,
            expected_thread_remote_config(HashSet::new(), live_filters, history_filters, false)
        );
    }
}
