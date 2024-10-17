use std::borrow::Cow;

use enostr::{Keypair, Pubkey};
use serde::{Deserialize, Serialize};

use super::file_key_storage::FileKeyStorage;
use super::key_storage_impl::{KeyStorage, KeyStorageResponse};
use super::security_framework_key_storage::SecurityFrameworkKeyStorage;
use crate::settings::StorageSettings;

pub struct MacOSKeyStorage<'a> {
    pub settings: &'a StorageSettings,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum MacOSKeyStorageType {
    BasicFileStorage,
    SecurityFramework(Cow<'static, str>),
}

impl<'a> KeyStorage for MacOSKeyStorage<'a> {
    fn get_keys(&self) -> KeyStorageResponse<Vec<Keypair>> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => FileKeyStorage::new().get_keys(),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(service_name).get_keys()
            }
        }
    }

    fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => FileKeyStorage::new().add_key(key),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(service_name).add_key(key)
            }
        }
    }

    fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => FileKeyStorage::new().remove_key(key),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(service_name).remove_key(key)
            }
        }
    }

    fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => FileKeyStorage::new().get_selected_key(),
            MacOSKeyStorageType::SecurityFramework(_) => unimplemented!(),
        }
    }

    fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => FileKeyStorage::new().select_key(key),
            MacOSKeyStorageType::SecurityFramework(_) => unimplemented!(),
        }
    }
}

impl<'a> MacOSKeyStorage<'a> {
    pub fn new(settings: &'a StorageSettings) -> Self {
        Self { settings }
    }
}
