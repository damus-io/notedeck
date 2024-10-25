use enostr::{Keypair, Pubkey};

use super::file_key_storage::FileKeyStorage;
use crate::Error;

#[cfg(target_os = "macos")]
use super::security_framework_key_storage::SecurityFrameworkKeyStorage;

#[derive(Debug, PartialEq)]
pub enum KeyStorageType {
    None,
    FileSystem(FileKeyStorage),
    #[cfg(target_os = "macos")]
    SecurityFramework(SecurityFrameworkKeyStorage),
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum KeyStorageResponse<R> {
    Waiting,
    ReceivedResult(Result<R, KeyStorageError>),
}

impl<R: PartialEq> PartialEq for KeyStorageResponse<R> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyStorageResponse::Waiting, KeyStorageResponse::Waiting) => true,
            (
                KeyStorageResponse::ReceivedResult(Ok(r1)),
                KeyStorageResponse::ReceivedResult(Ok(r2)),
            ) => r1 == r2,
            (
                KeyStorageResponse::ReceivedResult(Err(_)),
                KeyStorageResponse::ReceivedResult(Err(_)),
            ) => true,
            _ => false,
        }
    }
}

impl KeyStorageType {
    pub fn get_keys(&self) -> KeyStorageResponse<Vec<Keypair>> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(Vec::new())),
            Self::FileSystem(f) => f.get_keys(),
            #[cfg(target_os = "macos")]
            Self::SecurityFramework(f) => f.get_keys(),
        }
    }

    pub fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.add_key(key),
            #[cfg(target_os = "macos")]
            Self::SecurityFramework(f) => f.add_key(key),
        }
    }

    pub fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.remove_key(key),
            #[cfg(target_os = "macos")]
            Self::SecurityFramework(f) => f.remove_key(key),
        }
    }

    pub fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(None)),
            Self::FileSystem(f) => f.get_selected_key(),
            #[cfg(target_os = "macos")]
            Self::SecurityFramework(_) => unimplemented!(),
        }
    }

    pub fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.select_key(key),
            #[cfg(target_os = "macos")]
            Self::SecurityFramework(_) => unimplemented!(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum KeyStorageError {
    Retrieval(Error),
    Addition(Error),
    Selection(Error),
    Removal(Error),
    OSError(Error),
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
