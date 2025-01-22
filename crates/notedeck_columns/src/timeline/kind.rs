use crate::error::Error;
use crate::timeline::{Timeline, TimelineTab};
use enostr::{Filter, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{filter::default_limit, FilterError, FilterState, RootNoteIdBuf};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, fmt::Display};
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};
use tracing::{error, warn};

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PubkeySource {
    Explicit(Pubkey),
    #[default]
    DeckAuthor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    Contact(PubkeySource),
}

impl PubkeySource {
    pub fn pubkey(pubkey: Pubkey) -> Self {
        PubkeySource::Explicit(pubkey)
    }

    pub fn to_pubkey<'a>(&'a self, deck_author: &'a Pubkey) -> &'a Pubkey {
        match self {
            PubkeySource::Explicit(pk) => pk,
            PubkeySource::DeckAuthor => deck_author,
        }
    }

    pub fn to_pubkey_bytes<'a>(&'a self, deck_author: &'a [u8; 32]) -> &'a [u8; 32] {
        match self {
            PubkeySource::Explicit(pk) => pk.bytes(),
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
    pub fn contact_list(pk_src: PubkeySource) -> Self {
        ListKind::Contact(pk_src)
    }

    pub fn pubkey_source(&self) -> Option<&PubkeySource> {
        match self {
            ListKind::Contact(pk_src) => Some(pk_src),
        }
    }
}

impl TokenSerializable for ListKind {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            ListKind::Contact(pk_src) => {
                writer.write_token("contact");
                pk_src.serialize_tokens(writer);
            }
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.parse_all(|p| {
            p.parse_token("contact")?;
            let pk_src = PubkeySource::parse_from_tokens(p)?;
            Ok(ListKind::Contact(pk_src))
        })

        /* here for u when you need more things to parse
        TokenParser::alt(
            parser,
            &[|p| {
                p.parse_all(|p| {
                    p.parse_token("contact")?;
                    let pk_src = PubkeySource::parse_from_tokens(p)?;
                    Ok(ListKind::Contact(pk_src))
                });
            },|p| {
                // more cases...
            }],
        )
        */
    }
}

///
/// What kind of timeline is it?
///   - Follow List
///   - Notifications
///   - DM
///   - filter
///   - ... etc
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineKind {
    List(ListKind),

    /// The last not per pubkey
    Algo(AlgoTimeline),

    Notifications(PubkeySource),

    Profile(PubkeySource),

    /// This could be any note id, doesn't need to be the root id
    Thread(RootNoteIdBuf),

    Universe,

    /// Generic filter
    Generic,

    Hashtag(String),
}

const NOTIFS_TOKEN_DEPRECATED: &str = "notifs";
const NOTIFS_TOKEN: &str = "notifications";

fn parse_hex_id<'a>(parser: &mut TokenParser<'a>) -> Result<[u8; 32], ParseError<'a>> {
    let hex = parser.pull_token()?;
    hex::decode(hex)
        .map_err(|_| ParseError::HexDecodeFailed)?
        .as_slice()
        .try_into()
        .map_err(|_| ParseError::HexDecodeFailed)
}

impl TokenSerializable for TimelineKind {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            TimelineKind::List(list_kind) => list_kind.serialize_tokens(writer),
            TimelineKind::Algo(algo_timeline) => algo_timeline.serialize_tokens(writer),
            TimelineKind::Notifications(pk_src) => {
                writer.write_token(NOTIFS_TOKEN);
                pk_src.serialize_tokens(writer);
            }
            TimelineKind::Profile(pk_src) => {
                writer.write_token("profile");
                pk_src.serialize_tokens(writer);
            }
            TimelineKind::Thread(root_note_id) => {
                writer.write_token("thread");
                writer.write_token(&root_note_id.hex());
            }
            TimelineKind::Universe => {
                writer.write_token("universe");
            }
            TimelineKind::Generic => {
                writer.write_token("generic");
            }
            TimelineKind::Hashtag(ht) => {
                writer.write_token("hashtag");
                writer.write_token(ht);
            }
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        TokenParser::alt(
            parser,
            &[
                |p| Ok(TimelineKind::List(ListKind::parse_from_tokens(p)?)),
                |p| Ok(TimelineKind::Algo(AlgoTimeline::parse_from_tokens(p)?)),
                |p| {
                    // still handle deprecated form (notifs)
                    p.parse_any_token(&[NOTIFS_TOKEN, NOTIFS_TOKEN_DEPRECATED])?;
                    Ok(TimelineKind::Notifications(
                        PubkeySource::parse_from_tokens(p)?,
                    ))
                },
                |p| {
                    p.parse_token("profile")?;
                    Ok(TimelineKind::Profile(PubkeySource::parse_from_tokens(p)?))
                },
                |p| {
                    p.parse_token("thread")?;
                    let note_id = RootNoteIdBuf::new_unsafe(parse_hex_id(p)?);
                    Ok(TimelineKind::Thread(note_id))
                },
                |p| {
                    p.parse_token("universe")?;
                    Ok(TimelineKind::Universe)
                },
                |p| {
                    p.parse_token("generic")?;
                    Ok(TimelineKind::Generic)
                },
                |p| {
                    p.parse_token("hashtag")?;
                    Ok(TimelineKind::Hashtag(p.pull_token()?.to_string()))
                },
            ],
        )
    }
}

