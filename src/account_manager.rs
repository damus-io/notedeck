use std::cmp::Ordering;

use enostr::FullKeypair;

pub use crate::user_account::UserAccount;
use crate::{key_storage::KeyStorage, relay_generation::RelayGenerator};

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct AccountManager {
    currently_selected_account: Option<usize>,
    accounts: Vec<UserAccount>,
    key_store: KeyStorage,
    relay_generator: RelayGenerator,
}

impl AccountManager {
    pub fn new(
        currently_selected_account: Option<usize>,
        key_store: KeyStorage,
        // TODO: right now, there is only one way of generating relays for all accounts. In the future
        // each account should have the option of generating relays differently
        relay_generator: RelayGenerator,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Self {
        let accounts = if let Ok(keys) = key_store.get_keys() {
            keys.into_iter()
                .map(|key| {
                    let relays = relay_generator.generate_relays_for(&key.pubkey, wakeup.clone());
                    UserAccount { key, relays }
                })
                .collect()
        } else {
            Vec::new()
        };

        AccountManager {
            currently_selected_account,
            accounts,
            key_store,
            relay_generator,
        }
    }

    pub fn get_accounts(&self) -> &Vec<UserAccount> {
        &self.accounts
    }

    pub fn get_account(&self, index: usize) -> Option<&UserAccount> {
        self.accounts.get(index)
    }

    pub fn remove_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            let _ = self.key_store.remove_key(&account.key);
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

    pub fn add_account(
        &mut self,
        key: FullKeypair,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        let _ = self.key_store.add_key(&key);
        let relays = self
            .relay_generator
            .generate_relays_for(&key.pubkey, wakeup);
        let account = UserAccount { key, relays };

        self.accounts.push(account)
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
