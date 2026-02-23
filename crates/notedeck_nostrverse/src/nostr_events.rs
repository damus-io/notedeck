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

/// Build a coarse presence heartbeat event (kind 10555).
///
/// Published on meaningful position change, plus periodic keep-alive.
/// Tags: ["a", room_naddr], ["position", "x y z"], ["expiration", unix_ts]
/// Content: empty
///
/// The expiration tag (NIP-40) tells relays/nostrdb to discard the event
/// after 90 seconds, matching the client-side stale timeout.
pub fn build_presence_event<'a>(
    room_naddr: &str,
    position: glam::Vec3,
    velocity: glam::Vec3,
) -> NoteBuilder<'a> {
    let pos_str = format!("{} {} {}", position.x, position.y, position.z);
    let vel_str = format!("{} {} {}", velocity.x, velocity.y, velocity.z);

    let expiration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 90;
    let exp_str = expiration.to_string();

    NoteBuilder::new()
        .kind(kinds::PRESENCE as u32)
        .content("")
        .start_tag()
        .tag_str("a")
        .tag_str(room_naddr)
        .start_tag()
        .tag_str("position")
        .tag_str(&pos_str)
        .start_tag()
        .tag_str("velocity")
        .tag_str(&vel_str)
        .start_tag()
        .tag_str("expiration")
        .tag_str(&exp_str)
}

/// Parse a presence event's position tag into a Vec3.
pub fn parse_presence_position(note: &Note<'_>) -> Option<glam::Vec3> {
    let pos_str = get_tag_value(note, "position")?;
    let mut parts = pos_str.split_whitespace();
    let x: f32 = parts.next()?.parse().ok()?;
    let y: f32 = parts.next()?.parse().ok()?;
    let z: f32 = parts.next()?.parse().ok()?;
    Some(glam::Vec3::new(x, y, z))
}

/// Parse a presence event's velocity tag into a Vec3.
/// Returns Vec3::ZERO if no velocity tag (backward compatible with old events).
pub fn parse_presence_velocity(note: &Note<'_>) -> glam::Vec3 {
    let Some(vel_str) = get_tag_value(note, "velocity") else {
        return glam::Vec3::ZERO;
    };
    let mut parts = vel_str.split_whitespace();
    let x: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let y: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let z: f32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    glam::Vec3::new(x, y, z)
}

/// Extract the "a" tag (room naddr) from a presence note.
pub fn get_presence_room<'a>(note: &'a Note<'a>) -> Option<&'a str> {
    get_tag_value(note, "a")
}

/// Sign and ingest a nostr event into the local nostrdb only (no relay publishing).
/// Returns the 32-byte event ID on success.
pub fn ingest_event(builder: NoteBuilder<'_>, ndb: &Ndb, kp: FilledKeypair) -> Option<[u8; 32]> {
    let note = builder
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("build note");

    let id = *note.id();

    let Ok(event) = &enostr::ClientMessage::event(&note) else {
        tracing::error!("ingest_event: failed to build client message");
        return None;
    };

    let Ok(json) = event.to_json() else {
        tracing::error!("ingest_event: failed to serialize json");
        return None;
    };

    let _ = ndb.process_event_with(&json, nostrdb::IngestMetadata::new().client(true));
    Some(id)
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

    #[test]
    fn test_build_presence_event() {
        let pos = glam::Vec3::new(1.5, 0.0, -3.2);
        let vel = glam::Vec3::new(2.0, 0.0, -1.0);
        let mut builder = build_presence_event("37555:abc123:my-room", pos, vel);
        let note = builder.build().expect("build note");

        assert_eq!(note.content(), "");
        assert_eq!(get_presence_room(&note), Some("37555:abc123:my-room"));

        let parsed_pos = parse_presence_position(&note).expect("parse position");
        assert!((parsed_pos.x - 1.5).abs() < 0.01);
        assert!((parsed_pos.y - 0.0).abs() < 0.01);
        assert!((parsed_pos.z - (-3.2)).abs() < 0.01);

        let parsed_vel = parse_presence_velocity(&note);
        assert!((parsed_vel.x - 2.0).abs() < 0.01);
        assert!((parsed_vel.y - 0.0).abs() < 0.01);
        assert!((parsed_vel.z - (-1.0)).abs() < 0.01);

        // Should have an expiration tag (NIP-40)
        let exp = get_tag_value(&note, "expiration").expect("missing expiration tag");
        let exp_ts: u64 = exp.parse().expect("expiration should be a number");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(exp_ts > now, "expiration should be in the future");
        assert!(exp_ts <= now + 91, "expiration should be ~90s from now");
    }
}
