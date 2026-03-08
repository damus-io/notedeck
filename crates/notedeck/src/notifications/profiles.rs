//! Profile utilities for notifications.
//!
//! Provides functions for resolving @npub mentions to display names.
//! Profile lookups are done via nostrdb in the main event loop before
//! sending NotificationData to the worker.

use super::types::CachedProfile;
use bech32::Bech32;
use std::collections::HashMap;

/// Decode a bech32 npub to hex pubkey.
///
/// Returns None if decoding fails.
pub fn decode_npub(npub: &str) -> Option<String> {
    use bech32::primitives::decode::CheckedHrpstring;

    if !npub.starts_with("npub1") {
        return None;
    }

    let checked = CheckedHrpstring::new::<Bech32>(npub).ok()?;
    if checked.hrp().as_str() != "npub" {
        return None;
    }

    let data: Vec<u8> = checked.byte_iter().collect();
    if data.len() != 32 {
        return None;
    }

    Some(hex::encode(data))
}

/// Extract hex pubkeys from nostr:npub mentions in content.
pub fn extract_mentioned_pubkeys(content: &str) -> Vec<String> {
    let mut pubkeys = Vec::new();
    let mut search_start = 0;

    while let Some(pos) = content[search_start..].find("nostr:npub1") {
        let abs_pos = search_start + pos;
        let after_prefix = abs_pos + 11;

        let npub_end = content[after_prefix..]
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .map(|p| after_prefix + p)
            .unwrap_or(content.len());

        let npub = &content[abs_pos + 6..npub_end];

        if let Some(hex_pubkey) = decode_npub(npub) {
            pubkeys.push(hex_pubkey);
        }

        search_start = npub_end;
    }

    pubkeys
}

/// Resolve nostr:npub mentions in content to display names.
///
/// Looks up profiles from the provided cache and replaces `nostr:npub1...`
/// references with `@name`. Falls back to shortened npub if profile name
/// is not available.
#[profiling::function]
pub fn resolve_mentions(content: &str, profile_cache: &HashMap<String, CachedProfile>) -> String {
    let mut result = content.to_string();
    let mut search_start = 0;

    while let Some(pos) = result[search_start..].find("nostr:npub1") {
        let abs_pos = search_start + pos;
        let after_prefix = abs_pos + 11;

        let npub_end = result[after_prefix..]
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .map(|p| after_prefix + p)
            .unwrap_or(result.len());

        let npub = &result[abs_pos + 6..npub_end];

        let short_npub = format!("@{}...", &npub[..npub.len().min(12)]);
        let replacement = match decode_npub(npub)
            .and_then(|hex| profile_cache.get(&hex))
            .and_then(|p| p.name.as_ref())
        {
            Some(name) => format!("@{}", name),
            None => short_npub,
        };

        result = format!(
            "{}{}{}",
            &result[..abs_pos],
            replacement,
            &result[npub_end..]
        );
        search_start = abs_pos + replacement.len();
    }

    result
}
