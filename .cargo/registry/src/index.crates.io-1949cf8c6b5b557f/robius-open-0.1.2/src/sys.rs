cfg_if::cfg_if! {
    if #[cfg(target_os = "android")] {
        mod android;
        pub(crate) use android::*;
    } else if #[cfg(target_os = "ios")] {
        mod ios;
        pub(crate) use ios::*;
    } else if #[cfg(target_os = "linux")] {
        mod linux;
        pub(crate) use linux::*;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
        pub(crate) use macos::*;
    } else if #[cfg(target_os = "windows")] {
        mod windows;
        pub(crate) use windows::*;
    } else {
        mod unsupported;
        pub(crate) use unsupported::*;
    }
}
