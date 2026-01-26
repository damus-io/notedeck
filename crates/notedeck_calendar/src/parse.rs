//! Tag parsing utilities for NIP-52 calendar events.
//!
//! This module provides helper functions for parsing common tag patterns
//! found in Nostr events, particularly those used in NIP-52 calendar events.

use nostrdb::Note;

/// A participant in a calendar event.
///
/// Participants are identified by their pubkey and may have an optional
/// relay URL hint and role in the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// The participant's 32-byte public key.
    pub pubkey: [u8; 32],
    /// Optional relay URL where the participant can be reached.
    pub relay_url: Option<String>,
    /// Optional role of the participant in the event (e.g., "organizer", "speaker").
    pub role: Option<String>,
}

/// Gets the first value for a tag with the given name.
///
/// Searches through the note's tags and returns the first value (second element)
/// for a tag whose first element matches `tag_name`.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
/// * `tag_name` - The tag name to look for (e.g., "d", "title", "start")
///
/// # Returns
///
/// The tag value if found, or `None` if no matching tag exists.
///
/// # Example
///
/// ```ignore
/// // For a note with tags: [["title", "My Event"], ["d", "abc123"]]
/// let title = get_tag_value(&note, "title"); // Some("My Event")
/// let missing = get_tag_value(&note, "foo"); // None
/// ```
pub fn get_tag_value<'a>(note: &Note<'a>, tag_name: &str) -> Option<&'a str> {
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };

        if name != tag_name {
            continue;
        }

        return tag.get_str(1);
    }

    None
}

/// Gets all values for tags with the given name.
///
/// Searches through the note's tags and collects all values (second elements)
/// for tags whose first element matches `tag_name`. This is useful for
/// repeated tags like "location", "p" (participants), "t" (hashtags), etc.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
/// * `tag_name` - The tag name to look for
///
/// # Returns
///
/// A vector of all matching tag values. Empty if no matches found.
///
/// # Example
///
/// ```ignore
/// // For a note with tags: [["location", "NYC"], ["location", "Remote"]]
/// let locations = get_all_tag_values(&note, "location"); // vec!["NYC", "Remote"]
/// ```
pub fn get_all_tag_values<'a>(note: &Note<'a>, tag_name: &str) -> Vec<&'a str> {
    let mut values = Vec::new();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };

        if name != tag_name {
            continue;
        }

        let Some(value) = tag.get_str(1) else {
            continue;
        };

        values.push(value);
    }

    values
}

/// Parses an NIP-33 "a" tag value into its components.
///
/// The "a" tag format is: `<kind>:<pubkey>:<d-tag>`
///
/// # Arguments
///
/// * `value` - The "a" tag value string to parse
///
/// # Returns
///
/// A tuple of (kind, pubkey, d-tag) if parsing succeeds, or `None` if the
/// format is invalid.
///
/// # Example
///
/// ```
/// use notedeck_calendar::parse_a_tag;
///
/// let result = parse_a_tag("31922:abc123def456789012345678901234567890123456789012345678901234:my-event");
/// // Returns Some((31922, [pubkey bytes], "my-event"))
/// ```
pub fn parse_a_tag(value: &str) -> Option<(u64, [u8; 32], String)> {
    let parts: Vec<&str> = value.splitn(3, ':').collect();

    if parts.len() != 3 {
        return None;
    }

    let kind: u64 = parts[0].parse().ok()?;

    let pubkey_hex = parts[1];
    if pubkey_hex.len() != 64 {
        return None;
    }

    let pubkey_bytes = hex::decode(pubkey_hex).ok()?;
    if pubkey_bytes.len() != 32 {
        return None;
    }

    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&pubkey_bytes);

    let d_tag = parts[2].to_string();

    Some((kind, pubkey, d_tag))
}

