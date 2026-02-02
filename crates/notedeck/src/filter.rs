use crate::error::{Error, FilterError};
use crate::note::NoteRef;
use enostr::OutboxSubId;
use nostrdb::{Filter, FilterBuilder, Note, Subscription};
use tracing::{debug, warn};

/// A unified subscription has a local and remote component. The remote subid
/// tracks data received remotely, and local
#[derive(Debug, Clone)]
pub struct UnifiedSubscription {
    pub local: Subscription,
    pub remote: OutboxSubId, // abstracted ID to a remote subscription
}

/// We may need to fetch some data from relays before our filter is ready.
/// [`FilterState`] tracks this.
#[derive(Debug, Clone)]
pub enum FilterState {
    NeedsRemote,
    FetchingRemote,
    GotRemote,
    Ready(HybridFilter),
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
        Self::Ready(HybridFilter::unsplit(filter))
    }

    /// Our hybrid filter is ready (either split or unsplit)
    pub fn ready_hybrid(filter: HybridFilter) -> Self {
        Self::Ready(filter)
    }

    /// We need some data from relays before we can continue. Example:
    /// for home timelines where we don't have a contact list yet. We
    /// need to fetch the contact list before we have the right timeline
    /// filter.
    pub fn needs_remote() -> Self {
        Self::NeedsRemote
    }
}

pub fn should_since_optimize(limit: u64, num_notes: usize) -> bool {
    // rough heuristic for bailing since optimization if we don't have enough notes
    limit as usize <= num_notes
}

pub fn since_optimize_filter_with(
    filter: Filter,
    latest_note: Option<&NoteRef>,
    since_gap: u64,
) -> Filter {
    // Get the latest entry in the events
    let Some(latest) = latest_note else {
        return filter;
    };

    // get the latest note
    let since = latest.created_at - since_gap;

    filter.since_mut(since)
}

pub fn since_optimize_filter(filter: Filter, latest: Option<&NoteRef>) -> Filter {
    since_optimize_filter_with(filter, latest, 60)
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

/// The local and remote filter are related but slightly different
#[derive(Debug, Clone)]
pub struct SplitFilter {
    pub local: Vec<NdbQueryPackage>,
    pub remote: Vec<Filter>,
}

/// Either a [`SplitFilter`] or a regular unsplit filter,. Split filters
/// have different remote and local filters but are tracked together.
#[derive(Debug, Clone)]
pub enum HybridFilter {
    Split(SplitFilter),
    Unsplit(Vec<Filter>),
}

impl HybridFilter {
    pub fn unsplit(filter: Vec<Filter>) -> Self {
        HybridFilter::Unsplit(filter)
    }

    pub fn split(local: Vec<NdbQueryPackage>, remote: Vec<Filter>) -> Self {
        HybridFilter::Split(SplitFilter { local, remote })
    }

    pub fn local(&self) -> NdbQueryPackages<'_> {
        match self {
            Self::Split(split) => NdbQueryPackages {
                packages: split.local.iter().map(NdbQueryPackage::borrow).collect(),
            },

            // local as the same as remote in unsplit
            Self::Unsplit(local) => NdbQueryPackages {
                packages: vec![NdbQueryPackageUnowned {
                    filters: local,
                    kind: None,
                }],
            },
        }
    }

    pub fn remote(&self) -> &[Filter] {
        match self {
            Self::Split(split) => &split.remote,

            // local as the same as remote in unsplit
            Self::Unsplit(remote) => remote,
        }
    }
}

impl FilteredTags {
    pub fn into_query_package(self, kind: ValidKind, limit: u64) -> NdbQueryPackage {
        let mut filters: Vec<Filter> = Vec::with_capacity(2);

        if let Some(authors) = self.authors {
            filters.push(authors.kinds(vec![kind.kind()]).limit(limit).build())
        }

        if let Some(hashtags) = self.hashtags {
            if matches!(&kind, ValidKind::One | ValidKind::Zero) {
                filters.push(hashtags.kinds(vec![kind.kind()]).limit(limit).build())
            }
        }

        NdbQueryPackage { filters, kind }
    }

    // TODO: make this more general
    pub fn into_filter(self, shared_kinds: Vec<u64>, limit: u64) -> Vec<Filter> {
        let mut filters: Vec<Filter> = Vec::with_capacity(2);

        if let Some(authors) = self.authors {
            let mut author_kinds = shared_kinds.clone();
            author_kinds.insert(0, 6);

            filters.push(authors.kinds(author_kinds).limit(limit).build())
        }

        if let Some(hashtags) = self.hashtags {
            filters.push(hashtags.kinds(shared_kinds).limit(limit).build())
        }

        filters
    }
}

