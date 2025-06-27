//! Internationalization (i18n) module for Notedeck
//!
//! This module provides localization support using fluent and fluent-resmgr.
//! It handles loading translation files, managing locales, and providing
//! localized strings throughout the application.

pub mod manager;

pub use manager::CacheStats;
pub use manager::LocalizationContext;
pub use manager::LocalizationManager;

/// Re-export commonly used types for convenience
pub use fluent::FluentArgs;
pub use fluent::FluentValue;
pub use unic_langid::LanguageIdentifier;

use md5;
use once_cell::sync::OnceCell;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::info;

/// Global localization manager for easy access from anywhere
static GLOBAL_I18N: OnceCell<Arc<LocalizationManager>> = OnceCell::new();

/// Cache for normalized FTL keys to avoid repeated normalization
static NORMALIZED_KEY_CACHE: OnceCell<Mutex<HashMap<String, String>>> = OnceCell::new();

/// Initialize the global localization context
pub fn init_global_i18n(context: LocalizationContext) {
    info!("Initializing global i18n context");
    let _ = GLOBAL_I18N.set(context.manager().clone());

    // Initialize the normalized key cache
    let _ = NORMALIZED_KEY_CACHE.set(Mutex::new(HashMap::new()));

    info!("Global i18n context initialized successfully");
}

/// Get the global localization manager
pub fn get_global_i18n() -> Option<Arc<LocalizationManager>> {
    GLOBAL_I18N.get().cloned()
}

fn simple_hash(s: &str) -> String {
    let digest = md5::compute(s.as_bytes());
    // Take the first 2 bytes and convert to 4 hex characters
    format!("{:02x}{:02x}", digest[0], digest[1])
}

pub fn normalize_ftl_key(key: &str, comment: Option<&str>) -> String {
    // Try to get from cache first
    let cache_key = if let Some(comment) = comment {
        format!("{key}:{comment}")
    } else {
        key.to_string()
    };

    if let Some(cache) = NORMALIZED_KEY_CACHE.get() {
        if let Ok(cache) = cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }
    }

    // Replace each invalid character with exactly one underscore
    // This matches the behavior of the Python extraction script
    let re = Regex::new(r"[^a-zA-Z0-9_-]").unwrap();
    let mut result = re.replace_all(key, "_").to_string();

    // Remove leading/trailing underscores
    result = result.trim_matches('_').to_string();

    // Ensure the key starts with a letter (Fluent requirement)
    if result.is_empty() || !result.chars().next().unwrap().is_ascii_alphabetic() {
        result = format!("k_{result}");
    }

    // If we have a comment, append a hash of it to reduce collisions
    if let Some(comment) = comment {
        let hash_str = format!("_{}", simple_hash(comment));
        result.push_str(&hash_str);
    }

    // Cache the result
    if let Some(cache) = NORMALIZED_KEY_CACHE.get() {
        if let Ok(mut cache) = cache.lock() {
            cache.insert(cache_key, result.clone());
        }
    }

    tracing::debug!(
        "normalize_ftl_key: original='{}', comment='{:?}', final='{}'",
        key,
        comment,
        result
    );
    result
}

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
    // Simple case: just message and comment
    ($message:expr, $comment:expr) => {
        {
            let norm_key = $crate::i18n::normalize_ftl_key($message, Some($comment));
            if let Some(i18n) = $crate::i18n::get_global_i18n() {
                let result = i18n.get_string(&norm_key);
                match result {
                    Ok(ref s) if s != $message => s.clone(),
                    _ => {
                        tracing::warn!("FALLBACK: Using key '{}' as string (not found in FTL)", $message);
                        $message.to_string()
                    }
                }
            } else {
                tracing::warn!("FALLBACK: Global i18n not initialized, using key '{}' as string", $message);
                $message.to_string()
            }
        }
    };

    // Case with named parameters: message, comment, param=value, ...
    ($message:expr, $comment:expr, $($param:ident = $value:expr),*) => {
        {
            let norm_key = $crate::i18n::normalize_ftl_key($message, Some($comment));
            if let Some(i18n) = $crate::i18n::get_global_i18n() {
                let mut args = $crate::i18n::FluentArgs::new();
                $(
                    args.set(stringify!($param), $value);
                )*
                match i18n.get_string_with_args(&norm_key, Some(&args)) {
                    Ok(s) => s,
                    Err(_) => {
                        // Fallback: replace placeholders with values
                        let mut result = $message.to_string();
                        $(
                            result = result.replace(&format!("{{{}}}", stringify!($param)), &$value.to_string());
                        )*
                        result
                    }
                }
            } else {
                // Fallback: replace placeholders with values
                let mut result = $message.to_string();
                $(
                    result = result.replace(&format!("{{{}}}", stringify!($param)), &$value.to_string());
                )*
                result
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
    ($one:expr, $other:expr, $comment:expr, $count:expr, $($param:ident = $value:expr),*) => {{
        let norm_key = $crate::i18n::normalize_ftl_key($other, Some($comment));
        if let Some(i18n) = $crate::i18n::get_global_i18n() {
            let mut args = $crate::i18n::FluentArgs::new();
            args.set("count", $count);
            $(args.set(stringify!($param), $value);)*
            match i18n.get_string_with_args(&norm_key, Some(&args)) {
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
        } else {
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
    }};
    // Without named parameters
    ($one:expr, $other:expr, $comment:expr, $count:expr) => {{
        $crate::tr_plural!($one, $other, $comment, $count, )
    }};
}
