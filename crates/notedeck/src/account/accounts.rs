use tracing::{debug, info};

use crate::account::cache::AccountCache;
use crate::account::mute::AccountMutedData;
use crate::account::relay::{AccountRelayData, RelayDefaults};
use crate::user_account::UserAccountSerializable;
use crate::{AccountStorage, MuteFun, RelaySpec, SingleUnkIdAction, UnknownIds, UserAccount};
use enostr::{ClientMessage, FilledKeypair, Keypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, Transaction};
use std::collections::BTreeSet;

// TODO: remove this
use std::sync::Arc;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    pub cache: AccountCache,
    key_store: Option<AccountStorage>,
    relay_defaults: RelayDefaults,
    needs_relay_config: bool,
}

impl Accounts {
    pub fn new(
        key_store: Option<AccountStorage>,
        forced_relays: Vec<String>,
        fallback: Pubkey,
        ndb: &Ndb,
        txn: &Transaction,
        unknown_ids: &mut UnknownIds,
    ) -> Self {
        let (mut cache, unknown_id) = AccountCache::new(UserAccount::new(
            Keypair::only_pubkey(fallback),
            AccountData {
                relay: AccountRelayData::new(ndb, txn, fallback.bytes()),
                muted: AccountMutedData::new(ndb, txn, fallback.bytes()),
            },
        ));

        unknown_id.process_action(unknown_ids, ndb, txn);

        if let Some(keystore) = &key_store {
            match keystore.get_accounts() {
                Ok(accounts) => {
                    for account in accounts {
                        add_account_from_storage(&mut cache, ndb, txn, account).process_action(
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
            if let Some(selected) = keystore.get_selected_key().ok().flatten() {
                cache.select(selected);
            }
        };

        let relay_defaults = RelayDefaults::new(forced_relays);

        Accounts {
            cache,
            key_store,
            relay_defaults,
            needs_relay_config: true,
        }
    }

    pub fn remove_account(&mut self, pk: &Pubkey) {
        let Some(removed) = self.cache.remove(pk) else {
            return;
        };

        if let Some(key_store) = &self.key_store {
            if let Err(e) = key_store.remove_key(&removed.key) {
                tracing::error!("Could not remove account {pk}: {e}");
            }
        }
    }

    pub fn needs_relay_config(&mut self) {
        self.needs_relay_config = true;
    }

    pub fn contains_full_kp(&self, pubkey: &enostr::Pubkey) -> bool {
        self.cache
            .get(pubkey)
            .is_some_and(|u| u.key.secret_key.is_some())
    }

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(
        &mut self,
        ndb: &Ndb,
        txn: &Transaction,
        kp: Keypair,
    ) -> Option<AddAccountResponse> {
        let acc = if let Some(acc) = self.cache.get_mut(&kp.pubkey) {
            if kp.secret_key.is_none() || acc.key.secret_key.is_some() {
                tracing::info!("Already have account, not adding");
                return None;
            }

            acc.key = kp.clone();
            AccType::Acc(&*acc)
        } else {
            let new_account_data = AccountData {
                relay: AccountRelayData::new(ndb, txn, kp.pubkey.bytes()),
                muted: AccountMutedData::new(ndb, txn, kp.pubkey.bytes()),
            };
            AccType::Entry(
                self.cache
                    .add(UserAccount::new(kp.clone(), new_account_data)),
            )
        };

        if let Some(key_store) = &self.key_store {
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

        let Some(key_store) = &self.key_store else {
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

    pub fn get_selected_account_mut(&mut self) -> &mut UserAccount {
        self.cache.selected_mut()
    }

    fn get_selected_account_data(&self) -> &AccountData {
        &self.cache.selected().data
    }

    fn get_selected_account_data_mut(&mut self) -> &mut AccountData {
        &mut self.cache.selected_mut().data
    }

    pub fn select_account(&mut self, pk: &Pubkey) {
        if !self.cache.select(*pk) {
            return;
        }

        if let Some(key_store) = &self.key_store {
            if let Err(e) = key_store.select_key(Some(*pk)) {
                tracing::error!("Could not select key {:?}: {e}", pk);
            }
        }
    }

    pub fn mutefun(&self) -> Box<MuteFun> {
        let account_data = self.get_selected_account_data();

        let muted = Arc::clone(&account_data.muted.muted);
        Box::new(move |note: &Note, thread: &[u8; 32]| muted.is_muted(note, thread))
    }

    pub fn send_initial_filters(&mut self, pool: &mut RelayPool, relay_url: &str) {
        for data in (&self.cache).into_iter().map(|(_, acc)| &acc.data) {
            // send the active account's relay list subscription
            if let Some(relay_subid) = &data.relay.subid {
                pool.send_to(
                    &ClientMessage::req(relay_subid.clone(), vec![data.relay.filter.clone()]),
                    relay_url,
                );
            }
            // send the active account's muted subscription
            if let Some(muted_subid) = &data.muted.subid {
                pool.send_to(
                    &ClientMessage::req(muted_subid.clone(), vec![data.muted.filter.clone()]),
                    relay_url,
                );
            }
        }
    }

    // Return accounts which have no account_data yet (added) and accounts
    // which have still data but are no longer in our account list (removed).
    fn delta_accounts(&self) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
        let mut added = Vec::new();
        for pubkey in (&self.cache).into_iter().map(|(pk, _)| pk.bytes()) {
            if !self.cache.contains(pubkey) {
                added.push(*pubkey);
            }
        }
        let mut removed = Vec::new();
        for (pubkey, _) in &self.cache {
            if self.cache.get_bytes(pubkey).is_none() {
                removed.push(**pubkey);
            }
        }
        (added, removed)
    }

    fn poll_for_updates(&mut self, ndb: &Ndb) -> bool {
        let mut changed = false;
        for (pubkey, data) in &mut self.cache.iter_mut().map(|(pk, a)| (pk, &mut a.data)) {
            if let Some(sub) = data.relay.sub {
                let nks = ndb.poll_for_notes(sub, 1);
                if !nks.is_empty() {
                    let txn = Transaction::new(ndb).expect("txn");
                    let relays = AccountRelayData::harvest_nip65_relays(ndb, &txn, &nks);
                    debug!("pubkey {}: updated relays {:?}", pubkey.hex(), relays);
                    data.relay.advertised = relays.into_iter().collect();
                    changed = true;
                }
            }
            if let Some(sub) = data.muted.sub {
                let nks = ndb.poll_for_notes(sub, 1);
                if !nks.is_empty() {
                    let txn = Transaction::new(ndb).expect("txn");
                    let muted = AccountMutedData::harvest_nip51_muted(ndb, &txn, &nks);
                    debug!("pubkey {}: updated muted {:?}", pubkey.hex(), muted);
                    data.muted.muted = Arc::new(muted);
                    changed = true;
                }
            }
        }
        changed
    }

    fn update_relay_configuration(
        &mut self,
        pool: &mut RelayPool,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        debug!(
            "updating relay configuration for currently selected {:?}",
            self.cache.selected().key.pubkey.hex()
        );

        // If forced relays are set use them only
        let mut desired_relays = self.relay_defaults.forced_relays.clone();

        // Compose the desired relay lists from the selected account
        if desired_relays.is_empty() {
            let data = self.get_selected_account_data_mut();
            desired_relays.extend(data.relay.local.iter().cloned());
            desired_relays.extend(data.relay.advertised.iter().cloned());
        }

        // If no relays are specified at this point use the bootstrap list
        if desired_relays.is_empty() {
            desired_relays = self.relay_defaults.bootstrap_relays.clone();
        }

        debug!("current relays: {:?}", pool.urls());
        debug!("desired relays: {:?}", desired_relays);

        let pool_specs = pool
            .urls()
            .iter()
            .map(|url| RelaySpec::new(url.clone(), false, false))
            .collect();
        let add: BTreeSet<RelaySpec> = desired_relays.difference(&pool_specs).cloned().collect();
        let mut sub: BTreeSet<RelaySpec> =
            pool_specs.difference(&desired_relays).cloned().collect();
        if !add.is_empty() {
            debug!("configuring added relays: {:?}", add);
            let _ = pool.add_urls(add.iter().map(|r| r.url.clone()).collect(), wakeup);
        }
        if !sub.is_empty() {
            // certain relays are persistent like the multicast relay,
            // although we should probably have a way to explicitly
            // disable it
            sub.remove(&RelaySpec::new("multicast", false, false));

            debug!("removing unwanted relays: {:?}", sub);
            pool.remove_urls(&sub.iter().map(|r| r.url.clone()).collect());
        }

        debug!("current relays: {:?}", pool.urls());
    }

    pub fn update(&mut self, ndb: &mut Ndb, pool: &mut RelayPool, ctx: &egui::Context) {
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        // On the initial update the relays need config even if nothing changes below
        let mut need_reconfig = self.needs_relay_config;

        let ctx2 = ctx.clone();
        let wakeup = move || {
            ctx2.request_repaint();
        };

        // Do we need to deactivate any existing account subs?

        let selected = self.cache.selected().key.pubkey;

        for (pk, account) in &mut self.cache.iter_mut() {
            if *pk == selected {
                continue;
            }

            let data = &mut account.data;
            // this account is not currently selected
            if data.relay.sub.is_some() {
                // this account has relay subs, deactivate them
                data.relay.deactivate(ndb, pool);
            }
            if data.muted.sub.is_some() {
                // this account has muted subs, deactivate them
                data.muted.deactivate(ndb, pool);
            }
        }

        // Were any accounts added or removed?
        let (added, removed) = self.delta_accounts();
        if !added.is_empty() || !removed.is_empty() {
            need_reconfig = true;
        }

        // Did any accounts receive updates (ie NIP-65 relay lists)
        need_reconfig = self.poll_for_updates(ndb) || need_reconfig;

        // If needed, update the relay configuration
        if need_reconfig {
            self.update_relay_configuration(pool, wakeup);
            self.needs_relay_config = false;
        }

        // Do we need to activate account subs?
        let data = self.get_selected_account_data_mut();
        if data.relay.sub.is_none() {
            // the currently selected account doesn't have relay subs, activate them
            data.relay.activate(ndb, pool);
        }
        if data.muted.sub.is_none() {
            // the currently selected account doesn't have muted subs, activate them
            data.muted.activate(ndb, pool);
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &Pubkey) -> Option<FilledKeypair<'a>> {
        self.cache.get(pubkey).and_then(|r| r.key.to_full())
    }

    fn modify_advertised_relays(
        &mut self,
        relay_url: &str,
        pool: &mut RelayPool,
        action: RelayAction,
    ) {
        let relay_url = AccountRelayData::canonicalize_url(relay_url);
        match action {
            RelayAction::Add => info!("add advertised relay \"{}\"", relay_url),
            RelayAction::Remove => info!("remove advertised relay \"{}\"", relay_url),
        }

        let selected = self.cache.selected_mut();
        let account_data = &mut selected.data;

        let advertised = &mut account_data.relay.advertised;
        if advertised.is_empty() {
            // If the selected account has no advertised relays,
            // initialize with the bootstrapping set.
            advertised.extend(self.relay_defaults.bootstrap_relays.iter().cloned());
        }
        match action {
            RelayAction::Add => {
                advertised.insert(RelaySpec::new(relay_url, false, false));
            }
            RelayAction::Remove => {
                advertised.remove(&RelaySpec::new(relay_url, false, false));
            }
        }
        self.needs_relay_config = true;

        // If we have the secret key publish the NIP-65 relay list
        if let Some(secretkey) = &selected.key.secret_key {
            account_data
                .relay
                .publish_nip65_relays(&secretkey.to_secret_bytes(), pool);
        }
    }

    pub fn add_advertised_relay(&mut self, relay_to_add: &str, pool: &mut RelayPool) {
        self.modify_advertised_relays(relay_to_add, pool, RelayAction::Add);
    }

    pub fn remove_advertised_relay(&mut self, relay_to_remove: &str, pool: &mut RelayPool) {
        self.modify_advertised_relays(relay_to_remove, pool, RelayAction::Remove);
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
    ndb: &Ndb,
    txn: &Transaction,
    user_account_serializable: UserAccountSerializable,
) -> SingleUnkIdAction {
    let Some(acc) = get_acc_from_storage(ndb, txn, user_account_serializable) else {
        return SingleUnkIdAction::NoAction;
    };

    let pk = acc.key.pubkey;
    cache.add(acc);

    SingleUnkIdAction::pubkey(pk)
}

fn get_acc_from_storage(
    ndb: &Ndb,
    txn: &Transaction,
    user_account_serializable: UserAccountSerializable,
) -> Option<UserAccount> {
    let keypair = user_account_serializable.key;
    let new_account_data = AccountData {
        relay: AccountRelayData::new(ndb, txn, keypair.pubkey.bytes()),
        muted: AccountMutedData::new(ndb, txn, keypair.pubkey.bytes()),
    };

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

enum RelayAction {
    Add,
    Remove,
}

pub struct AccountData {
    pub(crate) relay: AccountRelayData,
    pub(crate) muted: AccountMutedData,
}

pub struct AddAccountResponse {
    pub switch_to: Pubkey,
    pub unk_id_action: SingleUnkIdAction,
}
