use nostr_sdk::Keys;
use nostrdb::{Ndb, Transaction};

pub use crate::user_account::UserAccount;
use crate::{
    imgcache::ImageCache, key_storage::KeyStorage, relay_generation::RelayGenerator,
    ui::profile::preview::SimpleProfilePreview,
};

pub struct SimpleProfilePreviewController<'a> {
    ndb: &'a Ndb,
    img_cache: &'a mut ImageCache,
}

impl<'a> SimpleProfilePreviewController<'a> {
    pub fn new(ndb: &'a Ndb, img_cache: &'a mut ImageCache) -> Self {
        SimpleProfilePreviewController { ndb, img_cache }
    }

    pub fn set_profile_previews(
        &mut self,
        account_manager: &AccountManager<'a>,
        ui: &mut egui::Ui,
        edit_mode: bool,
        add_preview_ui: fn(
            ui: &mut egui::Ui,
            preview: SimpleProfilePreview,
            edit_mode: bool,
        ) -> bool,
    ) -> Option<Vec<usize>> {
        let mut to_remove: Option<Vec<usize>> = None;

        for i in 0..account_manager.num_accounts() {
            if let Some(account) = account_manager.get_account(i) {
                if let Ok(txn) = Transaction::new(self.ndb) {
                    let profile = self
                        .ndb
                        .get_profile_by_pubkey(&txn, &account.key.public_key().to_bytes());

                    if let Ok(profile) = profile {
                        let preview = SimpleProfilePreview::new(&profile, self.img_cache);

                        if add_preview_ui(ui, preview, edit_mode) {
                            if to_remove.is_none() {
                                to_remove = Some(Vec::new());
                            }
                            to_remove.as_mut().unwrap().push(i);
                        }
                    };
                }
            }
        }

        to_remove
    }
}

/// The interface for managing the user's accounts.
/// Represents all user-facing operations related to account management.
pub struct AccountManager<'a> {
    accounts: &'a mut Vec<UserAccount>,
    key_store: KeyStorage,
    relay_generator: RelayGenerator,
}

impl<'a> AccountManager<'a> {
    pub fn new(
        accounts: &'a mut Vec<UserAccount>,
        key_store: KeyStorage,
        relay_generator: RelayGenerator,
    ) -> Self {
        AccountManager {
            accounts,
            key_store,
            relay_generator,
        }
    }

    pub fn get_accounts(&'a self) -> &'a Vec<UserAccount> {
        self.accounts
    }

    pub fn get_account(&'a self, index: usize) -> Option<&'a UserAccount> {
        self.accounts.get(index)
    }

    pub fn remove_account(&mut self, index: usize) {
        if let Some(account) = self.accounts.get(index) {
            self.key_store.remove_key(&account.key);
        }
        if index < self.accounts.len() {
            self.accounts.remove(index);
        }
    }

    pub fn add_account(&'a mut self, key: Keys, ctx: &egui::Context) {
        self.key_store.add_key(&key);
        let relays = self.relay_generator.generate_relays_for(&key, ctx);
        let account = UserAccount { key, relays };

        self.accounts.push(account)
    }

    pub fn num_accounts(&self) -> usize {
        self.accounts.len()
    }
}
