//! A crate for opening URIs, e.g., URLs, `tel:`, `mailto:`, `file://`, etc.
//!
//! ```
//! # use robius_open::Uri;
//! Uri::new("tel:+61 123 456 789")
//!     .open()
//!     .expect("failed to open telephone URI");
//! ```
//!
//! Supports:
//! - macOS (`NSWorkspace`)
//! - Android (`android/content/Intent`)
//! - Linux (`xdg-open`)
//! - Windows (`start`)
//! - iOS (`UIApplication`)
//!
//! # Android
//! To use the library on Android, you must add the following to the app
//! manifest:
//! ```xml
//! <uses-permission android:name="android.permission.QUERY_ALL_PACKAGES"
//!     tools:ignore="QueryAllPackagesPermission" />
//!
//! <queries>
//!     <intent>
//!         <action android:name="android.intent.action.MAIN" />
//!     </intent>
//! </queries>
//! ```
//! or alternatively, disable the `android-result` feature. However, disabling
//! this feature will make [`Uri::open`] always return `Ok`, regardless of
//! whether the URI was successfully opened.
#![allow(clippy::result_unit_err)]

mod error;
mod sys;

pub use error::{Error, Result};

/// A uniform resource identifier.
pub struct Uri<'a, 'b> {
    inner: sys::Uri<'a, 'b>,
}

impl<'a, 'b> Uri<'a, 'b> {
    /// Constructs a new URI.
    pub fn new(s: &'a str) -> Self {
        Self {
            inner: sys::Uri::new(s),
        }
    }

    /// Sets the action to perform with this URI.
    ///
    /// This only has an effect on Android, and corresponds to an [action
    /// activity][aa]. By default, it is set to `"ACTION_VIEW"`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use robius_open::Uri;
    /// Uri::new("tel:+61 123 456 789")
    ///     .action("ACTION_DIAL")
    ///     .open()
    ///     .expect("failed to open telephone URI");
    /// ```
    ///
    /// [aa]: https://developer.android.com/reference/android/content/Intent#standard-activity-actions
    pub fn action(self, action: &'b str) -> Self {
        Self {
            inner: self.inner.action(action),
        }
    }

    /// Opens the URI.
    pub fn open(self) -> Result<()> {
        self.inner.open()
    }
}
