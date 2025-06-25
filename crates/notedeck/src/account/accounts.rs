use tracing::{debug, error, info};

use crate::account::mute::AccountMutedData;
use crate::account::relay::{AccountRelayData, RelayDefaults};
use crate::{AccountStorage, MuteFun, RelaySpec, SingleUnkIdAction, UnknownIds, UserAccount};
use enostr::{ClientMessage, FilledKeypair, Keypair, Pubkey, RelayPool};
use nostrdb::{Ndb, Note, Transaction};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

// TODO: remove this
use std::sync::Arc;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: Option<AccountStorage>,
    account_data: BTreeMap<[u8; 32], AccountData>,
    relay_defaults: RelayDefaults,
    needs_relay_config: bool,
    fallback: Option<Pubkey>,
}

impl Accounts {
    pub fn new(key_store: Option<AccountStorage>, forced_relays: Vec<String>) -> Self {
        let accounts = match &key_store {
            Some(keystore) => match keystore.get_accounts() {
                Ok(k) => k,
                Err(e) => {
                    tracing::error!("could not get keys: {e}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        let currently_selected_account = if let Some(key_store) = &key_store {
            get_selected_index(&accounts, key_store)
        } else {
            None
        };

        let account_data = BTreeMap::new();

        let relay_defaults = RelayDefaults::new(forced_relays);

        Accounts {
            currently_selected_account,
            accounts,
            key_store,
            account_data,
            relay_defaults,
            needs_relay_config: true,
            fallback: None,
        }
    }

    pub fn get_accounts(&self) -> &Vec<UserAccount> {
        &self.accounts
    }

    pub fn get_account(&self, ind: usize) -> Option<&UserAccount> {
        self.accounts.get(ind)
    }

    pub fn get_account_mut(&mut self, ind: usize) -> Option<&mut UserAccount> {
        self.accounts.get_mut(ind)
    }

    pub fn find_account(&self, pk: &[u8; 32]) -> Option<&UserAccount> {
        self.accounts
            .iter()
            .find(|acc| acc.key.pubkey.bytes() == pk)
    }

    pub fn find_account_mut(&mut self, pk: &[u8; 32]) -> Option<&mut UserAccount> {
        self.accounts
            .iter_mut()
            .find(|acc| acc.key.pubkey.bytes() == pk)
    }

    pub fn with_fallback(&mut self, fallback: Pubkey) {
        self.fallback = Some(fallback);
    }

    pub fn remove_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            if let Some(key_store) = &self.key_store {
                if let Err(e) = key_store.remove_key(&account.key) {
                    tracing::error!("Could not remove account at index {index}: {e}");
                }
            }

            self.accounts.remove(index);

            if let Some(selected_index) = self.currently_selected_account {
                match selected_index.cmp(&index) {
                    Ordering::Greater => {
                        self.select_account(selected_index - 1);
                    }
                    Ordering::Equal => {
                        if self.accounts.is_empty() {
                            // If no accounts remain, clear the selection
                            self.clear_selected_account();
                        } else if index >= self.accounts.len() {
                            // If the removed account was the last one, select the new last account
                            self.select_account(self.accounts.len() - 1);
                        } else {
                            // Otherwise, select the account at the same position
                            self.select_account(index);
                        }
                    }
                    Ordering::Less => {}
                }
            }
        }
    }

    pub fn needs_relay_config(&mut self) {
        self.needs_relay_config = true;
    }

    fn contains_account(&self, pubkey: &[u8; 32]) -> Option<ContainsAccount> {
        for (index, account) in self.accounts.iter().enumerate() {
            let has_pubkey = account.key.pubkey.bytes() == pubkey;
            let has_nsec = account.key.secret_key.is_some();
            if has_pubkey {
                return Some(ContainsAccount { has_nsec, index });
            }
        }

        None
    }

    pub fn contains_full_kp(&self, pubkey: &enostr::Pubkey) -> bool {
        if let Some(contains) = self.contains_account(pubkey.bytes()) {
            contains.has_nsec
        } else {
            false
        }
    }

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(&mut self, key: Keypair) -> AddAccountAction {
        let pubkey = key.pubkey;
        let switch_to_index = if let Some(contains_acc) = self.contains_account(pubkey.bytes()) {
            if key.secret_key.is_some() && !contains_acc.has_nsec {
                info!(
                    "user provided nsec, but we already have npub {}. Upgrading to nsec",
                    pubkey
                );

                if let Some(key_store) = &self.key_store {
                    if let Err(e) = key_store.write_account(&UserAccount::new(key.clone())) {
                        tracing::error!("Could not add key for {:?}: {e}", key.pubkey);
                    }
                }

                self.accounts[contains_acc.index].key = key;
            } else {
                info!("already have account, not adding {}", pubkey);
            }
            contains_acc.index
        } else {
            info!("adding new account {}", pubkey);
            if let Some(key_store) = &self.key_store {
                if let Err(e) = key_store.write_account(&UserAccount::new(key.clone())) {
                    tracing::error!("Could not add key for {:?}: {e}", key.pubkey);
                }
            }
            self.accounts.push(UserAccount::new(key));
            self.accounts.len() - 1
        };

        let source: Option<usize> = None;
        AddAccountAction {
            accounts_action: Some(AccountsAction::Switch(SwitchAccountAction::new(
                source,
                switch_to_index,
            ))),
            unk_id_action: SingleUnkIdAction::pubkey(pubkey),
        }
    }

    /// Update the `UserAccount` via callback and save the result to disk.
    /// return true if the update was successful
    pub fn update_current_account(&mut self, update: impl FnOnce(&mut UserAccount)) -> bool {
        {
            let Some(cur_account) = self.get_selected_account_mut() else {
                return false;
            };

            update(cur_account);
        }

        let Some(cur_acc) = self.get_selected_account() else {
            return false;
        };

        let Some(key_store) = &self.key_store else {
            return false;
        };

        if let Err(err) = key_store.write_account(cur_acc) {
            tracing::error!("Could not add account {:?} to storage: {err}", cur_acc.key);
            return false;
        }

        true
    }

    pub fn num_accounts(&self) -> usize {
        self.accounts.len()
    }

    pub fn get_selected_account_index(&self) -> Option<usize> {
        self.currently_selected_account
    }

    pub fn selected_or_first_nsec(&self) -> Option<FilledKeypair<'_>> {
        self.get_selected_account()
            .and_then(|kp| kp.key.to_full())
            .or_else(|| self.accounts.iter().find_map(|a| a.key.to_full()))
    }

