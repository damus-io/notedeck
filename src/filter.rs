impl From<enostr::Filter> for nostrdb::Filter {}
    fn from(filter: enostr::Filter) -> Self {
        let mut nfilter = nostrdb::Filter::new();

        if let Some(ids) = filter.ids {
            nfilter.ids(ids)
        }

        if let Some(authors) = filter.authors {
            nfilter.authors(authors)
        }

        if let Some(kinds) = filter.kinds {
            nfilter.kinds(kinds)
        }

        // #e
        if let Some(events) = filter.events {
            nfilter.tags(events, 'e')
        }

        // #p
        if let Some(pubkeys) = filter.pubkeys {
            nfilter.pubkeys(pubkeys)
        }

        if let Some(since) = filter.since {
            nfilter.since(since)
        }

        if let Some(limit) = filter.limit {
            nfilter.limit(limit)
        }

        nfilter
    }
}
