//! Profile caching and mention resolution for notifications.
//!
//! Handles fetching and caching author profiles for notification display,
//! and resolving @npub mentions to display names.

use super::types::{CachedProfile, ExtractedEvent, WorkerState};
use bech32::Bech32;
use enostr::Pubkey;
use nostrdb::Filter;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Subscription ID prefix for profile requests.
const SUB_PROFILES: &str = "notedeck_profiles";

/// Extract profile info (name and picture URL) from profile content JSON.
///
/// Prefers "display_name" over "name" for the name field.
pub fn extract_profile_info(content: &str) -> CachedProfile {
    // Log content length only to avoid exposing PII
    debug!("Parsing profile content ({} chars)", content.len());

    let value: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse profile JSON: {}", e);
            return CachedProfile::default();
        }
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            warn!("Profile content is not a JSON object");
            return CachedProfile::default();
        }
    };

    // Log key count only, not actual keys which may reveal profile structure
    debug!("Profile has {} keys", obj.keys().len());

    // Prefer display_name, fall back to name (handle empty strings properly)
    let display_name_str = obj
        .get("display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let name_str = obj
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // Log presence only, not actual values to protect privacy
    debug!(
        "Profile fields: display_name={}, name={}",
        display_name_str.is_some(),
        name_str.is_some()
    );

    let name = display_name_str.or(name_str).map(|s| s.to_string());

    // Get picture URL
    let picture_url = obj
        .get("picture")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && (s.starts_with("http://") || s.starts_with("https://")))
        .map(|s| s.to_string());

    CachedProfile { name, picture_url }
}

/// Handle a kind 0 (profile metadata) event by extracting and caching profile info.
///
/// Always cleans up the subscription and requested set, even if profile is empty,
/// to prevent unbounded subscription growth.
pub fn handle_profile_event(state: &mut WorkerState, event: &ExtractedEvent) {
    // Always clean up subscription and requested set, regardless of profile content
    state.requested_profiles.remove(&event.pubkey);
    let sub_id = format!(
        "{}_{}",
        SUB_PROFILES,
        event.pubkey.get(..16).unwrap_or(&event.pubkey)
    );
    state.pool.unsubscribe(sub_id);

    let profile = extract_profile_info(&event.content);
    if profile.name.is_none() && profile.picture_url.is_none() {
        debug!(
            "Empty profile for {}, skipping cache",
            event.pubkey.get(..8).unwrap_or(&event.pubkey)
        );
        return;
    }

    // Safe preview for logging (pubkeys are hex, but guard against malformed input)
    let pubkey_preview = event.pubkey.get(..8).unwrap_or(&event.pubkey);
    debug!(
        "Cached profile for {}: name={:?}, picture={:?}",
        pubkey_preview,
        profile.name,
        profile.picture_url.as_ref().map(|s| &s[..s.len().min(50)])
    );

    state.profile_cache.insert(event.pubkey.clone(), profile);

    // Prune cache if too large - also clear requested_profiles to allow re-requests
    if state.profile_cache.len() > 1000 {
        state.profile_cache.clear();
        state.requested_profiles.clear();
        debug!("Pruned profile cache and requested set");
    }
}

/// Request profile for the given pubkey if not already requested.
///
/// Uses unique subscription IDs per pubkey to avoid overwriting previous requests.
pub fn request_profile_if_needed(state: &mut WorkerState, pubkey: &str) {
    // Don't request if already requested or cached
    if state.requested_profiles.contains(pubkey) {
        return;
    }
    if state.profile_cache.contains_key(pubkey) {
        return;
    }

    // Limit pending requests to avoid too many open subscriptions
    if state.requested_profiles.len() >= 50 {
        let pubkey_preview = pubkey.get(..8).unwrap_or(pubkey);
        debug!(
            "Too many pending profile requests, skipping {}",
            pubkey_preview
        );
        return;
    }

    state.requested_profiles.insert(pubkey.to_string());

    // Parse pubkey and subscribe to profile
    let pubkey_bytes = match Pubkey::from_hex(pubkey) {
        Ok(pk) => pk,
        Err(_) => return,
    };

    let profile_filter = Filter::new()
        .kinds([0])
        .authors([pubkey_bytes.bytes()])
        .limit(1)
        .build();

    // Use unique subscription ID per pubkey to avoid overwriting previous requests
    // Safe slicing with fallback to full string for malformed input
    let sub_id = format!("{}_{}", SUB_PROFILES, pubkey.get(..16).unwrap_or(pubkey));
    state.pool.subscribe(sub_id, vec![profile_filter]);
    debug!(
        "Requested profile for {}",
        pubkey.get(..8).unwrap_or(pubkey)
    );
}

/// Decode a bech32 npub to hex pubkey.
///
/// Returns None if decoding fails.
pub fn decode_npub(npub: &str) -> Option<String> {
    use bech32::primitives::decode::CheckedHrpstring;

    // npub must start with "npub1"
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
/// Looks up profiles from cache and replaces `nostr:npub1...` references with `@name`.
/// Falls back to shortened npub if profile name is not cached.
#[profiling::function]
pub fn resolve_mentions(content: &str, profile_cache: &HashMap<String, CachedProfile>) -> String {
    let mut result = content.to_string();
    let mut search_start = 0;

    // Find all nostr:npub1... patterns
    while let Some(pos) = result[search_start..].find("nostr:npub1") {
        let abs_pos = search_start + pos;
        let after_prefix = abs_pos + 11; // length of "nostr:npub1"

        // Find end of npub (bech32 chars are lowercase alphanumeric)
        let npub_end = result[after_prefix..]
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .map(|p| after_prefix + p)
            .unwrap_or(result.len());

        let npub = &result[abs_pos + 6..npub_end]; // skip "nostr:"

        // Decode npub to hex pubkey and look up profile
        let replacement = if let Some(hex_pubkey) = decode_npub(npub) {
            if let Some(profile) = profile_cache.get(&hex_pubkey) {
                if let Some(name) = &profile.name {
                    format!("@{}", name)
                } else {
                    // Fallback: shorten npub
                    format!("@{}...", &npub[..npub.len().min(12)])
                }
            } else {
                format!("@{}...", &npub[..npub.len().min(12)])
            }
        } else {
            format!("@{}...", &npub[..npub.len().min(12)])
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
