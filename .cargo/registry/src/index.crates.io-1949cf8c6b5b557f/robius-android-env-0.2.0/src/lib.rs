//! Abstractions for Rust access to Android state (native Java objects)
//! managed by UI toolkits.
//!
//! # Usage of this crate
//! This crate exists for two kinds of downstream users:
//! 1. The UI toolkit that exposes its key internal states that hold
//!    the current Android activity being displayed and the Java VM / JNI environment.
//!    Either the UI toolkit or the app itself should set these states on startup,
//!    either by using [ndk-context] or by activating a feature for a specific UI toolkit.
//! 2. The platform feature "middleware" crates that need to access the current activity
//!    and JNI environment from Rust code in order to interact with the Android platform.
//!
//! ## Supported UI toolkits
//! * [Makepad]: enable the `makepad` Cargo feature.
//! * UI toolkits compatible with [ndk-context]: supported by default.
//! * Others coming soon! (in the meantime, see below)
//!
//! ## Usage of this crate for other UI toolkits
//! For any other UI toolkits that support [ndk-context], you don't need to enable any cargo features.
//! However, either your application code or the UI toolkit must manually initialize the Android context
//! owned by [ndk-context], i.e., by invoking [`initialize_android_context()`](https://docs.rs/ndk-context/latest/ndk_context/fn.initialize_android_context.html).
//! Some UI toolkits automatically do this for you, typically via the [ndk-glue] crate.
//!
//! [Makepad]: https://github.com/makepad/makepad/
//! [ndk-context]: https://docs.rs/ndk-context/latest/ndk_context/
//! [ndk-glue]: https://crates.io/crates/ndk-glue

#[cfg_attr(feature = "makepad", path = "makepad.rs")]
#[cfg_attr(not(feature = "makepad"), path = "ndk_context.rs")]
mod inner;

#[cfg(all(not(feature = "makepad"), not(feature = "ndk-context")))]
compile_error!("Must enable either 'makepad' or 'ndk-context' feature");

// Re-export all types that downstream users might need to instantiate.
pub use jni::{JavaVM, JNIEnv, objects::JObject, errors::{Result, Error, JniError}};

/// Re-exports of low-level pointer types from the `jni-sys` crate.
pub mod sys {
    pub use jni::sys::{jobject, JNIEnv};
}

/// Invokes the given closure `f` with the current JNI environment
/// and the current activity.
///
/// Returns `None` upon error, including:
/// * If the function that gets the current activity and JNI environment
///   has not been set.
/// * If the current JNI environment cannot be obtained.
pub fn with_activity<F, R>(f: F) -> Result<R>
where
    F: for<'a, 'b, 'c, 'd> FnOnce(&'a mut JNIEnv<'b>, &'c JObject<'d>) -> R,
{
    inner::with_activity_inner(f)
}
