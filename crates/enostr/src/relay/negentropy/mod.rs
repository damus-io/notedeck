mod protocol;
mod relay;
mod session;
mod state;

#[cfg(test)]
mod tests;

use hashbrown::HashSet;
use negentropy::NegentropyStorageVector;
use nostrdb::Filter;
use tokio::sync::oneshot;

use crate::NoteId;

pub(crate) use relay::NegentropyRelay;
pub(crate) use state::NegentropyData;

/// Builds the local event set for negentropy comparison on a background worker.
pub trait NegSetProvider: Send + Sync {
    /// Schedule a local-set build for `filter` and return a non-blocking receiver.
    fn provide(&self, filter: &Filter) -> oneshot::Receiver<NegentropyStorageVector>;
}

/// Cheap synchronous filter over candidate ids against local storage.
pub trait EventChecker: Send + Sync {
    /// Retain only ids that are still missing from local storage.
    fn retain_missing(&self, ids: &mut HashSet<NoteId>);
}
