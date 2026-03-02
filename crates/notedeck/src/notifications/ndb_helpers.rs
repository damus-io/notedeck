//! Nostrdb-based helpers for notification event processing.
//!
//! Thin wrappers around existing `notedeck` utilities (`get_p_tags`,
//! `get_display_name`, `get_profile_url`) adapted for the notification
//! pipeline's specific needs (hex-encoded strings, `Option` picture URLs).

use crate::name::get_display_name;
use crate::note::get_p_tags;
use crate::zaps::parse_bolt11_msats;
use nostrdb::{Ndb, Note, Transaction};

/// Find which of the monitored pubkeys is the target of a notification event.
///
/// Uses `get_p_tags` to extract p-tags, then returns the first pubkey that
/// matches one of our monitored accounts.
pub fn find_target_ptag(note: &Note, monitored: &[[u8; 32]]) -> Option<String> {
    for pk_bytes in get_p_tags(note) {
        if monitored.iter().any(|m| m == pk_bytes) {
            return Some(hex::encode(pk_bytes));
        }
    }
    None
}

/// Extract all p-tag pubkeys from a note as hex strings.
///
/// Delegates to `get_p_tags` and hex-encodes each result.
pub fn extract_p_tags_from_note(note: &Note) -> Vec<String> {
    get_p_tags(note).into_iter().map(hex::encode).collect()
}

/// Extract zap amount in satoshis from a note's bolt11 tag.
///
/// Finds the `bolt11` tag, delegates parsing to `parse_bolt11_msats`,
/// and converts millisatoshis to satoshis.
///
/// Note: We don't use `zaps::get_zap_tags` here because it requires
/// description + recipient fields, which may be absent in notification
/// contexts where we only need the amount.
pub fn extract_zap_amount_from_note(note: &Note) -> Option<i64> {
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }
        if tag.get_str(0) != Some("bolt11") {
            continue;
        }
        let Some(bolt11) = tag.get_str(1) else {
            continue;
        };
        return parse_bolt11_msats(bolt11).map(|msats| msats.div_ceil(1000) as i64);
    }
    None
}

/// Look up a profile's display name and picture URL from nostrdb.
///
/// Uses `name::get_display_name` for the name (prefers display_name over
/// username) and extracts picture URL directly from the profile record.
/// Returns `None` for picture instead of a default URL, since notifications
/// should only show pictures when one actually exists.
pub fn lookup_profile_ndb(
    ndb: &Ndb,
    txn: &Transaction,
    pubkey: &[u8; 32],
) -> (Option<String>, Option<String>) {
    let Ok(profile) = ndb.get_profile_by_pubkey(txn, pubkey) else {
        return (None, None);
    };

    let nostr_name = get_display_name(Some(&profile));
    let name = nostr_name
        .display_name
        .or(nostr_name.username)
        .map(|s| s.to_string());

    let picture = profile
        .record()
        .profile()
        .and_then(|p| p.picture())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    (name, picture)
}
