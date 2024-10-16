use enostr::Pubkey;
use serde::{Deserialize, Serialize};

use crate::key_storage::{KeyStorage, KeyStorageResponse};
use crate::settings::StorageSettings;

use super::file_key_storage::BasicFileStorage;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum LinuxKeyStorageType {
    BasicFileStorage,
    // TODO(kernelkind): could use the secret service api, and maybe even allow password manager integration via a settings menu
}

pub struct LinuxKeyStorage<'a> {
    settings: &'a StorageSettings,
}

impl<'a> LinuxKeyStorage<'a> {
    pub fn new(settings: &'a StorageSettings) -> Self {
        Self { settings }
    }
}

impl KeyStorage for LinuxKeyStorage<'_> {
    fn get_keys(&self) -> KeyStorageResponse<Vec<enostr::Keypair>> {
        match self.settings.linux_key_storage_type {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().get_keys(),
        }
    }

    fn add_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        match self.settings.linux_key_storage_type {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().add_key(key),
        }
    }

    fn remove_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        match self.settings.linux_key_storage_type {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().remove_key(key),
        }
    }

    fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        match self.settings.linux_key_storage_type {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().get_selected_key(),
        }
    }

    fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        match self.settings.linux_key_storage_type {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().select_key(key),
        }
    }
}
