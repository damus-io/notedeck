#![cfg(target_os = "macos")]

use std::borrow::Cow;

use enostr::Keypair;
use serde::{Deserialize, Serialize};

use crate::file_key_storage::BasicFileStorage;
use crate::key_storage::{KeyStorage, KeyStorageResponse};
use crate::security_framework_key_storage::SecurityFrameworkKeyStorage;
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
            MacOSKeyStorageType::BasicFileStorage => BasicFileStorage::new().get_keys(),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(&service_name).get_keys()
            }
        }
    }

    fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => BasicFileStorage::new().add_key(key),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(&service_name).add_key(key)
            }
        }
    }

    fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        match &self.settings.macos_key_storage_type {
            MacOSKeyStorageType::BasicFileStorage => BasicFileStorage::new().remove_key(key),
            MacOSKeyStorageType::SecurityFramework(service_name) => {
                SecurityFrameworkKeyStorage::new(&service_name).remove_key(key)
            }
        }
    }
}

impl<'a> MacOSKeyStorage<'a> {
    pub fn new(settings: &'a StorageSettings) -> Self {
        Self { settings }
    }
}
