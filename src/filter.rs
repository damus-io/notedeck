use crate::note::NoteRef;
use crate::{Error, Result};
use nostrdb::{Filter, FilterBuilder, Note};
use tracing::{debug, warn};

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

pub struct FilteredTags {
    pub authors: Option<FilterBuilder>,
    pub hashtags: Option<FilterBuilder>,
}

impl FilteredTags {
    // TODO: make this more general
    pub fn into_filter<I>(self, kinds: I) -> Vec<Filter>
    where
        I: IntoIterator<Item = u64> + Copy,
    {
        let mut filters: Vec<Filter> = Vec::with_capacity(2);

        if let Some(authors) = self.authors {
            filters.push(authors.kinds(kinds).build())
        }

        if let Some(hashtags) = self.hashtags {
            filters.push(hashtags.kinds(kinds).build())
        }

        filters
    }
}

/// Create a filter from tags. This can be used to create a filter
/// from a contact list
pub fn filter_from_tags(note: &Note) -> Result<FilteredTags> {
    let mut author_filter = Filter::new();
    let mut hashtag_filter = Filter::new();
    let mut author_res: Option<FilterBuilder> = None;
    let mut hashtag_res: Option<FilterBuilder> = None;
    let mut author_count = 0i32;
    let mut hashtag_count = 0i32;

    let tags = note.tags();

    author_filter.start_authors_field()?;
    hashtag_filter.start_tags_field('t')?;

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

            author_filter.add_id_element(author)?;
            author_count += 1;
        } else if t == "t" {
            let hashtag = if let Some(hashtag) = tag.get_unchecked(1).variant().str() {
                hashtag
            } else {
                continue;
            };

            hashtag_filter.add_str_element(hashtag)?;
            hashtag_count += 1;
        }
    }

    author_filter.end_field();
    hashtag_filter.end_field();

    if author_count == 0 && hashtag_count == 0 {
        warn!("no authors or hashtags found in contact list");
        return Err(Error::EmptyContactList);
    }

    debug!(
        "adding {} authors and {} hashtags to contact filter",
        author_count, hashtag_count
    );

    // if we hit these ooms, we need to expand filter buffer size
    if author_count > 0 {
        author_res = Some(author_filter)
    }

    if hashtag_count > 0 {
        hashtag_res = Some(hashtag_filter)
    }

    Ok(FilteredTags {
        authors: author_res,
        hashtags: hashtag_res,
    })
}
