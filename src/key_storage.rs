use nostr_sdk::Keys;

pub enum KeyStorage {
    None,
    // TODO:
    // Linux,
    // Windows,
    // Android,
}

impl KeyStorage {
    pub fn get_keys(&self) -> Result<Vec<Keys>, KeyStorageError> {
        match self {
            Self::None => Ok(Vec::new()),
        }
    }

    pub fn add_key(&self, key: &Keys) -> Result<(), KeyStorageError> {
        match self {
            Self::None => Ok(()),
        }
    }

    pub fn remove_key(&self, key: &Keys) -> Result<(), KeyStorageError> {
        match self {
            Self::None => Ok(()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum KeyStorageError<'a> {
    Retrieval,
    Addition(&'a Keys),
    Removal(&'a Keys),
}

impl std::fmt::Display for KeyStorageError<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Retrieval => write!(f, "Failed to retrieve keys."),
            Self::Addition(key) => write!(f, "Failed to add key: {:?}", key.public_key()),
            Self::Removal(key) => write!(f, "Failed to remove key: {:?}", key.public_key()),
        }
    }
}

impl std::error::Error for KeyStorageError<'_> {}
