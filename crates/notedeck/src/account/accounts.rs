use crate::account::cache::AccountCache;
use crate::account::contacts::Contacts;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{
    calculate_relays, modify_advertised_relays, write_relays, AccountRelayData, RelayAction,
    RelayDefaults,
};
use crate::scoped_subs::{RelaySelection, ScopedSubIdentity, SubConfig, SubKey};
use crate::storage::AccountStorageWriter;
use crate::user_account::UserAccountSerializable;
use crate::{
    AccountStorage, MuteFun, RemoteApi, ScopedSubApi, SingleUnkIdAction, SubOwnerKey, UnknownIds,
    UserAccount, ZapWallet,
};
use enostr::{FilledKeypair, Keypair, NormRelayUrl, Pubkey, RelayId, RelayRoutingPreference};
use hashbrown::HashSet;
use nostrdb::{Filter, Ndb, Note, Subscription, Transaction};

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

        let relay_defaults = RelayDefaults::new(forced_relays);

        let selected = cache.selected_mut();
        let selected_data = &mut selected.data;

        selected_data.query(ndb, txn);

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
            let old_pk = resp.deleted.pubkey;
            let txn = Transaction::new(ndb).expect("txn");
            self.select_account_internal(&swap_to, old_pk, ndb, &txn, remote);
        }

        {
            let mut scoped_subs = remote.scoped_subs(&*self);
            clear_account_remote_subs_for_account(&mut scoped_subs, resp.deleted.pubkey);
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
        self.select_account_internal_entry(pk_to_select, ndb, txn, remote);
    }

    fn select_account_internal_entry(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        remote: &mut RemoteApi<'_>,
    ) {
        let old_pk = *self.selected_account_pubkey();

        if !self.cache.select(*pk_to_select) {
            return;
        }
        if let Some(filled) = self.selected_filled() {
            ndb.add_key(&filled.secret_key.secret_bytes());
        }

        self.select_account_internal(pk_to_select, old_pk, ndb, txn, remote);
    }

    /// Have already selected in `AccountCache`, updating other things
    fn select_account_internal(
        &mut self,
        pk_to_select: &Pubkey,
        old_pk: Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        remote: &mut RemoteApi<'_>,
    ) {
        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.select_key(Some(*pk_to_select)) {
                tracing::error!("Could not select key {:?}: {e}", pk_to_select);
            }
        }

        self.get_selected_account_mut().data.query(ndb, txn);
        self.ndb_subs.swap_to(ndb, &self.cache.selected().data);

        remote.on_account_switched(old_pk, *pk_to_select, self);

        selected_account_request_subs(&mut remote.scoped_subs(self), self.get_selected_account());
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
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        let relay_updated = self
            .cache
            .selected_mut()
            .data
            .poll_for_updates(ndb, &self.ndb_subs);

        if !self.scoped_remote_initialized {
            selected_account_request_subs(
                &mut remote.scoped_subs(self),
                self.get_selected_account(),
            );
            self.scoped_remote_initialized = true;
            return;
        }

        if !relay_updated {
            return;
        }

        self.retarget_selected_account_read_relays(remote);
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    pub(crate) fn process_relay_action(&mut self, remote: &mut RemoteApi<'_>, action: RelayAction) {
        let acc = self.cache.selected_mut();
        modify_advertised_relays(
            &acc.key,
            action,
            remote,
            &self.relay_defaults,
            &mut acc.data,
        );

        self.retarget_selected_account_read_relays(remote);
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

    pub fn selected_account_write_relays(&self) -> Vec<RelayId> {
        write_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
        )
    }

    fn retarget_selected_account_read_relays(&mut self, remote: &mut RemoteApi<'_>) {
        remote.retarget_selected_account_read_relays(self);
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
    pub(super) fn poll_for_updates(&mut self, ndb: &Ndb, ndb_subs: &AccountNdbSubs) -> bool {
        let txn = Transaction::new(ndb).expect("txn");
        let relay_updated = self.relay.poll_for_updates(ndb, &txn, ndb_subs.relay_ndb);

        self.muted.poll_for_updates(ndb, &txn, ndb_subs.mute_ndb);
        self.contacts
            .poll_for_updates(ndb, &txn, ndb_subs.contacts_ndb);

        relay_updated
    }

    /// Note: query should be called as close to the subscription as possible
    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        self.relay.query(ndb, txn);
        self.muted.query(ndb, txn);
        self.contacts.query(ndb, txn);
    }
}

