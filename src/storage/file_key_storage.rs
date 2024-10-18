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

    fn mock() -> Result<Self, KeyStorageError> {
        Ok(Self {
            keys_interactor: FileWriterFactory::new(FileWriterType::Keys)
                .testing()
                .build()
                .map_err(KeyStorageError::OSError)?,
            selected_key_interactor: FileWriterFactory::new(FileWriterType::SelectedKey)
                .testing()
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

mod tests {
    use crate::storage::key_storage_impl::{KeyStorage, KeyStorageResponse};

    use super::FileKeyStorage;

    #[allow(unused)]
    fn remove_all() {
        match FileKeyStorage::mock().unwrap().get_keys() {
            KeyStorageResponse::ReceivedResult(Ok(keys)) => {
                for key in keys {
                    if let KeyStorageResponse::ReceivedResult(res) =
                        FileKeyStorage::mock().unwrap().remove_key(&key)
                    {
                        assert!(res.is_ok());
                    }
                }
            }
            KeyStorageResponse::ReceivedResult(Err(e)) => {
                panic!("could not get keys");
            }
            _ => {}
        }
    }

    #[test]
    fn test_basic() {
        remove_all();
        let kp = enostr::FullKeypair::generate().to_keypair();
        let resp = FileKeyStorage::mock().unwrap().add_key(&kp);

        assert_eq!(resp, KeyStorageResponse::ReceivedResult(Ok(())));
        assert_num_storage(1);

        let resp = FileKeyStorage::mock().unwrap().remove_key(&kp);
        assert_eq!(resp, KeyStorageResponse::ReceivedResult(Ok(())));
        assert_num_storage(0);
        remove_all();
    }

    #[allow(dead_code)]
    fn assert_num_storage(n: usize) {
        let resp = FileKeyStorage::mock().unwrap().get_keys();

        if let KeyStorageResponse::ReceivedResult(Ok(vec)) = resp {
            assert_eq!(vec.len(), n);
            return;
        }
        panic!();
    }

    #[test]
    fn test_select_key() {
        remove_all();
        let kp = enostr::FullKeypair::generate().to_keypair();

        let _ = FileKeyStorage::mock().unwrap().add_key(&kp);
        assert_num_storage(1);

        let resp = FileKeyStorage::mock()
            .unwrap()
            .select_pubkey(Some(kp.pubkey));
        assert!(resp.is_ok());

        let resp = FileKeyStorage::mock().unwrap().get_selected_pubkey();

        assert!(resp.is_ok());

        remove_all();
    }
}
