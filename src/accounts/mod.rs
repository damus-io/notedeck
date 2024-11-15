use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use url::Url;
use uuid::Uuid;

use enostr::{ClientMessage, FilledKeypair, FullKeypair, Keypair, RelayPool};
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};

use crate::{
    column::Columns,
    imgcache::ImageCache,
    login_manager::AcquireKeyState,
    route::{Route, Router},
    storage::{KeyStorageResponse, KeyStorageType},
    ui::{
        account_login_view::{AccountLoginResponse, AccountLoginView},
        accounts::{AccountsView, AccountsViewResponse},
    },
    unknowns::SingleUnkIdAction,
    unknowns::UnknownIds,
    user_account::UserAccount,
};
use tracing::{debug, error, info};

mod route;

pub use route::{AccountsRoute, AccountsRouteResponse};

pub struct AccountRelayData {
    filter: Filter,
    subid: String,
    sub: Option<Subscription>,
    local: BTreeSet<String>,      // used locally but not advertised
    advertised: BTreeSet<String>, // advertised via NIP-65
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

    fn harvest_nip65_relays(ndb: &Ndb, txn: &Transaction, nks: &[NoteKey]) -> Vec<String> {
        let mut relays = Vec::new();
        for nk in nks.iter() {
            if let Ok(note) = ndb.get_note_by_key(txn, *nk) {
                for tag in note.tags() {
                    match tag.get(0).and_then(|t| t.variant().str()) {
                        Some("r") => {
                            if let Some(url) = tag.get(1).and_then(|f| f.variant().str()) {
                                relays.push(Self::canonicalize_url(url));
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
}

pub struct AccountData {
    relay: AccountRelayData,
}

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorageType,
    account_data: BTreeMap<[u8; 32], AccountData>,
    forced_relays: BTreeSet<String>,
    bootstrap_relays: BTreeSet<String>,
    needs_relay_config: bool,
}

/// Render account management views from a route
#[allow(clippy::too_many_arguments)]
pub fn render_accounts_route(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    col: usize,
    columns: &mut Columns,
    img_cache: &mut ImageCache,
    accounts: &mut Accounts,
    login_state: &mut AcquireKeyState,
    route: AccountsRoute,
) -> SingleUnkIdAction {
    let router = columns.column_mut(col).router_mut();
    let resp = match route {
        AccountsRoute::Accounts => AccountsView::new(ndb, accounts, img_cache)
            .ui(ui)
            .inner
            .map(AccountsRouteResponse::Accounts),

        AccountsRoute::AddAccount => AccountLoginView::new(login_state)
            .ui(ui)
            .inner
            .map(AccountsRouteResponse::AddAccount),
    };

    if let Some(resp) = resp {
        match resp {
            AccountsRouteResponse::Accounts(response) => {
                process_accounts_view_response(accounts, response, router);
                SingleUnkIdAction::no_action()
            }
            AccountsRouteResponse::AddAccount(response) => {
                let action = process_login_view_response(accounts, response);
                *login_state = Default::default();
                router.go_back();
                action
            }
        }
    } else {
        SingleUnkIdAction::no_action()
    }
}

pub fn process_accounts_view_response(
    manager: &mut Accounts,
    response: AccountsViewResponse,
    router: &mut Router<Route>,
) {
    match response {
        AccountsViewResponse::RemoveAccount(index) => {
            manager.remove_account(index);
        }
        AccountsViewResponse::SelectAccount(index) => {
            manager.select_account(index);
        }
        AccountsViewResponse::RouteToLogin => {
            router.route_to(Route::add_account());
        }
    }
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
        let forced_relays: BTreeSet<String> = forced_relays
            .into_iter()
            .map(|u| AccountRelayData::canonicalize_url(&u))
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
        .map(|u| AccountRelayData::canonicalize_url(&u))
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

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(&mut self, account: Keypair) -> LoginAction {
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

        LoginAction {
            unk: SingleUnkIdAction::pubkey(pubkey),
            switch_to_index,
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

    pub fn send_initial_filters(&mut self, pool: &mut RelayPool, relay_url: &str) {
        for data in self.account_data.values() {
            pool.send_to(
                &ClientMessage::req(data.relay.subid.clone(), vec![data.relay.filter.clone()]),
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
        }
        changed
    }

    fn update_relay_configuration(
        &mut self,
        pool: &mut RelayPool,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        // If forced relays are set use them only
        let mut desired_relays = self.forced_relays.clone();

        // Compose the desired relay lists from the accounts
        if desired_relays.is_empty() {
            for data in self.account_data.values() {
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

        let add: BTreeSet<String> = desired_relays.difference(&pool.urls()).cloned().collect();
        let sub: BTreeSet<String> = pool.urls().difference(&desired_relays).cloned().collect();
        if !add.is_empty() {
            debug!("configuring added relays: {:?}", add);
            let _ = pool.add_urls(add, wakeup);
        }
        if !sub.is_empty() {
            debug!("removing unwanted relays: {:?}", sub);
            pool.remove_urls(&sub);
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

pub fn process_login_view_response(
    manager: &mut Accounts,
    response: AccountLoginResponse,
) -> SingleUnkIdAction {
    let login_action = match response {
        AccountLoginResponse::CreateNew => {
            manager.add_account(FullKeypair::generate().to_keypair())
        }
        AccountLoginResponse::LoginWith(keypair) => manager.add_account(keypair),
    };
    manager.select_account(login_action.switch_to_index);
    login_action.unk
}

#[must_use = "You must call process_login_action on this to handle unknown ids"]
pub struct LoginAction {
    unk: SingleUnkIdAction,
    pub switch_to_index: usize,
}

impl LoginAction {
    // Simple wrapper around processing the unknown action to expose too
    // much internal logic. This allows us to have a must_use on our
    // LoginAction type, otherwise the SingleUnkIdAction's must_use will
    // be lost when returned in the login action
    pub fn process_action(&mut self, ids: &mut UnknownIds, ndb: &Ndb, txn: &Transaction) {
        self.unk.process_action(ids, ndb, txn);
    }
}

#[derive(Default)]
struct ContainsAccount {
    pub has_nsec: bool,
    pub index: usize,
}
