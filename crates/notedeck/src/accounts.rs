use tracing::{debug, error, info};

use crate::{
    KeyStorageResponse, KeyStorageType, MuteFun, Muted, RelaySpec, SingleUnkIdAction, UnknownIds,
    UserAccount,
};
use enostr::{ClientMessage, FilledKeypair, Keypair, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteBuilder, NoteKey, Subscription, Transaction};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;
use uuid::Uuid;

// TODO: remove this
use std::sync::Arc;

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

pub struct AccountRelayData {
    filter: Filter,
    subid: String,
    sub: Option<Subscription>,
    local: BTreeSet<RelaySpec>,      // used locally but not advertised
    advertised: BTreeSet<RelaySpec>, // advertised via NIP-65
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

impl AccountRelayData {
    pub fn new(ndb: &Ndb, pool: &mut RelayPool, pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-65 relay list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10002])
            .limit(1)
            .build();

        // Local ndb subscription
        let ndbsub = ndb
            .subscribe(&[filter.clone()])
            .expect("ndb relay list subscription");

        // Query the ndb immediately to see if the user list is already there
        let txn = Transaction::new(ndb).expect("transaction");
        let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(&txn, &[filter.clone()], lim)
            .expect("query user relays results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let relays = Self::harvest_nip65_relays(ndb, &txn, &nks);
        debug!(
            "pubkey {}: initial relays {:?}",
            hex::encode(pubkey),
            relays
        );

        // Id for future remote relay subscriptions
        let subid = Uuid::new_v4().to_string();

        // Add remote subscription to existing relays
        pool.subscribe(subid.clone(), vec![filter.clone()]);

        AccountRelayData {
            filter,
            subid,
            sub: Some(ndbsub),
            local: BTreeSet::new(),
            advertised: relays.into_iter().collect(),
        }
    }

    // standardize the format (ie, trailing slashes) to avoid dups
    pub fn canonicalize_url(url: &str) -> String {
        match Url::parse(url) {
            Ok(parsed_url) => parsed_url.to_string(),
            Err(_) => url.to_owned(), // If parsing fails, return the original URL.
        }
    }

    fn harvest_nip65_relays(ndb: &Ndb, txn: &Transaction, nks: &[NoteKey]) -> Vec<RelaySpec> {
        let mut relays = Vec::new();
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                for tag in note.tags() {
                    match tag.get(0).and_then(|t| t.variant().str()) {
                        Some("r") => {
                            if let Some(url) = tag.get(1).and_then(|f| f.variant().str()) {
                                let has_read_marker = tag
                                    .get(2)
                                    .is_some_and(|m| m.variant().str() == Some("read"));
                                let has_write_marker = tag
                                    .get(2)
                                    .is_some_and(|m| m.variant().str() == Some("write"));
                                relays.push(RelaySpec::new(
                                    Self::canonicalize_url(url),
                                    has_read_marker,
                                    has_write_marker,
                                ));
                            }
                        }
                        Some("alt") => {
                            // ignore for now
                        }
                        Some(x) => {
                            error!("harvest_nip65_relays: unexpected tag type: {}", x);
                        }
                        None => {
                            error!("harvest_nip65_relays: invalid tag");
                        }
                    }
                }
            }
        }
        relays
    }

    pub fn publish_nip65_relays(&self, seckey: &[u8; 32], pool: &mut RelayPool) {
        let mut builder = NoteBuilder::new().kind(10002).content("");
        for rs in &self.advertised {
            builder = builder.start_tag().tag_str("r").tag_str(&rs.url);
            if rs.has_read_marker {
                builder = builder.tag_str("read");
            } else if rs.has_write_marker {
                builder = builder.tag_str("write");
            }
        }
        let note = builder.sign(seckey).build().expect("note build");
        pool.send(&enostr::ClientMessage::event(note).expect("note client message"));
    }
}

pub struct AccountMutedData {
    filter: Filter,
    subid: String,
    sub: Option<Subscription>,
    muted: Arc<Muted>,
}

impl AccountMutedData {
    pub fn new(ndb: &Ndb, pool: &mut RelayPool, pubkey: &[u8; 32]) -> Self {
        // Construct a filter for the user's NIP-51 muted list
        let filter = Filter::new()
            .authors([pubkey])
            .kinds([10000])
            .limit(1)
            .build();

        // Local ndb subscription
        let ndbsub = ndb
            .subscribe(&[filter.clone()])
            .expect("ndb muted subscription");

        // Query the ndb immediately to see if the user's muted list is already there
        let txn = Transaction::new(ndb).expect("transaction");
        let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;
        let nks = ndb
            .query(&txn, &[filter.clone()], lim)
            .expect("query user muted results")
            .iter()
            .map(|qr| qr.note_key)
            .collect::<Vec<NoteKey>>();
        let muted = Self::harvest_nip51_muted(ndb, &txn, &nks);
        debug!("pubkey {}: initial muted {:?}", hex::encode(pubkey), muted);

        // Id for future remote relay subscriptions
        let subid = Uuid::new_v4().to_string();

        // Add remote subscription to existing relays
        pool.subscribe(subid.clone(), vec![filter.clone()]);

        AccountMutedData {
            filter,
            subid,
            sub: Some(ndbsub),
            muted: Arc::new(muted),
        }
    }

