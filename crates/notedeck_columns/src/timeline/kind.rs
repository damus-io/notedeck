use crate::error::Error;
use crate::timeline::{Timeline, TimelineTab};
use enostr::{Filter, Pubkey};
use nostrdb::{Ndb, Transaction};
use notedeck::{filter::default_limit, FilterError, FilterState};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, fmt::Display};
use tracing::{error, warn};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PubkeySource {
    Explicit(Pubkey),
    DeckAuthor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    Contact(PubkeySource),
}

impl PubkeySource {
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

impl ListKind {
    pub fn pubkey_source(&self) -> Option<&PubkeySource> {
        match self {
            ListKind::Contact(pk_src) => Some(pk_src),
        }
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

    Notifications(PubkeySource),

    Profile(PubkeySource),

    Universe,

    /// Generic filter
    Generic,

    Hashtag(String),
}

impl Display for TimelineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimelineKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            TimelineKind::Generic => f.write_str("Timeline"),
            TimelineKind::Notifications(_) => f.write_str("Notifications"),
            TimelineKind::Profile(_) => f.write_str("Profile"),
            TimelineKind::Universe => f.write_str("Universe"),
            TimelineKind::Hashtag(_) => f.write_str("Hashtag"),
        }
    }
}

impl TimelineKind {
    pub fn pubkey_source(&self) -> Option<&PubkeySource> {
        match self {
            TimelineKind::List(list_kind) => list_kind.pubkey_source(),
            TimelineKind::Notifications(pk_src) => Some(pk_src),
            TimelineKind::Profile(pk_src) => Some(pk_src),
            TimelineKind::Universe => None,
            TimelineKind::Generic => None,
            TimelineKind::Hashtag(_ht) => None,
        }
    }

    pub fn contact_list(pk: PubkeySource) -> Self {
        TimelineKind::List(ListKind::Contact(pk))
    }

    pub fn is_contacts(&self) -> bool {
        matches!(self, TimelineKind::List(ListKind::Contact(_)))
    }

    pub fn profile(pk: PubkeySource) -> Self {
        TimelineKind::Profile(pk)
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

            TimelineKind::Generic => {
                warn!("you can't convert a TimelineKind::Generic to a Timeline");
                None
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
            TimelineKind::Notifications(_pubkey_source) => ColumnTitle::simple("Notifications"),
            TimelineKind::Profile(_pubkey_source) => ColumnTitle::needs_db(self),
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
