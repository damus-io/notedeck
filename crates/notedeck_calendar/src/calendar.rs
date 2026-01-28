//! Calendar data structure for NIP-52.
//!
//! This module defines the `Calendar` struct which represents a collection
//! of calendar events (kind 31924).

use nostrdb::Note;

use crate::parse::{get_all_tag_values, get_tag_value, parse_a_tag};
use crate::KIND_CALENDAR;

/// A reference to a calendar event within a calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRef {
    /// The kind of the referenced event (31922 or 31923).
    pub kind: u64,
    /// The pubkey of the event creator.
    pub pubkey: [u8; 32],
    /// The d-tag identifier of the event.
    pub d_tag: String,
    /// Optional relay URL hint.
    pub relay_url: Option<String>,
}

impl EventRef {
    /// Returns the NIP-33 addressable event coordinates for this reference.
    ///
    /// Format: `<kind>:<pubkey_hex>:<d-tag>`
    pub fn coordinates(&self) -> String {
        format!("{}:{}:{}", self.kind, hex::encode(self.pubkey), self.d_tag)
    }
}

/// A NIP-52 Calendar (kind 31924).
///
/// A calendar is a collection of calendar events, represented as an addressable
/// list event. Users can have multiple calendars to organize events by purpose
/// (e.g., personal, work, travel).
#[derive(Debug, Clone)]
pub struct Calendar {
    /// Unique identifier for this calendar (d-tag).
    pub d_tag: String,

    /// Title of the calendar (required).
    pub title: String,

    /// Description of the calendar (from content field).
    pub content: String,

    /// References to calendar events in this calendar.
    pub event_refs: Vec<EventRef>,

    /// Public key of the calendar owner.
    pub pubkey: [u8; 32],

    /// Unix timestamp when the calendar was created/updated.
    pub created_at: u64,
}

impl Calendar {
    /// Parses a `Calendar` from a nostrdb `Note`.
    ///
    /// Returns `None` if the note is not a valid calendar (wrong kind
    /// or missing required fields).
    ///
    /// # Arguments
    ///
    /// * `note` - The nostrdb Note to parse
    ///
    /// # Returns
    ///
    /// A `Calendar` if parsing succeeds, or `None` if the note is not
    /// a valid calendar.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use notedeck_calendar::Calendar;
    ///
    /// if let Some(calendar) = Calendar::from_note(&note) {
    ///     println!("Calendar: {}", calendar.title);
    ///     println!("Events: {}", calendar.event_refs.len());
    /// }
    /// ```
    pub fn from_note(note: &Note<'_>) -> Option<Self> {
        // Verify this is a calendar kind
        if note.kind() != KIND_CALENDAR {
            return None;
        }

        // Required: d-tag
        let d_tag = get_tag_value(note, "d")?;

        // Required: title
        let title = get_tag_value(note, "title")?;

        // Parse event references from "a" tags
        let event_refs = parse_event_refs(note);

        Some(Calendar {
            d_tag: d_tag.to_string(),
            title: title.to_string(),
            content: note.content().to_string(),
            event_refs,
            pubkey: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Returns the NIP-33 addressable event coordinates for this calendar.
    ///
    /// Format: `<kind>:<pubkey_hex>:<d-tag>`
    pub fn coordinates(&self) -> String {
        format!(
            "{}:{}:{}",
            KIND_CALENDAR,
            hex::encode(self.pubkey),
            self.d_tag
        )
    }

    /// Returns the number of events in this calendar.
    pub fn event_count(&self) -> usize {
        self.event_refs.len()
    }

    /// Returns true if this calendar contains no events.
    pub fn is_empty(&self) -> bool {
        self.event_refs.is_empty()
    }
}

/// Parses all event references from a note's "a" tags.
///
/// Only includes references to calendar event kinds (31922, 31923).
fn parse_event_refs(note: &Note<'_>) -> Vec<EventRef> {
    let mut refs = Vec::new();

    // Get all "a" tags
    let a_tags = get_all_tag_values(note, "a");

    for (idx, a_tag) in a_tags.iter().enumerate() {
        let Some((kind, pubkey, d_tag)) = parse_a_tag(a_tag) else {
            continue;
        };

        // Only include calendar event kinds
        if kind != 31922 && kind != 31923 {
            continue;
        }

        // Try to get the relay URL from the tag (third element)
        let relay_url = get_a_tag_relay_url(note, idx);

        refs.push(EventRef {
            kind,
            pubkey,
            d_tag,
            relay_url,
        });
    }

    refs
}

/// Gets the relay URL from an "a" tag at the given index.
fn get_a_tag_relay_url(note: &Note<'_>, a_tag_index: usize) -> Option<String> {
    let mut current_a_idx = 0;

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(name) = tag.get_str(0) else {
            continue;
        };

        if name != "a" {
            continue;
        }

        if current_a_idx == a_tag_index {
            // Check for relay URL in third position
            if tag.count() > 2 {
                return tag
                    .get_str(2)
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
            }
            return None;
        }

        current_a_idx += 1;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_ref_coordinates() {
        let event_ref = EventRef {
            kind: 31922,
            pubkey: [0u8; 32],
            d_tag: "my-event".to_string(),
            relay_url: None,
        };

        let coords = event_ref.coordinates();
        assert!(coords.starts_with("31922:"));
        assert!(coords.ends_with(":my-event"));
    }
}
