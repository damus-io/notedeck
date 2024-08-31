use crate::{timeline::Timeline, Error};
use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Transaction};
use std::fmt::Display;

#[derive(Clone, Debug)]
pub enum PubkeySource {
    Explicit(Pubkey),
    DeckAuthor,
}

#[derive(Debug)]
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
#[derive(Debug)]
pub enum ColumnKind {
    List(ListKind),
    Universe,

    /// Generic filter
    Generic,
}

impl Display for ColumnKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnKind::List(ListKind::Contact(_src)) => f.write_str("Contacts"),
            ColumnKind::Generic => f.write_str("Timeline"),
            ColumnKind::Universe => f.write_str("Universe"),
        }
    }
}

impl ColumnKind {
    pub fn contact_list(pk: PubkeySource) -> Self {
        ColumnKind::List(ListKind::Contact(pk))
    }

    pub fn into_timeline(self, ndb: &Ndb, default_user: Option<&[u8; 32]>) -> Timeline {
        match self {
            ColumnKind::Universe => Timeline::new(ColumnKind::Universe, Some(vec![])),

            ColumnKind::Generic => {
                panic!("you can't convert a ColumnKind::Generic to a Timeline")
            }

            ColumnKind::List(ListKind::Contact(ref pk_src)) => {
                let pk = match pk_src {
                    PubkeySource::DeckAuthor => {
                        if let Some(user_pk) = default_user {
                            user_pk
                        } else {
                            // No user loaded, so we have to return an unloaded
                            // contact list columns
                            return Timeline::new(
                                ColumnKind::contact_list(PubkeySource::DeckAuthor),
                                None,
                            );
                        }
                    }
                    PubkeySource::Explicit(pk) => pk.bytes(),
                };

                let contact_filter = Filter::new().authors([pk]).kinds([3]).limit(1).build();
                let txn = Transaction::new(ndb).expect("txn");
                let results = ndb
                    .query(&txn, vec![contact_filter], 1)
                    .expect("contact query failed?");

                if results.is_empty() {
                    return Timeline::new(ColumnKind::contact_list(pk_src.to_owned()), None);
                }

                match Timeline::contact_list(&results[0].note) {
                    Err(Error::EmptyContactList) => {
                        Timeline::new(ColumnKind::contact_list(pk_src.to_owned()), None)
                    }
                    Err(e) => panic!("Unexpected error: {e}"),
                    Ok(tl) => tl,
                }
            }
        }
    }
}
