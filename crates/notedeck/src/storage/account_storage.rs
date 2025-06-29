use crate::{user_account::UserAccountSerializable, Result};
use enostr::{Keypair, Pubkey, SerializableKeypair};
use tokenator::{TokenParser, TokenSerializable, TokenWriter};

use super::file_storage::{delete_file, write_file, Directory};

static SELECTED_PUBKEY_FILE_NAME: &str = "selected_pubkey";

/// An OS agnostic file key storage implementation
#[derive(Debug, PartialEq, Clone)]
pub struct AccountStorage {
    accounts_directory: Directory,
    selected_key_directory: Directory,
}

impl AccountStorage {
    pub fn new(accounts_directory: Directory, selected_key_directory: Directory) -> Self {
        Self {
            accounts_directory,
            selected_key_directory,
        }
    }

    pub fn rw(self) -> (AccountStorageReader, AccountStorageWriter) {
        (
            AccountStorageReader::new(self.clone()),
            AccountStorageWriter::new(self),
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
        let mut writer = TokenWriter::new("\t");
        account.serialize_tokens(&mut writer);
        write_file(
            &self.storage.accounts_directory.file_path,
            account.key.pubkey.hex(),
            writer.str(),
        )
    }

    pub fn remove_key(&self, key: &Keypair) -> Result<()> {
        delete_file(&self.storage.accounts_directory.file_path, key.pubkey.hex())
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
        let keys = self
            .storage
            .accounts_directory
            .get_files()?
            .values()
            .filter_map(|serialized| deserialize_storage(serialized).ok())
            .collect();
        Ok(keys)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Result;
    use super::*;

    static CREATE_TMP_DIR: fn() -> Result<PathBuf> =
        || Ok(tempfile::TempDir::new()?.path().to_path_buf());

    impl AccountStorage {
        fn mock() -> Result<Self> {
            Ok(Self {
                accounts_directory: Directory::new(CREATE_TMP_DIR()?),
                selected_key_directory: Directory::new(CREATE_TMP_DIR()?),
            })
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
