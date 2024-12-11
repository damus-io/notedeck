use crate::Result;
use enostr::{Keypair, Pubkey, SerializableKeypair};

use super::{
    file_storage::{delete_file, write_file, Directory},
    key_storage_impl::KeyStorageResponse,
};

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

    fn add_key_internal(&self, key: &Keypair) -> Result<()> {
        write_file(
            &self.keys_directory.file_path,
            key.pubkey.hex(),
            &serde_json::to_string(&SerializableKeypair::from_keypair(key, "", 7))?,
        )
    }

    fn get_keys_internal(&self) -> Result<Vec<Keypair>> {
        let keys = self
            .keys_directory
            .get_files()?
            .values()
            .filter_map(|str_key| serde_json::from_str::<SerializableKeypair>(str_key).ok())
            .map(|serializable_keypair| serializable_keypair.to_keypair(""))
            .collect();
        Ok(keys)
    }

    fn remove_key_internal(&self, key: &Keypair) -> Result<()> {
        delete_file(&self.keys_directory.file_path, key.pubkey.hex())
    }

    fn get_selected_pubkey(&self) -> Result<Option<Pubkey>> {
        let pubkey_str = self
            .selected_key_directory
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())?;

        Ok(serde_json::from_str(&pubkey_str)?)
    }

    fn select_pubkey(&self, pubkey: Option<Pubkey>) -> Result<()> {
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

impl FileKeyStorage {
    pub fn get_keys(&self) -> KeyStorageResponse<Vec<enostr::Keypair>> {
        KeyStorageResponse::ReceivedResult(self.get_keys_internal())
    }

    pub fn add_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.add_key_internal(key))
    }

    pub fn remove_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.remove_key_internal(key))
    }

    pub fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        KeyStorageResponse::ReceivedResult(self.get_selected_pubkey())
    }

    pub fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.select_pubkey(key))
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

        assert_eq!(resp, KeyStorageResponse::ReceivedResult(Ok(())));
        assert_num_storage(&storage.get_keys(), 1);

        assert_eq!(
            storage.remove_key(&kp),
            KeyStorageResponse::ReceivedResult(Ok(()))
        );
        assert_num_storage(&storage.get_keys(), 0);
    }

    fn assert_num_storage(keys_response: &KeyStorageResponse<Vec<Keypair>>, n: usize) {
        match keys_response {
            KeyStorageResponse::ReceivedResult(Ok(keys)) => {
                assert_eq!(keys.len(), n);
            }
            KeyStorageResponse::ReceivedResult(Err(_e)) => {
                panic!("could not get keys");
            }
            KeyStorageResponse::Waiting => {
                panic!("did not receive result");
            }
        }
    }

    #[test]
    fn test_select_key() {
        let kp = enostr::FullKeypair::generate().to_keypair();

        let storage = FileKeyStorage::mock().unwrap();
        let _ = storage.add_key(&kp);
        assert_num_storage(&storage.get_keys(), 1);

        let resp = storage.select_pubkey(Some(kp.pubkey));
        assert!(resp.is_ok());

        let resp = storage.get_selected_pubkey();

        assert!(resp.is_ok());
    }
}
