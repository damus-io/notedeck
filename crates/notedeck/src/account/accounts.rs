use crate::account::cache::AccountCache;
use crate::account::contacts::Contacts;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{
    apply_local_advertised_relay_action, calculate_relays, modify_advertised_relays,
    modify_private_relays, write_relays, AccountRelayData, RelayAction, RelayDefaults,
};
use crate::scoped_subs::{ScopedSubIdentity, SubConfig, SubKey, SubRelayPolicy};
use crate::storage::AccountStorageWriter;
use crate::user_account::UserAccountSerializable;
use crate::{
    AccountStorage, FullHistoryConfig, MuteFun, RemoteApi, ScopedSubApi, SingleUnkIdAction,
    SubOwnerKey, UnknownIds, UserAccount, ZapWallet,
};
use enostr::{FilledKeypair, Keypair, NormRelayUrl, Pubkey, RelayId, RelayRoutingPreference};
use hashbrown::HashSet;
use nostrdb::{Filter, IngestMetadata, Ndb, Note, Subscription, Transaction};

use std::slice::from_ref;
// TODO: remove this
use std::sync::Arc;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    pub cache: AccountCache,
    storage_writer: Option<AccountStorageWriter>,
    relay_defaults: RelayDefaults,
    ndb_subs: AccountNdbSubs,
    scoped_remote_initialized: bool,
}

