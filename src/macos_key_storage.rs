#![cfg(target_os = "macos")]

use enostr::{FullKeypair, Pubkey, SecretKey};

use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
use security_framework::passwords::{delete_generic_password, set_generic_password};

use crate::key_storage::KeyStorageError;

pub struct MacOSKeyStorage<'a> {
    pub service_name: &'a str,
}

impl<'a> MacOSKeyStorage<'a> {
    pub fn new(service_name: &'a str) -> Self {
        MacOSKeyStorage { service_name }
    }

    pub fn add_key(&self, key: &FullKeypair) -> Result<(), KeyStorageError> {
        match set_generic_password(
            self.service_name,
            key.pubkey.hex().as_str(),
            key.secret_key.as_secret_bytes(),
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(KeyStorageError::Addition(key.pubkey.hex())),
        }
    }

    fn get_pubkey_strings(&self) -> Vec<String> {
        let search_results = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(self.service_name)
            .load_attributes(true)
            .limit(Limit::All)
            .search();

        let mut accounts = Vec::new();

        if let Ok(search_results) = search_results {
            for result in search_results {
                if let Some(map) = result.simplify_dict() {
                    if let Some(val) = map.get("acct") {
                        accounts.push(val.clone());
                    }
                }
            }
        }

        accounts
    }

    pub fn get_pubkeys(&self) -> Vec<Pubkey> {
        self.get_pubkey_strings()
            .iter_mut()
            .filter_map(|pubkey_str| Pubkey::from_hex(pubkey_str.as_str()).ok())
            .collect()
    }

    fn get_privkey_bytes_for(&self, account: &str) -> Option<Vec<u8>> {
        let search_result = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(self.service_name)
            .load_data(true)
            .account(account)
            .search();

        if let Ok(results) = search_result {
            if let Some(SearchResult::Data(vec)) = results.first() {
                return Some(vec.clone());
            }
        }

        None
    }

    fn get_secret_key_for_pubkey(&self, pubkey: &Pubkey) -> Option<SecretKey> {
        if let Some(bytes) = self.get_privkey_bytes_for(pubkey.hex().as_str()) {
            SecretKey::from_slice(bytes.as_slice()).ok()
        } else {
            None
        }
    }

    pub fn get_all_fullkeypairs(&self) -> Vec<FullKeypair> {
        self.get_pubkeys()
            .iter()
            .filter_map(|pubkey| {
                let maybe_secret = self.get_secret_key_for_pubkey(pubkey);
                maybe_secret.map(|secret| FullKeypair::new(pubkey.clone(), secret))
            })
            .collect()
    }

    pub fn delete_key(&self, pubkey: &Pubkey) -> Result<(), KeyStorageError> {
        match delete_generic_password(self.service_name, pubkey.hex().as_str()) {
            Ok(_) => Ok(()),
            Err(e) => {
                println!("got error: {}", e);
                Err(KeyStorageError::Removal(pubkey.hex()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_SERVICE_NAME: &str = "NOTEDECKTEST";
    static STORAGE: MacOSKeyStorage = MacOSKeyStorage {
        service_name: TEST_SERVICE_NAME,
    };

    // individual tests are ignored so test runner doesn't run them all concurrently
    // TODO: a way to run them all serially should be devised

    #[test]
    #[ignore]
    fn add_and_remove_test_pubkey_only() {
        let num_keys_before_test = STORAGE.get_pubkeys().len();

        let keypair = FullKeypair::generate();
        let add_result = STORAGE.add_key(&keypair);
        assert_eq!(add_result, Ok(()));

        let get_pubkeys_result = STORAGE.get_pubkeys();
        assert_eq!(get_pubkeys_result.len() - num_keys_before_test, 1);

        let remove_result = STORAGE.delete_key(&keypair.pubkey);
        assert_eq!(remove_result, Ok(()));

        let keys = STORAGE.get_pubkeys();
        assert_eq!(keys.len() - num_keys_before_test, 0);
    }

    fn add_and_remove_full_n(n: usize) {
        let num_keys_before_test = STORAGE.get_all_fullkeypairs().len();
        // there must be zero keys in storage for the test to work as intended
        assert_eq!(num_keys_before_test, 0);

        let expected_keypairs: Vec<FullKeypair> = (0..n).map(|_| FullKeypair::generate()).collect();

        expected_keypairs.iter().for_each(|keypair| {
            let add_result = STORAGE.add_key(keypair);
            assert_eq!(add_result, Ok(()));
        });

        let asserted_keypairs = STORAGE.get_all_fullkeypairs();
        assert_eq!(expected_keypairs, asserted_keypairs);

        expected_keypairs.iter().for_each(|keypair| {
            let remove_result = STORAGE.delete_key(&keypair.pubkey);
            assert_eq!(remove_result, Ok(()));
        });

        let num_keys_after_test = STORAGE.get_all_fullkeypairs().len();
        assert_eq!(num_keys_after_test, 0);
    }

    #[test]
    #[ignore]
    fn add_and_remove_full() {
        add_and_remove_full_n(1);
    }

    #[test]
    #[ignore]
    fn add_and_remove_full_10() {
        add_and_remove_full_n(10);
    }
}
