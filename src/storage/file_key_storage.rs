use eframe::Result;
use enostr::{Keypair, Pubkey, SerializableKeypair};

use crate::Error;

use super::{
    file_storage::{FileDirectoryInteractor, FileWriterFactory, FileWriterType},
    key_storage_impl::{KeyStorage, KeyStorageError, KeyStorageResponse},
};

static SELECTED_PUBKEY_FILE_NAME: &str = "selected_pubkey";

/// An OS agnostic file key storage implementation
#[derive(Debug, PartialEq)]
pub struct FileKeyStorage {
    keys_interactor: FileDirectoryInteractor,
    selected_key_interactor: FileDirectoryInteractor,
}

impl FileKeyStorage {
    pub fn new() -> Result<Self, KeyStorageError> {
        Ok(Self {
            keys_interactor: FileWriterFactory::new(FileWriterType::Keys)
                .build()
                .map_err(KeyStorageError::OSError)?,
            selected_key_interactor: FileWriterFactory::new(FileWriterType::SelectedKey)
                .build()
                .map_err(KeyStorageError::OSError)?,
        })
    }

    fn add_key_internal(&self, key: &Keypair) -> Result<(), KeyStorageError> {
        self.keys_interactor
            .write(
                key.pubkey.hex(),
                &serde_json::to_string(&SerializableKeypair::from_keypair(key, "", 7))
                    .map_err(|e| KeyStorageError::Addition(Error::Generic(e.to_string())))?,
            )
            .map_err(KeyStorageError::Addition)
    }

    fn get_keys_internal(&self) -> Result<Vec<Keypair>, KeyStorageError> {
        let keys = self
            .keys_interactor
            .get_files()
            .map_err(KeyStorageError::Retrieval)?
            .values()
            .filter_map(|str_key| serde_json::from_str::<SerializableKeypair>(str_key).ok())
            .map(|serializable_keypair| serializable_keypair.to_keypair(""))
            .collect();
        Ok(keys)
    }

    fn remove_key_internal(&self, key: &Keypair) -> Result<(), KeyStorageError> {
        self.keys_interactor
            .delete_file(key.pubkey.hex())
            .map_err(KeyStorageError::Removal)
    }

    fn get_selected_pubkey(&self) -> Result<Option<Pubkey>, KeyStorageError> {
        let pubkey_str = self
            .selected_key_interactor
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
            .map_err(KeyStorageError::Selection)?;

        serde_json::from_str(&pubkey_str)
            .map_err(|e| KeyStorageError::Selection(Error::Generic(e.to_string())))
    }

    fn select_pubkey(&self, pubkey: Option<Pubkey>) -> Result<(), KeyStorageError> {
        if let Some(pubkey) = pubkey {
            self.selected_key_interactor
                .write(
                    SELECTED_PUBKEY_FILE_NAME.to_owned(),
                    &serde_json::to_string(&pubkey.hex())
                        .map_err(|e| KeyStorageError::Selection(Error::Generic(e.to_string())))?,
                )
                .map_err(KeyStorageError::Selection)
        } else if self
            .selected_key_interactor
            .get_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
            .is_ok()
        {
            // Case where user chose to have no selected pubkey, but one already exists
            self.selected_key_interactor
                .delete_file(SELECTED_PUBKEY_FILE_NAME.to_owned())
                .map_err(KeyStorageError::Selection)
        } else {
            Ok(())
        }
    }
}

impl KeyStorage for FileKeyStorage {
    fn get_keys(&self) -> KeyStorageResponse<Vec<enostr::Keypair>> {
        KeyStorageResponse::ReceivedResult(self.get_keys_internal())
    }

    fn add_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.add_key_internal(key))
    }

    fn remove_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.remove_key_internal(key))
    }

    fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        KeyStorageResponse::ReceivedResult(self.get_selected_pubkey())
    }

    fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.select_pubkey(key))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use enostr::Keypair;
    static CREATE_TMP_DIR: fn() -> Result<PathBuf, Error> =
        || Ok(tempfile::TempDir::new()?.path().to_path_buf());

    impl FileKeyStorage {
        fn mock() -> Result<Self, KeyStorageError> {
            Ok(Self {
                keys_interactor: FileWriterFactory::new(FileWriterType::Keys)
                    .testing_with(CREATE_TMP_DIR)
                    .build()
                    .map_err(KeyStorageError::OSError)?,
                selected_key_interactor: FileWriterFactory::new(FileWriterType::SelectedKey)
                    .testing_with(CREATE_TMP_DIR)
                    .build()
                    .map_err(KeyStorageError::OSError)?,
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
