use enostr::FullKeypair;

pub use crate::user_account::UserAccount;
use crate::{key_storage::KeyStorage, relay_generation::RelayGenerator};

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct AccountManager {
    accounts: Vec<UserAccount>,
    key_store: KeyStorage,
    relay_generator: RelayGenerator,
}

impl AccountManager {
    pub fn new(
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
        }
        if index < self.accounts.len() {
            self.accounts.remove(index);
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
}
