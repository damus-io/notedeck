use crate::account::cache::AccountCache;
use crate::account::contacts::Contacts;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{
    calculate_relays, modify_advertised_relays, write_relays, AccountRelayData, RelayAction,
    RelayDefaults,
};
use crate::storage::AccountStorageWriter;
use crate::user_account::UserAccountSerializable;
use crate::{
    AccountStorage, MuteFun, Outbox, SingleUnkIdAction, UnknownIds, UserAccount, ZapWallet,
};
use enostr::{FilledKeypair, Keypair, NormRelayUrl, OutboxSubId, Pubkey, RelayId, RelayUrlPkgs};
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
    subs: AccountSubs,
}

impl Accounts {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key_store: Option<AccountStorage>,
        forced_relays: Vec<String>,
        fallback: Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut Outbox,
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

        let subs = {
            AccountSubs::new(
                ndb,
                pool,
                &relay_defaults,
                &selected.key.pubkey,
                selected_data,
            )
        };

        Accounts {
            cache,
            storage_writer,
            relay_defaults,
            subs,
        }
    }

    pub fn remove_account(&mut self, pk: &Pubkey, ndb: &mut Ndb, pool: &mut Outbox) -> bool {
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
            self.select_account_internal(&swap_to, ndb, &txn, pool);
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

    pub fn select_account(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut Outbox,
    ) {
        if !self.cache.select(*pk_to_select) {
            return;
        }

        self.select_account_internal(pk_to_select, ndb, txn, pool);
    }

    /// Have already selected in `AccountCache`, updating other things
    fn select_account_internal(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut Outbox,
    ) {
        if let Some(key_store) = &self.storage_writer {
            if let Err(e) = key_store.select_key(Some(*pk_to_select)) {
                tracing::error!("Could not select key {:?}: {e}", pk_to_select);
            }
        }

        self.get_selected_account_mut().data.query(ndb, txn);
        self.subs.swap_to(
            ndb,
            pool,
            &self.relay_defaults,
            pk_to_select,
            &self.cache.selected().data,
        );
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

    pub fn update(&mut self, ndb: &mut Ndb, pool: &mut Outbox) {
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        let Some(update) = self
            .cache
            .selected_mut()
            .data
            .poll_for_updates(ndb, &self.subs)
        else {
            return;
        };

        match update {
            // If needed, update the relay configuration
            AccountDataUpdate::Relay => {
                let acc = self.cache.selected();
                self.subs.update_subs_with_current_relays(
                    pool,
                    &self.relay_defaults,
                    &acc.data.relay,
                );
            }
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    pub fn process_relay_action(&mut self, pool: &mut Outbox, action: RelayAction) {
        let acc = self.cache.selected_mut();
        modify_advertised_relays(&acc.key, action, pool, &self.relay_defaults, &mut acc.data);

        self.subs
            .update_subs_with_current_relays(pool, &self.relay_defaults, &acc.data.relay);
    }

    pub fn get_subs(&self) -> &AccountSubs {
        &self.subs
    }

    pub fn selected_account_read_relays(&self) -> HashSet<NormRelayUrl> {
        calculate_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
            true,
        )
    }

    pub fn selected_account_write_relays(&self) -> Vec<RelayId> {
        write_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
        )
    }

    pub fn selected_account_write_relay_urls(&self) -> HashSet<NormRelayUrl> {
        calculate_relays(
            &self.relay_defaults,
            &self.get_selected_account_data().relay,
            false,
        )
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

    pub(super) fn poll_for_updates(
        &mut self,
        ndb: &Ndb,
        subs: &AccountSubs,
    ) -> Option<AccountDataUpdate> {
        let txn = Transaction::new(ndb).expect("txn");
        let mut resp = None;
        if self
            .relay
            .poll_for_updates(ndb, &txn, subs.ndb_subs.relay_ndb)
        {
            resp = Some(AccountDataUpdate::Relay);
        }

        self.muted
            .poll_for_updates(ndb, &txn, subs.ndb_subs.mute_ndb);
        self.contacts
            .poll_for_updates(ndb, &txn, subs.ndb_subs.contacts_ndb);

        resp
    }

    /// Note: query should be called as close to the subscription as possible
    pub(super) fn query(&mut self, ndb: &Ndb, txn: &Transaction) {
        self.relay.query(ndb, txn);
        self.muted.query(ndb, txn);
        self.contacts.query(ndb, txn);
    }
}

pub(super) enum AccountDataUpdate {
    Relay,
}

pub struct AddAccountResponse {
    pub switch_to: Pubkey,
    pub unk_id_action: SingleUnkIdAction,
}

pub struct AccountSubs {
    ndb_subs: AccountNdbSubs,
    relay_remote: OutboxSubId,
    giftwrap_remote: OutboxSubId,
    mute_remote: OutboxSubId,
    pub contacts_remote: OutboxSubId,
}

impl AccountSubs {
    pub(super) fn new(
        ndb: &mut Ndb,
        pool: &mut Outbox,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        data: &AccountData,
    ) -> Self {
        let relays = calculate_relays(relay_defaults, &data.relay, true);

        let relay_remote = pool.subscribe(
            vec![data.relay.filter.clone()],
            RelayUrlPkgs::new(relays.clone()),
        );

        let giftwrap_remote =
            pool.subscribe(vec![giftwrap_filter(pk)], RelayUrlPkgs::new(relays.clone()));

        let mute_remote = pool.subscribe(
            vec![data.muted.filter.clone()],
            RelayUrlPkgs::new(relays.clone()),
        );

        let contacts_remote = pool.subscribe(
            vec![data.contacts.filter.clone()],
            RelayUrlPkgs {
                urls: relays,
                use_transparent: true, // contacts must get EOSE asap
            },
        );

        let ndb_subs = AccountNdbSubs::new(ndb, data);
        Self {
            ndb_subs,
            relay_remote,
            giftwrap_remote,
            mute_remote,
            contacts_remote,
        }
    }

    pub(super) fn swap_to(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut Outbox,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        new_selection_data: &AccountData,
    ) {
        self.ndb_subs.swap_to(ndb, new_selection_data);
        self.resub_remote(pool, relay_defaults, pk, new_selection_data);
    }

    fn resub_remote(
        &mut self,
        pool: &mut Outbox,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        new_selection_data: &AccountData,
    ) {
        let relays = calculate_relays(relay_defaults, &new_selection_data.relay, true);

        pool.unsubscribe(self.relay_remote);
        self.relay_remote = pool.subscribe(
            vec![new_selection_data.relay.filter.clone()],
            RelayUrlPkgs::new(relays.clone()),
        );

        pool.unsubscribe(self.mute_remote);
        self.mute_remote = pool.subscribe(
            vec![new_selection_data.muted.filter.clone()],
            RelayUrlPkgs::new(relays.clone()),
        );

        pool.unsubscribe(self.giftwrap_remote);
        self.giftwrap_remote =
            pool.subscribe(vec![giftwrap_filter(pk)], RelayUrlPkgs::new(relays.clone()));

        pool.unsubscribe(self.contacts_remote);
        self.contacts_remote = pool.subscribe(
            vec![new_selection_data.contacts.filter.clone()],
            RelayUrlPkgs {
                urls: relays,
                use_transparent: true, // contacts must get EOSE asap
            },
        );
    }

    fn update_subs_with_current_relays(
        &mut self,
        pool: &mut Outbox,
        relay_defaults: &RelayDefaults,
        new_selection_data: &AccountRelayData,
    ) {
        let relays = calculate_relays(relay_defaults, new_selection_data, true);

        pool.modify_relays(self.relay_remote, relays.clone());
        pool.modify_relays(self.mute_remote, relays.clone());
        pool.modify_relays(self.giftwrap_remote, relays.clone());
        pool.modify_relays(self.contacts_remote, relays);
    }
}

fn giftwrap_filter(pk: &Pubkey) -> Filter {
    // TODO: since optimize
    nostrdb::Filter::new()
        .kinds([1059])
        .pubkeys([pk.bytes()])
        .build()
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
