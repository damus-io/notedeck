#[cfg(any(target_os = "linux", target_os = "macos"))]
mod file_key_storage;
mod file_storage;

pub use file_key_storage::FileKeyStorage;
pub use file_storage::write_file;
pub use file_storage::DataPaths;
pub use file_storage::Directory;

#[cfg(target_os = "macos")]
mod security_framework_key_storage;

pub mod key_storage_impl;
pub use key_storage_impl::{KeyStorage, KeyStorageResponse, KeyStorageType};
