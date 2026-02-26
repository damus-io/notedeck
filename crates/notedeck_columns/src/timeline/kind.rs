use crate::error::Error;
use crate::search::SearchQuery;
use crate::timeline::{Timeline, TimelineTab};
use enostr::{Filter, NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::filter::{NdbQueryPackage, ValidKind};
use notedeck::{
    contacts::{contacts_filter, hybrid_contacts_filter},
    filter::{self, default_limit, default_remote_limit, HybridFilter},
    tr, FilterError, FilterState, Localization, NoteCache, RootIdError, RootNoteIdBuf,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};
use tracing::{debug, error, warn};

#[derive(Clone, Hash, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PubkeySource {
    Explicit(Pubkey),
    #[default]
    DeckAuthor,
}

/// Reference to a NIP-51 people list (kind 30000), identified by author + "d" tag
#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct PeopleListRef {
    pub author: Pubkey,
    pub identifier: String,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub enum ListKind {
    Contact(Pubkey),
    /// A NIP-51 people list (kind 30000)
    PeopleList(PeopleListRef),
}

impl ListKind {
    pub fn pubkey(&self) -> Option<&Pubkey> {
        match self {
            Self::Contact(pk) => Some(pk),
            Self::PeopleList(plr) => Some(&plr.author),
        }
    }
}

impl PubkeySource {
    pub fn pubkey(pubkey: Pubkey) -> Self {
        PubkeySource::Explicit(pubkey)
    }

    pub fn as_pubkey<'a>(&'a self, deck_author: &'a Pubkey) -> &'a Pubkey {
        match self {
            PubkeySource::Explicit(pk) => pk,
            PubkeySource::DeckAuthor => deck_author,
        }
    }
}

impl TokenSerializable for PubkeySource {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            PubkeySource::DeckAuthor => {
                writer.write_token("deck_author");
            }
            PubkeySource::Explicit(pk) => {
                writer.write_token(&hex::encode(pk.bytes()));
            }
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.try_parse(|p| {
            match p.pull_token() {
                // we handle bare payloads and assume they are explicit pubkey sources
                Ok("explicit") => {
                    if let Ok(hex) = p.pull_token() {
                        let pk = Pubkey::from_hex(hex).map_err(|_| ParseError::HexDecodeFailed)?;
                        Ok(PubkeySource::Explicit(pk))
                    } else {
                        Err(ParseError::HexDecodeFailed)
                    }
                }

                Err(_) | Ok("deck_author") => Ok(PubkeySource::DeckAuthor),

                Ok(hex) => {
                    let pk = Pubkey::from_hex(hex).map_err(|_| ParseError::HexDecodeFailed)?;
                    Ok(PubkeySource::Explicit(pk))
                }
            }
        })
    }
}

impl ListKind {
    pub fn contact_list(pk: Pubkey) -> Self {
        ListKind::Contact(pk)
    }

    pub fn people_list(author: Pubkey, identifier: String) -> Self {
        ListKind::PeopleList(PeopleListRef { author, identifier })
    }

