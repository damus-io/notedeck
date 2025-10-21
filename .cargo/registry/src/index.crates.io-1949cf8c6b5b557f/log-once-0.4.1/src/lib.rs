#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::new_without_default, clippy::new_without_default)]
#![allow(clippy::useless_attribute, clippy::missing_docs_in_private_items)]
#![allow(clippy::use_self)]

//! Collection of helper macros for logging some events only once.
//!
//! This crate provide macro in the `log_once` family (`warn_once!`,
//! `trace_once!`, ...); that only send a logging event once for every message.
//! It rely and uses the logging infrastructure in the [log][log] crate; and
//! is fully compatible with any logger implementation.
//!
//! These macro will store the already seen messages in a static `BTreeSet`, and
//! check if a message is in the set before sending the log event.
//!
//! [log]: https://crates.io/crates/log
//!
//! # Examples
//!
//! ```rust
//! use log::info;
//! use log_once::{info_once, warn_once};
//!
//! # #[derive(Debug)] pub struct Yak(String);
//! # impl Yak { fn shave(&self, _: u32) {} }
//! # fn find_a_razor() -> Result<u32, u32> { Ok(1) }
//! pub fn shave_the_yak(yaks: &[Yak]) {
//!     for yak in yaks {
//!         info!(target: "yak_events", "Commencing yak shaving for {yak:?}");
//!
//!         loop {
//!             match find_a_razor() {
//!                 Ok(razor) => {
//!                     // This will only appear once in the logger output for each razor
//!                     info_once!("Razor located: {razor}");
//!                     yak.shave(razor);
//!                     break;
//!                 }
//!                 Err(err) => {
//!                     // This will only appear once in the logger output for each error
//!                     warn_once!("Unable to locate a razor: {err}, retrying");
//!                 }
//!             }
//!         }
//!     }
//! }
//!
//! # fn main() {}
//! ```

// We re-export the log crate so that the log_once macros can use it directly.
// That way users don't need to depend on `log` explicitly.
// This is especially nice for people who use `tracing` for logging, but still use `log_once`.
pub use log;

pub use log::Level;

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[doc(hidden)]
pub struct MessagesSet {
    inner: Mutex<BTreeSet<String>>,
}

impl MessagesSet {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeSet::new()),
        }
    }

    /// # Errors
    /// Mutex poisoning.
    pub fn lock(
        &self,
    ) -> Result<MutexGuard<BTreeSet<String>>, PoisonError<MutexGuard<BTreeSet<String>>>> {
        self.inner.lock()
    }
}

/// Standard logging macro, logging events once for each arguments.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// This macro will generically log with the specified `Level` and `format!`
/// based argument list.
///
/// The `max_level_*` features can be used to statically disable logging at
/// various levels.
#[macro_export]
macro_rules! log_once {
    (@CREATE STATIC) => ({
        use ::std::sync::Once;
        static mut SEEN_MESSAGES: *const $crate::MessagesSet = 0 as *const _;
        static ONCE: Once = Once::new();
        unsafe {
            ONCE.call_once(|| {
                let singleton = $crate::MessagesSet::new();
                SEEN_MESSAGES = ::std::mem::transmute(Box::new(singleton));
            });
            &(*SEEN_MESSAGES)
        }
    });

    // log_once!(target: "my_target", Level::Info, "Some {}", "logging")
    (target: $target:expr, $lvl:expr, $($arg:tt)+) => ({
        let message = format!($($arg)+);
        let seen_messages_mutex = $crate::log_once!(@CREATE STATIC);
        let mut seen_messages_lock = seen_messages_mutex.lock().expect("Mutex was poisoned");
        let event = String::from(stringify!($target)) + stringify!($lvl) + message.as_ref();
        if seen_messages_lock.insert(event) {
            $crate::log::log!(target: $target, $lvl, "{}", message);
        }
    });

    // log_once!(Level::Info, "Some {}", "logging")
    ($lvl:expr, $($arg:tt)+) => ($crate::log_once!(target: module_path!(), $lvl,  $($arg)+));
}

