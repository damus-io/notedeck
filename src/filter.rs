use crate::note::NoteRef;
use nostrdb::Filter;

pub fn should_since_optimize(limit: u64, num_notes: usize) -> bool {
    // rough heuristic for bailing since optimization if we don't have enough notes
    limit as usize <= num_notes
}

pub fn since_optimize_filter_with(filter: Filter, notes: &[NoteRef], since_gap: u64) -> Filter {
    // Get the latest entry in the events
    if notes.is_empty() {
        return filter;
    }

    // get the latest note
    let latest = notes[0];
    let since = latest.created_at - since_gap;

    filter.since_mut(since)
}

pub fn since_optimize_filter(filter: Filter, notes: &[NoteRef]) -> Filter {
    since_optimize_filter_with(filter, notes, 60)
}

pub fn default_limit() -> u64 {
    250
}

pub fn default_remote_limit() -> u64 {
    150
}