/// Parses a participant "p" tag from a note.
///
/// The "p" tag format is: `["p", "<pubkey>", "<optional relay>", "<optional role>"]`
///
/// # Arguments
///
/// * `note` - The nostrdb Note containing the tag
/// * `tag_index` - The index of the tag to parse
///
/// # Returns
///
/// A `Participant` if the tag is a valid "p" tag, or `None` otherwise.
pub fn parse_participant_tag(note: &Note<'_>, tag_index: u16) -> Option<Participant> {
    let mut current_idx: u16 = 0;

    for tag in note.tags() {
        if current_idx != tag_index {
            current_idx += 1;
            continue;
        }

        if tag.count() < 2 {
            return None;
        }

        let tag_name = tag.get_str(0)?;
        if tag_name != "p" {
            return None;
        }

        let pubkey = tag.get_id(1)?;

        let relay_url: Option<String> = if tag.count() > 2 {
            tag.get_str(2)
                .map(|s: &str| s.to_string())
                .filter(|s: &String| !s.is_empty())
        } else {
            None
        };

        let role: Option<String> = if tag.count() > 3 {
            tag.get_str(3)
                .map(|s: &str| s.to_string())
                .filter(|s: &String| !s.is_empty())
        } else {
            None
        };

        return Some(Participant {
            pubkey: *pubkey,
            relay_url,
            role,
        });
    }

    None
}

/// Collects all participant tags from a note.
///
/// Iterates through all tags in the note and parses any valid "p" tags
/// into `Participant` structs.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
///
/// # Returns
///
/// A vector of all participants found in the note's tags.
pub fn get_participants(note: &Note<'_>) -> Vec<Participant> {
    let mut participants = Vec::new();

    for i in 0..note.tags().count() {
        if let Some(participant) = parse_participant_tag(note, i) {
            participants.push(participant);
        }
    }

    participants
}

/// Gets the first "e" tag value (event ID) from a note.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
///
/// # Returns
///
/// The 32-byte event ID if an "e" tag is found, or `None` otherwise.
pub fn get_e_tag(note: &Note<'_>) -> Option<[u8; 32]> {
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };

        if name != "e" {
            continue;
        }

        let Some(id) = tag.get_id(1) else {
            continue;
        };

        return Some(*id);
    }

    None
}

/// Gets the first "a" tag value from a note.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
///
/// # Returns
///
/// The raw "a" tag value string if found, or `None` otherwise.
pub fn get_a_tag_value<'a>(note: &Note<'a>) -> Option<&'a str> {
    get_tag_value(note, "a")
}

/// Gets all "a" tag values from a note.
///
/// # Arguments
///
/// * `note` - The nostrdb Note to search
///
/// # Returns
///
/// A vector of all "a" tag value strings.
pub fn get_all_a_tags<'a>(note: &Note<'a>) -> Vec<&'a str> {
    get_all_tag_values(note, "a")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_a_tag_valid() {
        let value =
            "31922:0000000000000000000000000000000000000000000000000000000000000000:my-event";
        let result = parse_a_tag(value);

        assert!(result.is_some());
        let (kind, pubkey, d_tag) = result.unwrap();
        assert_eq!(kind, 31922);
        assert_eq!(pubkey, [0u8; 32]);
        assert_eq!(d_tag, "my-event");
    }

    #[test]
    fn test_parse_a_tag_invalid_format() {
        // Missing parts
        assert!(parse_a_tag("31922:abc").is_none());
        assert!(parse_a_tag("31922").is_none());
        assert!(parse_a_tag("").is_none());

        // Invalid kind
        assert!(parse_a_tag(
            "notanumber:0000000000000000000000000000000000000000000000000000000000000000:event"
        )
        .is_none());

        // Invalid pubkey (wrong length)
        assert!(parse_a_tag("31922:abc:event").is_none());
        assert!(parse_a_tag("31922:0000:event").is_none());
    }

    #[test]
    fn test_parse_a_tag_with_colons_in_dtag() {
        // d-tag can contain colons
        let value = "31922:0000000000000000000000000000000000000000000000000000000000000000:my:event:with:colons";
        let result = parse_a_tag(value);

        assert!(result.is_some());
        let (kind, _, d_tag) = result.unwrap();
        assert_eq!(kind, 31922);
        assert_eq!(d_tag, "my:event:with:colons");
    }
}
