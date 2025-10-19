#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod desktop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use desktop::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "android")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "android")))]
pub use stub::*;