pub struct AddAccountResponse {
    pub switch_to: Pubkey,
    pub unk_id_action: SingleUnkIdAction,
}

fn giftwrap_filter(pk: &Pubkey) -> Filter {
    nostrdb::Filter::new()
        .kinds([1059])
        .pubkeys([pk.bytes()])
        .limit(500)
        .build()
}

fn account_remote_owner_key() -> SubOwnerKey {
    SubOwnerKey::new("core/accounts/remote-subs")
}

fn selected_account_request_subs(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    selected_account: &UserAccount,
) {
    let data = &selected_account.data;
    let owner = account_remote_owner_key();
    for kind in account_remote_sub_kinds() {
        let key = account_remote_sub_key(kind);
        let identity = ScopedSubIdentity::account(owner, key);
        match kind {
            AccountRemoteSubKind::RelayList => {
                let _ = scoped_subs.ensure_sub(
                    identity,
                    make_account_remote_config(
                        vec![data.relay.filter.clone()],
                        RelayRoutingPreference::default(),
                    ),
                );
            }
            AccountRemoteSubKind::MuteList => {
                let _ = scoped_subs.ensure_sub(
                    identity,
                    make_account_remote_config(
                        vec![data.muted.filter.clone()],
                        RelayRoutingPreference::default(),
                    ),
                );
            }
            AccountRemoteSubKind::ContactsList => {
                let _ = scoped_subs.ensure_sub(
                    identity,
                    make_account_remote_config(
                        vec![data.contacts.filter.clone()],
                        RelayRoutingPreference::RequireDedicated,
                    ),
                );
            }
            AccountRemoteSubKind::Giftwrap => {
                let pk = &selected_account.key.pubkey;
                scoped_subs.set_sub(
                    identity,
                    make_account_remote_config(
                        vec![giftwrap_filter(pk)],
                        RelayRoutingPreference::RequireDedicated,
                    ),
                );
            }
        };
    }
}