impl Accounts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key_store: Option<AccountStorage>,
        forced_relays: Vec<String>,
        bootstrap_relays: Vec<String>,
        fallback: Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
    ) -> Self {
        let (mut cache, unknown_id) = AccountCache::new(UserAccount::new(
            Keypair::only_pubkey(fallback),
            AccountData::new(fallback.bytes()),
        ));

        unknown_id.process_action(unknown_ids, ndb, txn);

        let mut storage_writer = None;
        if let Some(keystore) = key_store {
            let (reader, writer) = keystore.rw();
            match reader.get_accounts() {
                Ok(accounts) => {
                    for account in accounts {
                        add_account_from_storage(&mut cache, account).process_action(
                            unknown_ids,
                            ndb,
                            txn,
                        )
                    }
                }
                Err(e) => {
                    tracing::error!("could not get keys: {e}");
                }
            }
            if let Some(selected) = reader.get_selected_key().ok().flatten() {
                cache.select(selected);
            }

            storage_writer = Some(writer);
        };

        let relay_defaults = RelayDefaults::new(forced_relays, bootstrap_relays);

        let selected = cache.selected_mut();
        let selected_key = selected.key.clone();
        let selected_data = &mut selected.data;

        selected_data.query(ndb, txn, &selected_key);

        let ndb_subs = AccountNdbSubs::new(ndb, selected_data);

        Accounts {
            cache,
            storage_writer,
            relay_defaults,
            ndb_subs,
            scoped_remote_initialized: false,
        }
    }

    pub(crate) fn remove_account(
        &mut self,
        pk: &Pubkey,
        ndb: &mut Ndb,
        remote: &mut RemoteApi<'_>,
    ) -> bool {
        self.remove_account_internal(pk, ndb, remote)
    }

    fn remove_account_internal(
        &mut self,
        pk: &Pubkey,
        ndb: &mut Ndb,
        remote: &mut RemoteApi<'_>,
    ) -> bool {
        let Some(resp) = self.cache.remove(pk) else {
            return false;
        };

        if pk != self.cache.fallback() {
            if let Some(key_store) = &self.storage_writer {
                if let Err(e) = key_store.remove_key(&resp.deleted) {
                    tracing::error!("Could not remove account {pk}: {e}");
                }
            }
        }

        if let Some(swap_to) = resp.swap_to {
            let txn = Transaction::new(ndb).expect("txn");
            self.finish_account_selection_with_session(&swap_to, ndb, &txn, remote);
        }

        {
            let mut scoped_subs = remote.scoped_subs(&*self);
            scoped_subs.purge_account_scope(resp.deleted.pubkey);
        }

        true
    }

    pub fn contains_full_kp(&self, pubkey: &enostr::Pubkey) -> bool {
        self.cache
            .get(pubkey)
            .is_some_and(|u| u.key.secret_key.is_some())
    }

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(&mut self, kp: Keypair) -> Option<AddAccountResponse> {
        let acc = if let Some(acc) = self.cache.get_mut(&kp.pubkey) {
            if kp.secret_key.is_none() || acc.key.secret_key.is_some() {
                tracing::info!("Already have account, not adding");
                return None;
            }

            acc.key = kp.clone();
            AccType::Acc(&*acc)
        } else {
            let new_account_data = AccountData::new(kp.pubkey.bytes());
            AccType::Entry(
                self.cache
                    .add(UserAccount::new(kp.clone(), new_account_data)),
            )
        };

        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.write_account(&acc.get_acc().into()) {
                tracing::error!("Could not add key for {:?}: {e}", kp.pubkey);
            }
        }

        Some(AddAccountResponse {
            switch_to: kp.pubkey,
            unk_id_action: SingleUnkIdAction::pubkey(kp.pubkey),
        })
    }

    /// Update the `UserAccount` via callback and save the result to disk.
    /// return true if the update was successful
    pub fn update_current_account(&mut self, update: impl FnOnce(&mut UserAccount)) -> bool {
        let cur_account = self.get_selected_account_mut();

        update(cur_account);

        let cur_acc = self.get_selected_account();

        let Some(key_store) = &self.storage_writer else {
            return false;
        };

        if let Err(err) = key_store.write_account(&cur_acc.into()) {
            tracing::error!("Could not add account {:?} to storage: {err}", cur_acc.key);
            return false;
        }

        true
    }

    pub fn selected_filled(&self) -> Option<FilledKeypair<'_>> {
        self.get_selected_account().key.to_full()
    }

    /// Get the selected account's pubkey as bytes. Common operation so
    /// we make it a helper here.
    pub fn selected_account_pubkey_bytes(&self) -> &[u8; 32] {
        self.get_selected_account().key.pubkey.bytes()
    }

    pub fn selected_account_pubkey(&self) -> &Pubkey {
        &self.get_selected_account().key.pubkey
    }

    pub fn get_selected_account(&self) -> &UserAccount {
        self.cache.selected()
    }

    pub fn selected_account_has_wallet(&self) -> bool {
        self.get_selected_account().wallet.is_some()
    }

    fn get_selected_account_mut(&mut self) -> &mut UserAccount {
        self.cache.selected_mut()
    }

    pub fn get_selected_wallet(&self) -> Option<&ZapWallet> {
        self.cache.selected().wallet.as_ref()
    }

    pub fn get_selected_wallet_mut(&mut self) -> Option<&mut ZapWallet> {
        self.cache.selected_mut().wallet.as_mut()
    }

    fn get_selected_account_data(&self) -> &AccountData {
        &self.cache.selected().data
    }

    /// Select a new current account and apply the corresponding host-side
    /// scoped-subscription transition.
    pub(crate) fn select_account(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        remote: &mut RemoteApi<'_>,
    ) {
        self.select_account_with_session(pk_to_select, ndb, txn, remote);
    }

    /// Select the active account during startup before any remote session exists.
    ///
    /// This updates the local account cache, persistence, and `nostrdb`
    /// subscriptions without touching remote outbox state. The first real
    /// frame-scoped `RemoteApi` will later initialize the corresponding remote
    /// subscriptions through the normal `update()` path.
    pub(crate) fn select_account_for_startup(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
    ) {
        if !self.begin_account_selection(pk_to_select, ndb) {
            return;
        }

        self.refresh_selected_account_state(pk_to_select, ndb, txn);
        self.scoped_remote_initialized = false;
    }

    fn select_account_with_session(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        remote: &mut RemoteApi<'_>,
    ) {
        if !self.begin_account_selection(pk_to_select, ndb) {
            return;
        }

        self.finish_account_selection_with_session(pk_to_select, ndb, txn, remote);
    }

    /// Complete an account selection after the local cache already points at the new account.
    fn finish_account_selection_with_session(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        remote: &mut RemoteApi<'_>,
    ) {
        self.refresh_selected_account_state(pk_to_select, ndb, txn);
        remote.on_account_switched(self);
        selected_account_request_subs(&mut remote.scoped_subs(self), self.get_selected_account());
    }

    /// Complete the local side of an account selection after cache selection succeeds.
    fn refresh_selected_account_state(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
    ) {
        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.select_key(Some(*pk_to_select)) {
                tracing::error!("Could not select key {:?}: {e}", pk_to_select);
            }
        }

        let selected = self.get_selected_account_mut();
        let selected_key = selected.key.clone();
        selected.data.query(ndb, txn, &selected_key);
        self.ndb_subs.swap_to(ndb, &self.cache.selected().data);
    }

    /// Select the account in the local cache and register any available secret
    /// key with `nostrdb` so giftwrap ingestion follows the new selection.
    fn begin_account_selection(&mut self, pk_to_select: &Pubkey, ndb: &mut Ndb) -> bool {
        if !self.cache.select(*pk_to_select) {
            return false;
        }
        if let Some(filled) = self.selected_filled() {
            ndb.add_key(&filled.secret_key.secret_bytes());
        }
        true
    }

    pub fn mutefun(&self) -> Box<MuteFun> {
        let account_data = self.get_selected_account_data();

        let muted = Arc::clone(&account_data.muted.muted);
        Box::new(move |note: &Note, thread: &[u8; 32]| muted.is_muted(note, thread))
    }

    pub fn mute(&self) -> Box<Arc<crate::Muted>> {
        let account_data = self.get_selected_account_data();
        Box::new(Arc::clone(&account_data.muted.muted))
    }

    pub fn update_max_hashtags_per_note(&mut self, max_hashtags: usize) {
        for account in self.cache.accounts_mut() {
            account.data.muted.update_max_hashtags(max_hashtags);
        }
    }

    #[profiling::function]
    pub fn update(&mut self, ndb: &mut Ndb, remote: &mut RemoteApi<'_>) {
        let selected = self.cache.selected_mut();
        let previous_relay_state =
            selected
                .data
                .poll_for_updates(ndb, &selected.key, &self.ndb_subs);

        if !self.scoped_remote_initialized {
            remote.on_selected_account_changed(self);
            selected_account_request_subs(
                &mut remote.scoped_subs(self),
                self.get_selected_account(),
            );
            self.scoped_remote_initialized = true;
            return;
        }

        let Some(previous_relay_state) = previous_relay_state else {
            return;
        };

        if self.selected_account_remote_state_changed_from(previous_relay_state) {
            remote.on_selected_account_changed(self);
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    /// Apply a selected-account relay edit through local NDB before broadcasting it.
    pub(crate) fn process_relay_action(
        &mut self,
        ndb: &Ndb,
        remote: &mut RemoteApi<'_>,
        action: RelayAction,
    ) {
        if matches!(
            action,
            RelayAction::AddPrivate(_) | RelayAction::RemovePrivate(_)
        ) {
            let selected = self.cache.selected_mut();
            modify_private_relays(
                &selected.key,
                action,
                remote,
                &self.relay_defaults,
                &mut selected.data,
            );
            return;
        }

        if selected_account_is_pubkey_only(&self.cache) {
            let old_read_relays = self.selected_account_read_relays();
            let old_write_relays = self.selected_account_write_relays();
            let changed = apply_local_advertised_relay_action(
                action,
                &self.relay_defaults,
                &mut self.cache.selected_mut().data.relay,
            );

            if changed
                && self.selected_account_remote_state_changed(old_read_relays, old_write_relays)
            {
                remote.on_selected_account_changed(self);
            }

            return;
        }

        let selected = self.cache.selected();
        let existing_relay_list = {
            let Ok(txn) = Transaction::new(ndb) else {
                tracing::error!("process_relay_action: failed to open relay list projection txn");
                return;
            };
            selected.data.relay.newest_effective_nip65_relays(ndb, &txn)
        };
        let created_after = existing_relay_list
            .as_ref()
            .map(|(created_at, _)| *created_at);
        let (base_advertised, bootstrap_if_empty) = match existing_relay_list.as_ref() {
            Some((_, advertised)) => (advertised, false),
            None => (&selected.data.relay.advertised, true),
        };

        let Some(relay_edit) = modify_advertised_relays(
            &selected.key,
            action,
            &self.relay_defaults,
            &selected.data.relay,
            base_advertised,
            bootstrap_if_empty,
            created_after,
        ) else {
            return;
        };

        let Ok(event) = enostr::ClientMessage::event(&relay_edit.note) else {
            tracing::error!("process_relay_action: failed to build client event");
            return;
        };

        let Ok(local_event_json) = event.to_json() else {
            tracing::error!("process_relay_action: failed to serialize relay list event");
            return;
        };

        let Ok(note_json) = relay_edit.note.json() else {
            tracing::error!("process_relay_action: failed to serialize relay list note");
            return;
        };

        if let Err(err) =
            ndb.process_event_with(&local_event_json, IngestMetadata::new().client(true))
        {
            tracing::error!("process_relay_action: failed to ingest local relay list event: {err}");
            return;
        }

        let old_read_relays = self.selected_account_read_relays();
        let old_write_relays = self.selected_account_write_relays();
        self.cache
            .selected_mut()
            .data
            .relay
            .accept_local_relay_list(relay_edit.projection);

        let mut publisher = remote.publisher_explicit();
        publisher.publish_event_json(note_json, relay_edit.write_relays);

        if self.selected_account_remote_state_changed(old_read_relays, old_write_relays) {
            remote.on_selected_account_changed(self);
        }
    }

    pub fn selected_account_read_relays(&self) -> HashSet<NormRelayUrl> {
        calculate_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
            true,
        )
    }

    /// Return the selected account's advertised NIP-65 relays with marker metadata.
    pub fn selected_account_advertised_relays(
        &self,
    ) -> &std::collections::BTreeSet<crate::RelaySpec> {
        &self.get_selected_account_data().relay.advertised
    }

    /// Return the selected account's kind-10013 NIP-37 private-sync relay set.
    pub fn selected_account_private_relay_set(&self) -> &std::collections::BTreeSet<NormRelayUrl> {
        &self.get_selected_account_data().relay.private
    }

    pub fn selected_account_write_relays(&self) -> Vec<RelayId> {
        write_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
        )
    }

    /// Return the selected account's private-sync relays from its decrypted
    /// kind-10013 NIP-37 list, as `RelayId`s. Used by dave/headway/notebook to
    /// sync private state across the user's own devices.
    pub fn selected_account_private_relays(&self) -> Vec<RelayId> {
        self.get_selected_account_data()
            .relay
            .private
            .iter()
            .map(|url| RelayId::Websocket(url.clone()))
            .collect()
    }

    fn selected_account_remote_state_changed_from(
        &self,
        previous_relay_state: AccountRelayData,
    ) -> bool {
        let previous_read_relays =
            calculate_relays(&self.relay_defaults, &previous_relay_state, true);
        let previous_write_relays = write_relays(&self.relay_defaults, &previous_relay_state);
        self.selected_account_remote_state_changed(previous_read_relays, previous_write_relays)
    }

    fn selected_account_remote_state_changed(
        &self,
        previous_read_relays: HashSet<NormRelayUrl>,
        previous_write_relays: Vec<RelayId>,
    ) -> bool {
        previous_read_relays != self.selected_account_read_relays()
            || previous_write_relays != self.selected_account_write_relays()
    }
}

