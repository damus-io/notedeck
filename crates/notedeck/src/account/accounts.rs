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
use enostr::{FilledKeypair, Keypair, NormRelayUrl, Pubkey, RelayId};
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

        self.ensure_selected_account_remote_subs(remote);
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
            self.ensure_selected_account_remote_subs(remote);
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

    fn ensure_selected_account_remote_subs(&mut self, remote: &mut RemoteApi<'_>) {
        {
            let mut scoped_subs = remote.scoped_subs(&*self);
            ensure_selected_account_remote_subs_api(&mut scoped_subs, self);
        }
        self.scoped_remote_initialized = true;
    }

    fn retarget_selected_account_read_relays(&mut self, remote: &mut RemoteApi<'_>) {
        remote.retarget_selected_account_read_relays(self);
        self.scoped_remote_initialized = true;
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
    // TODO: since optimize
    nostrdb::Filter::new()
        .kinds([1059])
        .pubkeys([pk.bytes()])
        .build()
}

fn account_remote_owner_key() -> SubOwnerKey {
    SubOwnerKey::new("core/accounts/remote-subs")
}

fn ensure_selected_account_remote_subs_api(
    scoped_subs: &mut ScopedSubApi<'_, '_>,
    accounts: &Accounts,
) {
    let owner = account_remote_owner_key();
    for kind in account_remote_sub_kinds() {
        let key = account_remote_sub_key(kind);
        let identity = ScopedSubIdentity::account(owner, key);
        let config = selected_account_remote_config(accounts, kind);
        let _ = scoped_subs.ensure_sub(identity, config);
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

fn make_account_remote_config(filters: Vec<Filter>, use_transparent: bool) -> SubConfig {
    SubConfig {
        relays: RelaySelection::AccountsRead,
        filters,
        use_transparent,
    }
}

fn selected_account_remote_config(accounts: &Accounts, kind: AccountRemoteSubKind) -> SubConfig {
    let selected = accounts.get_selected_account_data();
    match kind {
        AccountRemoteSubKind::RelayList => {
            make_account_remote_config(vec![selected.relay.filter.clone()], false)
        }
        AccountRemoteSubKind::MuteList => {
            make_account_remote_config(vec![selected.muted.filter.clone()], false)
        }
        AccountRemoteSubKind::ContactsList => {
            make_account_remote_config(vec![selected.contacts.filter.clone()], true)
        }
        AccountRemoteSubKind::Giftwrap => make_account_remote_config(
            vec![giftwrap_filter(accounts.selected_account_pubkey())],
            false,
        ),
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
