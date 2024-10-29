use enostr::Pubkey;
use std::collections::HashSet;

#[derive(Default)]
pub struct Muted {
    // TODO - implement private mutes
    pub pubkeys: HashSet<Pubkey>,
    pub hashtags: HashSet<String>,
    pub words: HashSet<String>,
    pub threads: HashSet<[u8; 32]>,
}

impl std::fmt::Debug for Muted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let threads_hex: Vec<String> = self
            .threads
            .iter()
            .map(|thread| hex::encode(thread))
            .collect();
        f.debug_struct("Muted")
            .field("pubkeys", &self.pubkeys)
            .field("hashtags", &self.hashtags)
            .field("words", &self.words)
            .field("threads", &threads_hex)
            .finish()
    }
}