    pub fn parse<'a>(
        parser: &mut TokenParser<'a>,
        deck_author: &Pubkey,
    ) -> Result<Self, ParseError<'a>> {
        let contact = parser.try_parse(|p| {
            p.parse_all(|p| {
                p.parse_token("contact")?;
                let pk_src = PubkeySource::parse_from_tokens(p)?;
                Ok(ListKind::Contact(*pk_src.as_pubkey(deck_author)))
            })
        });
        if contact.is_ok() {
            return contact;
        }

        parser.parse_all(|p| {
            p.parse_token("people_list")?;
            let pk_src = PubkeySource::parse_from_tokens(p)?;
            let identifier = p.pull_token()?.to_string();
            Ok(ListKind::PeopleList(PeopleListRef {
                author: *pk_src.as_pubkey(deck_author),
                identifier,
            }))
        })
    }

    pub fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            ListKind::Contact(pk) => {
                writer.write_token("contact");
                PubkeySource::pubkey(*pk).serialize_tokens(writer);
            }
            ListKind::PeopleList(plr) => {
                writer.write_token("people_list");
                PubkeySource::pubkey(plr.author).serialize_tokens(writer);
                writer.write_token(&plr.identifier);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThreadSelection {
    pub root_id: RootNoteIdBuf,

    /// The selected note, if different than the root_id. None here
    /// means the root is selected
    pub selected_note: Option<NoteId>,
}

impl ThreadSelection {
    pub fn selected_or_root(&self) -> &[u8; 32] {
        self.selected_note
            .as_ref()
            .map(|sn| sn.bytes())
            .unwrap_or(self.root_id.bytes())
    }

    pub fn from_root_id(root_id: RootNoteIdBuf) -> Self {
        Self {
            root_id,
            selected_note: None,
        }
    }

    pub fn from_note_id(
        ndb: &Ndb,
        note_cache: &mut NoteCache,
        txn: &Transaction,
        note_id: NoteId,
    ) -> Result<Self, RootIdError> {
        let root_id = RootNoteIdBuf::new(ndb, note_cache, txn, note_id.bytes())?;
        Ok(if root_id.bytes() == note_id.bytes() {
            Self::from_root_id(root_id)
        } else {
            Self {
                root_id,
                selected_note: Some(note_id),
            }
        })
    }
}

/// Thread selection hashing is done in a specific way. For TimelineCache
/// lookups, we want to only let the root_id influence thread selection.
/// This way Thread TimelineKinds always map to the same cached timeline
/// for now (we will likely have to rework this since threads aren't
/// *really* timelines)
impl Hash for ThreadSelection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // only hash the root id for thread selection
        self.root_id.hash(state)
    }
}

// need this to only match root_id or else hash lookups will fail
impl PartialEq for ThreadSelection {
    fn eq(&self, other: &Self) -> bool {
        self.root_id == other.root_id
    }
}

impl Eq for ThreadSelection {}

///
/// What kind of timeline is it?
///   - Follow List
///   - Notifications
///   - DM
///   - filter
///   - ... etc
///
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TimelineKind {
    List(ListKind),

    Search(SearchQuery),

    /// The last not per pubkey
    Algo(AlgoTimeline),

    Notifications(Pubkey),

    Profile(Pubkey),

    Universe,

    /// Generic filter, references a hash of a filter
    Generic(u64),

    Hashtag(Vec<String>),
}

const NOTIFS_TOKEN_DEPRECATED: &str = "notifs";
const NOTIFS_TOKEN: &str = "notifications";

/// Hardcoded algo timelines
#[derive(Debug, Hash, Clone, PartialEq, Eq)]
pub enum AlgoTimeline {
    /// LastPerPubkey: a special nostr query that fetches the last N
    /// notes for each pubkey on the list
    LastPerPubkey(ListKind),
}

/// The identifier for our last per pubkey algo
const LAST_PER_PUBKEY_TOKEN: &str = "last_per_pubkey";

impl AlgoTimeline {
    pub fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            AlgoTimeline::LastPerPubkey(list_kind) => {
                writer.write_token(LAST_PER_PUBKEY_TOKEN);
                list_kind.serialize_tokens(writer);
            }
        }
    }

    pub fn parse<'a>(
        parser: &mut TokenParser<'a>,
        deck_author: &Pubkey,
    ) -> Result<Self, ParseError<'a>> {
        parser.parse_all(|p| {
            p.parse_token(LAST_PER_PUBKEY_TOKEN)?;
            Ok(AlgoTimeline::LastPerPubkey(ListKind::parse(
                p,
                deck_author,
            )?))
        })
    }
}

/*
impl Display for TimelineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimelineKind::List(ListKind::Contact(_src)) => write!(
                f,
                "{}",
                tr!("Home", "Timeline kind label for contact lists")
            ),
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(_lk)) => write!(
                f,
                "{}",
                tr!(
                    "Last Notes",
                    "Timeline kind label for last notes per pubkey"
                )
            ),
            TimelineKind::Generic(_) => {
                write!(f, "{}", tr!("Timeline", "Generic timeline kind label"))
            }
            TimelineKind::Notifications(_) => write!(
                f,
                "{}",
                tr!("Notifications", "Timeline kind label for notifications")
            ),
            TimelineKind::Profile(_) => write!(
                f,
                "{}",
                tr!("Profile", "Timeline kind label for user profiles")
            ),
            TimelineKind::Universe => write!(
                f,
                "{}",
                tr!("Universe", "Timeline kind label for universe feed")
            ),
            TimelineKind::Hashtag(_) => write!(
                f,
                "{}",
                tr!("Hashtag", "Timeline kind label for hashtag feeds")
            ),
            TimelineKind::Search(_) => write!(
                f,
                "{}",
                tr!("Search", "Timeline kind label for search results")
            ),
        }
    }
}
*/

