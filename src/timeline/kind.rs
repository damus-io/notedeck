use crate::error::{Error, FilterError};
use crate::filter;
use crate::filter::FilterState;
use crate::timeline::Timeline;
use crate::ui::profile::preview::get_profile_displayname_string;
use enostr::{Filter, Pubkey};
use nostrdb::{Ndb, Transaction};
use std::fmt::Display;
use tracing::{error, warn};

#[derive(Clone, Debug)]
pub enum PubkeySource {
    Explicit(Pubkey),
    DeckAuthor,
}

#[derive(Debug, Clone)]
pub enum ListKind {
    Contact(PubkeySource),
}

///
/// What kind of timeline is it?
///   - Follow List
///   - Notifications
///   - DM
///   - filter
///   - ... etc
///
#[derive(Debug, Clone)]
pub enum TimelineKind {
    List(ListKind),

    Notifications(PubkeySource),

    Profile(PubkeySource),

    Universe,

    /// Generic filter
    Generic,
}

impl Display for TimelineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimelineKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            TimelineKind::Generic => f.write_str("Timeline"),
            TimelineKind::Notifications(_) => f.write_str("Notifications"),
            TimelineKind::Profile(_) => f.write_str("Profile"),
            TimelineKind::Universe => f.write_str("Universe"),
        }
    }
}

impl TimelineKind {
    pub fn contact_list(pk: PubkeySource) -> Self {
        TimelineKind::List(ListKind::Contact(pk))
    }

    pub fn profile(pk: PubkeySource) -> Self {
        TimelineKind::Profile(pk)
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
                    .limit(filter::default_limit())
                    .build()]),
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
                    .limit(filter::default_limit())
                    .build();

                Some(Timeline::new(
                    TimelineKind::profile(pk_src),
                    FilterState::ready(vec![filter]),
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
                    .limit(crate::filter::default_limit())
                    .build();

                Some(Timeline::new(
                    TimelineKind::notifications(pk_src),
                    FilterState::ready(vec![notifications_filter]),
                ))
            }

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
                    ));
                }

                match Timeline::contact_list(&results[0].note) {
                    Err(Error::Filter(FilterError::EmptyContactList)) => Some(Timeline::new(
                        TimelineKind::contact_list(pk_src),
                        FilterState::needs_remote(vec![contact_filter]),
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

    pub fn to_title(&self, ndb: &Ndb) -> String {
        match self {
            TimelineKind::List(list_kind) => match list_kind {
                ListKind::Contact(pubkey_source) => match pubkey_source {
                    PubkeySource::Explicit(pubkey) => format!(
                        "{}'s Home Timeline",
                        get_profile_displayname_string(ndb, pubkey)
                    ),
                    PubkeySource::DeckAuthor => "Your Home Timeline".to_owned(),
                },
            },
            TimelineKind::Notifications(pubkey_source) => match pubkey_source {
                PubkeySource::DeckAuthor => "Your Notifications".to_owned(),
                PubkeySource::Explicit(pk) => format!(
                    "{}'s Notifications",
                    get_profile_displayname_string(ndb, pk)
                ),
            },
            TimelineKind::Profile(pubkey_source) => match pubkey_source {
                PubkeySource::DeckAuthor => "Your Profile".to_owned(),
                PubkeySource::Explicit(pk) => {
                    format!("{}'s Profile", get_profile_displayname_string(ndb, pk))
                }
            },
            TimelineKind::Universe => "Universe".to_owned(),
            TimelineKind::Generic => "Custom Filter".to_owned(),
        }
    }
}