enum AccType<'a> {
    Entry(hashbrown::hash_map::OccupiedEntry<'a, Pubkey, UserAccount>),
    Acc(&'a UserAccount),
}

impl<'a> AccType<'a> {
    fn get_acc(&'a self) -> &'a UserAccount {
        match self {
            AccType::Entry(occupied_entry) => occupied_entry.get(),
            AccType::Acc(user_account) => user_account,
        }
    }
}

fn add_account_from_storage(
    cache: &mut AccountCache,
    user_account_serializable: UserAccountSerializable,
) -> SingleUnkIdAction {
    let Some(acc) = get_acc_from_storage(user_account_serializable) else {
        return SingleUnkIdAction::NoAction;
    };

    let pk = acc.key.pubkey;
    cache.add(acc);

    SingleUnkIdAction::pubkey(pk)
}

fn get_acc_from_storage(user_account_serializable: UserAccountSerializable) -> Option<UserAccount> {
    let keypair = user_account_serializable.key;
    let new_account_data = AccountData::new(keypair.pubkey.bytes());

    let mut wallet = None;
    if let Some(wallet_s) = user_account_serializable.wallet {
        let m_wallet: Result<crate::ZapWallet, crate::Error> = wallet_s.into();
        match m_wallet {
            Ok(w) => wallet = Some(w),
            Err(e) => {
                tracing::error!("Problem creating wallet from disk: {e}");
            }
        };
    }

    Some(UserAccount {
        key: keypair,
        wallet,
        data: new_account_data,
    })
}

#[derive(Clone)]
pub struct AccountData {
    pub(crate) relay: AccountRelayData,
    pub(crate) muted: AccountMutedData,
    pub contacts: Contacts,
}

impl AccountData {
    pub fn new(pubkey: &[u8; 32]) -> Self {
        Self {
            relay: AccountRelayData::new(pubkey),
            muted: AccountMutedData::new(pubkey),
            contacts: Contacts::new(pubkey),
        }
    }

    #[profiling::function]
    pub(super) fn poll_for_updates(
        &mut self,
        ndb: &Ndb,
        keypair: &Keypair,
        ndb_subs: &AccountNdbSubs,
    ) -> Option<AccountRelayData> {
        let txn = Transaction::new(ndb).expect("txn");
        let previous_relay_state = self.relay.poll_for_updates(ndb, &txn, ndb_subs.relay_ndb);
        self.relay
            .poll_private_for_updates(ndb, &txn, ndb_subs.private_relay_ndb, keypair);

        self.muted.poll_for_updates(ndb, &txn, ndb_subs.mute_ndb);
        self.contacts
            .poll_for_updates(ndb, &txn, ndb_subs.contacts_ndb);

        previous_relay_state
    }

    /// Note: query should be called as close to the subscription as possible
    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction, keypair: &Keypair) {
        self.relay.query(ndb, txn, keypair);
        self.muted.query(ndb, txn);
        self.contacts.query(ndb, txn);
    }
}

pub struct AddAccountResponse {
    pub switch_to: Pubkey,
    pub unk_id_action: SingleUnkIdAction,
}

fn giftwrap_live_filter(pk: &Pubkey) -> Filter {
    nostrdb::Filter::new()
        .kinds([1059])
        .pubkeys([pk.bytes()])
        .limit(500)
        .build()
}

fn giftwrap_history_filter(pk: &Pubkey) -> Filter {
    nostrdb::Filter::new()
        .kinds([1059])
        .pubkeys([pk.bytes()])
        .build()
}

fn account_remote_owner_key() -> SubOwnerKey {
    SubOwnerKey::new("core/accounts/remote-subs")
}

fn selected_account_request_subs(
    scoped_subs: &mut ScopedSubApi<'_>,
    selected_account: &UserAccount,
) {
    for declaration in selected_account_remote_declarations(selected_account) {
        let _ = scoped_subs.ensure_sub(declaration.identity, declaration.config);
    }
}

#[derive(Clone)]
struct AccountRemoteDeclaration {
    identity: ScopedSubIdentity,
    config: SubConfig,
}

fn selected_account_remote_declarations(
    selected_account: &UserAccount,
) -> Vec<AccountRemoteDeclaration> {
    let data = &selected_account.data;
    let owner = account_remote_owner_key();
    account_remote_sub_kinds()
        .into_iter()
        .map(|kind| {
            let key = account_remote_sub_key(kind);
            let identity = ScopedSubIdentity::account(owner, key);
            let config = match kind {
                AccountRemoteSubKind::RelayList => make_account_remote_config(
                    vec![data.relay.filter.clone()],
                    RelayRoutingPreference::default(),
                ),
                AccountRemoteSubKind::PrivateRelayList => make_account_remote_config(
                    vec![data.relay.private_filter.clone()],
                    RelayRoutingPreference::default(),
                ),
                AccountRemoteSubKind::MuteList => make_account_remote_config(
                    vec![data.muted.filter.clone()],
                    RelayRoutingPreference::default(),
                ),
                AccountRemoteSubKind::ContactsList => make_account_remote_config(
                    vec![data.contacts.filter.clone()],
                    RelayRoutingPreference::RequireDedicated,
                ),
                AccountRemoteSubKind::Giftwrap => {
                    make_giftwrap_remote_config(&selected_account.key.pubkey)
                }
            };
            AccountRemoteDeclaration { identity, config }
        })
        .collect()
}

fn selected_account_is_pubkey_only(cache: &AccountCache) -> bool {
    cache.selected().key.secret_key.is_none()
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AccountRemoteSubKind {
    RelayList,
    PrivateRelayList,
    MuteList,
    ContactsList,
    Giftwrap,
}

fn account_remote_sub_kinds() -> [AccountRemoteSubKind; 5] {
    [
        AccountRemoteSubKind::RelayList,
        AccountRemoteSubKind::PrivateRelayList,
        AccountRemoteSubKind::MuteList,
        AccountRemoteSubKind::ContactsList,
        AccountRemoteSubKind::Giftwrap,
    ]
}

fn account_remote_sub_key(kind: AccountRemoteSubKind) -> SubKey {
    SubKey::new(kind)
}

/// Returns the [`ScopedSubIdentity`] used for the account giftwrap subscription.
///
/// Useful for test harnesses that need to verify the giftwrap subscription
/// has reached EOSE before sending messages.
pub fn giftwrap_sub_identity() -> ScopedSubIdentity {
    let owner = account_remote_owner_key();
    let key = account_remote_sub_key(AccountRemoteSubKind::Giftwrap);
    ScopedSubIdentity::account(owner, key)
}

fn make_account_remote_config(
    filters: Vec<Filter>,
    routing_preference: RelayRoutingPreference,
) -> SubConfig {
    SubConfig::builder(filters)
        .accounts_read(SubRelayPolicy::accounts_read_important_with_preference(
            routing_preference,
        ))
        .build()
}

fn make_giftwrap_remote_config(pk: &Pubkey) -> SubConfig {
    SubConfig::builder(vec![giftwrap_live_filter(pk)])
        .full_history(FullHistoryConfig::new(vec![giftwrap_history_filter(pk)]))
        .accounts_read(SubRelayPolicy::accounts_read_important_with_preference(
            RelayRoutingPreference::RequireDedicated,
        ))
        .build()
}
struct AccountNdbSubs {
    relay_ndb: Subscription,
    private_relay_ndb: Subscription,
    mute_ndb: Subscription,
    contacts_ndb: Subscription,
}

impl AccountNdbSubs {
    pub fn new(ndb: &mut Ndb, data: &AccountData) -> Self {
        let relay_ndb = ndb
            .subscribe(from_ref(&data.relay.filter))
            .expect("ndb relay list subscription");
        let private_relay_ndb = ndb
            .subscribe(from_ref(&data.relay.private_filter))
            .expect("ndb private relay list subscription");
        let mute_ndb = ndb
            .subscribe(from_ref(&data.muted.filter))
            .expect("ndb sub");
        let contacts_ndb = ndb
            .subscribe(from_ref(&data.contacts.filter))
            .expect("ndb sub");
        Self {
            relay_ndb,
            private_relay_ndb,
            mute_ndb,
            contacts_ndb,
        }
    }

    pub fn swap_to(&mut self, ndb: &mut Ndb, new_selection_data: &AccountData) {
        let _ = ndb.unsubscribe(self.relay_ndb);
        let _ = ndb.unsubscribe(self.private_relay_ndb);
        let _ = ndb.unsubscribe(self.mute_ndb);
        let _ = ndb.unsubscribe(self.contacts_ndb);

        *self = AccountNdbSubs::new(ndb, new_selection_data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        construct_nip65_relays_note, remote_data::RemoteState, JobPool, RelaySpec,
        ScopedSubReadiness, FALLBACK_PUBKEY,
    };
    use enostr::FullKeypair;
    use nostrdb::Config;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    struct AccountRemoteHarness {
        _tmp: TempDir,
        ndb: Ndb,
        accounts: Accounts,
        remote: RemoteState,
        _job_pool: JobPool,
    }

    impl AccountRemoteHarness {
        fn new() -> Self {
            Self::with_forced_relays(Vec::new())
        }

        fn with_forced_relays(forced_relays: Vec<String>) -> Self {
            let tmp = TempDir::new().expect("tmp dir");
            let mut ndb =
                Ndb::new(tmp.path().to_str().expect("path"), &Config::new()).expect("ndb");
            let txn = Transaction::new(&ndb).expect("txn");
            let mut unknown_ids = UnknownIds::default();
            let accounts = Accounts::new(
                None,
                forced_relays,
                crate::account::relay::default_bootstrap_relays(),
                FALLBACK_PUBKEY(),
                &mut ndb,
                &txn,
                &mut unknown_ids,
            );
            let job_pool = JobPool::new(1);
            crate::app::install_crypto();
            let remote = RemoteState::new_with_config(
                &ndb,
                job_pool.spawner(),
                || {},
                crate::remote_data::RemoteBridgeConfig::default(),
            );

            Self {
                _tmp: tmp,
                ndb,
                accounts,
                remote,
                _job_pool: job_pool,
            }
        }

        fn with_remote<T>(
            &mut self,
            f: impl FnOnce(&mut crate::RemoteApi<'_>, &mut Accounts, &mut Ndb) -> T,
        ) -> T {
            let mut remote = self.remote.api();
            let result = f(&mut remote, &mut self.accounts, &mut self.ndb);
            remote.flush();
            result
        }

        fn identity_for(kind: AccountRemoteSubKind) -> ScopedSubIdentity {
            ScopedSubIdentity::account(account_remote_owner_key(), account_remote_sub_key(kind))
        }

        fn readiness_for(
            &mut self,
            account_pk: Pubkey,
            identity: ScopedSubIdentity,
        ) -> ScopedSubReadiness {
            self.remote.poll_bridge();
            let mut remote = self.remote.api();
            let scoped_subs = remote.scoped_subs(&self.accounts);
            scoped_subs.sub_readiness_for_account(account_pk, identity)
        }

        fn wait_for_readiness(
            &mut self,
            account_pk: Pubkey,
            identity: ScopedSubIdentity,
            accepts: impl Fn(ScopedSubReadiness) -> bool,
            message: &str,
        ) -> ScopedSubReadiness {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let readiness = self.readiness_for(account_pk, identity);
                if accepts(readiness) {
                    return readiness;
                }

                if Instant::now() >= deadline {
                    panic!("{message}: {readiness:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn expect_live(&mut self, account_pk: Pubkey, identity: ScopedSubIdentity, message: &str) {
            let _ = self.wait_for_readiness(
                account_pk,
                identity,
                |readiness| matches!(readiness, ScopedSubReadiness::Live(_)),
                message,
            );
        }

        fn expect_inactive(
            &mut self,
            account_pk: Pubkey,
            identity: ScopedSubIdentity,
            message: &str,
        ) {
            let _ = self.wait_for_readiness(
                account_pk,
                identity,
                |readiness| readiness == ScopedSubReadiness::Inactive,
                message,
            );
        }

        fn expect_missing(
            &mut self,
            account_pk: Pubkey,
            identity: ScopedSubIdentity,
            message: &str,
        ) {
            let _ = self.wait_for_readiness(
                account_pk,
                identity,
                |readiness| readiness == ScopedSubReadiness::Missing,
                message,
            );
        }
    }

    fn selected_nip65_projection(
        ndb: &Ndb,
        account_pk: &Pubkey,
    ) -> Option<(u64, std::collections::BTreeSet<RelaySpec>)> {
        let txn = Transaction::new(ndb).expect("txn");
        let filter = Filter::new()
            .authors([account_pk.bytes()])
            .kinds([10002])
            .limit(1)
            .build();
        let results = ndb
            .query(&txn, &[filter], 1)
            .expect("query selected account NIP-65 relay list");
        let note_key = results.first()?.note_key;
        let note = ndb
            .get_note_by_key(&txn, note_key)
            .expect("selected account NIP-65 note");
        let relays = crate::relayspec::relays_from_nip65_note(&note)
            .into_iter()
            .collect();
        Some((note.created_at(), relays))
    }

    #[tokio::test]
    async fn account_remote_sub_configs_set_full_history_for_giftwrap() {
        let filter = Filter::new().kinds(vec![0]).limit(1).build();

        let config = make_account_remote_config(vec![filter], RelayRoutingPreference::default());
        assert!(
            config.full_history_config().is_none(),
            "non-giftwrap account remote sub should stay live-only"
        );

        let pk = Pubkey::new([9; 32]);
        let config = make_giftwrap_remote_config(&pk);
        let live_filter = giftwrap_live_filter(&pk);
        let live_json = live_filter.json().expect("live filter json");
        assert!(
            live_json.contains("\"limit\":500"),
            "giftwrap live filter should keep its transport limit"
        );
        let full_history = config.full_history_config().expect("giftwrap full history");
        let history_json = full_history.filters()[0]
            .as_filter()
            .json()
            .expect("history filter json");
        assert!(
            !history_json.contains("\"limit\""),
            "giftwrap history filter should be constructed without the live filter transport limit"
        );
    }

    #[tokio::test]
    async fn update_initializes_selected_account_remote_subs_with_expected_routing() {
        let mut h = AccountRemoteHarness::new();
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let selected = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let mute_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::MuteList);
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);
        let giftwrap = giftwrap_sub_identity();

        h.expect_live(selected, relay_list, "relay list should be live");
        h.expect_live(selected, mute_list, "mute list should be live");
        h.expect_live(selected, contacts_list, "contacts list should be live");
        h.expect_live(selected, giftwrap, "giftwrap should be live");

        let giftwrap_declaration =
            selected_account_remote_declarations(h.accounts.get_selected_account())
                .into_iter()
                .find(|declaration| declaration.identity == giftwrap)
                .expect("giftwrap declaration");
        assert_eq!(
            giftwrap_declaration.config,
            make_giftwrap_remote_config(&selected),
            "giftwrap live sub should target the selected account's pubkey"
        );
    }

    /// Startup account selection should only flip local account state and defer
    /// remote scoped-sub initialization until the first real update pass.
    #[tokio::test]
    async fn startup_selection_defers_remote_sub_initialization_until_first_update() {
        let mut h = AccountRemoteHarness::new();
        let selected_keypair = FullKeypair::generate().to_keypair();
        let selected = selected_keypair.pubkey;
        let add_response = h
            .accounts
            .add_account(selected_keypair)
            .expect("add selected account");
        assert_eq!(add_response.switch_to, selected);

        {
            let txn = Transaction::new(&h.ndb).expect("txn");
            h.accounts
                .select_account_for_startup(&selected, &mut h.ndb, &txn);
        }

        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let mute_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::MuteList);
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);
        let giftwrap = giftwrap_sub_identity();

        assert!(!h.accounts.scoped_remote_initialized);
        h.expect_missing(
            selected,
            relay_list,
            "relay list should be missing before first update",
        );
        h.expect_missing(
            selected,
            mute_list,
            "mute list should be missing before first update",
        );
        h.expect_missing(
            selected,
            contacts_list,
            "contacts list should be missing before first update",
        );
        h.expect_missing(
            selected,
            giftwrap,
            "giftwrap should be missing before first update",
        );

        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        assert!(h.accounts.scoped_remote_initialized);
        h.expect_live(
            selected,
            giftwrap,
            "giftwrap should be live after first update",
        );
        let giftwrap_declaration =
            selected_account_remote_declarations(h.accounts.get_selected_account())
                .into_iter()
                .find(|declaration| declaration.identity == giftwrap)
                .expect("giftwrap declaration");
        assert_eq!(
            giftwrap_declaration.config,
            make_giftwrap_remote_config(&selected),
            "first update after startup selection should initialize remote subs for the selected account",
        );
    }

    #[tokio::test]
    async fn account_switch_replaces_remote_subs_and_restores_them_on_switch_back() {
        let mut h = AccountRemoteHarness::new();
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let account_a = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let giftwrap = giftwrap_sub_identity();
        h.expect_live(account_a, relay_list, "relay list should be live for A");
        h.expect_live(account_a, giftwrap, "giftwrap should be live for A");

        let account_b = FullKeypair::generate().to_keypair();
        let account_b_pk = account_b.pubkey;
        let add_response = h.accounts.add_account(account_b).expect("add account");
        assert_eq!(add_response.switch_to, account_b_pk);

        h.with_remote(|remote, accounts, ndb| {
            let txn = Transaction::new(ndb).expect("txn");
            accounts.select_account(&account_b_pk, ndb, &txn, remote);
        });

        h.expect_inactive(
            account_a,
            relay_list,
            "switching away should deactivate relay list for A",
        );
        h.expect_inactive(
            account_a,
            giftwrap,
            "switching away should deactivate giftwrap for A",
        );

        h.expect_live(account_b_pk, relay_list, "relay list should be live for B");
        h.expect_live(account_b_pk, giftwrap, "giftwrap should be live for B");

        let giftwrap_declaration_b =
            selected_account_remote_declarations(h.accounts.get_selected_account())
                .into_iter()
                .find(|declaration| declaration.identity == giftwrap)
                .expect("giftwrap declaration for B");
        assert_eq!(
            giftwrap_declaration_b.config,
            make_giftwrap_remote_config(&account_b_pk),
            "giftwrap live sub should retarget when the selected account changes"
        );

        h.with_remote(|remote, accounts, ndb| {
            let txn = Transaction::new(ndb).expect("txn");
            accounts.select_account(&account_a, ndb, &txn, remote);
        });

        h.expect_live(account_a, relay_list, "relay list should restore for A");
        h.expect_live(account_a, giftwrap, "giftwrap should restore for A");
        h.expect_inactive(
            account_b_pk,
            relay_list,
            "switching back should deactivate relay list for B",
        );
        h.expect_inactive(
            account_b_pk,
            giftwrap,
            "switching back should deactivate giftwrap for B",
        );
        let giftwrap_declaration_a =
            selected_account_remote_declarations(h.accounts.get_selected_account())
                .into_iter()
                .find(|declaration| declaration.identity == giftwrap)
                .expect("giftwrap declaration for A");
        assert_eq!(
            giftwrap_declaration_a.config,
            make_giftwrap_remote_config(&account_a),
            "switching back should restore the original account's giftwrap target"
        );
    }

    #[test]
    fn remove_account_purges_retained_account_scoped_subs() {
        let mut h = AccountRemoteHarness::new();
        let account = FullKeypair::generate().to_keypair();
        let account_pk = account.pubkey;
        let add_response = h
            .accounts
            .add_account(account.clone())
            .expect("add account");
        assert_eq!(add_response.switch_to, account_pk);

        h.with_remote(|remote, accounts, ndb| {
            let txn = Transaction::new(ndb).expect("txn");
            accounts.select_account(&account_pk, ndb, &txn, remote);
        });

        let identity = ScopedSubIdentity::account(
            SubOwnerKey::new("tests/accounts/remove-account-purge"),
            SubKey::new(("remove-account-purge", 1u8)),
        );
        let filter = vec![Filter::new().kinds(vec![1]).limit(10).build()];
        let config = SubConfig::builder(filter).accounts_read_important().build();

        h.with_remote(|remote, accounts, _ndb| {
            let mut scoped_subs = remote.scoped_subs(accounts);
            let _ =
                scoped_subs.set_sub_for_account(account_pk, identity.owner, identity.key, config);
        });

        h.expect_live(
            account_pk,
            identity,
            "selected account-scoped sub should be live before account removal",
        );

        let removed = h
            .with_remote(|remote, accounts, ndb| accounts.remove_account(&account_pk, ndb, remote));
        assert!(removed);

        h.expect_missing(
            account_pk,
            identity,
            "account removal should remove live state for the deleted account",
        );

        let add_response = h.accounts.add_account(account).expect("re-add account");
        assert_eq!(add_response.switch_to, account_pk);
        h.with_remote(|remote, accounts, ndb| {
            let txn = Transaction::new(ndb).expect("txn");
            accounts.select_account(&account_pk, ndb, &txn, remote);
        });

        h.expect_missing(
            account_pk,
            identity,
            "deleted account scoped desired state must not restore after re-adding the same pubkey",
        );
        h.with_remote(|remote, accounts, _ndb| {
            let scoped_subs = remote.scoped_subs(accounts);
            assert_eq!(
                scoped_subs.sub_readiness(identity),
                ScopedSubReadiness::Missing
            );
        });
    }

    #[tokio::test]
    async fn selected_account_relay_action_retargets_existing_accountsread_remote_subs() {
        let mut h = AccountRemoteHarness::new();
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let selected = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let mute_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::MuteList);
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);

        h.expect_live(selected, relay_list, "relay list should start live");
        h.expect_live(selected, mute_list, "mute list should start live");
        h.expect_live(selected, contacts_list, "contacts list should start live");

        let relay_before = h.accounts.selected_account_read_relays();
        let new_relay =
            NormRelayUrl::new("wss://relay-account-retarget.example.com").expect("relay url");

        h.with_remote(|remote, accounts, ndb| {
            accounts.process_relay_action(ndb, remote, RelayAction::Add(new_relay.to_string()));
        });

        let relay_after = h.accounts.selected_account_read_relays();
        assert!(relay_after.contains(&new_relay));
        assert_ne!(relay_before, relay_after);

        h.expect_live(selected, relay_list, "relay list should remain live");
        h.expect_live(selected, mute_list, "mute list should remain live");
        h.expect_live(selected, contacts_list, "contacts list should remain live");
    }

    #[test]
    fn pubkey_only_relay_action_updates_local_advertised_without_ndb_event() {
        let mut h = AccountRemoteHarness::new();
        let selected = *h.accounts.selected_account_pubkey();
        assert!(h.accounts.selected_filled().is_none());

        let new_relay =
            NormRelayUrl::new("wss://relay-pubkey-only-local.example.com").expect("relay url");
        h.with_remote(|remote, accounts, ndb| {
            accounts.process_relay_action(ndb, remote, RelayAction::Add(new_relay.to_string()));
        });

        assert!(
            h.accounts
                .selected_account_advertised_relays()
                .iter()
                .any(|relay| relay.url == new_relay),
            "pubkey-only relay edits should retain master-compatible local advertised state"
        );
        assert!(
            selected_nip65_projection(&h.ndb, &selected).is_none(),
            "pubkey-only relay edits must not create a signed kind 10002 relay list"
        );
    }

    #[tokio::test]
    async fn update_skips_full_history_retarget_when_kind_10002_keeps_same_read_relays() {
        let mut h = AccountRemoteHarness::new();
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let selected_keypair = FullKeypair::generate().to_keypair();
        let selected = selected_keypair.pubkey;
        let add_response = h
            .accounts
            .add_account(selected_keypair)
            .expect("add selected account");
        assert_eq!(add_response.switch_to, selected);
        h.with_remote(|remote, accounts, ndb| {
            let txn = Transaction::new(ndb).expect("txn");
            accounts.select_account(&selected, ndb, &txn, remote);
        });

        let identity = ScopedSubIdentity::global(
            SubOwnerKey::new("tests/accounts/noop-relay-refresh"),
            SubKey::new(("full-history", "relay-refresh", 1u8)),
        );
        let filter = vec![Filter::new().kinds(vec![1]).limit(10).build()];
        let config = SubConfig::builder(filter.clone())
            .full_history(FullHistoryConfig::new(filter))
            .accounts_read_important()
            .build();
        h.with_remote(|remote, accounts, _ndb| {
            let mut scoped_subs = remote.scoped_subs(accounts);
            let _ = scoped_subs.ensure_sub(identity, config);
        });
        h.expect_live(
            selected,
            identity,
            "full-history scoped sub should start live",
        );

        let selected_secret = h
            .accounts
            .selected_filled()
            .expect("selected full keypair")
            .secret_key
            .secret_bytes();
        let relay_a = RelaySpec::new(
            NormRelayUrl::new("wss://relay-read.example.com").expect("read relay"),
            false,
            false,
        );
        let relay_a_read = RelaySpec::new(
            NormRelayUrl::new("wss://relay-read.example.com").expect("read relay"),
            true,
            false,
        );
        let relay_b = RelaySpec::new(
            NormRelayUrl::new("wss://relay-write.example.com").expect("write relay"),
            false,
            true,
        );

        let note_one = construct_nip65_relays_note([&relay_a, &relay_b])
            .created_at(1_700_000_100)
            .sign(&selected_secret)
            .build()
            .expect("first relay-list note");
        let note_one_json = note_one.json().expect("first relay-list note json");
        h.ndb
            .process_client_event(&note_one_json)
            .expect("ingest first relay-list note");
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let note_two = construct_nip65_relays_note([&relay_a_read, &relay_b])
            .created_at(1_700_000_101)
            .sign(&selected_secret)
            .build()
            .expect("second relay-list note");
        let note_two_json = note_two.json().expect("second relay-list note json");
        h.ndb
            .process_client_event(&note_two_json)
            .expect("ingest second relay-list note");

        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        h.expect_live(
            selected,
            identity,
            "same effective read-relay set should keep the scoped sub live",
        );
    }

    #[tokio::test]
    async fn duplicate_relay_action_add_skips_full_history_retarget() {
        let mut h = AccountRemoteHarness::new();
        h.with_remote(|remote, accounts, ndb| accounts.update(ndb, remote));

        let selected = *h.accounts.selected_account_pubkey();
        let identity = ScopedSubIdentity::global(
            SubOwnerKey::new("tests/accounts/duplicate-relay-add"),
            SubKey::new(("full-history", "relay-action", 2u8)),
        );
        let filter = vec![Filter::new().kinds(vec![1]).limit(10).build()];
        let config = SubConfig::builder(filter.clone())
            .full_history(FullHistoryConfig::new(filter))
            .accounts_read_important()
            .build();
        h.with_remote(|remote, accounts, _ndb| {
            let mut scoped_subs = remote.scoped_subs(accounts);
            let _ = scoped_subs.ensure_sub(identity, config);
        });
        h.expect_live(
            selected,
            identity,
            "full-history scoped sub should start live",
        );

        let new_relay =
            NormRelayUrl::new("wss://relay-duplicate-add.example.com").expect("relay url");
        h.with_remote(|remote, accounts, ndb| {
            accounts.process_relay_action(ndb, remote, RelayAction::Add(new_relay.to_string()));
        });

        h.with_remote(|remote, accounts, ndb| {
            accounts.process_relay_action(ndb, remote, RelayAction::Add(new_relay.to_string()));
        });

        h.expect_live(
            selected,
            identity,
            "duplicate relay add should keep the scoped sub live",
        );
    }
}
