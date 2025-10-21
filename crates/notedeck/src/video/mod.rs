#[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
mod desktop;

#[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
pub use desktop::*;

#[cfg(any(target_os = "windows", target_os = "android"))]
mod stub;

#[cfg(any(target_os = "windows", target_os = "android"))]
pub use stub::*;
