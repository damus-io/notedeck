//! NIP-22 Comment support for calendar events.
//!
//! This module provides the `Comment` struct for parsing and storing
//! kind 1111 comments that reference calendar events.
//!
//! NIP-22 specifies that comments use:
//! - Uppercase tags (K, E, A, P) for root scope (the original event being commented on)
//! - Lowercase tags (k, e, a, p) for parent item (direct reply target)
//!
//! For calendar events, the root is typically an addressable event (kind 31922/31923),
//! so comments will have an "A" tag pointing to the calendar event coordinates.

use nostrdb::{Note, NoteKey};

/// Kind number for NIP-22 comments.
pub const KIND_COMMENT: u32 = 1111;

/// A NIP-22 comment on a calendar event or another comment.
///
/// Comments form a threaded structure where each comment references:
/// - A root event (the calendar event being commented on) via uppercase tags
/// - An optional parent comment (for replies to comments) via lowercase tags
#[derive(Debug, Clone)]
pub struct Comment {
    /// The note key in nostrdb.
    pub note_key: NoteKey,

    /// The comment content/text.
    pub content: String,

    /// Public key of the comment author.
    pub pubkey: [u8; 32],

    /// Unix timestamp when the comment was created.
    pub created_at: u64,

    /// Root scope - the calendar event this comment thread belongs to.
    /// This is the "A" tag value (addressable event coordinates).
    pub root_a_tag: Option<String>,

    /// Root scope - event ID if referencing by "E" tag.
    pub root_e_tag: Option<[u8; 32]>,

    /// Root scope - the kind of the root event ("K" tag).
    pub root_kind: Option<u32>,

    /// Parent item - for threaded replies, the parent comment's event ID ("e" tag).
    pub parent_e_tag: Option<[u8; 32]>,

    /// Parent item - the parent's addressable coordinates ("a" tag).
    pub parent_a_tag: Option<String>,

    /// Parent item - the kind of the parent ("k" tag).
    pub parent_kind: Option<u32>,

    /// Mentioned pubkeys from "p" tags (lowercase, for reply notifications).
    pub mentioned_pubkeys: Vec<[u8; 32]>,
}

impl Comment {
    /// Parses a `Comment` from a nostrdb `Note`.
    ///
    /// Returns `None` if the note is not a valid comment (wrong kind).
    ///
    /// # Arguments
    ///
    /// * `note` - The nostrdb Note to parse
    /// * `note_key` - The NoteKey for this note
    ///
    /// # Returns
    ///
    /// A `Comment` if parsing succeeds, or `None` if the note is not a comment.
    pub fn from_note(note: &Note<'_>, note_key: NoteKey) -> Option<Self> {
        if note.kind() != KIND_COMMENT {
            return None;
        }

        let mut root_a_tag = None;
        let mut root_e_tag = None;
        let mut root_kind = None;
        let mut parent_a_tag = None;
        let mut parent_e_tag = None;
        let mut parent_kind = None;
        let mut mentioned_pubkeys = Vec::new();

        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }

            let Some(tag_name) = tag.get_str(0) else {
                continue;
            };

