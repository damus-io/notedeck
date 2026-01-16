use uuid::Uuid;

use crate::account::cache::AccountCache;
use crate::account::contacts::Contacts;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{
    modify_advertised_relays, update_relay_configuration, AccountRelayData, RelayAction,
    RelayDefaults,
};
use crate::storage::AccountStorageWriter;
use crate::user_account::UserAccountSerializable;
use crate::{
    AccountStorage, MuteFun, SingleUnkIdAction, UnifiedSubscription, UnknownIds, UserAccount,
    ZapWallet,
};
use enostr::{ClientMessage, FilledKeypair, Keypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, Transaction};

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
        pool: &mut RelayPool,
        ctx: &egui::Context,
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
                create_wakeup(ctx),
            )
        };

        Accounts {
            cache,
            storage_writer,
            relay_defaults,
            subs,
        }
    }

    pub fn remove_account(
        &mut self,
        pk: &Pubkey,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        ctx: &egui::Context,
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
            self.select_account_internal(&swap_to, ndb, &txn, pool, ctx);
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

    /// Get the relay URLs configured for the selected account.
    /// Returns both local and advertised (NIP-65) relays, or bootstrap relays if none configured.
    pub fn get_selected_account_relay_urls(&self) -> Vec<String> {
        let account_data = self.get_selected_account_data();
        let relay_data = &account_data.relay;

        // Collect all configured relays (local + advertised)
        let mut urls: Vec<String> = relay_data
            .local
            .iter()
            .chain(relay_data.advertised.iter())
            .map(|spec| spec.url.clone())
            .collect();

        // If no relays configured, use bootstrap relays
        if urls.is_empty() {
            urls = self
                .relay_defaults
                .bootstrap_relays
                .iter()
                .map(|spec| spec.url.clone())
                .collect();
        }

        urls
    }

    pub fn select_account(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut RelayPool,
        ctx: &egui::Context,
    ) {
        if !self.cache.select(*pk_to_select) {
            return;
        }

        self.select_account_internal(pk_to_select, ndb, txn, pool, ctx);
    }

    /// Have already selected in `AccountCache`, updating other things
    fn select_account_internal(
        &mut self,
        pk_to_select: &Pubkey,
        ndb: &mut Ndb,
        txn: &Transaction,
        pool: &mut RelayPool,
        ctx: &egui::Context,
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
            create_wakeup(ctx),
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

    pub fn send_initial_filters(&mut self, pool: &mut RelayPool, relay_url: &str) {
        let data = &self.get_selected_account().data;
        // send the active account's relay list subscription
        pool.send_to(
            &ClientMessage::req(
                self.subs.relay.remote.clone(),
                vec![data.relay.filter.clone()],
            ),
            relay_url,
        );
        // send the active account's muted subscription
        pool.send_to(
            &ClientMessage::req(
                self.subs.mute.remote.clone(),
                vec![data.muted.filter.clone()],
            ),
            relay_url,
        );
        pool.send_to(
            &ClientMessage::req(
                self.subs.contacts.remote.clone(),
                vec![data.contacts.filter.clone()],
            ),
            relay_url,
        );
        if let Some(cur_pk) = self.selected_filled().map(|s| s.pubkey) {
            let giftwraps_filter = nostrdb::Filter::new()
                .kinds([1059])
                .pubkeys([cur_pk.bytes()])
                .build();
            pool.send_to(
                &ClientMessage::req(self.subs.giftwraps.remote.clone(), vec![giftwraps_filter]),
                relay_url,
            );
        }
    }

    pub fn update(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, ctx: &egui::Context) {
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
                update_relay_configuration(
                    pool,
                    &self.relay_defaults,
                    &acc.key.pubkey,
                    &acc.data.relay,
                    create_wakeup(ctx),
                );
            }
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    pub fn process_relay_action(
        &mut self,
        ctx: &egui::Context,
        pool: &mut RelayPool,
        action: RelayAction,
    ) {
        let acc = self.cache.selected_mut();
        modify_advertised_relays(&acc.key, action, pool, &self.relay_defaults, &mut acc.data);

        update_relay_configuration(
            pool,
            &self.relay_defaults,
            &acc.key.pubkey,
            &acc.data.relay,
            create_wakeup(ctx),
        );
    }

    pub fn get_subs(&self) -> &AccountSubs {
        &self.subs
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

fn create_wakeup(ctx: &egui::Context) -> impl Fn() + Send + Sync + Clone + 'static {
    let ctx = ctx.clone();
    move || {
        ctx.request_repaint();
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
        if self.relay.poll_for_updates(ndb, &txn, subs.relay.local) {
            resp = Some(AccountDataUpdate::Relay);
        }

        self.muted.poll_for_updates(ndb, &txn, subs.mute.local);
        self.contacts
            .poll_for_updates(ndb, &txn, subs.contacts.local);

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
    relay: UnifiedSubscription,
    giftwraps: UnifiedSubscription,
    mute: UnifiedSubscription,
    pub contacts: UnifiedSubscription,
}

impl AccountSubs {
    pub(super) fn new(
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        data: &AccountData,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Self {
        // TODO: since optimize
        let giftwraps_filter = nostrdb::Filter::new()
            .kinds([1059])
            .pubkeys([pk.bytes()])
            .build();

        update_relay_configuration(pool, relay_defaults, pk, &data.relay, wakeup);

        let relay = subscribe(ndb, pool, &data.relay.filter);
        let giftwraps = subscribe(ndb, pool, &giftwraps_filter);
        let mute = subscribe(ndb, pool, &data.muted.filter);
        let contacts = subscribe(ndb, pool, &data.contacts.filter);

        Self {
            relay,
            mute,
            contacts,
            giftwraps,
        }
    }

    pub(super) fn swap_to(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        relay_defaults: &RelayDefaults,
        pk: &Pubkey,
        new_selection_data: &AccountData,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        unsubscribe(ndb, pool, &self.relay);
        unsubscribe(ndb, pool, &self.mute);
        unsubscribe(ndb, pool, &self.contacts);
        unsubscribe(ndb, pool, &self.giftwraps);

        *self = AccountSubs::new(ndb, pool, relay_defaults, pk, new_selection_data, wakeup);
    }
}

fn subscribe(ndb: &Ndb, pool: &mut RelayPool, filter: &nostrdb::Filter) -> UnifiedSubscription {
    let filters = vec![filter.clone()];
    let sub = ndb
        .subscribe(&filters)
        .expect("ndb relay list subscription");

    // remote subscription
    let subid = Uuid::new_v4().to_string();
    pool.subscribe(subid.clone(), filters);

    UnifiedSubscription {
        local: sub,
        remote: subid,
    }
}

fn unsubscribe(ndb: &mut Ndb, pool: &mut RelayPool, sub: &UnifiedSubscription) {
    pool.unsubscribe(sub.remote.clone());

    // local subscription
    ndb.unsubscribe(sub.local)
        .expect("ndb relay list unsubscribe");
}