/// Hardcoded algo timelines
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlgoTimeline {
    /// LastPerPubkey: a special nostr query that fetches the last N
    /// notes for each pubkey on the list
    LastPerPubkey(ListKind),
}

/// The identifier for our last per pubkey algo
const LAST_PER_PUBKEY_TOKEN: &str = "last_per_pubkey";

impl TokenSerializable for AlgoTimeline {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            AlgoTimeline::LastPerPubkey(list_kind) => {
                writer.write_token(LAST_PER_PUBKEY_TOKEN);
                list_kind.serialize_tokens(writer);
            }
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        TokenParser::alt(
            parser,
            &[|p| {
                p.parse_all(|p| {
                    p.parse_token(LAST_PER_PUBKEY_TOKEN)?;
                    Ok(AlgoTimeline::LastPerPubkey(ListKind::parse_from_tokens(p)?))
                })
            }],
        )
    }
}

impl Display for TimelineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimelineKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(_lk)) => f.write_str("Last Notes"),
            TimelineKind::Generic => f.write_str("Timeline"),
            TimelineKind::Notifications(_) => f.write_str("Notifications"),
            TimelineKind::Profile(_) => f.write_str("Profile"),
            TimelineKind::Universe => f.write_str("Universe"),
            TimelineKind::Hashtag(_) => f.write_str("Hashtag"),
            TimelineKind::Thread(_) => f.write_str("Thread"),
        }
    }
}

