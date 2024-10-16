use enostr::{Keypair, Pubkey};

#[cfg(target_os = "linux")]
use super::linux_key_storage::LinuxKeyStorage;
use crate::settings::StorageSettings;

#[cfg(target_os = "macos")]
use super::macos_key_storage::MacOSKeyStorage;

#[derive(Debug, PartialEq)]
pub enum KeyStorageType {
    None,
    #[cfg(target_os = "macos")]
    MacOS(StorageSettings),
    #[cfg(target_os = "linux")]
    Linux(StorageSettings),
    // TODO:
    // Windows,
    // Android,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub enum KeyStorageResponse<R> {
    Waiting,
    ReceivedResult(Result<R, KeyStorageError>),
}

pub trait KeyStorage {
    fn get_keys(&self) -> KeyStorageResponse<Vec<Keypair>>;
    fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()>;
    fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()>;
    fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>>;
    fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()>;
}

impl KeyStorage for KeyStorageType {
    fn get_keys(&self) -> KeyStorageResponse<Vec<Keypair>> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(Vec::new())),
            #[cfg(target_os = "macos")]
            Self::MacOS(settings) => MacOSKeyStorage::new(settings).get_keys(),
            #[cfg(target_os = "linux")]
            Self::Linux(settings) => LinuxKeyStorage::new(settings).get_keys(),
        }
    }

    fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            #[cfg(target_os = "macos")]
            Self::MacOS(settings) => MacOSKeyStorage::new(settings).add_key(key),
            #[cfg(target_os = "linux")]
            Self::Linux(settings) => LinuxKeyStorage::new(settings).add_key(key),
        }
    }

    fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            #[cfg(target_os = "macos")]
            Self::MacOS(settings) => MacOSKeyStorage::new(settings).remove_key(key),
            #[cfg(target_os = "linux")]
            Self::Linux(settings) => LinuxKeyStorage::new(settings).remove_key(key),
        }
    }

    fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(None)),
            #[cfg(target_os = "macos")]
            Self::MacOS(settings) => MacOSKeyStorage::new(settings).get_selected_key(),
            #[cfg(target_os = "linux")]
            Self::Linux(settings) => LinuxKeyStorage::new(settings).get_selected_key(),
        }
    }

    fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            #[cfg(target_os = "macos")]
            Self::MacOS(settings) => MacOSKeyStorage::new(settings).select_key(key),
            #[cfg(target_os = "linux")]
            Self::Linux(settings) => LinuxKeyStorage::new(settings).select_key(key),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Clone)]
pub enum KeyStorageError {
    Retrieval(String),
    Addition(String),
    Selection(String),
    Removal(String),
    OSError(String),
}

impl std::fmt::Display for KeyStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Retrieval(e) => write!(f, "Failed to retrieve keys: {:?}", e),
            Self::Addition(key) => write!(f, "Failed to add key: {:?}", key),
            Self::Selection(pubkey) => write!(f, "Failed to select key: {:?}", pubkey),
            Self::Removal(key) => write!(f, "Failed to remove key: {:?}", key),
            Self::OSError(e) => write!(f, "OS had an error: {:?}", e),
        }
    }
}

impl std::error::Error for KeyStorageError {}

pub fn get_key_storage(storage_settings: StorageSettings) -> KeyStorageType {
    if cfg!(target_os = "macos") {
        #[cfg(target_os = "macos")]
        return KeyStorageType::MacOS(storage_settings);
    }

    if cfg!(target_os = "linux") {
        #[cfg(target_os = "linux")]
        return KeyStorageType::Linux(storage_settings);
    }

    KeyStorageType::None
}
