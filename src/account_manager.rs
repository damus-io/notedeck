use std::cmp::Ordering;

use enostr::Keypair;

use crate::key_storage::{KeyStorage, KeyStorageResponse, KeyStorageType};
pub use crate::user_account::UserAccount;
use tracing::info;

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct AccountManager {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorageType,
}

impl AccountManager {
    pub fn new(currently_selected_account: Option<usize>, key_store: KeyStorageType) -> Self {
        let accounts = if let KeyStorageResponse::ReceivedResult(res) = key_store.get_keys() {
            res.unwrap_or_default()
        } else {
            Vec::new()
        };

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
        if self.accounts.get(index).is_some() {
            self.currently_selected_account = Some(index)
        }
    }

    pub fn clear_selected_account(&mut self) {
        self.currently_selected_account = None
    }
}
