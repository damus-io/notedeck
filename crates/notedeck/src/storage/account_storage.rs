use crate::{user_account::UserAccountSerializable, Result};
use enostr::{Keypair, Pubkey, SerializableKeypair};
use tokenator::{TokenParser, TokenSerializable, TokenWriter};

use super::{
    file_storage::{delete_file, write_file, Directory},
    keyring_store::KeyringStore,
};

static SELECTED_PUBKEY_FILE_NAME: &str = "selected_pubkey";

/// An OS agnostic key storage implementation backed by the operating system's secure store.
#[derive(Debug, Clone)]
pub struct AccountStorage {
    accounts_directory: Directory,
    selected_key_directory: Directory,
    keyring: KeyringStore,
}

impl AccountStorage {
    pub fn new(accounts_directory: Directory, selected_key_directory: Directory) -> Self {
        Self::with_keyring(
            accounts_directory,
            selected_key_directory,
            KeyringStore::default(),
        )
    }

    pub(crate) fn with_keyring(
        accounts_directory: Directory,
        selected_key_directory: Directory,
        keyring: KeyringStore,
    ) -> Self {
        Self {
            accounts_directory,
            selected_key_directory,
            keyring,
        }
    }

    pub fn rw(self) -> (AccountStorageReader, AccountStorageWriter) {
        (
            AccountStorageReader::new(self.clone()),
            AccountStorageWriter::new(self),
        )
    }

    fn persist_account(&self, account: &UserAccountSerializable) -> Result<()> {
        if let Some(secret) = account.key.secret_key.as_ref() {
            self.keyring.store_secret(&account.key.pubkey, secret)?;
            self.write_account_without_secret(account)?;
        } else {
            // if the account is npub only, make sure the db doesn't somehow have the nsec
            self.keyring.remove_secret(&account.key.pubkey)?;
        }

        Ok(())
    }

    fn write_account_without_secret(&self, account: &UserAccountSerializable) -> Result<()> {
        let mut writer = TokenWriter::new("\t");
        sanitized_account(account).serialize_tokens(&mut writer);

        write_file(
            &self.accounts_directory.file_path,
            account.key.pubkey.hex(),
            writer.str(),
        )
    }
}

pub struct AccountStorageWriter {
    storage: AccountStorage,
}

impl AccountStorageWriter {
    pub fn new(storage: AccountStorage) -> Self {
        Self { storage }
    }

    pub fn write_account(&self, account: &UserAccountSerializable) -> Result<()> {
        self.storage.persist_account(account)
    }

    pub fn remove_key(&self, key: &Keypair) -> Result<()> {
        delete_file(&self.storage.accounts_directory.file_path, key.pubkey.hex())?;
        self.storage.keyring.remove_secret(&key.pubkey)?;
        Ok(())
    }

    pub fn select_key(&self, pubkey: Option<Pubkey>) -> Result<()> {
        if let Some(pubkey) = pubkey {
            write_file(
                &self.storage.selected_key_directory.file_path,
                SELECTED_PUBKEY_FILE_NAME.to_owned(),
                &serde_json::to_string(&pubkey.hex())?,
            )
        } else if self
            .storage
            .selected_key_directory
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
            .is_ok()
        {
            // Case where user chose to have no selected pubkey, but one already exists
            Ok(delete_file(
                &self.storage.selected_key_directory.file_path,
                SELECTED_PUBKEY_FILE_NAME.to_owned(),
            )?)
        } else {
            Ok(())
        }
    }
}

pub struct AccountStorageReader {
    storage: AccountStorage,
}

impl AccountStorageReader {
    pub fn new(storage: AccountStorage) -> Self {
        Self { storage }
    }