    fn harvest_nip51_muted(ndb: &Ndb, txn: &Transaction, nks: &[NoteKey]) -> Muted {
        let mut muted = Muted::default();
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                for tag in note.tags() {
                    match tag.get(0).and_then(|t| t.variant().str()) {
                        Some("p") => {
                            if let Some(id) = tag.get(1).and_then(|f| f.variant().id()) {
                                muted.pubkeys.insert(*id);
                            }
                        }
                        Some("t") => {
                            if let Some(str) = tag.get(1).and_then(|f| f.variant().str()) {
                                muted.hashtags.insert(str.to_string());
                            }
                        }
                        Some("word") => {
                            if let Some(str) = tag.get(1).and_then(|f| f.variant().str()) {
                                muted.words.insert(str.to_string());
                            }
                        }
                        Some("e") => {
                            if let Some(id) = tag.get(1).and_then(|f| f.variant().id()) {
                                muted.threads.insert(*id);
                            }
                        }
                        Some("alt") => {
                            // maybe we can ignore these?
                        }
                        Some(x) => error!("query_nip51_muted: unexpected tag: {}", x),
                        None => error!(
                            "query_nip51_muted: bad tag value: {:?}",
                            tag.get_unchecked(0).variant()
                        ),
                    }
                }
            }
        }
        muted
    }
}

pub struct AccountData {
    relay: AccountRelayData,
    muted: AccountMutedData,
}

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorageType,
    account_data: BTreeMap<[u8; 32], AccountData>,
    forced_relays: BTreeSet<RelaySpec>,
    bootstrap_relays: BTreeSet<RelaySpec>,
    needs_relay_config: bool,
}

impl Accounts {
    pub fn new(key_store: KeyStorageType, forced_relays: Vec<String>) -> Self {
        let accounts = if let KeyStorageResponse::ReceivedResult(res) = key_store.get_keys() {
            res.unwrap_or_default()
        } else {
            Vec::new()
        };

        let currently_selected_account = get_selected_index(&accounts, &key_store);
        let account_data = BTreeMap::new();
        let forced_relays: BTreeSet<RelaySpec> = forced_relays
            .into_iter()
            .map(|u| RelaySpec::new(AccountRelayData::canonicalize_url(&u), false, false))
            .collect();
        let bootstrap_relays = [
            "wss://relay.damus.io",
            // "wss://pyramid.fiatjaf.com",  // Uncomment if needed
            "wss://nos.lol",
            "wss://nostr.wine",
            "wss://purplepag.es",
        ]
        .iter()
        .map(|&url| url.to_string())
        .map(|u| RelaySpec::new(AccountRelayData::canonicalize_url(&u), false, false))
        .collect();

        Accounts {
            currently_selected_account,
            accounts,
            key_store,
            account_data,
            forced_relays,
            bootstrap_relays,
            needs_relay_config: true,
        }
    }

    pub fn get_accounts(&self) -> &Vec<UserAccount> {
        &self.accounts
    }

    pub fn get_account(&self, ind: usize) -> Option<&UserAccount> {
        self.accounts.get(ind)
    }

    pub fn find_account(&self, pk: &[u8; 32]) -> Option<&UserAccount> {
        self.accounts.iter().find(|acc| acc.pubkey.bytes() == pk)
    }

