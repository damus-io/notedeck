//! Joining a live message that was split across notes back into one.
//!
//! A message too big for one wire note is cut into parts
//! ([`build_live_events`](crate::session_events::build_live_events)); every
//! part after the first names the first on an `["e", <id>, "", "split"]` tag
//! (see [`split_head`]). A reader shows the first part with the content of all
//! of them, in order, and skips the rest.

use crate::session_events::{
    get_tag_value, split_head, split_index, AI_CONVERSATION_KIND, LIVE_EVENT_SOURCE,
};
use nostrdb::{Filter, Ndb, Note, Transaction};
use std::collections::HashMap;

/// Whether `note` is a part of a live message split across notes: a later
/// part, or a live note tagged as the first. A converted JSONL line's `split`
/// parts are messages of their own, so they are not.
pub fn is_split_part(note: &Note) -> bool {
    split_head(note).is_some()
        || (split_index(note).is_some() && get_tag_value(note, "source") == Some(LIVE_EVENT_SOURCE))
}

/// The later parts of the split messages among a set of notes, keyed by the
/// id of the first part they continue.
pub struct SplitParts<'n, 'a> {
    by_head: HashMap<[u8; 32], Vec<&'n Note<'a>>>,
}

impl<'n, 'a> SplitParts<'n, 'a> {
    /// Gather the later parts among `notes`.
    pub fn collect(notes: &'n [Note<'a>]) -> Self {
        let mut by_head: HashMap<[u8; 32], Vec<&'n Note<'a>>> = HashMap::new();
        for note in notes {
            if let Some(head) = split_head(note) {
                by_head.entry(*head).or_default().push(note);
            }
        }
        for parts in by_head.values_mut() {
            parts.sort_by_key(|part| split_index(part).map(|(index, _)| index));
        }
        Self { by_head }
    }

    /// `head`'s content followed by that of the later parts found for it, or
    /// `None` when none were: `head` is a note on its own.
    pub fn joined(&self, head: &Note) -> Option<String> {
        let parts = self.by_head.get(head.id())?;
        Some(join(head, parts.iter().copied()))
    }
}

/// `head`'s content followed by each part's.
fn join<'p, 'a: 'p>(head: &Note, parts: impl Iterator<Item = &'p Note<'a>>) -> String {
    let mut content = head.content().to_string();
    for part in parts {
        content.push_str(part.content());
    }
    content
}

/// A split message read back from ndb, from any one of its parts.
#[derive(Debug, PartialEq, Eq)]
pub struct GatheredSplit {
    /// The first part's id: the message's own id.
    pub head: [u8; 32],
    /// The content of every part found, in order.
    pub content: String,
    /// Every part the first one counts is in ndb.
    pub complete: bool,
}