fn clear_account_remote_subs_for_account(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    account_pk: Pubkey,
) {
    let owner = account_remote_owner_key();
    for kind in account_remote_sub_kinds() {
        let key = account_remote_sub_key(kind);
        let identity = ScopedSubIdentity::account(owner, key);
        let _ = scoped_subs.clear_sub_for_account(account_pk, identity);
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AccountRemoteSubKind {
    RelayList,
    MuteList,
    ContactsList,
    Giftwrap,
}

fn account_remote_sub_kinds() -> [AccountRemoteSubKind; 4] {
    [
        AccountRemoteSubKind::RelayList,
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
    SubConfig {
        relays: RelaySelection::AccountsRead,
        filters,
        routing_preference,
    }
}
struct AccountNdbSubs {
    relay_ndb: Subscription,
    mute_ndb: Subscription,
    contacts_ndb: Subscription,
}

impl AccountNdbSubs {
    pub fn new(ndb: &mut Ndb, data: &AccountData) -> Self {
        let relay_ndb = ndb
            .subscribe(from_ref(&data.relay.filter))
            .expect("ndb relay list subscription");
        let mute_ndb = ndb
            .subscribe(from_ref(&data.muted.filter))
            .expect("ndb sub");
        let contacts_ndb = ndb
            .subscribe(from_ref(&data.contacts.filter))
            .expect("ndb sub");
        Self {
            relay_ndb,
            mute_ndb,
            contacts_ndb,
        }
    }

    pub fn swap_to(&mut self, ndb: &mut Ndb, new_selection_data: &AccountData) {
        let _ = ndb.unsubscribe(self.relay_ndb);
        let _ = ndb.unsubscribe(self.mute_ndb);
        let _ = ndb.unsubscribe(self.contacts_ndb);

        *self = AccountNdbSubs::new(ndb, new_selection_data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        test_utils::{live_id_with_selected_for_test, remote_for_test},
        EguiWakeup, ScopedSubEoseStatus, ScopedSubLiveEoseStatus, ScopedSubsState, FALLBACK_PUBKEY,
    };
    use enostr::{FullKeypair, OutboxPool, RelayUrlPkgs};
    use nostr_relay_builder::{LocalRelay, RelayBuilder};
    use nostrdb::Config;
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::TempDir;

    struct AccountRemoteHarness {
        _tmp: TempDir,
        ndb: Ndb,
        accounts: Accounts,
        scoped_sub_state: ScopedSubsState,
        pool: OutboxPool,
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
                FALLBACK_PUBKEY(),
                &mut ndb,
                &txn,
                &mut unknown_ids,
            );

            Self {
                _tmp: tmp,
                ndb,
                accounts,
                scoped_sub_state: ScopedSubsState::default(),
                pool: OutboxPool::default(),
            }
        }

        fn identity_for(kind: AccountRemoteSubKind) -> ScopedSubIdentity {
            ScopedSubIdentity::account(account_remote_owner_key(), account_remote_sub_key(kind))
        }

        fn live_id_for(
            &mut self,
            account_pk: Pubkey,
            identity: ScopedSubIdentity,
        ) -> Option<enostr::OutboxSubId> {
            live_id_with_selected_for_test(
                &mut self.scoped_sub_state,
                account_pk,
                identity.key,
                identity.scope,
            )
        }
    }

    fn filter_jsons(filters: &[Filter]) -> Vec<String> {
        filters
            .iter()
            .map(|filter| filter.json().expect("filter json"))
            .collect()
    }

    async fn pump_pool_until<F>(
        pool: &mut OutboxPool,
        max_attempts: usize,
        mut predicate: F,
    ) -> bool
    where
        F: FnMut(&mut OutboxPool) -> bool,
    {
        for _ in 0..max_attempts {
            pool.try_recv(10, |_| {});
            if predicate(pool) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        predicate(pool)
    }

    /// Saturates one relay to `max_subscriptions = 1`, then promotes a
    /// `NoPreference` subscription into the live compaction lane by first
    /// occupying the dedicated slot with a `PreferDedicated` request and then
    /// unsubscribing it.
    async fn install_active_compaction_lane(
        pool: &mut OutboxPool,
        relay: &NormRelayUrl,
    ) -> enostr::OutboxSubId {
        let relay_pkgs = |routing_preference| {
            RelayUrlPkgs::with_preference(HashSet::from([relay.clone()]), routing_preference)
        };

        let preferred_id = {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.subscribe(
                vec![Filter::new().kinds(vec![1]).limit(10).build()],
                relay_pkgs(RelayRoutingPreference::PreferDedicated),
            )
        };
        let applied = pool.apply_nip11_limits(
            relay,
            enostr::Nip11LimitationsRaw {
                max_subscriptions: Some(1),
                ..Default::default()
            },
            UNIX_EPOCH + Duration::from_secs(1_700_000_400),
        );
        assert!(matches!(
            applied,
            enostr::Nip11ApplyOutcome::Applied | enostr::Nip11ApplyOutcome::Unchanged
        ));

        let compaction_id = {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.subscribe(
                vec![Filter::new().kinds(vec![2]).limit(10).build()],
                relay_pkgs(RelayRoutingPreference::NoPreference),
            )
        };

        let preferred_ready = pump_pool_until(pool, 100, |pool| pool.has_eose(&preferred_id)).await;
        assert!(
            preferred_ready,
            "preferred baseline subscription should stay active while the fallback request waits"
        );
        assert!(
            !pool.has_eose(&compaction_id),
            "fallback request should stay queued until the preferred dedicated slot is released"
        );

        {
            let mut session = pool.start_session(EguiWakeup::new(egui::Context::default()));
            session.unsubscribe(preferred_id);
        }

        let compaction_ready =
            pump_pool_until(pool, 100, |pool| pool.has_eose(&compaction_id)).await;
        assert!(
            compaction_ready,
            "fallback request should become the active compaction route once the preferred slot is released"
        );
        assert!(
            !pool.status(&compaction_id).is_empty(),
            "active compaction route should expose one routed relay leg before account subscriptions are added"
        );

        compaction_id
    }

    #[test]
    fn update_initializes_selected_account_remote_subs_with_expected_routing() {
        let mut h = AccountRemoteHarness::new();
        {
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts.update(&mut h.ndb, &mut remote);
        }

        let selected = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let mute_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::MuteList);
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);
        let giftwrap = giftwrap_sub_identity();

        let _relay_list_id = h
            .live_id_for(selected, relay_list)
            .expect("relay list live id");
        let _mute_list_id = h
            .live_id_for(selected, mute_list)
            .expect("mute list live id");
        let _contacts_list_id = h
            .live_id_for(selected, contacts_list)
            .expect("contacts list live id");
        let giftwrap_id = h.live_id_for(selected, giftwrap).expect("giftwrap live id");

        let expected_giftwrap = vec![giftwrap_filter(&selected)];
        let stored_giftwrap = h.pool.filters(&giftwrap_id).expect("giftwrap filters");
        assert_eq!(
            filter_jsons(stored_giftwrap),
            filter_jsons(&expected_giftwrap),
            "giftwrap live sub should target the selected account's pubkey"
        );
    }

    #[test]
    fn account_switch_replaces_remote_subs_and_restores_them_on_switch_back() {
        let mut h = AccountRemoteHarness::new();
        {
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts.update(&mut h.ndb, &mut remote);
        }

        let account_a = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let giftwrap = giftwrap_sub_identity();
        let relay_a_id = h
            .live_id_for(account_a, relay_list)
            .expect("relay list for A");
        let giftwrap_a_id = h.live_id_for(account_a, giftwrap).expect("giftwrap for A");

        let account_b = FullKeypair::generate().to_keypair();
        let account_b_pk = account_b.pubkey;
        let add_response = h.accounts.add_account(account_b).expect("add account");
        assert_eq!(add_response.switch_to, account_b_pk);

        {
            let txn = Transaction::new(&h.ndb).expect("txn");
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts
                .select_account(&account_b_pk, &mut h.ndb, &txn, &mut remote);
        }

        assert!(
            h.live_id_for(account_a, relay_list).is_none()
                && h.live_id_for(account_a, giftwrap).is_none(),
            "switching away should unsubscribe the old account-scoped remote subs"
        );

        let relay_b_id = h
            .live_id_for(account_b_pk, relay_list)
            .expect("relay list for B");
        let giftwrap_b_id = h
            .live_id_for(account_b_pk, giftwrap)
            .expect("giftwrap for B");
        assert_ne!(relay_a_id, relay_b_id);
        assert_ne!(giftwrap_a_id, giftwrap_b_id);

        let stored_giftwrap_b = h
            .pool
            .filters(&giftwrap_b_id)
            .expect("giftwrap filters for B");
        assert_eq!(
            filter_jsons(stored_giftwrap_b),
            filter_jsons(&[giftwrap_filter(&account_b_pk)]),
            "giftwrap live sub should retarget when the selected account changes"
        );

        {
            let txn = Transaction::new(&h.ndb).expect("txn");
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts
                .select_account(&account_a, &mut h.ndb, &txn, &mut remote);
        }

        let restored_relay_a_id = h
            .live_id_for(account_a, relay_list)
            .expect("relay list restored for A");
        let restored_giftwrap_a_id = h
            .live_id_for(account_a, giftwrap)
            .expect("giftwrap restored for A");

        assert!(h.live_id_for(account_b_pk, relay_list).is_none());
        assert!(h.live_id_for(account_b_pk, giftwrap).is_none());
        assert_ne!(relay_a_id, restored_relay_a_id);
        assert_ne!(giftwrap_a_id, restored_giftwrap_a_id);
        assert_eq!(
            filter_jsons(
                h.pool
                    .filters(&restored_giftwrap_a_id)
                    .expect("giftwrap filters for A")
            ),
            filter_jsons(&[giftwrap_filter(&account_a)]),
            "switching back should restore the original account's giftwrap target"
        );
    }

    #[test]
    fn selected_account_relay_action_retargets_existing_accountsread_remote_subs() {
        let mut h = AccountRemoteHarness::new();
        {
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts.update(&mut h.ndb, &mut remote);
        }

        let selected = *h.accounts.selected_account_pubkey();
        let relay_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::RelayList);
        let mute_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::MuteList);
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);

        let relay_list_id = h
            .live_id_for(selected, relay_list)
            .expect("relay list live id");
        let mute_list_id = h
            .live_id_for(selected, mute_list)
            .expect("mute list live id");
        let contacts_list_id = h
            .live_id_for(selected, contacts_list)
            .expect("contacts list live id");

        let relay_before = h.accounts.selected_account_read_relays();
        let new_relay =
            NormRelayUrl::new("wss://relay-account-retarget.example.com").expect("relay url");

        {
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts
                .process_relay_action(&mut remote, RelayAction::Add(new_relay.to_string()));
        }

        let relay_after = h.accounts.selected_account_read_relays();
        assert!(relay_after.contains(&new_relay));
        assert_ne!(relay_before, relay_after);

        assert_eq!(h.live_id_for(selected, relay_list), Some(relay_list_id));
        assert_eq!(h.live_id_for(selected, mute_list), Some(mute_list_id));
        assert_eq!(
            h.live_id_for(selected, contacts_list),
            Some(contacts_list_id)
        );

        assert!(
            h.pool.filters(&relay_list_id).is_some()
                && h.pool.filters(&mute_list_id).is_some()
                && h.pool.filters(&contacts_list_id).is_some(),
            "retargeting should keep the existing live account-read subs active"
        );
    }

    /// Verifies that account-scoped `ContactsList`/`Giftwrap` subscriptions
    /// retain `RequireDedicated` routing under saturation by evicting a live
    /// non-preferred compaction leg instead of joining that shared route.
    #[tokio::test]
    async fn update_routes_contacts_and_giftwrap_as_required_dedicated_under_saturation() {
        let relay_task = LocalRelay::run(RelayBuilder::default())
            .await
            .expect("start local relay");
        let relay = NormRelayUrl::new(&relay_task.url()).expect("relay url");
        let mut h = AccountRemoteHarness::with_forced_relays(vec![relay.to_string()]);
        let compaction_id = install_active_compaction_lane(&mut h.pool, &relay).await;

        {
            let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
            h.accounts.update(&mut h.ndb, &mut remote);
        }

        let selected = *h.accounts.selected_account_pubkey();
        let contacts_list = AccountRemoteHarness::identity_for(AccountRemoteSubKind::ContactsList);
        let giftwrap = giftwrap_sub_identity();

        let contacts_list_id = h
            .live_id_for(selected, contacts_list)
            .expect("contacts list live id");
        let giftwrap_id = h.live_id_for(selected, giftwrap).expect("giftwrap live id");

        let contacts_routed = !h.pool.status(&contacts_list_id).is_empty();
        let giftwrap_routed = !h.pool.status(&giftwrap_id).is_empty();
        assert!(
            h.pool.status(&compaction_id).is_empty(),
            "a required account sub should evict the existing non-preferred compaction leg"
        );
        assert!(
            contacts_routed ^ giftwrap_routed,
            "with one dedicated slot, exactly one of contacts/giftwrap should be routed and the other should remain queued"
        );

        let mut remote = remote_for_test(&mut h.pool, &mut h.scoped_sub_state);
        let scoped_subs = remote.scoped_subs(&h.accounts);
        assert_eq!(
            scoped_subs.sub_eose_status(contacts_list),
            if contacts_routed {
                ScopedSubEoseStatus::Live(ScopedSubLiveEoseStatus {
                    tracked_relays: 1,
                    any_eose: false,
                    all_eosed: false,
                })
            } else {
                ScopedSubEoseStatus::Live(ScopedSubLiveEoseStatus {
                    tracked_relays: 0,
                    any_eose: false,
                    all_eosed: false,
                })
            }
        );
        assert_eq!(
            scoped_subs.sub_eose_status(giftwrap),
            if giftwrap_routed {
                ScopedSubEoseStatus::Live(ScopedSubLiveEoseStatus {
                    tracked_relays: 1,
                    any_eose: false,
                    all_eosed: false,
                })
            } else {
                ScopedSubEoseStatus::Live(ScopedSubLiveEoseStatus {
                    tracked_relays: 0,
                    any_eose: false,
                    all_eosed: false,
                })
            }
        );

        relay_task.shutdown();
    }
}
