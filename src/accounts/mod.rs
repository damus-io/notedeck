use std::cmp::Ordering;

use enostr::{FilledKeypair, FullKeypair, Keypair};
use nostrdb::Ndb;

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
    user_account::UserAccount,
};
use tracing::{error, info};

mod route;

pub use route::{AccountsRoute, AccountsRouteResponse};

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct Accounts {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorageType,
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
    pub fn new(key_store: KeyStorageType) -> Self {
        let accounts = if let KeyStorageResponse::ReceivedResult(res) = key_store.get_keys() {
            res.unwrap_or_default()
        } else {
            Vec::new()
        };

        let currently_selected_account = get_selected_index(&accounts, &key_store);
        Accounts {
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

    pub fn has_account_pubkey(&self, pubkey: &[u8; 32]) -> bool {
        for account in &self.accounts {
            if account.pubkey.bytes() == pubkey {
                return true;
            }
        }

        false
    }

    #[must_use = "UnknownIdAction's must be handled. Use .process_unknown_id_action()"]
    pub fn add_account(&mut self, account: Keypair) -> SingleUnkIdAction {
        if self.has_account_pubkey(account.pubkey.bytes()) {
            info!("already have account, not adding {}", account.pubkey);
            return SingleUnkIdAction::pubkey(account.pubkey);
        }

        let _ = self.key_store.add_key(&account);
        let pk = account.pubkey;
        self.accounts.push(account);
        SingleUnkIdAction::pubkey(pk)
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
    let r = match response {
        AccountLoginResponse::CreateNew => {
            manager.add_account(FullKeypair::generate().to_keypair())
        }
        AccountLoginResponse::LoginWith(keypair) => manager.add_account(keypair),
    };
    manager.select_account(manager.num_accounts() - 1);
    r
}