    pub fn remove_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            let _ = self.key_store.remove_key(account);
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
            let has_pubkey = account.pubkey.bytes() == pubkey;
            let has_nsec = account.secret_key.is_some();
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
    pub fn add_account(&mut self, account: Keypair) -> AddAccountAction {
        let pubkey = account.pubkey;
        let switch_to_index = if let Some(contains_acc) = self.contains_account(pubkey.bytes()) {
            if account.secret_key.is_some() && !contains_acc.has_nsec {
                info!(
                    "user provided nsec, but we already have npub {}. Upgrading to nsec",
                    pubkey
                );
                let _ = self.key_store.add_key(&account);

                self.accounts[contains_acc.index] = account;
            } else {
                info!("already have account, not adding {}", pubkey);
            }
            contains_acc.index
        } else {
            info!("adding new account {}", pubkey);
            let _ = self.key_store.add_key(&account);
            self.accounts.push(account);
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

    pub fn num_accounts(&self) -> usize {
        self.accounts.len()
    }

    pub fn get_selected_account_index(&self) -> Option<usize> {
        self.currently_selected_account
    }

    pub fn selected_or_first_nsec(&self) -> Option<FilledKeypair<'_>> {
        self.get_selected_account()
            .and_then(|kp| kp.to_full())
            .or_else(|| self.accounts.iter().find_map(|a| a.to_full()))
    }

    pub fn get_selected_account(&self) -> Option<&UserAccount> {
        if let Some(account_index) = self.currently_selected_account {
            if let Some(account) = self.get_account(account_index) {
                Some(account)
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn get_selected_account_data(&self) -> Option<&AccountData> {
        if let Some(account) = self.get_selected_account() {
            if let Some(account_data) = self.account_data.get(account.pubkey.bytes()) {
                return Some(account_data);
            }
        }
        None
    }

    pub fn select_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            self.currently_selected_account = Some(index);
            self.key_store.select_key(Some(account.pubkey));
        }
    }

    pub fn clear_selected_account(&mut self) {
        self.currently_selected_account = None;
        self.key_store.select_key(None);
    }

    pub fn mutefun(&self) -> Box<MuteFun> {
        if let Some(index) = self.currently_selected_account {
            if let Some(account) = self.accounts.get(index) {
                let pubkey = account.pubkey.bytes();
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
            pool.send_to(
                &ClientMessage::req(data.relay.subid.clone(), vec![data.relay.filter.clone()]),
                relay_url,
            );
            pool.send_to(
                &ClientMessage::req(data.muted.subid.clone(), vec![data.muted.filter.clone()]),
                relay_url,
            );
        }
    }

    // Returns added and removed accounts
    fn delta_accounts(&self) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
        let mut added = Vec::new();
        for pubkey in self.accounts.iter().map(|a| a.pubkey.bytes()) {
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

    fn handle_added_account(&mut self, ndb: &Ndb, pool: &mut RelayPool, pubkey: &[u8; 32]) {
        debug!("handle_added_account {}", hex::encode(pubkey));

        // Create the user account data
        let new_account_data = AccountData {
            relay: AccountRelayData::new(ndb, pool, pubkey),
            muted: AccountMutedData::new(ndb, pool, pubkey),
        };
        self.account_data.insert(*pubkey, new_account_data);
    }

    fn handle_removed_account(&mut self, pubkey: &[u8; 32]) {
        debug!("handle_removed_account {}", hex::encode(pubkey));
        // FIXME - we need to unsubscribe here
        self.account_data.remove(pubkey);
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
            "updating relay configuration for currently selected account {:?}",
            self.currently_selected_account
        );

        // If forced relays are set use them only
        let mut desired_relays = self.forced_relays.clone();

        // Compose the desired relay lists from the selected account
        if desired_relays.is_empty() {
            if let Some(data) = self.get_selected_account_data() {
                desired_relays.extend(data.relay.local.iter().cloned());
                desired_relays.extend(data.relay.advertised.iter().cloned());
            }
        }

        // If no relays are specified at this point use the bootstrap list
        if desired_relays.is_empty() {
            desired_relays = self.bootstrap_relays.clone();
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

    pub fn update(&mut self, ndb: &Ndb, pool: &mut RelayPool, ctx: &egui::Context) {
        // IMPORTANT - This function is called in the UI update loop,
        // make sure it is fast when idle

        // On the initial update the relays need config even if nothing changes below
        let mut relays_changed = self.needs_relay_config;

        let ctx2 = ctx.clone();
        let wakeup = move || {
            ctx2.request_repaint();
        };

        // Were any accounts added or removed?
        let (added, removed) = self.delta_accounts();
        for pk in added {
            self.handle_added_account(ndb, pool, &pk);
            relays_changed = true;
        }
        for pk in removed {
            self.handle_removed_account(&pk);
            relays_changed = true;
        }

        // Did any accounts receive updates (ie NIP-65 relay lists)
        relays_changed = self.poll_for_updates(ndb) || relays_changed;

        // If needed, update the relay configuration
        if relays_changed {
            self.update_relay_configuration(pool, wakeup);
            self.needs_relay_config = false;
        }
    }

    pub fn get_full<'a>(&'a self, pubkey: &[u8; 32]) -> Option<FilledKeypair<'a>> {
        if let Some(contains) = self.contains_account(pubkey) {
            if contains.has_nsec {
                if let Some(kp) = self.get_account(contains.index) {
                    return kp.to_full();
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
                    let key_bytes: [u8; 32] = *keypair.pubkey.bytes();
                    match self.account_data.get_mut(&key_bytes) {
                        None => error!("no account data found for the provided key."),
                        Some(account_data) => {
                            let advertised = &mut account_data.relay.advertised;
                            if advertised.is_empty() {
                                // If the selected account has no advertised relays,
                                // initialize with the bootstrapping set.
                                advertised.extend(self.bootstrap_relays.iter().cloned());
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
                            if let Some(secretkey) = &keypair.secret_key {
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

fn get_selected_index(accounts: &[UserAccount], keystore: &KeyStorageType) -> Option<usize> {
    match keystore.get_selected_key() {
        KeyStorageResponse::ReceivedResult(Ok(Some(pubkey))) => {
            return accounts.iter().position(|account| account.pubkey == pubkey);
        }

        KeyStorageResponse::ReceivedResult(Err(e)) => error!("Error getting selected key: {}", e),
        KeyStorageResponse::Waiting | KeyStorageResponse::ReceivedResult(Ok(None)) => {}
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
