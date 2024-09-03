use crate::error::FilterError;
use crate::filter;
use crate::filter::FilterState;
use crate::{timeline::Timeline, Error};
use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Transaction};
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
/// What kind of column is it?
///   - Follow List
///   - Notifications
///   - DM
///   - filter
///   - ... etc
#[derive(Debug, Clone)]
pub enum ColumnKind {
    List(ListKind),

    Notifications(PubkeySource),

    Profile(PubkeySource),

    Universe,

    /// Generic filter
    Generic,
}

impl Display for ColumnKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            ColumnKind::Generic => f.write_str("Timeline"),
            ColumnKind::Notifications(_) => f.write_str("Notifications"),
            ColumnKind::Profile(_) => f.write_str("Profile"),
            ColumnKind::Universe => f.write_str("Universe"),
        }
    }
}

impl ColumnKind {
    pub fn contact_list(pk: PubkeySource) -> Self {
        ColumnKind::List(ListKind::Contact(pk))
    }

    pub fn profile(pk: PubkeySource) -> Self {
        ColumnKind::Profile(pk)
    }

    pub fn notifications(pk: PubkeySource) -> Self {
        ColumnKind::Notifications(pk)
    }

    pub fn into_timeline(self, ndb: &Ndb, default_user: Option<&[u8; 32]>) -> Option<Timeline> {
        match self {
            ColumnKind::Universe => Some(Timeline::new(
                ColumnKind::Universe,
                FilterState::ready(vec![]),
            )),

            ColumnKind::Generic => {
                warn!("you can't convert a ColumnKind::Generic to a Timeline");
                None
            }

            ColumnKind::Profile(pk_src) => {
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
                    ColumnKind::profile(pk_src),
                    FilterState::ready(vec![filter]),
                ))
            }

            ColumnKind::Notifications(pk_src) => {
                let pk = match &pk_src {
                    PubkeySource::DeckAuthor => default_user?,
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let notifications_filter = Filter::new()
                    .pubkeys([pk])
                    .kinds([1])
                    .limit(filter::default_limit())
                    .build();

                Some(Timeline::new(
                    ColumnKind::notifications(pk_src),
                    FilterState::ready(vec![notifications_filter]),
                ))
            }

            ColumnKind::List(ListKind::Contact(pk_src)) => {
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
                        ColumnKind::contact_list(pk_src),
                        FilterState::needs_remote(vec![contact_filter.clone()]),
                    ));
                }

                match Timeline::contact_list(&results[0].note) {
                    Err(Error::Filter(FilterError::EmptyContactList)) => Some(Timeline::new(
                        ColumnKind::contact_list(pk_src),
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
}