impl TimelineKind {
    pub fn pubkey_source(&self) -> Option<&PubkeySource> {
        match self {
            TimelineKind::List(list_kind) => list_kind.pubkey_source(),
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind)) => list_kind.pubkey_source(),
            TimelineKind::Notifications(pk_src) => Some(pk_src),
            TimelineKind::Profile(pk_src) => Some(pk_src),
            TimelineKind::Universe => None,
            TimelineKind::Generic => None,
            TimelineKind::Hashtag(_ht) => None,
            TimelineKind::Thread(_ht) => None,
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
            TimelineKind::Generic => true,
            TimelineKind::Hashtag(_ht) => true,
            TimelineKind::Thread(_ht) => true,
        }
    }

    pub fn last_per_pubkey(list_kind: ListKind) -> Self {
        TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind))
    }

    pub fn contact_list(pk: PubkeySource) -> Self {
        TimelineKind::List(ListKind::contact_list(pk))
    }

    pub fn is_contacts(&self) -> bool {
        matches!(self, TimelineKind::List(ListKind::Contact(_)))
    }

    pub fn profile(pk: PubkeySource) -> Self {
        TimelineKind::Profile(pk)
    }

    pub fn thread(root_id: RootNoteIdBuf) -> Self {
        TimelineKind::Thread(root_id)
    }

    pub fn is_notifications(&self) -> bool {
        matches!(self, TimelineKind::Notifications(_))
    }

    pub fn notifications(pk: PubkeySource) -> Self {
        TimelineKind::Notifications(pk)
    }

    pub fn into_timeline(self, ndb: &Ndb, default_user: Option<&[u8; 32]>) -> Option<Timeline> {
        match self {
            TimelineKind::Universe => Some(Timeline::new(
                TimelineKind::Universe,
                FilterState::ready(vec![Filter::new()
                    .kinds([1])
                    .limit(default_limit())
                    .build()]),
                TimelineTab::no_replies(),
            )),

            TimelineKind::Thread(root_id) => Some(Timeline::thread(root_id)),

            TimelineKind::Generic => {
                warn!("you can't convert a TimelineKind::Generic to a Timeline");
                None
            }

            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(ListKind::Contact(pk_src))) => {
                let pk = match &pk_src {
                    PubkeySource::DeckAuthor => default_user?,
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let contact_filter = Filter::new().authors([pk]).kinds([3]).limit(1).build();

                let txn = Transaction::new(ndb).expect("txn");
                let results = ndb
                    .query(&txn, &[contact_filter.clone()], 1)
                    .expect("contact query failed?");

                let kind_fn = TimelineKind::last_per_pubkey;
                let tabs = TimelineTab::only_notes_and_replies();

                if results.is_empty() {
                    return Some(Timeline::new(
                        kind_fn(ListKind::contact_list(pk_src)),
                        FilterState::needs_remote(vec![contact_filter.clone()]),
                        tabs,
                    ));
                }

                let list_kind = ListKind::contact_list(pk_src);

                match Timeline::last_per_pubkey(&results[0].note, &list_kind) {
                    Err(Error::App(notedeck::Error::Filter(FilterError::EmptyContactList))) => {
                        Some(Timeline::new(
                            kind_fn(list_kind),
                            FilterState::needs_remote(vec![contact_filter]),
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

            TimelineKind::Profile(pk_src) => {
                let pk = match &pk_src {
                    PubkeySource::DeckAuthor => default_user?,
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let filter = Filter::new()
                    .authors([pk])
                    .kinds([1])
                    .limit(default_limit())
                    .build();

                Some(Timeline::new(
                    TimelineKind::profile(pk_src),
                    FilterState::ready(vec![filter]),
                    TimelineTab::full_tabs(),
                ))
            }

            TimelineKind::Notifications(pk_src) => {
                let pk = match &pk_src {
                    PubkeySource::DeckAuthor => default_user?,
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let notifications_filter = Filter::new()
                    .pubkeys([pk])
                    .kinds([1])
                    .limit(default_limit())
                    .build();

                Some(Timeline::new(
                    TimelineKind::notifications(pk_src),
                    FilterState::ready(vec![notifications_filter]),
                    TimelineTab::only_notes_and_replies(),
                ))
            }

            TimelineKind::Hashtag(hashtag) => Some(Timeline::hashtag(hashtag)),

            TimelineKind::List(ListKind::Contact(pk_src)) => {
                let pk = match &pk_src {
                    PubkeySource::DeckAuthor => default_user?,
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let contact_filter = Filter::new().authors([pk]).kinds([3]).limit(1).build();

                let txn = Transaction::new(ndb).expect("txn");
                let results = ndb
                    .query(&txn, &[contact_filter.clone()], 1)
                    .expect("contact query failed?");

                if results.is_empty() {
                    return Some(Timeline::new(
                        TimelineKind::contact_list(pk_src),
                        FilterState::needs_remote(vec![contact_filter.clone()]),
                        TimelineTab::full_tabs(),
                    ));
                }

                match Timeline::contact_list(&results[0].note, pk_src.clone(), default_user) {
                    Err(Error::App(notedeck::Error::Filter(FilterError::EmptyContactList))) => {
                        Some(Timeline::new(
                            TimelineKind::contact_list(pk_src),
                            FilterState::needs_remote(vec![contact_filter]),
                            TimelineTab::full_tabs(),
                        ))
                    }
                    Err(e) => {
                        error!("Unexpected error: {e}");
                        None
                    }
                    Ok(tl) => Some(tl),
                }
            }
        }
    }

    pub fn to_title(&self) -> ColumnTitle<'_> {
        match self {
            TimelineKind::List(list_kind) => match list_kind {
                ListKind::Contact(_pubkey_source) => ColumnTitle::simple("Contacts"),
            },
            TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind)) => match list_kind {
                ListKind::Contact(_pubkey_source) => ColumnTitle::simple("Contacts (last notes)"),
            },
            TimelineKind::Notifications(_pubkey_source) => ColumnTitle::simple("Notifications"),
            TimelineKind::Profile(_pubkey_source) => ColumnTitle::needs_db(self),
            TimelineKind::Thread(_root_id) => ColumnTitle::simple("Thread"),
            TimelineKind::Universe => ColumnTitle::simple("Universe"),
            TimelineKind::Generic => ColumnTitle::simple("Custom"),
            TimelineKind::Hashtag(hashtag) => ColumnTitle::formatted(hashtag.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct TitleNeedsDb<'a> {
    kind: &'a TimelineKind,
}

impl<'a> TitleNeedsDb<'a> {
    pub fn new(kind: &'a TimelineKind) -> Self {
        TitleNeedsDb { kind }
    }

    pub fn title<'txn>(
        &self,
        txn: &'txn Transaction,
        ndb: &Ndb,
        deck_author: Option<&Pubkey>,
    ) -> &'txn str {
        if let TimelineKind::Profile(pubkey_source) = self.kind {
            if let Some(deck_author) = deck_author {
                let pubkey = pubkey_source.to_pubkey(deck_author);
                let profile = ndb.get_profile_by_pubkey(txn, pubkey);
                let m_name = profile
                    .as_ref()
                    .ok()
                    .map(|p| crate::profile::get_display_name(Some(p)).name());

                m_name.unwrap_or("Profile")
            } else {
                // why would be there be no deck author? weird
                "nostrich"
            }
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