    /// Get the selected account's pubkey as bytes. Common operation so
    /// we make it a helper here.
    pub fn selected_account_pubkey_bytes(&self) -> Option<&[u8; 32]> {
        self.get_selected_account().map(|kp| kp.key.pubkey.bytes())
    }

    pub fn selected_account_pubkey(&self) -> Option<&Pubkey> {
        self.get_selected_account().map(|acc| &acc.key.pubkey)
    }

    pub fn get_selected_account(&self) -> Option<&UserAccount> {
        self.currently_selected_account
            .map(|i| self.get_account(i))?
    }

    pub fn selected_account_has_wallet(&self) -> bool {
        if let Some(acc) = self.get_selected_account() {
            return acc.wallet.is_some();
        }

        false
    }

    pub fn get_selected_account_mut(&mut self) -> Option<&mut UserAccount> {
        self.currently_selected_account
            .map(|i| self.get_account_mut(i))?
    }

    pub fn get_account_mut_optimized(&mut self, pk: &[u8; 32]) -> Option<&mut UserAccount> {
        if let Some(ind) = self.currently_selected_account {
            if ind < self.accounts.len() {
                return Some(&mut self.accounts[ind]);
            }
        }
        self.accounts
            .iter_mut()
            .find(|acc| acc.key.pubkey.bytes() == pk)
    }

    pub fn get_selected_account_data(&mut self) -> Option<&mut AccountData> {
        let account_pubkey = *self.selected_account_pubkey_bytes()?;
        self.account_data.get_mut(&account_pubkey)
    }

