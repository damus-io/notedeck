use nostrdb::{Note, NoteKey, NoteReply, NoteReplyBuf};
use std::collections::HashMap;

#[derive(Default)]
pub struct NoteCache {
    pub cache: HashMap<NoteKey, CachedNote>,
}

impl NoteCache {
    pub fn cached_note_or_insert_mut(&mut self, note_key: NoteKey, note: &Note) -> &mut CachedNote {
        self.cache
            .entry(note_key)
            .or_insert_with(|| CachedNote::new(note))
    }

    pub fn cached_note(&self, note_key: NoteKey) -> Option<&CachedNote> {
        self.cache.get(&note_key)
    }

    pub fn cache_mut(&mut self) -> &mut HashMap<NoteKey, CachedNote> {
        &mut self.cache
    }

    pub fn cached_note_or_insert(&mut self, note_key: NoteKey, note: &Note) -> &CachedNote {
        self.cache
            .entry(note_key)
            .or_insert_with(|| CachedNote::new(note))
    }
}

#[derive(Clone)]
pub struct CachedNote {
    //reltime: TimeCached<String>,
    pub client: Option<String>,
    pub reply: NoteReplyBuf,
}

impl CachedNote {
    pub fn new(note: &Note) -> Self {
        use crate::note::event_tag;

        /*
        let created_at = note.created_at();
        let reltime = TimeCached::new(
            Duration::from_secs(1),
            Box::new(move || time_ago_since(i18n, created_at)),
        );
        */
        let reply = NoteReply::new(note.tags()).to_owned();

        let client = event_tag(note, "client");

        CachedNote {
            client: client.map(|c| c.to_string()),
            //   reltime,
            reply,
        }
    }

    /*
    pub fn reltime_str_mut(&mut self) -> &str {
        self.reltime.get_mut()
    }

    pub fn reltime_str(&self) -> Option<&str> {
        self.reltime.get().map(|x| x.as_str())
    }
    */
}