impl TimelineKind {
    pub fn pubkey(&self) -> Option<&Pubkey> {
        match self {
            TimelineKind::List(list_kind) => list_kind.pubkey(),
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind)) => list_kind.pubkey(),
            TimelineKind::Notifications(pk) => Some(pk),
            TimelineKind::Profile(pk) => Some(pk),
            TimelineKind::Universe => None,
            TimelineKind::Generic(_) => None,
            TimelineKind::Hashtag(_ht) => None,
            TimelineKind::Search(query) => query.author(),
        }
    }

    /// Some feeds are not realtime, like certain algo feeds
    pub fn should_subscribe_locally(&self) -> bool {
        match self {
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(_list_kind)) => false,

            TimelineKind::List(_list_kind) => true,
            TimelineKind::Notifications(_pk_src) => true,
            TimelineKind::Profile(_pk_src) => true,
            TimelineKind::Universe => true,
            TimelineKind::Generic(_) => true,
            TimelineKind::Hashtag(_ht) => true,
            TimelineKind::Search(_q) => true,
        }
    }

    // NOTE!!: if you just added a TimelineKind enum, make sure to update
    //         the parser below as well
    pub fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            TimelineKind::Search(query) => {
                writer.write_token("search");
                query.serialize_tokens(writer)
            }
            TimelineKind::List(list_kind) => list_kind.serialize_tokens(writer),
            TimelineKind::Algo(algo_timeline) => algo_timeline.serialize_tokens(writer),
            TimelineKind::Notifications(pk) => {
                writer.write_token(NOTIFS_TOKEN);
                PubkeySource::pubkey(*pk).serialize_tokens(writer);
            }
            TimelineKind::Profile(pk) => {
                writer.write_token("profile");
                PubkeySource::pubkey(*pk).serialize_tokens(writer);
            }
            TimelineKind::Universe => {
                writer.write_token("universe");
            }
            TimelineKind::Generic(_usize) => {
                // TODO: lookup filter and then serialize
                writer.write_token("generic");
            }
            TimelineKind::Hashtag(ht) => {
                writer.write_token("hashtag");
                writer.write_token(&ht.join(" "));
            }
        }
    }

    pub fn parse<'a>(
        parser: &mut TokenParser<'a>,
        deck_author: &Pubkey,
    ) -> Result<Self, ParseError<'a>> {
        let profile = parser.try_parse(|p| {
            p.parse_token("profile")?;
            let pk_src = PubkeySource::parse_from_tokens(p)?;
            Ok(TimelineKind::Profile(*pk_src.as_pubkey(deck_author)))
        });
        if profile.is_ok() {
            return profile;
        }

        let notifications = parser.try_parse(|p| {
            // still handle deprecated form (notifs)
            p.parse_any_token(&[NOTIFS_TOKEN, NOTIFS_TOKEN_DEPRECATED])?;
            let pk_src = PubkeySource::parse_from_tokens(p)?;
            Ok(TimelineKind::Notifications(*pk_src.as_pubkey(deck_author)))
        });
        if notifications.is_ok() {
            return notifications;
        }

        let list_tl =
            parser.try_parse(|p| Ok(TimelineKind::List(ListKind::parse(p, deck_author)?)));
        if list_tl.is_ok() {
            return list_tl;
        }

        let algo_tl =
            parser.try_parse(|p| Ok(TimelineKind::Algo(AlgoTimeline::parse(p, deck_author)?)));
        if algo_tl.is_ok() {
            return algo_tl;
        }

        TokenParser::alt(
            parser,
            &[
                |p| {
                    p.parse_token("universe")?;
                    Ok(TimelineKind::Universe)
                },
                |p| {
                    p.parse_token("generic")?;
                    // TODO: generic filter serialization
                    Ok(TimelineKind::Generic(0))
                },
                |p| {
                    p.parse_token("hashtag")?;
                    Ok(TimelineKind::Hashtag(
                        p.pull_token()?
                            .split_whitespace()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_lowercase().to_string())
                            .collect(),
                    ))
                },
                |p| {
                    p.parse_token("search")?;
                    let search_query = SearchQuery::parse_from_tokens(p)?;
                    Ok(TimelineKind::Search(search_query))
                },
            ],
        )
    }

    pub fn last_per_pubkey(list_kind: ListKind) -> Self {
        TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind))
    }

    pub fn contact_list(pk: Pubkey) -> Self {
        TimelineKind::List(ListKind::contact_list(pk))
    }

    pub fn people_list(author: Pubkey, identifier: String) -> Self {
        TimelineKind::List(ListKind::people_list(author, identifier))
    }

    pub fn search(s: String) -> Self {
        TimelineKind::Search(SearchQuery::new(s))
    }

    pub fn is_contacts(&self) -> bool {
        matches!(self, TimelineKind::List(ListKind::Contact(_)))
    }

    pub fn profile(pk: Pubkey) -> Self {
        TimelineKind::Profile(pk)
    }

    pub fn is_notifications(&self) -> bool {
        matches!(self, TimelineKind::Notifications(_))
    }

    pub fn notifications(pk: Pubkey) -> Self {
        TimelineKind::Notifications(pk)
    }

    // TODO: probably should set default limit here
    /// Build the filter state for this timeline kind.
    pub fn filters(&self, txn: &Transaction, ndb: &Ndb) -> FilterState {
        match self {
            TimelineKind::Search(s) => FilterState::ready(search_filter(s)),

            TimelineKind::Universe => FilterState::ready(universe_filter()),

            TimelineKind::List(list_k) => match list_k {
                ListKind::Contact(pubkey) => contact_filter_state(txn, ndb, pubkey),
                ListKind::PeopleList(plr) => people_list_filter_state(txn, ndb, plr),
            },

            // TODO: still need to update this to fetch likes, zaps, etc
            TimelineKind::Notifications(pubkey) => {
                FilterState::ready(vec![notifications_filter(pubkey)])
            }

            TimelineKind::Hashtag(hashtag) => {
                let mut filters = Vec::new();
                for tag in hashtag.iter().filter(|tag| !tag.is_empty()) {
                    let tag_lower = tag.to_lowercase();
                    filters.push(
                        Filter::new()
                            .kinds([1])
                            .limit(filter::default_limit())
                            .tags([tag_lower.as_str()], 't')
                            .build(),
                    );
                }

                if filters.is_empty() {
                    warn!(?hashtag, "hashtag timeline has no usable tags");
                } else if filters.len() != hashtag.len() {
                    debug!(
                        ?hashtag,
                        usable_tags = filters.len(),
                        "hashtag timeline dropped empty tags"
                    );
                }

                FilterState::ready(filters)
            }

            TimelineKind::Algo(algo_timeline) => match algo_timeline {
                AlgoTimeline::LastPerPubkey(list_k) => match list_k {
                    ListKind::Contact(pubkey) => last_per_pubkey_filter_state(txn, ndb, pubkey),
                    ListKind::PeopleList(plr) => {
                        people_list_last_per_pubkey_filter_state(txn, ndb, plr)
                    }
                },
            },

            TimelineKind::Generic(_) => {
                todo!("implement generic filter lookups")
            }

            TimelineKind::Profile(pk) => FilterState::ready_hybrid(profile_filter(pk.bytes())),
        }
    }

    pub fn into_timeline(self, txn: &Transaction, ndb: &Ndb) -> Option<Timeline> {
        match self {
            TimelineKind::Search(s) => {
                let filter = FilterState::ready(search_filter(&s));
                Some(Timeline::new(
                    TimelineKind::Search(s),
                    filter,
                    TimelineTab::full_tabs(),
                ))
            }

            TimelineKind::Universe => Some(Timeline::new(
                TimelineKind::Universe,
                FilterState::ready(universe_filter()),
                TimelineTab::full_tabs(),
            )),

            TimelineKind::Generic(_filter_id) => {
                warn!("you can't convert a TimelineKind::Generic to a Timeline");
                // TODO: you actually can! just need to look up the filter id
                None
            }

            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::Contact(pk))) => {
                let contact_filter = contacts_filter(pk.bytes());

                let results = ndb
                    .query(txn, std::slice::from_ref(&contact_filter), 1)
                    .expect("contact query failed?");

                let kind_fn = TimelineKind::last_per_pubkey;
                let tabs = TimelineTab::only_notes_and_replies();

                if results.is_empty() {
                    return Some(Timeline::new(
                        kind_fn(ListKind::contact_list(pk)),
                        FilterState::needs_remote(),
                        tabs,
                    ));
                }

                let list_kind = ListKind::contact_list(pk);

                match Timeline::last_per_pubkey(&results[0].note, &list_kind) {
                    Err(Error::App(notedeck::Error::Filter(FilterError::EmptyContactList))) => {
                        Some(Timeline::new(
                            kind_fn(list_kind),
                            FilterState::needs_remote(),
                            tabs,
                        ))
                    }
                    Err(e) => {
                        error!("Unexpected error: {e}");
                        None
                    }
                    Ok(tl) => Some(tl),
                }
            }

            TimelineKind::Profile(pk) => {
                let filter = profile_filter(pk.bytes());
                Some(Timeline::new(
                    TimelineKind::profile(pk),
                    FilterState::ready_hybrid(filter),
                    TimelineTab::full_tabs(),
                ))
            }

            TimelineKind::Notifications(pk) => {
                let notifications_filter = notifications_filter(&pk);

                Some(Timeline::new(
                    TimelineKind::notifications(pk),
                    FilterState::ready(vec![notifications_filter]),
                    TimelineTab::notifications(),
                ))
            }

            TimelineKind::Hashtag(hashtag) => Some(Timeline::hashtag(hashtag)),

            TimelineKind::List(ListKind::Contact(pk)) => Some(Timeline::new(
                TimelineKind::contact_list(pk),
                contact_filter_state(txn, ndb, &pk),
                TimelineTab::full_tabs(),
            )),

            TimelineKind::List(ListKind::PeopleList(plr)) => Some(Timeline::new(
                TimelineKind::List(ListKind::PeopleList(plr.clone())),
                people_list_filter_state(txn, ndb, &plr),
                TimelineTab::full_tabs(),
            )),

            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::PeopleList(plr))) => {
                let list_filter = people_list_note_filter(&plr);
                let results = ndb
                    .query(txn, std::slice::from_ref(&list_filter), 1)
                    .expect("people list query failed?");

                let list_kind = ListKind::PeopleList(plr);
                let kind_fn = TimelineKind::last_per_pubkey;
                let tabs = TimelineTab::only_notes_and_replies();

                if results.is_empty() {
                    return Some(Timeline::new(
                        kind_fn(list_kind),
                        FilterState::needs_remote(),
                        tabs,
                    ));
                }

                match Timeline::last_per_pubkey(&results[0].note, &list_kind) {
                    Err(Error::App(notedeck::Error::Filter(
                        FilterError::EmptyContactList | FilterError::EmptyList,
                    ))) => Some(Timeline::new(
                        kind_fn(list_kind),
                        FilterState::needs_remote(),
                        tabs,
                    )),
                    Err(e) => {
                        error!("Unexpected error: {e}");
                        None
                    }
                    Ok(tl) => Some(tl),
                }
            }
        }
    }

    pub fn to_title(&self, i18n: &mut Localization) -> ColumnTitle<'_> {
        match self {
            TimelineKind::Search(query) => {
                ColumnTitle::formatted(format!("Search \"{}\"", query.search))
            }
            TimelineKind::List(list_kind) => match list_kind {
                ListKind::Contact(_pubkey_source) => {
                    ColumnTitle::formatted(tr!(i18n, "Contacts", "Column title for contact lists"))
                }
                ListKind::PeopleList(plr) => ColumnTitle::formatted(plr.identifier.clone()),
            },
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind)) => match list_kind {
                ListKind::Contact(_pubkey_source) => ColumnTitle::formatted(tr!(
                    i18n,
                    "Contacts (last notes)",
                    "Column title for last notes per contact"
                )),
                ListKind::PeopleList(plr) => {
                    ColumnTitle::formatted(format!("{} (last notes)", plr.identifier))
                }
            },
            TimelineKind::Notifications(_pubkey_source) => {
                ColumnTitle::formatted(tr!(i18n, "Notifications", "Column title for notifications"))
            }
            TimelineKind::Profile(_pubkey_source) => ColumnTitle::needs_db(self),
            TimelineKind::Universe => {
                ColumnTitle::formatted(tr!(i18n, "Universe", "Column title for universe feed"))
            }
            TimelineKind::Generic(_) => {
                ColumnTitle::formatted(tr!(i18n, "Custom", "Column title for custom timelines"))
            }
            TimelineKind::Hashtag(hashtag) => ColumnTitle::formatted(hashtag.join(" ").to_string()),
        }
    }
}

