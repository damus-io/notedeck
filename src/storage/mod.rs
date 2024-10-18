#[cfg(any(target_os = "linux", target_os = "macos"))]
mod file_key_storage;
mod file_storage;

pub use file_key_storage::FileKeyStorage;
pub use file_storage::FileWriterFactory;
pub use file_storage::FileWriterType;

#[cfg(target_os = "macos")]
mod security_framework_key_storage;

pub mod key_storage_impl;
pub use key_storage_impl::{KeyStorage, KeyStorageResponse, KeyStorageType};
