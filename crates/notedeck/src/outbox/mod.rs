mod hints;
mod manager;

pub use hints::{OutboxRelayIndex, RelayHint};
pub use manager::{OutboxManager, RelayPlan, RelaySelection};

use crate::UnknownIds;
use enostr::{Pubkey, RelayPool};
use nostrdb::{Ndb, Transaction};
use std::collections::BTreeSet;

/// Subscription identifier used for the shared unknown-id fallback pipe.
pub const UNKNOWN_IDS_SUB: &str = "unknownids";

/// Drive the shared unknown-id pipeline, combining relay hints, preparing the
/// fallback connections, and dispatching the pooled request. Returns the relay
/// plan so callers can instrument diagnostics (or `None` if no work was done).
pub fn dispatch_unknown_ids(
    unknown_ids: &mut UnknownIds,
    outbox: &mut OutboxManager,
    pool: &mut RelayPool,
    ndb: &Ndb,
    wakeup: impl Fn() + Send + Sync + Clone + 'static,
) -> Option<RelayPlan> {
    if !unknown_ids.ready_to_send() {
        return None;
    }

    let mut hinted_relays: BTreeSet<String> = BTreeSet::new();
    let mut authors: Vec<Pubkey> = Vec::new();

    {
        let ids_map = unknown_ids.ids_mut();
        for (unknown, relays) in ids_map.iter() {
            if let Some(pk) = unknown.is_pubkey() {
                authors.push(*pk);
            }
            hinted_relays.extend(relays.iter().map(|url| url.to_string()));
        }
    }

    let Some(filter) = unknown_ids.filter() else {
        return None;
    };

    let txn = Transaction::new(ndb).ok()?;

    let plan = outbox.dispatch_req(
        pool,
        &txn,
        ndb,
        UNKNOWN_IDS_SUB,
        filter,
        hinted_relays.into_iter(),
        authors.iter().map(|pk| pk.as_ref()),
        wakeup,
    )?;

    unknown_ids.clear();

    Some(plan)
}