/// `Ndb::query` retrieves the most recent notes of one kind until it can't find anymore THEN proceeds to the next kind.
/// This is not optimal for many scenarios, so this data structure represents data that is packaged optimally for one `Ndb::query`,
#[derive(Debug, Clone)]
pub struct NdbQueryPackage {
    pub kind: ValidKind,
    pub filters: Vec<Filter>,
}

impl NdbQueryPackage {
    pub fn borrow(&self) -> NdbQueryPackageUnowned<'_> {
        NdbQueryPackageUnowned {
            filters: &self.filters,
            kind: Some(self.kind.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NdbQueryPackageUnowned<'a> {
    pub kind: Option<ValidKind>,
    pub filters: &'a Vec<Filter>,
}

pub struct NdbQueryPackages<'a> {
    pub packages: Vec<NdbQueryPackageUnowned<'a>>,
}

impl<'a> NdbQueryPackages<'a> {
    pub fn combined(&self) -> Vec<Filter> {
        let mut combined = Vec::new();
        for package in &self.packages {
            combined.extend_from_slice(package.filters);
        }

        combined
    }
}

#[derive(Debug, Clone)]
pub enum ValidKind {
    Zero,
    One,
    Six,
}

impl ValidKind {
    fn kind(&self) -> u64 {
        match self {
            ValidKind::Zero => 0,
            ValidKind::One => 1,
            ValidKind::Six => 6,
        }
    }
}

/// Create a "last N notes per pubkey" query.
pub fn last_n_per_pubkey_from_tags(
    note: &Note,
    kind: u64,
    notes_per_pubkey: u64,
) -> Result<Vec<Filter>, Error> {
    use rand::Rng;

    let mut filters: Vec<Filter> = vec![];
    let mut rng = rand::rng();

    // TODO: fix arbitrary MAX_FILTER limit in nostrdb
    const LIMIT: usize = 15;

    for (i, tag) in note.tags().iter().enumerate() {
        if tag.count() < 2 {
            continue;
        }

        let Some("p") = tag.get_str(0) else {
            continue;
        };

        let Some(author) = tag.get_id(1) else {
            continue;
        };

        let mk_filter = || {
            let mut filter = Filter::new();
            let _ = filter.start_authors_field();
            let _ = filter.add_id_element(author);
            filter.end_field();
            filter.kinds([kind]).limit(notes_per_pubkey).build()
        };

        // since we're limited due to a nostrdb bug, we reservoir sample to keep things interesting
        if filters.len() < LIMIT {
            filters.push(mk_filter());
        } else {
            let j = rng.random_range(0..=i);
            if j < LIMIT {
                filters[j] = mk_filter();
            }
        }
    }

    Ok(filters)
}

/// Create a filter from tags. This can be used to create a filter
/// from a contact list
pub fn filter_from_tags(
    note: &Note,
    add_pubkey: Option<&[u8; 32]>,
    with_hashtags: bool,
) -> Result<FilteredTags, Error> {
    let mut author_filter = Filter::new();
    let mut hashtag_filter = Filter::new();
    let mut author_res: Option<FilterBuilder> = None;
    let mut hashtag_res: Option<FilterBuilder> = None;
    let mut author_count = 0i32;
    let mut hashtag_count = 0i32;
    let mut has_added_pubkey = false;

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

            if let Some(pk) = add_pubkey {
                if author == pk {
                    // we don't need to add it afterwards
                    has_added_pubkey = true;
                }
            }

            author_filter.add_id_element(author)?;
            author_count += 1;
        } else if t == "t" && with_hashtags {
            let hashtag = if let Some(hashtag) = tag.get_unchecked(1).variant().str() {
                hashtag
            } else {
                continue;
            };

            hashtag_filter.add_str_element(hashtag)?;
            hashtag_count += 1;
        }
    }

    // some additional ad-hoc logic for adding a pubkey
    if let Some(pk) = add_pubkey {
        if !has_added_pubkey {
            author_filter.add_id_element(pk)?;
            author_count += 1;
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

pub fn make_filters_since(raw: &[Filter], since: u64) -> Vec<Filter> {
    let mut filters = Vec::with_capacity(raw.len());
    for builder in raw {
        filters.push(Filter::copy_from(builder).since(since).build());
    }
    filters
}