    pub fn get_accounts(&self) -> Result<Vec<UserAccountSerializable>> {
        let accounts = self
            .storage
            .accounts_directory
            .get_files()?
            .values()
            .filter_map(|serialized| match deserialize_storage(serialized) {
                Ok(account) => Some(account),
                Err(err) => {
                    tracing::error!("failed to deserialize stored account: {err}");
                    None
                }
            })
            // sanitize our storage of secrets & inject the secret from `keyring` into `UserAccountSerializable`
            .map(|mut account| -> Result<UserAccountSerializable> {
                if let Some(secret) = &account.key.secret_key {
                    match self
                        .storage
                        .keyring
                        .store_secret(&account.key.pubkey, secret)
                    {
                        Ok(_) => {
                            if let Err(e) = self.storage.write_account_without_secret(&account) {
                                tracing::error!(
                                    "failed to write account {:?} without secret: {e}",
                                    account.key.pubkey
                                );
                            }
                        }
                        Err(e) => tracing::error!("failed to store secret in OS secure store: {e}"),
                    }
                } else if let Ok(Some(secret)) =
                    self.storage.keyring.get_secret(&account.key.pubkey)
                {
                    account.key.secret_key = Some(secret);
                }
                Ok(account)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(accounts)
    }

    pub fn get_selected_key(&self) -> Result<Option<Pubkey>> {
        match self
            .storage
            .selected_key_directory
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
        {
            Ok(pubkey_str) => Ok(Some(serde_json::from_str(&pubkey_str)?)),
            Err(crate::Error::Io(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn deserialize_storage(serialized: &str) -> Result<UserAccountSerializable> {
    let data = serialized.split("\t").collect::<Vec<&str>>();
    let mut parser = TokenParser::new(&data);

    if let Ok(acc) = UserAccountSerializable::parse_from_tokens(&mut parser) {
        return Ok(acc);
    }

    // try old deserialization way
    Ok(UserAccountSerializable::new(old_deserialization(
        serialized,
    )?))
}

fn old_deserialization(serialized: &str) -> Result<Keypair> {
    Ok(serde_json::from_str::<SerializableKeypair>(serialized)?.to_keypair(""))
}

fn sanitized_account(account: &UserAccountSerializable) -> UserAccountSerializable {
    let mut sanitized = account.clone();
    sanitized.key.secret_key = None;
    sanitized
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::storage::KeyringStore;

    use super::Result;
    use super::*;

    static CREATE_TMP_DIR: fn() -> Result<PathBuf> =
        || Ok(tempfile::TempDir::new()?.path().to_path_buf());

    impl AccountStorage {
        fn mock() -> Result<Self> {
            Ok(Self::with_keyring(
                Directory::new(CREATE_TMP_DIR()?),
                Directory::new(CREATE_TMP_DIR()?),
                KeyringStore::in_memory(),
            ))
        }
    }

    #[test]
    fn test_basic() {
        let kp = enostr::FullKeypair::generate().to_keypair();
        let (reader, writer) = AccountStorage::mock().unwrap().rw();
        let resp = writer.write_account(&UserAccountSerializable::new(kp.clone()));

        assert!(resp.is_ok());
        assert_num_storage(&reader.get_accounts(), 1);

        assert!(writer.remove_key(&kp).is_ok());
        assert_num_storage(&reader.get_accounts(), 0);
    }

    #[test]
    fn test_secret_persisted_in_keyring_not_on_disk() {
        let kp = enostr::FullKeypair::generate().to_keypair();
        let (reader, writer) = AccountStorage::mock().unwrap().rw();

        writer
            .write_account(&UserAccountSerializable::new(kp.clone()))
            .unwrap();

        let files = reader
            .storage
            .accounts_directory
            .get_files()
            .expect("files");

        let stored = files
            .get(&kp.pubkey.hex())
            .expect("account file should exist");

        let secret_hex = {
            let secret = kp.secret_key.as_ref().expect("secret key");
            hex::encode(secret.to_secret_bytes())
        };
        assert!(
            !stored.contains(&secret_hex),
            "secret key unexpectedly persisted to disk"
        );

        let accounts = reader.get_accounts().expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert!(accounts[0].key.secret_key.is_some());
    }

    #[test]
    fn test_remove_key_removes_secret() {
        let kp = enostr::FullKeypair::generate().to_keypair();
        let (reader, writer) = AccountStorage::mock().unwrap().rw();

        writer
            .write_account(&UserAccountSerializable::new(kp.clone()))
            .expect("write account");

        writer.remove_key(&kp).expect("remove key");

        assert!(
            reader
                .storage
                .keyring
                .get_secret(&kp.pubkey)
                .expect("keyring read")
                .is_none(),
            "secret key should be removed from keyring"
        );
    }

    fn assert_num_storage(keys_response: &Result<Vec<UserAccountSerializable>>, n: usize) {
        match keys_response {
            Ok(keys) => {
                assert_eq!(keys.len(), n);
            }
            Err(_e) => {
                panic!("could not get keys");
            }
        }
    }

    #[test]
    fn test_select_key() {
        let kp = enostr::FullKeypair::generate().to_keypair();

        let (reader, writer) = AccountStorage::mock().unwrap().rw();
        let _ = writer.write_account(&UserAccountSerializable::new(kp.clone()));
        assert_num_storage(&reader.get_accounts(), 1);

        let resp = writer.select_key(Some(kp.pubkey));
        assert!(resp.is_ok());

        let resp = reader.get_selected_key();

        assert!(resp.is_ok());
    }

    #[test]
    fn test_get_selected_key_when_no_file() {
        let storage = AccountStorage::mock().unwrap().rw().0;

        // Should return Ok(None) when no key has been selected
        match storage.get_selected_key() {
            Ok(None) => (), // This is what we expect
            other => panic!("Expected Ok(None), got {:?}", other),
        }
    }
}
