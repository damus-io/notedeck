use crate::note::NoteRef;

pub fn should_since_optimize(limit: Option<u16>, num_notes: usize) -> bool {
    let limit = limit.unwrap_or(enostr::Filter::default_limit()) as usize;

    // rough heuristic for bailing since optimization if we don't have enough notes
    limit <= num_notes
}

pub fn since_optimize_filter_with(filter: &mut enostr::Filter, notes: &[NoteRef], since_gap: u64) {
    // Get the latest entry in the events
    if notes.is_empty() {
        return;
    }

    // get the latest note
    let latest = notes[0];
    let since = latest.created_at - since_gap;

    // update the filters
    filter.since = Some(since);
}

pub fn since_optimize_filter(filter: &mut enostr::Filter, notes: &[NoteRef]) {
    since_optimize_filter_with(filter, notes, 60);
}

pub fn convert_enostr_filter(filter: &enostr::Filter) -> nostrdb::Filter {
    let mut nfilter = nostrdb::Filter::new();

    if let Some(ref ids) = filter.ids {
        nfilter = nfilter.ids(ids.iter().map(|a| *a.bytes()).collect());
    }

    if let Some(ref authors) = filter.authors {
        let authors: Vec<[u8; 32]> = authors.iter().map(|a| *a.bytes()).collect();
        nfilter = nfilter.authors(authors);
    }

    if let Some(ref kinds) = filter.kinds {
        nfilter = nfilter.kinds(kinds.clone());
    }

    // #e
    if let Some(ref events) = filter.events {
        nfilter = nfilter.events(events.iter().map(|a| *a.bytes()).collect());
    }

    // #p
    if let Some(ref pubkeys) = filter.pubkeys {
        nfilter = nfilter.pubkeys(pubkeys.iter().map(|a| *a.bytes()).collect());
    }

    // #t
    if let Some(ref hashtags) = filter.hashtags {
        nfilter = nfilter.tags(hashtags.clone(), 't');
    }

    if let Some(since) = filter.since {
        nfilter = nfilter.since(since);
    }

    if let Some(until) = filter.until {
        nfilter = nfilter.until(until);
    }

    if let Some(limit) = filter.limit {
        nfilter = nfilter.limit(limit.into());
    }

    nfilter.build()
}