pub fn notifications_filter(pk: &Pubkey) -> Filter {
    Filter::new()
        .pubkeys([pk.bytes()])
        .kinds(notification_kinds())
        .limit(default_limit())
        .build()
}

pub fn notification_kinds() -> [u64; 3] {
    [1, 7, 6]
}

#[derive(Debug)]
pub struct TitleNeedsDb<'a> {
    kind: &'a TimelineKind,
}

impl<'a> TitleNeedsDb<'a> {
    pub fn new(kind: &'a TimelineKind) -> Self {
        TitleNeedsDb { kind }
    }

    pub fn title<'txn>(&self, txn: &'txn Transaction, ndb: &Ndb) -> &'txn str {
        if let TimelineKind::Profile(pubkey) = self.kind {
            let profile = ndb.get_profile_by_pubkey(txn, pubkey);
            let m_name = profile
                .as_ref()
                .ok()
                .map(|p| notedeck::name::get_display_name(Some(p)).name());

            m_name.unwrap_or("Profile")
        } else {
            "Unknown"
        }
    }
}

/// This saves us from having to construct a transaction if we don't need to
/// for a particular column when rendering the title
#[derive(Debug)]
pub enum ColumnTitle<'a> {
    Simple(Cow<'static, str>),
    NeedsDb(TitleNeedsDb<'a>),
}

impl<'a> ColumnTitle<'a> {
    pub fn simple(title: &'static str) -> Self {
        Self::Simple(Cow::Borrowed(title))
    }

    pub fn formatted(title: String) -> Self {
        Self::Simple(Cow::Owned(title))
    }

    pub fn needs_db(kind: &'a TimelineKind) -> ColumnTitle<'a> {
        Self::NeedsDb(TitleNeedsDb::new(kind))
    }
}

/// Build the filter state for a contact list timeline.
fn contact_filter_state(txn: &Transaction, ndb: &Ndb, pk: &Pubkey) -> FilterState {
    let contact_filter = contacts_filter(pk);

    let results = match ndb.query(txn, std::slice::from_ref(&contact_filter), 1) {
        Ok(results) => results,
        Err(err) => {
            error!("contact query failed: {err}");
            return FilterState::Broken(FilterError::EmptyContactList);
        }
    };

    if results.is_empty() {
        FilterState::needs_remote()
    } else {
        let with_hashtags = false;
        match hybrid_contacts_filter(&results[0].note, Some(pk.bytes()), with_hashtags) {
            Err(notedeck::Error::Filter(FilterError::EmptyContactList)) => {
                FilterState::needs_remote()
            }
            Err(err) => {
                error!("Error getting contact filter state: {err}");
                FilterState::Broken(FilterError::EmptyContactList)
            }
            Ok(filter) => FilterState::ready_hybrid(filter),
        }
    }
}

/// Build the filter state for a last-per-pubkey timeline.
fn last_per_pubkey_filter_state(txn: &Transaction, ndb: &Ndb, pk: &Pubkey) -> FilterState {
    let contact_filter = contacts_filter(pk.bytes());

    let results = match ndb.query(txn, std::slice::from_ref(&contact_filter), 1) {
        Ok(results) => results,
        Err(err) => {
            error!("contact query failed: {err}");
            return FilterState::Broken(FilterError::EmptyContactList);
        }
    };

    if results.is_empty() {
        FilterState::needs_remote()
    } else {
        let kind = 1;
        let notes_per_pk = 1;
        match filter::last_n_per_pubkey_from_tags(&results[0].note, kind, notes_per_pk) {
            Err(notedeck::Error::Filter(FilterError::EmptyContactList)) => {
                FilterState::needs_remote()
            }
            Err(err) => {
                error!("Error getting contact filter state: {err}");
                FilterState::Broken(FilterError::EmptyContactList)
            }
            Ok(filter) => FilterState::ready(filter),
        }
    }
}

fn profile_filter(pk: &[u8; 32]) -> HybridFilter {
    let local = vec![
        NdbQueryPackage {
            filters: vec![Filter::new()
                .authors([pk])
                .kinds([1])
                .limit(default_limit())
                .build()],
            kind: ValidKind::One,
        },
        NdbQueryPackage {
            filters: vec![Filter::new()
                .authors([pk])
                .kinds([6])
                .limit(default_limit())
                .build()],
            kind: ValidKind::Six,
        },
    ];

    let remote = vec![Filter::new()
        .authors([pk])
        .kinds([1, 6, 0, 3])
        .limit(default_remote_limit())
        .build()];

    HybridFilter::split(local, remote)
}

fn search_filter(s: &SearchQuery) -> Vec<Filter> {
    vec![s.filter().limit(default_limit()).build()]
}

fn universe_filter() -> Vec<Filter> {
    vec![Filter::new().kinds([1]).limit(default_limit()).build()]
}

/// Filter to fetch a kind 30000 people list event by author + d tag
pub fn people_list_note_filter(plr: &PeopleListRef) -> Filter {
    Filter::new()
        .authors([plr.author.bytes()])
        .kinds([30000])
        .tags([plr.identifier.as_str()], 'd')
        .limit(1)
        .build()
}

/// Build the filter state for a people list timeline.
fn people_list_filter_state(txn: &Transaction, ndb: &Ndb, plr: &PeopleListRef) -> FilterState {
    let list_filter = people_list_note_filter(plr);

    let results = match ndb.query(txn, std::slice::from_ref(&list_filter), 1) {
        Ok(results) => results,
        Err(err) => {
            error!("people list query failed: {err}");
            return FilterState::Broken(FilterError::EmptyList);
        }
    };

    if results.is_empty() {
        FilterState::needs_remote()
    } else {
        let with_hashtags = false;
        match hybrid_contacts_filter(&results[0].note, None, with_hashtags) {
            Err(notedeck::Error::Filter(FilterError::EmptyContactList)) => {
                FilterState::needs_remote()
            }
            Err(err) => {
                error!("Error getting people list filter state: {err}");
                FilterState::Broken(FilterError::EmptyList)
            }
            Ok(filter) => FilterState::ready_hybrid(filter),
        }
    }
}

/// Build the filter state for a last-per-pubkey timeline backed by a people list.
fn people_list_last_per_pubkey_filter_state(
    txn: &Transaction,
    ndb: &Ndb,
    plr: &PeopleListRef,
) -> FilterState {
    let list_filter = people_list_note_filter(plr);

    let results = match ndb.query(txn, std::slice::from_ref(&list_filter), 1) {
        Ok(results) => results,
        Err(err) => {
            error!("people list query failed: {err}");
            return FilterState::Broken(FilterError::EmptyList);
        }
    };

    if results.is_empty() {
        FilterState::needs_remote()
    } else {
        let kind = 1;
        let notes_per_pk = 1;
        match filter::last_n_per_pubkey_from_tags(&results[0].note, kind, notes_per_pk) {
            Err(notedeck::Error::Filter(FilterError::EmptyContactList)) => {
                FilterState::needs_remote()
            }
            Err(err) => {
                error!("Error getting people list filter state: {err}");
                FilterState::Broken(FilterError::EmptyList)
            }
            Ok(filter) => FilterState::ready(filter),
        }
    }
}
