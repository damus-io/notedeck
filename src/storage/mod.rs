#[cfg(target_os = "macos")]
mod macos_key_storage;
#[cfg(target_os = "macos")]
pub use macos_key_storage::MacOSKeyStorageType;
#[cfg(target_os = "macos")]
mod security_framework_key_storage;

#[cfg(target_os = "linux")]
pub use linux_key_storage::LinuxKeyStorageType;
#[cfg(target_os = "linux")]
mod linux_key_storage;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod file_key_storage;
mod file_storage;

pub mod key_storage_impl;
pub use key_storage_impl::{KeyStorage, KeyStorageResponse, KeyStorageType};
