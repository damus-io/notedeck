//! RSVP data structure for NIP-52 calendar events.
//!
//! This module defines the `Rsvp` struct which represents a response
//! to a calendar event (kind 31925).

use std::str::FromStr;

use nostrdb::Note;

use crate::parse::{get_e_tag, get_tag_value};
use crate::KIND_RSVP;

/// The attendance status for an RSVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RsvpStatus {
    /// The user has accepted the invitation and plans to attend.
    Accepted,
    /// The user has declined the invitation.
    Declined,
    /// The user is unsure and may or may not attend.
    Tentative,
}

/// Error type for parsing RSVP status from string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseRsvpStatusError;

impl std::fmt::Display for ParseRsvpStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid RSVP status")
    }
}

impl std::error::Error for ParseRsvpStatusError {}

impl FromStr for RsvpStatus {
    type Err = ParseRsvpStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "accepted" => Ok(Self::Accepted),
            "declined" => Ok(Self::Declined),
            "tentative" => Ok(Self::Tentative),
            _ => Err(ParseRsvpStatusError),
        }
    }
}

impl RsvpStatus {
    /// Returns the string representation of this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Tentative => "tentative",
        }
    }
}

impl std::fmt::Display for RsvpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// The free/busy status for an RSVP.
///
/// Indicates whether the user is available during the event time.
/// This is only meaningful if the RSVP status is not "declined".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreeBusy {
    /// The user is free during this time.
    Free,
    /// The user is busy during this time.
    Busy,
}

/// Error type for parsing free/busy status from string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseFreeBusyError;

impl std::fmt::Display for ParseFreeBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid free/busy status")
    }
}

impl std::error::Error for ParseFreeBusyError {}

impl FromStr for FreeBusy {
    type Err = ParseFreeBusyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "free" => Ok(Self::Free),
            "busy" => Ok(Self::Busy),
            _ => Err(ParseFreeBusyError),
        }
    }
}

impl FreeBusy {
    /// Returns the string representation of this status.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Busy => "busy",
        }
    }
}

impl std::fmt::Display for FreeBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A NIP-52 Calendar Event RSVP (kind 31925).
///
/// An RSVP is a response to a calendar event indicating a user's
/// attendance intention. Any user may RSVP to an event, even if
/// they were not explicitly invited.
#[derive(Debug, Clone)]
pub struct Rsvp {
    /// Unique identifier for this RSVP (d-tag).
    pub d_tag: String,

    /// Reference to the calendar event being responded to (a-tag value).
    /// Format: `<kind>:<pubkey>:<d-tag>`
    pub event_ref: String,

    /// Optional reference to a specific revision of the calendar event.
    pub event_id: Option<[u8; 32]>,

    /// The attendance status.
    pub status: RsvpStatus,

    /// Optional free/busy indicator.
    /// Should be ignored if status is "declined".
    pub free_busy: Option<FreeBusy>,

    /// Optional note/comment about the RSVP.
    pub content: String,

    /// Public key of the RSVP creator.
    pub pubkey: [u8; 32],

    /// Unix timestamp when the RSVP was created.
    pub created_at: u64,
}

impl Rsvp {
    /// Parses an `Rsvp` from a nostrdb `Note`.
    ///
    /// Returns `None` if the note is not a valid RSVP (wrong kind
    /// or missing required fields).
    ///
    /// # Arguments
    ///
    /// * `note` - The nostrdb Note to parse
    ///
    /// # Returns
    ///
    /// An `Rsvp` if parsing succeeds, or `None` if the note is not
    /// a valid RSVP.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use notedeck_calendar::Rsvp;
    ///
    /// if let Some(rsvp) = Rsvp::from_note(&note) {
    ///     println!("RSVP status: {}", rsvp.status);
    /// }
    /// ```
    pub fn from_note(note: &Note<'_>) -> Option<Self> {
        // Verify this is an RSVP kind
        if note.kind() != KIND_RSVP {
            return None;
        }

        // Required: d-tag
        let d_tag = get_tag_value(note, "d")?;

        // Required: a-tag (event reference)
        let event_ref = get_tag_value(note, "a")?;

        // Required: status
        let status_str = get_tag_value(note, "status")?;
        let status = status_str.parse::<RsvpStatus>().ok()?;

        // Optional: e-tag (specific event revision)
        let event_id = get_e_tag(note);

        // Optional: free/busy (ignored if status is declined)
        let free_busy = if status != RsvpStatus::Declined {
            get_tag_value(note, "fb").and_then(|s| s.parse::<FreeBusy>().ok())
        } else {
            None
        };

        Some(Rsvp {
            d_tag: d_tag.to_string(),
            event_ref: event_ref.to_string(),
            event_id,
            status,
            free_busy,
            content: note.content().to_string(),
            pubkey: *note.pubkey(),
            created_at: note.created_at(),
        })
    }

