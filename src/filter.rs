use crate::note::NoteRef;
use crate::{Error, Result};
use nostrdb::{Filter, FilterBuilder, Note};

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

/// Create a filter from tags. This can be used to create a filter
/// from a contact list
pub fn filter_from_tags(note: &Note) -> Result<FilterBuilder> {
    let mut filter = Filter::new();
    let tags = note.tags();
    let mut authors: Vec<&[u8; 32]> = Vec::with_capacity(tags.count() as usize);
    let mut hashtags: Vec<&str> = vec![];

    for tag in tags {
        if tag.count() < 2 {
            continue;
        }

        let t = if let Some(t) = tag.get_unchecked(0).variant().str() {
            t
        } else {
            continue;
        };

        if t == "p" {
            let author = if let Some(author) = tag.get_unchecked(1).variant().id() {
                author
            } else {
                continue;
            };

            authors.push(author);
        } else if t == "t" {
            let hashtag = if let Some(hashtag) = tag.get_unchecked(1).variant().str() {
                hashtag
            } else {
                continue;
            };

            hashtags.push(hashtag);
        }
    }

    if authors.is_empty() && hashtags.is_empty() {
        return Err(Error::EmptyContactList);
    }

    // if we hit these ooms, we need to expand filter buffer size
    if !authors.is_empty() {
        filter.start_authors_field()?;
        for author in authors {
            filter.add_id_element(author)?;
        }
        filter.end_field();
    }

    if !hashtags.is_empty() {
        filter.start_tags_field('t')?;
        for hashtag in hashtags {
            filter.add_str_element(hashtag)?;
        }
        filter.end_field();
    }

    Ok(filter)
}