/// The split message `note` is a part of, gathered from ndb, or `None` when
/// `note` is not part of one, or its first part isn't in ndb yet.
///
/// Only parts by the first part's author count.
pub fn gather_split(ndb: &Ndb, txn: &Transaction, note: &Note) -> Option<GatheredSplit> {
    if !is_split_part(note) {
        return None;
    }
    let head_id = split_head(note).copied().unwrap_or(*note.id());
    let head = ndb.get_note_by_id(txn, &head_id).ok()?;
    let (_, total) = split_index(&head)?;

    // The `#e` index is keyed on the id, so this finds every note that names
    // the head, its later parts among them.
    let filter = Filter::new()
        .kinds([AI_CONVERSATION_KIND as u64])
        .events([&head_id])
        .build();
    let author = *head.pubkey();
    let mut parts = ndb
        .fold(txn, &[filter], Vec::new(), |mut parts, part| {
            if split_head(&part) == Some(&head_id) && *part.pubkey() == author {
                parts.push(part);
            }
            parts
        })
        .ok()?;
    parts.sort_by_key(|part| split_index(part).map(|(index, _)| index));
    parts.dedup_by_key(|part| split_index(part).map(|(index, _)| index));

    let complete = parts
        .iter()
        .map(|part| split_index(part).map(|(index, _)| index))
        .eq((1..total).map(Some));
    Some(GatheredSplit {
        head: head_id,
        content: join(&head, parts.iter()),
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Message;
    use crate::session_events::{
        build_live_events, BuiltEvent, LiveEventTags, ThreadingState, MAX_WIRE_EVENT_BYTES,
    };
    use crate::session_loader::load_session_messages;
    use crate::test_support::{temp_ndb, TEST_SECKEY};
    use nostrdb::IngestMetadata;

    /// Text that escapes to far more than one note holds, with multi-byte
    /// chars that a cut must not land inside.
    fn oversized() -> String {
        "a \"quoted\" line é🦀\n".repeat(3 * MAX_WIRE_EVENT_BYTES / 20)
    }

    fn build(content: &str, role: &str, threading: &mut ThreadingState) -> Vec<BuiltEvent> {
        build_live_events(
            content,
            role,
            "split-session",
            None,
            LiveEventTags::default(),
            threading,
            &TEST_SECKEY,
        )
        .unwrap()
    }

    /// Ingest each event, waiting for it to be stored.
    async fn ingest(ndb: &Ndb, events: &[BuiltEvent]) {
        let filter = Filter::new().kinds([AI_CONVERSATION_KIND as u64]).build();
        for event in events {
            let sub = ndb.subscribe(std::slice::from_ref(&filter)).unwrap();
            ndb.process_event_with(&event.to_event_json(), IngestMetadata::new().client(true))
                .expect("ingest");
            let _ = ndb.wait_for_notes(sub, 1).await.unwrap();
        }
    }

    /// A message split across notes loads as one row, in its place, with all
    /// of its content.
    #[tokio::test]
    async fn split_message_loads_as_one_row() {
        let (_dir, ndb) = temp_ndb();
        let mut threading = ThreadingState::new();
        let text = oversized();
        let before = build("before", "user", &mut threading);
        let split = build(&text, "assistant", &mut threading);
        let after = build("after", "user", &mut threading);
        assert!(split.len() > 1);

        ingest(&ndb, &before).await;
        ingest(&ndb, &split).await;
        ingest(&ndb, &after).await;

        let txn = Transaction::new(&ndb).unwrap();
        let loaded = load_session_messages(&ndb, &txn, "split-session");
        let rows: Vec<(&str, &str)> = loaded
            .messages
            .iter()
            .map(|msg| match msg {
                Message::User(user) => ("user", user.as_str()),
                Message::Assistant(assistant) => ("assistant", assistant.text()),
                other => panic!("unexpected row {other:?}"),
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("user", "before"),
                ("assistant", text.as_str()),
                ("user", "after")
            ]
        );
        assert_eq!(loaded.messages.len(), loaded.orders.len());
    }

    /// A split message gathered from ndb is complete only once every part is
    /// there, from whichever part it's gathered, and always under its first
    /// part's id.
    #[tokio::test]
    async fn gather_split_waits_for_every_part() {
        let (_dir, ndb) = temp_ndb();
        let text = oversized();
        let parts = build(&text, "user", &mut ThreadingState::new());
        let last = parts.len() - 1;
        let head = parts[0].note_id;

        ingest(&ndb, &parts[..last]).await;
        {
            let txn = Transaction::new(&ndb).unwrap();
            let note = ndb.get_note_by_id(&txn, &parts[1].note_id).unwrap();
            let gathered = gather_split(&ndb, &txn, &note).expect("a part");
            assert_eq!(gathered.head, head);
            assert!(!gathered.complete);
        }

        ingest(&ndb, &parts[last..]).await;
        let txn = Transaction::new(&ndb).unwrap();
        for part in &parts {
            let note = ndb.get_note_by_id(&txn, &part.note_id).unwrap();
            let gathered = gather_split(&ndb, &txn, &note).expect("a part");
            assert_eq!(
                gathered,
                GatheredSplit {
                    head,
                    content: text.clone(),
                    complete: true,
                }
            );
        }
    }

    /// A note that fits is no part of anything.
    #[tokio::test]
    async fn a_whole_note_is_no_split() {
        let (_dir, ndb) = temp_ndb();
        let events = build("hello", "user", &mut ThreadingState::new());
        ingest(&ndb, &events).await;

        let txn = Transaction::new(&ndb).unwrap();
        let note = ndb.get_note_by_id(&txn, &events[0].note_id).unwrap();
        assert!(!is_split_part(&note));
        assert_eq!(gather_split(&ndb, &txn, &note), None);
    }
}
