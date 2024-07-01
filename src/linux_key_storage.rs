#![cfg(target_os = "linux")]

use enostr::{Keypair, SerializableKeypair};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::{env, fs::File};

use crate::key_storage::{KeyStorage, KeyStorageError, KeyStorageResponse};
use tracing::debug;

enum LinuxKeyStorageType {
    BasicFileStorage,
    // TODO(kernelkind): could use the secret service api, and maybe even allow password manager integration via a settings menu
}

pub struct LinuxKeyStorage {}

// TODO(kernelkind): read from settings instead of hard-coding
static USE_MECHANISM: LinuxKeyStorageType = LinuxKeyStorageType::BasicFileStorage;

impl LinuxKeyStorage {
    pub fn new() -> Self {
        Self {}
    }
}

impl KeyStorage for LinuxKeyStorage {
    fn get_keys(&self) -> KeyStorageResponse<Vec<enostr::Keypair>> {
        match USE_MECHANISM {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().get_keys(),
        }
    }

    fn add_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        match USE_MECHANISM {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().add_key(key),
        }
    }

    fn remove_key(&self, key: &enostr::Keypair) -> KeyStorageResponse<()> {
        match USE_MECHANISM {
            LinuxKeyStorageType::BasicFileStorage => BasicFileStorage::new().remove_key(key),
        }
    }
}

struct BasicFileStorage {
    credential_dir_name: String,
}

impl BasicFileStorage {
    pub fn new() -> Self {
        Self {
            credential_dir_name: ".credentials".to_string(),
        }
    }

    fn mock() -> Self {
        Self {
            credential_dir_name: ".credentials_test".to_string(),
        }
    }

    fn get_cred_dirpath(&self) -> Result<PathBuf, KeyStorageError> {
        let home_dir = env::var("HOME")
            .map_err(|_| KeyStorageError::OSError("HOME env variable not set".to_string()))?;
        let home_path = std::path::PathBuf::from(home_dir);
        let project_path_str = "notedeck";

        let config_path = {
            if let Some(xdg_config_str) = env::var_os("XDG_CONFIG_HOME") {
                let xdg_path = PathBuf::from(xdg_config_str);
                let xdg_path_config = if xdg_path.is_absolute() {
                    xdg_path
                } else {
                    home_path.join(".config")
                };
                xdg_path_config.join(project_path_str)
            } else {
                home_path.join(format!(".{}", project_path_str))
            }
        }
        .join(self.credential_dir_name.clone());

        std::fs::create_dir_all(&config_path).map_err(|_| {
            KeyStorageError::OSError(format!(
                "could not create config path: {}",
                config_path.display()
            ))
        })?;

        Ok(config_path)
    }

    fn add_key_internal(&self, key: &Keypair) -> Result<(), KeyStorageError> {
        let mut file_path = self.get_cred_dirpath()?;
        file_path.push(format!("{}", &key.pubkey));

        let mut file = File::create(file_path)
            .map_err(|_| KeyStorageError::Addition("could not create or open file".to_string()))?;

        let json_str = serde_json::to_string(&SerializableKeypair::from_keypair(key, "", 7))
            .map_err(|e| KeyStorageError::Addition(e.to_string()))?;
        file.write_all(json_str.as_bytes()).map_err(|_| {
            KeyStorageError::Addition("could not write keypair to file".to_string())
        })?;

        Ok(())
    }

    fn get_keys_internal(&self) -> Result<Vec<Keypair>, KeyStorageError> {
        let file_path = self.get_cred_dirpath()?;
        let mut keys: Vec<Keypair> = Vec::new();

        if !file_path.is_dir() {
            return Err(KeyStorageError::Retrieval(
                "path is not a directory".to_string(),
            ));
        }

        let dir = fs::read_dir(file_path).map_err(|_| {
            KeyStorageError::Retrieval("problem accessing credentials directory".to_string())
        })?;

        for entry in dir {
            let entry = entry.map_err(|_| {
                KeyStorageError::Retrieval("problem accessing crediential file".to_string())
            })?;

            let path = entry.path();

            if path.is_file() {
                if let Some(path_str) = path.to_str() {
                    debug!("key path {}", path_str);
                    let json_string = fs::read_to_string(path_str).map_err(|e| {
                        KeyStorageError::OSError(format!("File reading problem: {}", e))
                    })?;
                    let key: SerializableKeypair =
                        serde_json::from_str(&json_string).map_err(|e| {
                            KeyStorageError::OSError(format!(
                                "Deserialization problem: {}",
                                (e.to_string().as_str())
                            ))
                        })?;
                    keys.push(key.to_keypair(""))
                }
            }
        }

        Ok(keys)
    }

    fn remove_key_internal(&self, key: &Keypair) -> Result<(), KeyStorageError> {
        let path = self.get_cred_dirpath()?;

        let filepath = path.join(key.pubkey.to_string());

        if filepath.exists() && filepath.is_file() {
            fs::remove_file(&filepath)
                .map_err(|e| KeyStorageError::OSError(format!("failed to remove file: {}", e)))?;
        }

        Ok(())
    }
}

impl KeyStorage for BasicFileStorage {
    fn get_keys(&self) -> crate::key_storage::KeyStorageResponse<Vec<enostr::Keypair>> {
        KeyStorageResponse::ReceivedResult(self.get_keys_internal())
    }

    fn add_key(&self, key: &enostr::Keypair) -> crate::key_storage::KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.add_key_internal(key))
    }

    fn remove_key(&self, key: &enostr::Keypair) -> crate::key_storage::KeyStorageResponse<()> {
        KeyStorageResponse::ReceivedResult(self.remove_key_internal(key))
    }
}

mod tests {
    use crate::key_storage::{KeyStorage, KeyStorageResponse};

    use super::BasicFileStorage;

    #[test]
    fn test_basic() {
        let kp = enostr::FullKeypair::generate().to_keypair();
        let resp = BasicFileStorage::mock().add_key(&kp);

        assert_eq!(resp, KeyStorageResponse::ReceivedResult(Ok(())));
        assert_num_storage(1);

        let resp = BasicFileStorage::mock().remove_key(&kp);
        assert_eq!(resp, KeyStorageResponse::ReceivedResult(Ok(())));
        assert_num_storage(0);
    }

    #[allow(dead_code)]
    fn assert_num_storage(n: usize) {
        let resp = BasicFileStorage::mock().get_keys();

        if let KeyStorageResponse::ReceivedResult(Ok(vec)) = resp {
            assert_eq!(vec.len(), n);
            return;
        }
        panic!();
    }
}
