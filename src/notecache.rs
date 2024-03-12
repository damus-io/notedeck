use crate::time::time_ago_since;
use crate::timecache::TimeCached;
use std::time::Duration;

pub struct NoteCache {
    reltime: TimeCached<String>,
    pub bar_open: bool,
}

impl NoteCache {
    pub fn new(created_at: u64) -> Self {
        let reltime = TimeCached::new(
            Duration::from_secs(1),
            Box::new(move || time_ago_since(created_at)),
        );
        let bar_open = false;
        NoteCache { reltime, bar_open }
    }

    pub fn reltime_str(&mut self) -> &str {
        return &self.reltime.get();
    }
}
