use egui_nav::ReturnType;
use enostr::{NoteId, Pubkey};
use hashbrown::HashMap;
use nostrdb::{Filter, Ndb, Subscription};
use notedeck::{RelaySelection, ScopedSubApi, ScopedSubIdentity, SubConfig, SubKey, SubOwnerKey};

use crate::scoped_sub_owner_keys::thread_scope_owner_key;
use crate::timeline::{
    sub::{ndb_sub, ndb_unsub},
    ThreadSelection,
};

type RootNoteId = NoteId;

#[derive(Default)]
pub struct ThreadSubs {
    scopes: HashMap<MetaId, Vec<Scope>>,
}

// column id
type MetaId = usize;

/// Outcome of removing local thread subscriptions for a close action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnsubscribeOutcome {
    /// Local NDB sub(s) were removed, but the scope still has stack entries so the
    /// remote scoped-sub owner should remain.
    KeepOwner,
    /// The thread scope was fully removed and the remote scoped-sub owner should
    /// be released using the returned root note id.
    DropOwner(RootNoteId),
}

struct Scope {
    pub root_id: NoteId,
    stack: Vec<Sub>,
}

pub struct Sub {
    pub selected_id: NoteId,
    pub sub: Subscription,
    pub filter: Vec<Filter>,
}

impl ThreadSubs {
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &mut self,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_, '_>,
        meta_id: usize,
        id: &ThreadSelection,
        local_sub_filter: Vec<Filter>,
        new_scope: bool,
        remote_sub_filter: Vec<Filter>,
    ) {
        let account_pk = scoped_subs.selected_account_pubkey();
        let cur_scopes = self.scopes.entry(meta_id).or_default();

        let added_local = if new_scope || cur_scopes.is_empty() {
            local_sub_new_scope(
                ndb,
                scoped_subs,
                account_pk,
                meta_id,
                id,
                local_sub_filter,
                remote_sub_filter,
                cur_scopes,
            )
        } else {
            let cur_scope = cur_scopes.last_mut().expect("can't be empty");
            sub_current_scope(ndb, id, local_sub_filter, cur_scope)
        };

        if added_local {
            tracing::debug!("Sub stats: num locals: {}", self.scopes.len());
        }
    }

    pub fn unsubscribe(
        &mut self,
        ndb: &mut Ndb,
        scoped_subs: &mut ScopedSubApi<'_, '_>,
        meta_id: usize,
        id: &ThreadSelection,
        return_type: ReturnType,
    ) {
        let account_pk = scoped_subs.selected_account_pubkey();
        let Some(scopes) = self.scopes.get_mut(&meta_id) else {
            return;
        };

        let scope_depth = scopes.len().saturating_sub(1);
        let Some(unsub_outcome) = (match return_type {
            ReturnType::Drag => unsubscribe_drag(scopes, ndb, id),
            ReturnType::Click => unsubscribe_click(scopes, ndb, id),
        }) else {
            return;
        };

        if scopes.is_empty() {
            self.scopes.remove(&meta_id);
        }

        if let UnsubscribeOutcome::DropOwner(root_id) = unsub_outcome {
            let owner = thread_scope_owner_key(account_pk, meta_id, &root_id, scope_depth);
            let _ = scoped_subs.drop_owner(owner);
        }

        tracing::debug!(
            "unsub stats: num locals: {}, released owner: {}",
            self.scopes.len(),
            matches!(unsub_outcome, UnsubscribeOutcome::DropOwner(_)),
        );
    }

    pub fn get_local(&self, meta_id: usize) -> Option<&Sub> {
        self.scopes
            .get(&meta_id)
            .as_ref()
            .and_then(|s| s.last())
            .and_then(|s| s.stack.last())
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

fn log_scope_root_mismatch(scope: &Scope, id: &ThreadSelection) {
    if scope.root_id.bytes() != id.root_id.bytes() {
        tracing::error!(
            "Somehow the current scope's root is not equal to the selected note's root. scope's root: {:?}, thread's root: {:?}",
            scope.root_id.hex(),
            id.root_id.bytes()
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ThreadScopedSub {
    RepliesByRoot,
}

fn thread_remote_sub_key(root_id: &RootNoteId) -> SubKey {
    SubKey::builder(ThreadScopedSub::RepliesByRoot)
        .with(*root_id.bytes())
        .finish()
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

    if let Some(sub) = ndb_sub(ndb, &local_sub_filter, selection) {
        cur_scope.stack.push(Sub {
            selected_id: NoteId::new(*selection.selected_or_root()),
            sub,
            filter: local_sub_filter,
        });
        return true;
    }

    false
}

fn sub_remote(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    owner: SubOwnerKey,
    key: SubKey,
    filter: Vec<Filter>,
    id: impl std::fmt::Debug,
) {
    tracing::debug!("Remote subscribe for {:?}", id);

    let identity = ScopedSubIdentity::account(owner, key);
    let config = SubConfig {
        relays: RelaySelection::AccountsRead,
        filters: filter,
        use_transparent: false,
    };
    let _ = scoped_subs.ensure_sub(identity, config);
}

#[allow(clippy::too_many_arguments)]
fn local_sub_new_scope(
    ndb: &mut Ndb,
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    account_pk: Pubkey,
    meta_id: usize,
    id: &ThreadSelection,
    local_sub_filter: Vec<Filter>,
    remote_sub_filter: Vec<Filter>,
    scopes: &mut Vec<Scope>,
) -> bool {
    let root_id = id.root_id.to_note_id();
    let scope_depth = scopes.len();
    let owner = thread_scope_owner_key(account_pk, meta_id, &root_id, scope_depth);
    tracing::info!(
        "thread sub with owner: pk: {account_pk:?}, col: {meta_id}, rootid: {root_id:?}, depth: {scope_depth}"
    );
    sub_remote(
        scoped_subs,
        owner,
        thread_remote_sub_key(&root_id),
        remote_sub_filter,
        id,
    );

    let Some(sub) = ndb_sub(ndb, &local_sub_filter, id) else {
        let _ = scoped_subs.drop_owner(owner);
        return false;
    };

    scopes.push(Scope {
        root_id,
        stack: vec![Sub {
            selected_id: NoteId::new(*id.selected_or_root()),
            sub,
            filter: local_sub_filter,
        }],
    });

    true
}
