pub fn convert_enostr_filter(filter: &enostr::Filter) -> nostrdb::Filter {
    let mut nfilter = nostrdb::Filter::new();

    if let Some(ref ids) = filter.ids {
        nfilter.ids(ids.iter().map(|a| *a.bytes()).collect());
    }

    if let Some(ref authors) = filter.authors {
        let authors: Vec<[u8; 32]> = authors.iter().map(|a| a.bytes()).collect();
        nfilter.authors(authors);
    }

    if let Some(ref kinds) = filter.kinds {
        nfilter.kinds(kinds.clone());
    }

    // #e
    if let Some(ref events) = filter.events {
        nfilter.events(events.iter().map(|a| *a.bytes()).collect());
    }

    // #p
    if let Some(ref pubkeys) = filter.pubkeys {
        nfilter.pubkeys(pubkeys.iter().map(|a| a.bytes()).collect());
    }

    if let Some(since) = filter.since {
        nfilter.since(since);
    }

    if let Some(limit) = filter.limit {
        nfilter.limit(limit.into());
    }

    nfilter
}