    pub fn select_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            self.currently_selected_account = Some(index);
            if let Some(key_store) = &self.key_store {
                if let Err(e) = key_store.select_key(Some(account.key.pubkey)) {
                    tracing::error!("Could not select key {:?}: {e}", account.key.pubkey);
                }
            }
        }
    }

    pub fn clear_selected_account(&mut self) {
        self.currently_selected_account = None;
        if let Some(key_store) = &self.key_store {
            if let Err(e) = key_store.select_key(None) {
                tracing::error!("Could not select None key: {e}");
            }
        }
    }

    pub fn mutefun(&self) -> Box<MuteFun> {
        if let Some(index) = self.currently_selected_account {
            if let Some(account) = self.accounts.get(index) {
                let pubkey = account.key.pubkey.bytes();
                if let Some(account_data) = self.account_data.get(pubkey) {
                    let muted = Arc::clone(&account_data.muted.muted);
                    return Box::new(move |note: &Note, thread: &[u8; 32]| {
                        muted.is_muted(note, thread)
                    });
                }
            }
        }
        Box::new(|_: &Note, _: &[u8; 32]| false)
    }

    pub fn send_initial_filters(&mut self, pool: &mut RelayPool, relay_url: &str) {
        for data in self.account_data.values() {
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
        for pubkey in self.accounts.iter().map(|a| a.key.pubkey.bytes()) {
            if !self.account_data.contains_key(pubkey) {
                added.push(*pubkey);
            }
        }
        let mut removed = Vec::new();
        for pubkey in self.account_data.keys() {
            if self.contains_account(pubkey).is_none() {
                removed.push(*pubkey);
            }
        }
        (added, removed)
    }

    fn handle_added_account(&mut self, ndb: &Ndb, pubkey: &[u8; 32]) {
        debug!("handle_added_account {}", hex::encode(pubkey));

        // Create the user account data
        let new_account_data = AccountData {
            relay: AccountRelayData::new(ndb, pubkey),
            muted: AccountMutedData::new(ndb, pubkey),
        };
        self.account_data.insert(*pubkey, new_account_data);
    }

    fn handle_removed_account(&mut self, pubkey: &[u8; 32]) {
        debug!("handle_removed_account {}", hex::encode(pubkey));
        // FIXME - we need to unsubscribe here
        self.account_data.remove(pubkey);
    }

    fn handle_no_accounts(&mut self, unknown_ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        if let Some(fallback) = self.fallback {
            self.add_account(Keypair::new(fallback, None))
                .process_action(unknown_ids, ndb, txn);
            self.select_account(self.num_accounts() - 1);
        }
    }

    fn poll_for_updates(&mut self, ndb: &Ndb) -> bool {
        let mut changed = false;
        for (pubkey, data) in &mut self.account_data {
            if let Some(sub) = data.relay.sub {
                let nks = ndb.poll_for_notes(sub, 1);
                if !nks.is_empty() {
                    let txn = Transaction::new(ndb).expect("txn");
                    let relays = AccountRelayData::harvest_nip65_relays(ndb, &txn, &nks);
                    debug!(
                        "pubkey {}: updated relays {:?}",
                        hex::encode(pubkey),
                        relays
                    );
                    data.relay.advertised = relays.into_iter().collect();
                    changed = true;
                }
            }
            if let Some(sub) = data.muted.sub {
                let nks = ndb.poll_for_notes(sub, 1);
                if !nks.is_empty() {
                    let txn = Transaction::new(ndb).expect("txn");
                    let muted = AccountMutedData::harvest_nip51_muted(ndb, &txn, &nks);
                    debug!("pubkey {}: updated muted {:?}", hex::encode(pubkey), muted);
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
            self.currently_selected_account
                .map(|i| hex::encode(self.accounts.get(i).unwrap().key.pubkey.bytes()))
        );

        // If forced relays are set use them only
        let mut desired_relays = self.relay_defaults.forced_relays.clone();

        // Compose the desired relay lists from the selected account
        if desired_relays.is_empty() {
            if let Some(data) = self.get_selected_account_data() {
                desired_relays.extend(data.relay.local.iter().cloned());
                desired_relays.extend(data.relay.advertised.iter().cloned());
            }
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

    pub fn update(
        &mut self,
        ndb: &mut Ndb,
        pool: &mut RelayPool,
        ctx: &egui::Context,
        unknown_ids: &mut UnknownIds,
    ) {
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        // On the initial update the relays need config even if nothing changes below
        let mut need_reconfig = self.needs_relay_config;

        let ctx2 = ctx.clone();
        let wakeup = move || {
            ctx2.request_repaint();
        };

        // Do we need to deactivate any existing account subs?
        for (ndx, account) in self.accounts.iter().enumerate() {
            if Some(ndx) != self.currently_selected_account {
                // this account is not currently selected
                if let Some(data) = self.account_data.get_mut(account.key.pubkey.bytes()) {
                    if data.relay.sub.is_some() {
                        // this account has relay subs, deactivate them
                        data.relay.deactivate(ndb, pool);
                    }
                    if data.muted.sub.is_some() {
                        // this account has muted subs, deactivate them
                        data.muted.deactivate(ndb, pool);
                    }
                }
            }
        }

        // Were any accounts added or removed?
        let (added, removed) = self.delta_accounts();
        for pk in added {
            self.handle_added_account(ndb, &pk);
            need_reconfig = true;
        }
        for pk in removed {
            self.handle_removed_account(&pk);
            need_reconfig = true;
        }

        if self.accounts.is_empty() {
            let txn = Transaction::new(ndb).unwrap();
            self.handle_no_accounts(unknown_ids, ndb, &txn);
        }
        // Did any accounts receive updates (ie NIP-65 relay lists)
        need_reconfig = self.poll_for_updates(ndb) || need_reconfig;

        // If needed, update the relay configuration
        if need_reconfig {
            self.update_relay_configuration(pool, wakeup);
            self.needs_relay_config = false;
        }

        // Do we need to activate account subs?
        if let Some(data) = self.get_selected_account_data() {
            if data.relay.sub.is_none() {
                // the currently selected account doesn't have relay subs, activate them
                data.relay.activate(ndb, pool);
            }
            if data.muted.sub.is_none() {
                // the currently selected account doesn't have muted subs, activate them
                data.muted.activate(ndb, pool);
            }
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &[u8; 32]) -> Option<FilledKeypair<'a>> {
        if let Some(contains) = self.contains_account(pubkey) {
            if contains.has_nsec {
                if let Some(kp) = self.get_account(contains.index) {
                    return kp.key.to_full();
                }
            }
        }

        None
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
        match self.currently_selected_account {
            None => error!("no account is currently selected."),
            Some(index) => match self.accounts.get(index) {
                None => error!("selected account index {} is out of range.", index),
                Some(keypair) => {
                    let key_bytes: [u8; 32] = *keypair.key.pubkey.bytes();
                    match self.account_data.get_mut(&key_bytes) {
                        None => error!("no account data found for the provided key."),
                        Some(account_data) => {
                            let advertised = &mut account_data.relay.advertised;
                            if advertised.is_empty() {
                                // If the selected account has no advertised relays,
                                // initialize with the bootstrapping set.
                                advertised
                                    .extend(self.relay_defaults.bootstrap_relays.iter().cloned());
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
                            if let Some(secretkey) = &keypair.key.secret_key {
                                account_data
                                    .relay
                                    .publish_nip65_relays(&secretkey.to_secret_bytes(), pool);
                            }
                        }
                    }
                }
            },
        }
    }

    pub fn add_advertised_relay(&mut self, relay_to_add: &str, pool: &mut RelayPool) {
        self.modify_advertised_relays(relay_to_add, pool, RelayAction::Add);
    }

    pub fn remove_advertised_relay(&mut self, relay_to_remove: &str, pool: &mut RelayPool) {
        self.modify_advertised_relays(relay_to_remove, pool, RelayAction::Remove);
    }
}

enum RelayAction {
    Add,
    Remove,
}

fn get_selected_index(accounts: &[UserAccount], keystore: &AccountStorage) -> Option<usize> {
    match keystore.get_selected_key() {
        Ok(Some(pubkey)) => {
            return accounts
                .iter()
                .position(|account| account.key.pubkey == pubkey);
        }
        Ok(None) => {}
        Err(e) => error!("Error getting selected key: {}", e),
    };

    None
}

impl AddAccountAction {
    // Simple wrapper around processing the unknown action to expose too
    // much internal logic. This allows us to have a must_use on our
    // LoginAction type, otherwise the SingleUnkIdAction's must_use will
    // be lost when returned in the login action
    pub fn process_action(&mut self, ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        self.unk_id_action.process_action(ids, ndb, txn);
    }
}

#[derive(Debug, Clone)]
pub struct SwitchAccountAction {
    /// Some index representing the source of the action
    pub source: Option<usize>,

    /// The account index to switch to
    pub switch_to: usize,
}

impl SwitchAccountAction {
    pub fn new(source: Option<usize>, switch_to: usize) -> Self {
        SwitchAccountAction { source, switch_to }
    }
}

#[derive(Debug)]
pub enum AccountsAction {
    Switch(SwitchAccountAction),
    Remove(usize),
}

#[derive(Default)]
pub struct ContainsAccount {
    pub has_nsec: bool,
    pub index: usize,
}

#[must_use = "You must call process_login_action on this to handle unknown ids"]
pub struct AddAccountAction {
    pub accounts_action: Option<AccountsAction>,
    pub unk_id_action: SingleUnkIdAction,
}

pub struct AccountData {
    pub(crate) relay: AccountRelayData,
    pub(crate) muted: AccountMutedData,
}
