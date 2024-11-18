use std::cmp::Ordering;

use enostr::{FilledKeypair, FullKeypair, Keypair};
use nostrdb::Ndb;
use serde::{Deserialize, Serialize};

use crate::{
    app::get_active_columns_mut,
    decks::{AccountId, DecksCache},
    imgcache::ImageCache,
    login_manager::AcquireKeyState,
    route::Route,
    storage::{KeyStorageResponse, KeyStorageType},
    ui::{
        account_login_view::{AccountLoginResponse, AccountLoginView},
        account_management::{AccountsView, AccountsViewResponse},
    },
};
use tracing::{error, info};

pub use crate::user_account::UserAccount;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct AccountManager {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorageType,
}

// TODO(jb55): move to accounts/route.rs
pub enum AccountsRouteResponse {
    Accounts(AccountsViewResponse),
    AddAccount(AccountLoginResponse),
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum AccountsRoute {
    Accounts,
    AddAccount,
}

#[derive(Debug)]
pub enum AccountSelectionResponse {
    Delete(usize),
    Select(usize),
}

/// Render account management views from a route
#[allow(clippy::too_many_arguments)]
pub fn render_accounts_route(
    ui: &mut egui::Ui,
    ndb: &Ndb,
    col: usize,
    img_cache: &mut ImageCache,
    accounts: &mut AccountManager,
    decks: &mut DecksCache,
    login_state: &mut AcquireKeyState,
    route: AccountsRoute,
) -> Option<AccountSelectionResponse> {
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

    let mut selection = None;

    if let Some(resp) = resp {
        match resp {
            AccountsRouteResponse::Accounts(response) => {
                selection = process_accounts_view_response(accounts, decks, col, response);
            }
            AccountsRouteResponse::AddAccount(login_resp) => {
                let pubkey = match login_resp {
                    AccountLoginResponse::CreateNew => {
                        let kp = FullKeypair::generate().to_keypair();
                        let pubkey = kp.pubkey;
                        accounts.add_account(kp);
                        pubkey
                    }
                    AccountLoginResponse::LoginWith(keypair) => {
                        let pubkey = keypair.pubkey;
                        accounts.add_account(keypair);
                        pubkey
                    }
                };
                decks.add_deck_default(AccountId::User(pubkey));
                selection = Some(AccountSelectionResponse::Select(
                    accounts.num_accounts() - 1,
                ));
                *login_state = Default::default();
                let router = get_active_columns_mut(accounts, decks)
                    .column_mut(col)
                    .router_mut();
                router.go_back();
            }
        }
    }

    selection
}

pub fn process_accounts_view_response(
    accounts: &mut AccountManager,
    decks: &mut DecksCache,
    col: usize,
    response: AccountsViewResponse,
) -> Option<AccountSelectionResponse> {
    let router = get_active_columns_mut(accounts, decks)
        .column_mut(col)
        .router_mut();
    let mut selection = None;
    match response {
        AccountsViewResponse::RemoveAccount(index) => {
            let acc_sel = AccountSelectionResponse::Delete(index);
            info!("account selection: {:?}", acc_sel);
            selection = Some(acc_sel);
        }
        AccountsViewResponse::SelectAccount(index) => {
            let acc_sel = AccountSelectionResponse::Select(index);
            info!("account selection: {:?}", acc_sel);
            selection = Some(AccountSelectionResponse::Select(index));
        }
        AccountsViewResponse::RouteToLogin => {
            router.route_to(Route::add_account());
        }
    }

    selection
}

impl AccountManager {
    pub fn new(key_store: KeyStorageType) -> Self {
        let accounts = if let KeyStorageResponse::ReceivedResult(res) = key_store.get_keys() {
            res.unwrap_or_default()
        } else {
            Vec::new()
        };

        let currently_selected_account = get_selected_index(&accounts, &key_store);
        AccountManager {
            currently_selected_account,
            accounts,
            key_store,
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
                        self.clear_selected_account();
                    }
                    Ordering::Less => {}
                }
            }
        }
    }

    pub fn has_account_pubkey(&self, pubkey: &[u8; 32]) -> bool {
        for account in &self.accounts {
            if account.pubkey.bytes() == pubkey {
                return true;
            }
        }

        false
    }

    pub fn add_account(&mut self, account: Keypair) -> bool {
        if self.has_account_pubkey(account.pubkey.bytes()) {
            info!("already have account, not adding {}", account.pubkey);
            return false;
        }
        let _ = self.key_store.add_key(&account);
        self.accounts.push(account);
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

    /// Select the account at the index
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
