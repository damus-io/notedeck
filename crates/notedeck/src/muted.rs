use nostrdb::Note;
use std::collections::BTreeSet;

//use tracing::{debug, trace};

// If the note is muted return a reason string, otherwise None
pub type MuteFun = dyn Fn(&Note, &[u8; 32]) -> bool;

#[derive(Clone, Default)]
pub struct Muted {
    // TODO - implement private mutes
    pub pubkeys: BTreeSet<[u8; 32]>,
    pub hashtags: BTreeSet<String>,
    pub words: BTreeSet<String>,
    pub threads: BTreeSet<[u8; 32]>,
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
}
