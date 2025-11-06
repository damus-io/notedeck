use enostr::{Pubkey, SecretKey};
use keyring::Entry;

use crate::{Error, Result};

const KEYRING_SERVICE_NAME: &str = "com.damus.notedeck";

type BackendResult<T> = std::result::Result<T, keyring::Error>;

#[derive(Clone, Debug)]
enum KeyringBackendType {
    OS(OsKeyringBackend),
    #[cfg(test)]
    Memory(MemoryKeyringBackend),
}

impl KeyringBackendType {
    pub fn set(&self, service: &str, account: &str, secret: &str) -> BackendResult<()> {
        match self {
            KeyringBackendType::OS(os_keyring_backend) => {
                os_keyring_backend.set(service, account, secret)
            }
            #[cfg(test)]
            KeyringBackendType::Memory(mem) => mem.set(service, account, secret),
        }
    }

    pub fn get(&self, service: &str, account: &str) -> BackendResult<Option<String>> {
        match self {
            KeyringBackendType::OS(os_keyring_backend) => os_keyring_backend.get(service, account),
            #[cfg(test)]
            KeyringBackendType::Memory(memory_keyring_backend) => {
                memory_keyring_backend.get(service, account)
            }
        }
    }

    pub fn delete(&self, service: &str, account: &str) -> BackendResult<()> {
        match self {
            KeyringBackendType::OS(os_keyring_backend) => {
                os_keyring_backend.delete(service, account)
            }
            #[cfg(test)]
            KeyringBackendType::Memory(memory_keyring_backend) => {
                memory_keyring_backend.delete(service, account)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct OsKeyringBackend;

impl OsKeyringBackend {
    fn set(&self, service: &str, account: &str, secret: &str) -> BackendResult<()> {
        let entry = Entry::new(service, account)?;
        entry.set_password(secret)
    }

    fn get(&self, service: &str, account: &str) -> BackendResult<Option<String>> {
        let entry = Entry::new(service, account)?;

        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn delete(&self, service: &str, account: &str) -> BackendResult<()> {
        let entry = Entry::new(service, account)?;

        match entry.delete_credential() {
            Ok(_) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[derive(Clone, Debug)]
pub struct KeyringStore {
    backend: KeyringBackendType,
}

impl KeyringStore {
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self {
            backend: KeyringBackendType::Memory(MemoryKeyringBackend::default()),
        }
    }

    pub fn store_secret(&self, pubkey: &Pubkey, secret: &SecretKey) -> Result<()> {
        let res = self
            .backend
            .set(
                KEYRING_SERVICE_NAME,
                &Self::account_id(pubkey),
                &secret.to_secret_hex(),
            )
            .map_err(Error::from);

        tracing::trace!("Store secret result: {res:?}");

        res
    }

    pub fn get_secret(&self, pubkey: &Pubkey) -> Result<Option<SecretKey>> {
        let maybe_secret = self
            .backend
            .get(KEYRING_SERVICE_NAME, &Self::account_id(pubkey))
            .map_err(Error::from);

        let secret_hex = match maybe_secret {
            Ok(m_secret) => {
                let Some(secret) = m_secret else {
                    tracing::trace!("Keyring gave us empty secret for {pubkey}");
                    return Ok(None);
                };
                tracing::trace!("Received an actual secret for {pubkey} successfully");
                secret
            }
            Err(e) => {
                tracing::trace!("Failed to retrieve secret for {pubkey}: {e}");
                return Err(e);
            }
        };

        let secret_key = SecretKey::from_hex(secret_hex).map_err(|err| {
            Error::Generic(format!(
                "invalid secret key from keyring for {}: {err}",
                Self::account_id(pubkey)
            ))
        })?;

        Ok(Some(secret_key))
    }

    pub fn remove_secret(&self, pubkey: &Pubkey) -> Result<()> {
        self.backend
            .delete(KEYRING_SERVICE_NAME, &Self::account_id(pubkey))
            .map_err(Error::from)
    }

    fn account_id(pubkey: &Pubkey) -> String {
        pubkey.hex()
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self {
            backend: KeyringBackendType::OS(OsKeyringBackend),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default, Debug)]
struct MemoryKeyringBackend {
    // RwLock to not make the KeyringBackendType api mutable... it's only for testing so it's ok
    entries: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<(String, String), String>>>,
}

#[cfg(test)]
impl MemoryKeyringBackend {
    fn set(&self, service: &str, account: &str, secret: &str) -> BackendResult<()> {
        self.entries
            .write()
            .unwrap()
            .insert((service.to_owned(), account.to_owned()), secret.to_owned());
        Ok(())
    }

    fn get(&self, service: &str, account: &str) -> BackendResult<Option<String>> {
        Ok(self
            .entries
            .read()
            .unwrap()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn delete(&self, service: &str, account: &str) -> BackendResult<()> {
        self.entries
            .write()
            .unwrap()
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}
