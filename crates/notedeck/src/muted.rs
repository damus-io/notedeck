use nostrdb::Note;
use std::collections::BTreeSet;

//use tracing::{debug, trace};

// If the note is muted return a reason string, otherwise None
pub type MuteFun = dyn Fn(&Note, &[u8; 32]) -> bool;

#[derive(Clone)]
pub struct Muted {
    // TODO - implement private mutes
    pub pubkeys: BTreeSet<[u8; 32]>,
    pub hashtags: BTreeSet<String>,
    pub words: BTreeSet<String>,
    pub threads: BTreeSet<[u8; 32]>,
    pub max_hashtags_per_note: usize,
}

impl Default for Muted {
    fn default() -> Self {
        Muted {
            max_hashtags_per_note: crate::persist::DEFAULT_MAX_HASHTAGS_PER_NOTE,
            pubkeys: Default::default(),
            hashtags: Default::default(),
            words: Default::default(),
            threads: Default::default(),
        }
    }
}

impl std::fmt::Debug for Muted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Muted")
            .field(
                "pubkeys",
                &self.pubkeys.iter().map(hex::encode).collect::<Vec<_>>(),
            )
            .field("hashtags", &self.hashtags)
            .field("words", &self.words)
            .field(
                "threads",
                &self.threads.iter().map(hex::encode).collect::<Vec<_>>(),
            )
            .field("max_hashtags_per_note", &self.max_hashtags_per_note)
            .finish()
    }
}

impl Muted {
    // If the note is muted return a reason string, otherwise None
    pub fn is_muted(&self, note: &Note, thread: &[u8; 32]) -> bool {
        /*
        trace!(
            "{}: thread: {}",
            hex::encode(note.id()),
            hex::encode(thread)
        );
        */

        if self.pubkeys.contains(note.pubkey()) {
            /*
            trace!(
                "{}: MUTED pubkey: {}",
                hex::encode(note.id()),
                hex::encode(note.pubkey())
            );
            */
            return true;
        }

        // Filter notes with too many hashtags (early return on limit exceeded)
        if self.max_hashtags_per_note > 0 {
            let hashtag_count = self.count_hashtags(note);
            if hashtag_count > self.max_hashtags_per_note {
                return true;
            }
        }

        // FIXME - Implement hashtag muting here

        // TODO - let's not add this for now, we will likely need to
        // have an optimized data structure in nostrdb to properly
        // mute words. this mutes substrings which is not ideal.
        //
        // let content = note.content().to_lowercase();
        // for word in &self.words {
        //     if content.contains(&word.to_lowercase()) {
        //         debug!("{}: MUTED word: {}", hex::encode(note.id()), word);
        //         return Some(format!("muted word {}", word));
        //     }
        // }

        if self.threads.contains(thread) {
            /*
            trace!(
                "{}: MUTED thread: {}",
                hex::encode(note.id()),
                hex::encode(thread)
            );
            */
            return true;
        }

        false
    }

    /// Count the number of hashtags in a note by examining its tags
    fn count_hashtags(&self, note: &Note) -> usize {
        let mut count = 0;

        for tag in note.tags() {
            // Early continue if not enough elements
            if tag.count() < 2 {
                continue;
            }

            // Check if this is a hashtag tag (type "t")
            let tag_type = match tag.get_unchecked(0).variant().str() {
                Some(t) => t,
                None => continue,
            };

            if tag_type != "t" {
                continue;
            }

            // Verify the hashtag value exists
            if tag.get_unchecked(1).variant().str().is_some() {
                count += 1;
            }
        }

        count
    }

    pub fn is_pk_muted(&self, pk: &[u8; 32]) -> bool {
        self.pubkeys.contains(pk)
    }
}
