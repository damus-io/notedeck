//! Internationalization (i18n) module for Notedeck
//!
//! This module provides localization support using fluent and fluent-resmgr.
//! It handles loading translation files, managing locales, and providing
//! localized strings throughout the application.

mod error;
mod key;
pub mod manager;

pub use error::IntlError;
pub use key::{IntlKey, IntlKeyBuf};

pub use manager::CacheStats;
pub use manager::Localization;
pub use manager::StringCacheResult;

/// Re-export commonly used types for convenience
pub use fluent::FluentArgs;
pub use fluent::FluentValue;
pub use unic_langid::LanguageIdentifier;

/// Macro for getting localized strings with format-like syntax
///
/// Syntax: tr!("message", comment)
///         tr!("message with {param}", comment, param="value")
///         tr!("message with {first} and {second}", comment, first="value1", second="value2")
///
/// The first argument is the source message (like format!).
/// The second argument is always the comment to provide context for translators.
/// If `{name}` placeholders are found, there must be corresponding named arguments after the comment.
/// All placeholders must be named and start with a letter (a-zA-Z).
#[macro_export]
macro_rules! tr {
    ($i18n:expr, $message:expr, $comment:expr) => {
        {
            let key = $i18n.normalized_ftl_key($message, $comment);
            match $i18n.get_string(key.borrow()) {
                Ok(r) => r,
                Err(_err) => {
                    $message.to_string()
                }
            }
        }
    };

    // Case with named parameters: message, comment, param=value, ...
    ($i18n:expr, $message:expr, $comment:expr, $($param:ident = $value:expr),*) => {
        {
            let key = $i18n.normalized_ftl_key($message, $comment);
            let mut args = $crate::i18n::FluentArgs::new();
            $(
                args.set(stringify!($param), $value);
            )*
            match $i18n.get_cached_string(key.borrow(), Some(&args)) {
                Ok(r) => r,
                Err(_) => {
                    // Fallback: replace placeholders with values
                    let mut result = $message.to_string();
                    $(
                        result = result.replace(&format!("{{{}}}", stringify!($param)), &$value.to_string());
                    )*
                    result
                }
            }
        }
    };
}

/// Macro for getting localized pluralized strings with count and named arguments
///
/// Syntax: tr_plural!(one, other, comment, count, param1=..., param2=...)
///   - one: Message for the singular ("one") plural rule
///   - other: Message for the "other" plural rule
///   - comment: Context for translators
///   - count: The count value
///   - named arguments: Any additional named parameters for interpolation
#[macro_export]
macro_rules! tr_plural {
    // With named parameters
    ($i18n:expr, $one:expr, $other:expr, $comment:expr, $count:expr, $($param:ident = $value:expr),*) => {{
        let norm_key = $i18n.normalized_ftl_key($other, $comment);
        let mut args = $crate::i18n::FluentArgs::new();
        args.set("count", $count);
        $(args.set(stringify!($param), $value);)*
        match $i18n.get_cached_string(norm_key.borrow(), Some(&args)) {
            Ok(s) => s,
            Err(_) => {
                // Fallback: use simple pluralization
                if $count == 1 {
                    let mut result = $one.to_string();
                    $(result = result.replace(&format!("{{{}}}", stringify!($param)), &$value.to_string());)*
                    result = result.replace("{count}", &$count.to_string());
                    result
                } else {
                    let mut result = $other.to_string();
                    $(result = result.replace(&format!("{{{}}}", stringify!($param)), &$value.to_string());)*
                    result = result.replace("{count}", &$count.to_string());
                    result
                }
            }
        }
    }};
    // Without named parameters
    ($one:expr, $other:expr, $comment:expr, $count:expr) => {{
        $crate::tr_plural!($one, $other, $comment, $count, )
    }};
}
