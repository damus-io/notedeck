//! Nostr event creation and parsing for nostrverse rooms.
//!
//! Room events (kind 37555) are NIP-33 parameterized replaceable events
//! where the content is a protoverse `.space` s-expression.

use enostr::FilledKeypair;
use nostrdb::{Ndb, Note, NoteBuilder};
use protoverse::Space;

use crate::kinds;

/// Build a room event (kind 37555) from a protoverse Space.
///
/// Tags: ["d", room_id], ["name", room_name], ["summary", text_description]
/// Content: serialized .space s-expression
pub fn build_room_event<'a>(space: &Space, room_id: &str) -> NoteBuilder<'a> {
    let content = protoverse::serialize(space);
    let summary = protoverse::describe(space);
    let name = space.name(space.root).unwrap_or("Untitled Room");

    NoteBuilder::new()
        .kind(kinds::ROOM as u32)
        .content(&content)
        .start_tag()
        .tag_str("d")
        .tag_str(room_id)
        .start_tag()
        .tag_str("name")
        .tag_str(name)
        .start_tag()
        .tag_str("summary")
        .tag_str(&summary)
}

/// Parse a room event's content into a protoverse Space.
pub fn parse_room_event(note: &Note<'_>) -> Option<Space> {
    let content = note.content();
    if content.is_empty() {
        return None;
    }
    protoverse::parse(content).ok()
}

/// Extract the "d" tag (room identifier) from a note.
pub fn get_room_id<'a>(note: &'a Note<'a>) -> Option<&'a str> {
    get_tag_value(note, "d")
}

/// Extract a tag value by name from a note.
fn get_tag_value<'a>(note: &'a Note<'a>, tag_name: &str) -> Option<&'a str> {
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

/// Sign and ingest a room event into the local nostrdb only (no relay publishing).
pub fn ingest_room_event(builder: NoteBuilder<'_>, ndb: &Ndb, kp: FilledKeypair) {
    let note = builder
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("build note");

    let Ok(event) = &enostr::ClientMessage::event(&note) else {
        tracing::error!("ingest_room_event: failed to build client message");
        return;
    };

    let Ok(json) = event.to_json() else {
        tracing::error!("ingest_room_event: failed to serialize json");
        return;
    };

    let _ = ndb.process_event_with(&json, nostrdb::IngestMetadata::new().client(true));
    tracing::info!("ingested room event locally");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_room_event() {
        let space = protoverse::parse(
            r#"(room (name "Test Room") (shape rectangle) (width 10) (depth 8)
              (group (table (id desk) (name "My Desk"))))"#,
        )
        .unwrap();

        let mut builder = build_room_event(&space, "my-room");
        let note = builder.build().expect("build note");

        // Content should be the serialized space
        let content = note.content();
        assert!(content.contains("room"));
        assert!(content.contains("Test Room"));

        // Should have d, name, summary tags
        let mut has_d = false;
        let mut has_name = false;
        let mut has_summary = false;

        for tag in note.tags() {
            if tag.count() < 2 {
                continue;
            }
            match tag.get_str(0) {
                Some("d") => {
                    assert_eq!(tag.get_str(1), Some("my-room"));
                    has_d = true;
                }
                Some("name") => {
                    assert_eq!(tag.get_str(1), Some("Test Room"));
                    has_name = true;
                }
                Some("summary") => {
                    has_summary = true;
                }
                _ => {}
            }
        }

        assert!(has_d, "missing d tag");
        assert!(has_name, "missing name tag");
        assert!(has_summary, "missing summary tag");
    }

    #[test]
    fn test_parse_room_event_roundtrip() {
        let original = r#"(room (name "Test Room") (shape rectangle) (width 10) (depth 8)
              (group (table (id desk) (name "My Desk"))))"#;

        let space = protoverse::parse(original).unwrap();
        let mut builder = build_room_event(&space, "test-room");
        let note = builder.build().expect("build note");

        // Parse the event content back into a Space
        let parsed = parse_room_event(&note).expect("parse room event");
        assert_eq!(parsed.name(parsed.root), Some("Test Room"));

        // Should have same structure
        assert_eq!(space.cells.len(), parsed.cells.len());
    }

    #[test]
    fn test_get_room_id() {
        let space = protoverse::parse("(room (name \"X\"))").unwrap();
        let mut builder = build_room_event(&space, "my-id");
        let note = builder.build().expect("build note");

        assert_eq!(get_room_id(&note), Some("my-id"));
    }
}