            match tag_name {
                // Uppercase tags - root scope
                "K" => {
                    if let Some(kind_str) = tag.get_str(1) {
                        if let Ok(k) = kind_str.parse::<u32>() {
                            root_kind = Some(k);
                        }
                    }
                }
                "E" => {
                    if let Some(id) = tag.get_id(1) {
                        root_e_tag = Some(*id);
                    }
                }
                "A" => {
                    if let Some(coords) = tag.get_str(1) {
                        root_a_tag = Some(coords.to_string());
                    }
                }
                // Lowercase tags - parent item
                "k" => {
                    if let Some(kind_str) = tag.get_str(1) {
                        if let Ok(k) = kind_str.parse::<u32>() {
                            parent_kind = Some(k);
                        }
                    }
                }
                "e" => {
                    if let Some(id) = tag.get_id(1) {
                        parent_e_tag = Some(*id);
                    }
                }
                "a" => {
                    if let Some(coords) = tag.get_str(1) {
                        parent_a_tag = Some(coords.to_string());
                    }
                }
                "p" => {
                    if let Some(pubkey) = tag.get_id(1) {
                        mentioned_pubkeys.push(*pubkey);
                    }
                }
                _ => {}
            }
        }

        Some(Comment {
            note_key,
            content: note.content().to_string(),
            pubkey: *note.pubkey(),
            created_at: note.created_at(),
            root_a_tag,
            root_e_tag,
            root_kind,
            parent_e_tag,
            parent_a_tag,
            parent_kind,
            mentioned_pubkeys,
        })
    }

    /// Checks if this comment is a direct comment on the root event (not a reply to another comment).
    pub fn is_root_comment(&self) -> bool {
        self.parent_e_tag.is_none() && self.parent_a_tag.is_none()
    }

    /// Checks if this comment references the given calendar event coordinates.
    ///
    /// # Arguments
    ///
    /// * `event_coords` - The NIP-33 coordinates (e.g., "31923:pubkey:d-tag")
    pub fn references_event(&self, event_coords: &str) -> bool {
        self.root_a_tag.as_ref().is_some_and(|a| a == event_coords)
    }

    /// Returns the author's pubkey as a hex string (shortened for display).
    pub fn author_short(&self) -> String {
        let hex = hex::encode(self.pubkey);
        format!("{}...{}", &hex[..8], &hex[56..])
    }
}

/// Cached comment for display purposes.
///
/// This struct holds parsed comment data along with display-related fields.
#[derive(Debug, Clone)]
pub struct CachedComment {
    /// The underlying comment data.
    pub comment: Comment,

    /// Display name of the author (fetched from profile, falls back to short pubkey).
    pub author_name: String,
}

impl CachedComment {
    /// Creates a new cached comment with just the parsed data.
    /// The author_name defaults to the short pubkey and can be updated later.
    pub fn new(comment: Comment) -> Self {
        let author_name = comment.author_short();
        Self {
            comment,
            author_name,
        }
    }

    /// Updates the author's display name.
    pub fn set_author_name(&mut self, name: String) {
        self.author_name = name;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_constant() {
        assert_eq!(KIND_COMMENT, 1111);
    }

    #[test]
    fn test_comment_is_root_comment() {
        // A comment with no parent tags is a root comment
        let comment = Comment {
            note_key: NoteKey::new(1),
            content: "Test".to_string(),
            pubkey: [0u8; 32],
            created_at: 1234567890,
            root_a_tag: Some("31923:abc:test".to_string()),
            root_e_tag: None,
            root_kind: Some(31923),
            parent_e_tag: None,
            parent_a_tag: None,
            parent_kind: None,
            mentioned_pubkeys: vec![],
        };
        assert!(comment.is_root_comment());
    }

    #[test]
    fn test_comment_is_reply() {
        // A comment with parent tags is a reply
        let comment = Comment {
            note_key: NoteKey::new(1),
            content: "Reply".to_string(),
            pubkey: [0u8; 32],
            created_at: 1234567890,
            root_a_tag: Some("31923:abc:test".to_string()),
            root_e_tag: None,
            root_kind: Some(31923),
            parent_e_tag: Some([1u8; 32]),
            parent_a_tag: None,
            parent_kind: Some(1111),
            mentioned_pubkeys: vec![],
        };
        assert!(!comment.is_root_comment());
    }

    #[test]
    fn test_references_event() {
        let comment = Comment {
            note_key: NoteKey::new(1),
            content: "Test".to_string(),
            pubkey: [0u8; 32],
            created_at: 1234567890,
            root_a_tag: Some("31923:abc123:my-event".to_string()),
            root_e_tag: None,
            root_kind: Some(31923),
            parent_e_tag: None,
            parent_a_tag: None,
            parent_kind: None,
            mentioned_pubkeys: vec![],
        };

        assert!(comment.references_event("31923:abc123:my-event"));
        assert!(!comment.references_event("31923:other:event"));
    }
}
