use indexmap::IndexSet;
use nostrdb::Note;

/// Bookmarked note ids, in the order they appear as "e" tags on the
/// account's NIP-51 kind-10003 bookmark list note (oldest first, since
/// `send_bookmark_event` appends new tags at the end).
#[derive(Clone, Default)]
pub struct Bookmarks {
    pub note_ids: IndexSet<[u8; 32]>,
}

impl std::fmt::Debug for Bookmarks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bookmarks")
            .field(
                "note_ids",
                &self.note_ids.iter().map(hex::encode).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Bookmarks {
    pub fn is_bookmarked(&self, note_id: &[u8; 32]) -> bool {
        self.note_ids.contains(note_id)
    }

    /// Bookmarked note ids, most-recently-bookmarked first.
    pub fn most_recent_first(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.note_ids.iter().rev()
    }
}

/// Parse a NIP-51 kind-10003 bookmark list note's "e" tags into a
/// [`Bookmarks`]. Reusable by anything that has a fresh copy of the note
/// (the live account cache, or a one-off ndb query).
pub fn bookmarks_from_note(note: &Note<'_>) -> Bookmarks {
    let mut bookmarks = Bookmarks::default();

    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }
        let Some("e") = tag.get_str(0) else {
            continue;
        };
        if let Some(id) = tag.get_id(1) {
            bookmarks.note_ids.insert(*id);
        }
    }

    bookmarks
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::FullKeypair;
    use nostrdb::{NoteBuildOptions, NoteBuilder};

    #[test]
    fn bookmarks_from_note_preserves_tag_order() {
        let kp = FullKeypair::generate();
        let ids: [[u8; 32]; 3] = [[1; 32], [2; 32], [3; 32]];

        let mut builder = NoteBuilder::new()
            .content("")
            .kind(10003)
            .options(NoteBuildOptions::default());
        for id in &ids {
            builder = builder.start_tag().tag_str("e").tag_id(id);
        }
        let note = builder
            .sign(&kp.secret_key.secret_bytes())
            .build()
            .expect("build note");

        let bookmarks = bookmarks_from_note(&note);

        assert_eq!(
            bookmarks.most_recent_first().collect::<Vec<_>>(),
            vec![&ids[2], &ids[1], &ids[0]],
            "most_recent_first should return newest-tagged bookmark first"
        );
        assert!(bookmarks.is_bookmarked(&ids[0]));
        assert!(!bookmarks.is_bookmarked(&[9; 32]));
    }
}