    /// Returns the NIP-33 addressable event coordinates for this RSVP.
    ///
    /// Format: `<kind>:<pubkey_hex>:<d-tag>`
    pub fn coordinates(&self) -> String {
        format!("{}:{}:{}", KIND_RSVP, hex::encode(self.pubkey), self.d_tag)
    }

    /// Returns true if this RSVP indicates the user accepted the invitation.
    pub fn is_accepted(&self) -> bool {
        self.status == RsvpStatus::Accepted
    }

    /// Returns true if this RSVP indicates the user declined the invitation.
    pub fn is_declined(&self) -> bool {
        self.status == RsvpStatus::Declined
    }

    /// Returns true if this RSVP indicates the user is tentative.
    pub fn is_tentative(&self) -> bool {
        self.status == RsvpStatus::Tentative
    }

    /// Returns true if the user indicated they will be busy during the event.
    ///
    /// Returns false if:
    /// - The RSVP status is "declined"
    /// - No free/busy status was specified
    /// - The user indicated they are free
    pub fn is_busy(&self) -> bool {
        matches!(self.free_busy, Some(FreeBusy::Busy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rsvp_status_from_str() {
        assert_eq!(
            "accepted".parse::<RsvpStatus>().ok(),
            Some(RsvpStatus::Accepted)
        );
        assert_eq!(
            "ACCEPTED".parse::<RsvpStatus>().ok(),
            Some(RsvpStatus::Accepted)
        );
        assert_eq!(
            "declined".parse::<RsvpStatus>().ok(),
            Some(RsvpStatus::Declined)
        );
        assert_eq!(
            "tentative".parse::<RsvpStatus>().ok(),
            Some(RsvpStatus::Tentative)
        );
        assert!("invalid".parse::<RsvpStatus>().is_err());
    }

    #[test]
    fn test_rsvp_status_as_str() {
        assert_eq!(RsvpStatus::Accepted.as_str(), "accepted");
        assert_eq!(RsvpStatus::Declined.as_str(), "declined");
        assert_eq!(RsvpStatus::Tentative.as_str(), "tentative");
    }

    #[test]
    fn test_free_busy_from_str() {
        assert_eq!("free".parse::<FreeBusy>().ok(), Some(FreeBusy::Free));
        assert_eq!("FREE".parse::<FreeBusy>().ok(), Some(FreeBusy::Free));
        assert_eq!("busy".parse::<FreeBusy>().ok(), Some(FreeBusy::Busy));
        assert_eq!("BUSY".parse::<FreeBusy>().ok(), Some(FreeBusy::Busy));
        assert!("invalid".parse::<FreeBusy>().is_err());
    }

    #[test]
    fn test_free_busy_as_str() {
        assert_eq!(FreeBusy::Free.as_str(), "free");
        assert_eq!(FreeBusy::Busy.as_str(), "busy");
    }

    #[test]
    fn test_rsvp_status_display() {
        assert_eq!(format!("{}", RsvpStatus::Accepted), "accepted");
        assert_eq!(format!("{}", RsvpStatus::Declined), "declined");
        assert_eq!(format!("{}", RsvpStatus::Tentative), "tentative");
    }

    #[test]
    fn test_free_busy_display() {
        assert_eq!(format!("{}", FreeBusy::Free), "free");
        assert_eq!(format!("{}", FreeBusy::Busy), "busy");
    }
}
