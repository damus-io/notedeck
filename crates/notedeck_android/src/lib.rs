//! The shared library the Android app loads: `libnotedeck_android.so`,
//! named by `android.app.lib_name` in
//! `crates/notedeck_chrome/android/app/src/main/AndroidManifest.xml`.
//!
//! It has no code of its own. Linking `notedeck_chrome` exports its
//! `#[no_mangle] android_main`, along with the JNI entry points of the crates
//! under it (`notedeck`'s `Java_com_damus_notedeck_*`, android-activity's
//! `GameActivity_onCreate`), as the chrome's own cdylib used to. Off Android
//! it is empty.

// `extern crate`, not a `use`: nothing here names the chrome, and rustc
// links a dependency only when the crate loads it.
#[cfg(target_os = "android")]
extern crate notedeck_chrome;
