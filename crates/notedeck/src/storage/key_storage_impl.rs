use enostr::{Keypair, Pubkey};

use super::file_key_storage::FileKeyStorage;
use crate::Result;

#[derive(Debug, PartialEq)]
pub enum KeyStorageType {
    None,
    FileSystem(FileKeyStorage),
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum KeyStorageResponse<R> {
    Waiting,
    ReceivedResult(Result<R>),
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
        }
    }

    pub fn add_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.add_key(key),
        }
    }

    pub fn remove_key(&self, key: &Keypair) -> KeyStorageResponse<()> {
        let _ = key;
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.remove_key(key),
        }
    }

    pub fn get_selected_key(&self) -> KeyStorageResponse<Option<Pubkey>> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(None)),
            Self::FileSystem(f) => f.get_selected_key(),
        }
    }

    pub fn select_key(&self, key: Option<Pubkey>) -> KeyStorageResponse<()> {
        match self {
            Self::None => KeyStorageResponse::ReceivedResult(Ok(())),
            Self::FileSystem(f) => f.select_key(key),
        }
    }
}