/// Logs a message once at the error level.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// Logging at this level is disabled if the `max_level_off` feature is present.
#[macro_export]
macro_rules! error_once {
    (target: $target:expr, $($arg:tt)*) => (
        $crate::log_once!(target: $target, $crate::Level::Error, $($arg)*);
    );
    ($($arg:tt)*) => (
        $crate::log_once!($crate::Level::Error, $($arg)*);
    )
}

/// Logs a message once at the warn level.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// Logging at this level is disabled if any of the following features are
/// present: `max_level_off` or `max_level_error`.
///
/// When building in release mode (i.e., without the `debug_assertions` option),
/// logging at this level is also disabled if any of the following features are
/// present: `release_max_level_off` or `max_level_error`.
#[macro_export]
macro_rules! warn_once {
    (target: $target:expr, $($arg:tt)*) => (
        $crate::log_once!(target: $target, $crate::Level::Warn, $($arg)*);
    );
    ($($arg:tt)*) => (
        $crate::log_once!($crate::Level::Warn, $($arg)*);
    )
}

/// Logs a message once at the info level.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// Logging at this level is disabled if any of the following features are
/// present: `max_level_off`, `max_level_error`, or `max_level_warn`.
///
/// When building in release mode (i.e., without the `debug_assertions` option),
/// logging at this level is also disabled if any of the following features are
/// present: `release_max_level_off`, `release_max_level_error`, or
/// `release_max_level_warn`.
#[macro_export]
macro_rules! info_once {
    (target: $target:expr, $($arg:tt)*) => (
        $crate::log_once!(target: $target, $crate::Level::Info, $($arg)*);
    );
    ($($arg:tt)*) => (
        $crate::log_once!($crate::Level::Info, $($arg)*);
    )
}

/// Logs a message once at the debug level.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// Logging at this level is disabled if any of the following features are
/// present: `max_level_off`, `max_level_error`, `max_level_warn`, or
/// `max_level_info`.
///
/// When building in release mode (i.e., without the `debug_assertions` option),
/// logging at this level is also disabled if any of the following features are
/// present: `release_max_level_off`, `release_max_level_error`,
/// `release_max_level_warn`, or `release_max_level_info`.
#[macro_export]
macro_rules! debug_once {
    (target: $target:expr, $($arg:tt)*) => (
        $crate::log_once!(target: $target, $crate::Level::Debug, $($arg)*);
    );
    ($($arg:tt)*) => (
        $crate::log_once!($crate::Level::Debug, $($arg)*);
    )
}

/// Logs a message once at the trace level.
///
/// The log event will only be emitted once for each combinaison of target/arguments.
///
/// Logging at this level is disabled if any of the following features are
/// present: `max_level_off`, `max_level_error`, `max_level_warn`,
/// `max_level_info`, or `max_level_debug`.
///
/// When building in release mode (i.e., without the `debug_assertions` option),
/// logging at this level is also disabled if any of the following features are
/// present: `release_max_level_off`, `release_max_level_error`,
/// `release_max_level_warn`, `release_max_level_info`, or
/// `release_max_level_debug`.
#[macro_export]
macro_rules! trace_once {
    (target: $target:expr, $($arg:tt)*) => (
        $crate::log_once!(target: $target, $crate::Level::Trace, $($arg)*);
    );
    ($($arg:tt)*) => (
        $crate::log_once!($crate::Level::Trace, $($arg)*);
    )
}

#[cfg(test)]
mod tests {
    use log::{LevelFilter, Log, Metadata, Record};
    use std::cell::Cell;
    use std::sync::Once;

    struct SimpleLogger;
    impl Log for SimpleLogger {
        fn enabled(&self, _: &Metadata) -> bool {
            true
        }
        fn log(&self, _: &Record) {}
        fn flush(&self) {}
    }

    static LOGGER: SimpleLogger = SimpleLogger;

    #[test]
    fn called_once() {
        static START: Once = Once::new();
        START.call_once(|| {
            log::set_logger(&LOGGER).expect("Could not set the logger");
            log::set_max_level(LevelFilter::Trace);
        });

        let counter = Cell::new(0);
        let function = || {
            counter.set(counter.get() + 1);
            counter.get()
        };

        info_once!("Counter is: {}", function());
        assert_eq!(counter.get(), 1);
    }
}
