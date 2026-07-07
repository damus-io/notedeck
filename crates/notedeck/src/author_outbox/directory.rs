use crate::{relayspec::relays_from_nip65_note, RelaySpec};
use enostr::{NormRelayUrl, Pubkey};
use hashbrown::{HashMap, HashSet};
use nostrdb::{Filter, Ndb, Note, Transaction};
use std::ops::ControlFlow;

/// Current local kind `10002` resolution state for one author.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RelayDirectoryState<'a> {
    /// The author has a resolved writable relay set.
    Known(&'a HashSet<NormRelayUrl>),
    /// The author published a relay list, but it declares no writable relays.
    ExplicitNone,
    /// No local relay-list note was found in the queried snapshot.
    Missing,
}

/// Read-only relay-list resolution used by author-outbox route planning.
pub(crate) trait RelayDirectoryRead {
    /// Return the current resolution state for one author.
    fn author_state(&self, author: &Pubkey) -> RelayDirectoryState<'_>;
}

/// Owned local kind `10002` projection for one frozen author-outbox plan job.
#[derive(Clone, Debug, Default)]
pub(crate) struct RelayDirectorySnapshot {
    resolved: HashMap<Pubkey, RelayListSnapshot>,
}

impl RelayDirectorySnapshot {
    /// Query local NDB relay-list state for the authors needed by one plan.
    pub(crate) fn from_ndb_authors(ndb: &Ndb, authors: &HashSet<Pubkey>) -> Self {
        let Ok(txn) = Transaction::new(ndb) else {
            tracing::warn!("author-outbox local relay-list snapshot skipped: failed to open txn");
            return Self::default();
        };

        Self {
            resolved: authors
                .iter()
                .filter_map(|author| {
                    query_author_relay_list_snapshot(ndb, &txn, author)
                        .map(|snapshot| (*author, snapshot))
                })
                .collect(),
        }
    }

    /// Return authors that had no local kind `10002` note in this snapshot.
    pub(crate) fn missing_authors(&self, authors: &HashSet<Pubkey>) -> HashSet<Pubkey> {
        authors
            .iter()
            .filter(|author| !self.resolved.contains_key(*author))
            .copied()
            .collect()
    }
}

impl RelayDirectoryRead for RelayDirectorySnapshot {
    fn author_state(&self, author: &Pubkey) -> RelayDirectoryState<'_> {
        if let Some(snapshot) = self.resolved.get(author) {
            return match &snapshot.state {
                ResolvedRelayState::Known(relays) => RelayDirectoryState::Known(relays),
                ResolvedRelayState::ExplicitNone => RelayDirectoryState::ExplicitNone,
            };
        }

        RelayDirectoryState::Missing
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RelayListSnapshot {
    state: ResolvedRelayState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedRelayState {
    Known(HashSet<NormRelayUrl>),
    ExplicitNone,
}

impl ResolvedRelayState {
    fn from_relays(relays: HashSet<NormRelayUrl>) -> Self {
        if relays.is_empty() {
            Self::ExplicitNone
        } else {
            Self::Known(relays)
        }
    }
}

fn authors_query_filter<'a, I>(authors: I) -> Filter
where
    I: IntoIterator<Item = &'a Pubkey>,
{
    let author_bytes = authors.into_iter().map(Pubkey::bytes).collect::<Vec<_>>();
    Filter::new().authors(author_bytes).kinds([10002]).build()
}

fn query_author_relay_list_snapshot(
    ndb: &Ndb,
    txn: &Transaction,
    author: &Pubkey,
) -> Option<RelayListSnapshot> {
    // nostrdb's author+kind query plan is fastest for a single author and walks
    // newest-first, so plan jobs query per author and stop at the first relay list.
    let filters = [authors_query_filter(std::iter::once(author))];

    match ndb.try_fold(txn, &filters, None, |current, note| {
        let observed_author = Pubkey::new(*note.pubkey());
        if &observed_author != author {
            return ControlFlow::Continue(current);
        }
        ControlFlow::Break(Some(relay_list_snapshot_from_note(&note)))
    }) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::warn!(
                ?err,
                ?author,
                "author-outbox local relay-list snapshot query failed"
            );
            None
        }
    }
}

fn relay_list_snapshot_from_note(note: &Note<'_>) -> RelayListSnapshot {
    RelayListSnapshot {
        state: ResolvedRelayState::from_relays(extract_write_relays(note)),
    }
}

fn extract_write_relays(note: &Note<'_>) -> HashSet<NormRelayUrl> {
    relays_from_nip65_note(note)
        .into_iter()
        .filter(RelaySpec::is_writable)
        .map(|relay| relay.url)
        .collect()
}
