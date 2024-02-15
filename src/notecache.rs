use crate::time::time_ago_since;
use crate::timecache::TimeCached;
use std::time::Duration;

pub struct NoteCache {
    reltime: TimeCached<String>,
}

impl NoteCache {
    pub fn new(created_at: u64) -> Self {
        let reltime = TimeCached::new(
            Duration::from_secs(1),
            Box::new(move || time_ago_since(created_at)),
        );
        NoteCache { reltime }
    }

    pub fn reltime_str(&mut self) -> &str {
        return &self.reltime.get();
    }
}
