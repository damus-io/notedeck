use enostr::FullKeypair;

#[cfg(target_os = "macos")]
use crate::macos_key_storage::MacOSKeyStorage;

#[cfg(target_os = "macos")]
pub const SERVICE_NAME: &str = "Notedeck";

pub enum KeyStorage {
    None,
    #[cfg(target_os = "macos")]
    MacOS,
    // TODO:
    // Linux,
    // Windows,
    // Android,
}

impl KeyStorage {
    pub fn get_keys(&self) -> Result<Vec<FullKeypair>, KeyStorageError> {
        match self {
            Self::None => Ok(Vec::new()),
            #[cfg(target_os = "macos")]
            Self::MacOS => Ok(MacOSKeyStorage::new(SERVICE_NAME).get_all_fullkeypairs()),
        }
    }

    pub fn add_key(&self, key: &FullKeypair) -> Result<(), KeyStorageError> {
        let _ = key;
        match self {
            Self::None => Ok(()),
            #[cfg(target_os = "macos")]
            Self::MacOS => MacOSKeyStorage::new(SERVICE_NAME).add_key(key),
        }
    }

    pub fn remove_key(&self, key: &FullKeypair) -> Result<(), KeyStorageError> {
        let _ = key;
        match self {
            Self::None => Ok(()),
            #[cfg(target_os = "macos")]
            Self::MacOS => MacOSKeyStorage::new(SERVICE_NAME).delete_key(&key.pubkey),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum KeyStorageError {
    Retrieval,
    Addition(String),
    Removal(String),
    UnsupportedPlatform,
}

impl std::fmt::Display for KeyStorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Retrieval => write!(f, "Failed to retrieve keys."),
            Self::Addition(key) => write!(f, "Failed to add key: {:?}", key),
            Self::Removal(key) => write!(f, "Failed to remove key: {:?}", key),
            Self::UnsupportedPlatform => write!(
                f,
                "Attempted to use a key storage impl from an unsupported platform."
            ),
        }
    }
}

impl std::error::Error for KeyStorageError {}
