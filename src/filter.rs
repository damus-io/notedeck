use crate::error::{Error, FilterError};
use crate::note::NoteRef;
use crate::Result;
use nostrdb::{Filter, FilterBuilder, Note, Subscription};
use std::collections::HashMap;
use tracing::{debug, warn};

/// A unified subscription has a local and remote component. The remote subid
/// tracks data received remotely, and local
#[derive(Debug, Clone)]
pub struct UnifiedSubscription {
    pub local: Subscription,
    pub remote: String,
}

/// Each relay can have a different filter state. For example, some
/// relays may have the contact list, some may not. Let's capture all of
/// these states so that some relays don't stop the states of other
/// relays.
#[derive(Debug)]
pub struct FilterStates {
    pub initial_state: FilterState,
    pub states: HashMap<String, FilterState>,
}

impl FilterStates {
    pub fn get(&mut self, relay: &str) -> &FilterState {
        // if our initial state is ready, then just use that
        if let FilterState::Ready(_) = self.initial_state {
            &self.initial_state
        } else {
            // otherwise we look at relay states
            if !self.states.contains_key(relay) {
                self.states
                    .insert(relay.to_string(), self.initial_state.clone());
            }
            self.states.get(relay).unwrap()
        }
    }

    pub fn get_any_gotremote(&self) -> Option<(&str, Subscription)> {
        for (k, v) in self.states.iter() {
            if let FilterState::GotRemote(sub) = v {
                return Some((k, *sub));
            }
        }

        None
    }

    pub fn get_any_ready(&self) -> Option<&Vec<Filter>> {
        if let FilterState::Ready(fs) = &self.initial_state {
            Some(fs)
        } else {
            for (_k, v) in self.states.iter() {
                if let FilterState::Ready(ref fs) = v {
                    return Some(fs);
                }
            }

            None
        }
    }

    pub fn new(initial_state: FilterState) -> Self {
        Self {
            initial_state,
            states: HashMap::new(),
        }
    }

    pub fn set_relay_state(&mut self, relay: String, state: FilterState) {
        if self.states.contains_key(&relay) {
            let current_state = self.states.get(&relay).unwrap();
            debug!(
                "set_relay_state: {:?} -> {:?} on {}",
                current_state, state, &relay,
            );
        }
        self.states.insert(relay, state);
    }
}

/// We may need to fetch some data from relays before our filter is ready.
/// [`FilterState`] tracks this.
#[derive(Debug, Clone)]
pub enum FilterState {
    NeedsRemote(Vec<Filter>),
    FetchingRemote(UnifiedSubscription),
    GotRemote(Subscription),
    Ready(Vec<Filter>),
    Broken(FilterError),
}

impl FilterState {
    /// We tried to fetch a filter but we wither got no data or the data
    /// was corrupted, preventing us from getting to the Ready state.
    /// Just mark the timeline as broken so that we can signal to the
    /// user that something went wrong
    pub fn broken(reason: FilterError) -> Self {
        Self::Broken(reason)
    }

    /// The filter is ready
    pub fn ready(filter: Vec<Filter>) -> Self {
        Self::Ready(filter)
    }

    /// We need some data from relays before we can continue. Example:
    /// for home timelines where we don't have a contact list yet. We
    /// need to fetch the contact list before we have the right timeline
    /// filter.
    pub fn needs_remote(filter: Vec<Filter>) -> Self {
        Self::NeedsRemote(filter)
    }

    /// We got the remote data. Local data should be available to build
    /// the filter for the [`FilterState::Ready`] state
    pub fn got_remote(local_sub: Subscription) -> Self {
        Self::GotRemote(local_sub)
    }

    /// We have sent off a remote subscription to get data needed for the
    /// filter. The string is the subscription id
    pub fn fetching_remote(sub_id: String, local_sub: Subscription) -> Self {
        let unified_sub = UnifiedSubscription {
            local: local_sub,
            remote: sub_id,
        };
        Self::FetchingRemote(unified_sub)
    }
}

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
    500
}

pub fn default_remote_limit() -> u64 {
    250
}

pub struct FilteredTags {
    pub authors: Option<FilterBuilder>,
    pub hashtags: Option<FilterBuilder>,
}

impl FilteredTags {
    pub fn into_follow_filter(self) -> Vec<Filter> {
        self.into_filter([1], default_limit())
    }

    // TODO: make this more general
    pub fn into_filter<I>(self, kinds: I, limit: u64) -> Vec<Filter>
    where
        I: IntoIterator<Item = u64> + Copy,
    {
        let mut filters: Vec<Filter> = Vec::with_capacity(2);

        if let Some(authors) = self.authors {
            filters.push(authors.kinds(kinds).limit(limit).build())
        }

        if let Some(hashtags) = self.hashtags {
            filters.push(hashtags.kinds(kinds).limit(limit).build())
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
        return Err(Error::empty_contact_list());
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
