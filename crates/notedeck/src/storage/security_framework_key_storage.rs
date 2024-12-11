use std::borrow::Cow;

use enostr::{Keypair, Pubkey, SecretKey};
use security_framework::{
    item::{ItemClass, ItemSearchOptions, Limit, SearchResult},
    passwords::{delete_generic_password, set_generic_password},
};
use tracing::error;

use crate::{Error, Result};

use super::KeyStorageResponse;

#[derive(Debug, PartialEq)]
pub struct SecurityFrameworkKeyStorage {
    pub service_name: Cow<'static, str>,
}

impl SecurityFrameworkKeyStorage {
    pub fn new(service_name: String) -> Self {
        SecurityFrameworkKeyStorage {
            service_name: Cow::Owned(service_name),
        }
    }

    fn add_key_internal(&self, key: &Keypair) -> Result<()> {
        match set_generic_password(
            &self.service_name,
            key.pubkey.hex().as_str(),
            key.secret_key
                .as_ref()
                .map_or_else(|| &[] as &[u8], |sc| sc.as_secret_bytes()),
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(Error::Generic(e.to_string())),
        }
    }

    fn get_pubkey_strings(&self) -> Vec<String> {
        let search_results = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(&self.service_name)
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

    fn get_pubkeys(&self) -> Vec<Pubkey> {
        self.get_pubkey_strings()
            .iter_mut()
            .filter_map(|pubkey_str| Pubkey::from_hex(pubkey_str.as_str()).ok())
            .collect()
    }

    fn get_privkey_bytes_for(&self, account: &str) -> Option<Vec<u8>> {
        let search_result = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(&self.service_name)
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

    fn get_all_keypairs(&self) -> Vec<Keypair> {
        self.get_pubkeys()
            .iter()
            .map(|pubkey| {
                let maybe_secret = self.get_secret_key_for_pubkey(pubkey);
                Keypair::new(*pubkey, maybe_secret)
            })
            .collect()
    }

    fn delete_key(&self, pubkey: &Pubkey) -> Result<()> {
        match delete_generic_password(&self.service_name, pubkey.hex().as_str()) {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("delete key error {}", e);
                Err(Error::Generic(e.to_string()))
            }
        }
    }
}

impl SecurityFrameworkKeyStorage {
    pub fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.add_key_internal(key))
    }

    pub fn get_keys(&self) -> KeyStorageResponse<Vec<Keypair>> {
        KeyStorageResponse::ReceivedResult(Ok(self.get_all_keypairs()))
    }

    pub fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.delete_key(&key.pubkey))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;

    static TEST_SERVICE_NAME: &str = "NOTEDECKTEST";
    static STORAGE: SecurityFrameworkKeyStorage = SecurityFrameworkKeyStorage {
        service_name: Cow::Borrowed(TEST_SERVICE_NAME),
    };

    // individual tests are ignored so test runner doesn't run them all concurrently
    // TODO: a way to run them all serially should be devised

    #[test]
    #[ignore]
    fn add_and_remove_test_pubkey_only() {
        let num_keys_before_test = STORAGE.get_pubkeys().len();

        let keypair = FullKeypair::generate().to_keypair();
        let add_result = STORAGE.add_key_internal(&keypair);
        assert!(add_result.is_ok());

        let get_pubkeys_result = STORAGE.get_pubkeys();
        assert_eq!(get_pubkeys_result.len() - num_keys_before_test, 1);

        let remove_result = STORAGE.delete_key(&keypair.pubkey);
        assert!(remove_result.is_ok());

        let keys = STORAGE.get_pubkeys();
        assert_eq!(keys.len() - num_keys_before_test, 0);
    }

    fn add_and_remove_full_n(n: usize) {
        let num_keys_before_test = STORAGE.get_all_keypairs().len();
        // there must be zero keys in storage for the test to work as intended
        assert_eq!(num_keys_before_test, 0);

        let expected_keypairs: Vec<Keypair> = (0..n)
            .map(|_| FullKeypair::generate().to_keypair())
            .collect();

        expected_keypairs.iter().for_each(|keypair| {
            let add_result = STORAGE.add_key_internal(keypair);
            assert!(add_result.is_ok());
        });

        let asserted_keypairs = STORAGE.get_all_keypairs();
        assert_eq!(expected_keypairs, asserted_keypairs);

        expected_keypairs.iter().for_each(|keypair| {
            let remove_result = STORAGE.delete_key(&keypair.pubkey);
            assert!(remove_result.is_ok());
        });

        let num_keys_after_test = STORAGE.get_all_keypairs().len();
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
