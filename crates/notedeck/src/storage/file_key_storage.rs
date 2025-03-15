use crate::Result;
use enostr::{Keypair, Pubkey, SerializableKeypair};

use super::file_storage::{delete_file, write_file, Directory};

static SELECTED_PUBKEY_FILE_NAME: &str = "selected_pubkey";

/// An OS agnostic file key storage implementation
#[derive(Debug, PartialEq)]
pub struct FileKeyStorage {
    keys_directory: Directory,
    selected_key_directory: Directory,
}

impl FileKeyStorage {
    pub fn new(keys_directory: Directory, selected_key_directory: Directory) -> Self {
        Self {
            keys_directory,
            selected_key_directory,
        }
    }

    pub fn add_key(&self, key: &Keypair) -> Result<()> {
        write_file(
            &self.keys_directory.file_path,
            key.pubkey.hex(),
            &serde_json::to_string(&SerializableKeypair::from_keypair(key, "", 7))?,
        )
    }

    pub fn get_keys(&self) -> Result<Vec<Keypair>> {
        let keys = self
            .keys_directory
            .get_files()?
            .values()
            .filter_map(|str_key| serde_json::from_str::<SerializableKeypair>(str_key).ok())
            .map(|serializable_keypair| serializable_keypair.to_keypair(""))
            .collect();
        Ok(keys)
    }

    pub fn remove_key(&self, key: &Keypair) -> Result<()> {
        delete_file(&self.keys_directory.file_path, key.pubkey.hex())
    }

    pub fn get_selected_key(&self) -> Result<Option<Pubkey>> {
        match self
            .selected_key_directory
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
        {
            Ok(pubkey_str) => Ok(Some(serde_json::from_str(&pubkey_str)?)),
            Err(crate::Error::Io(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn select_key(&self, pubkey: Option<Pubkey>) -> Result<()> {
        if let Some(pubkey) = pubkey {
            write_file(
                &self.selected_key_directory.file_path,
                SELECTED_PUBKEY_FILE_NAME.to_owned(),
                &serde_json::to_string(&pubkey.hex())?,
            )
        } else if self
            .selected_key_directory
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
            .is_ok()
        {
            // Case where user chose to have no selected pubkey, but one already exists
            Ok(delete_file(
                &self.selected_key_directory.file_path,
                SELECTED_PUBKEY_FILE_NAME.to_owned(),
            )?)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Result;
    use super::*;

    use enostr::Keypair;
    static CREATE_TMP_DIR: fn() -> Result<PathBuf> =
        || Ok(tempfile::TempDir::new()?.path().to_path_buf());

    impl FileKeyStorage {
        fn mock() -> Result<Self> {
            Ok(Self {
                keys_directory: Directory::new(CREATE_TMP_DIR()?),
                selected_key_directory: Directory::new(CREATE_TMP_DIR()?),
            })
        }
    }

    #[test]
    fn test_basic() {
        let kp = enostr::FullKeypair::generate().to_keypair();
        let storage = FileKeyStorage::mock().unwrap();
        let resp = storage.add_key(&kp);

        assert!(resp.is_ok());
        assert_num_storage(&storage.get_keys(), 1);

        assert!(storage.remove_key(&kp).is_ok());
        assert_num_storage(&storage.get_keys(), 0);
    }

    fn assert_num_storage(keys_response: &Result<Vec<Keypair>>, n: usize) {
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

        let storage = FileKeyStorage::mock().unwrap();
        let _ = storage.add_key(&kp);
        assert_num_storage(&storage.get_keys(), 1);

        let resp = storage.select_key(Some(kp.pubkey));
        assert!(resp.is_ok());

        let resp = storage.get_selected_key();

        assert!(resp.is_ok());
    }

    #[test]
    fn test_get_selected_key_when_no_file() {
        let storage = FileKeyStorage::mock().unwrap();

        // Should return Ok(None) when no key has been selected
        match storage.get_selected_key() {
            Ok(None) => (), // This is what we expect
            other => panic!("Expected Ok(None), got {:?}", other),
        }
    }
}
