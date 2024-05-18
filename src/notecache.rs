use crate::time::time_ago_since;
use crate::timecache::TimeCached;
use nostrdb::{Note, NoteKey, NoteReply, NoteReplyBuf};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Default)]
pub struct NoteCache {
    pub cache: HashMap<NoteKey, CachedNote>,
}

pub struct CachedNote {
    reltime: TimeCached<String>,
    pub reply: NoteReplyBuf,
    pub bar_open: bool,
}

impl CachedNote {
    pub fn new(note: &Note<'_>) -> Self {
        let created_at = note.created_at();
        let reltime = TimeCached::new(
            Duration::from_secs(1),
            Box::new(move || time_ago_since(created_at)),
        );
        let reply = NoteReply::new(note.tags()).to_owned();
        let bar_open = false;
        CachedNote {
            reltime,
            reply,
            bar_open,
        }
    }

    pub fn reltime_str(&mut self) -> &str {
        return self.reltime.get();
    }
}
