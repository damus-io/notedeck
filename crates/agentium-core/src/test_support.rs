//! Shared helpers for the crate's hermetic end-to-end tests.
//!
//! These build throwaway nostrdbs, signed events, and an in-process
//! [`nostrdb_net`] relay ([`spawn_relay`]) so the engine's real remote flows
//! (publish, live subscribe, NIP-77 backfill) can be exercised deterministically
//! with no network or fixtures. Test-only.

#![cfg(test)]

use std::time::Duration;

use futures_util::StreamExt;
use nostrdb::{Config, Filter, Ndb, SubscriptionStream, Transaction};
use nostrdb_net::relay::server::{self, RelayHandle};
use tempfile::TempDir;

/// A fixed, deterministic secret key for signing test events. Doubles as the
/// engine device key in tests, so signed events author-match the engine.
pub(crate) const TEST_SECKEY: [u8; 32] = [7u8; 32];

/// Open a fresh nostrdb under a throwaway directory. The returned [`TempDir`]
/// must be kept alive for as long as the db is used.
pub(crate) fn temp_ndb() -> (TempDir, Ndb) {
    let dir = TempDir::new().expect("tmp dir");
    let ndb = Ndb::new(dir.path().to_str().expect("path"), &Config::new()).expect("ndb");
    (dir, ndb)
}

/// Spawn an in-process hermetic relay backed by `ndb` on an ephemeral port. Pass
/// a clone if you also need to inspect the backing db (`Ndb` is a shared handle).
pub(crate) fn spawn_relay(ndb: Ndb) -> RelayHandle {
    server::spawn(ndb, "127.0.0.1:0".parse().expect("addr")).expect("spawn relay")
}

/// Wait until note `id` (of `kind`) is queryable in `ndb`, driven by a
/// [`SubscriptionStream`] with a backstop `timeout`. Returns whether it arrived.
///
/// Subscribes first, then checks presence and awaits ingests — so it catches a
/// note whether it landed just before or after the call.
pub(crate) async fn await_note(ndb: &Ndb, id: [u8; 32], kind: u64, timeout: Duration) -> bool {
    let filter = Filter::new().kinds([kind]).build();
    let sub = ndb
        .subscribe(std::slice::from_ref(&filter))
        .expect("subscribe");
    let mut stream = SubscriptionStream::new(ndb.clone(), sub);

    let found = tokio::time::timeout(timeout, async {
        loop {
            {
                let txn = Transaction::new(ndb).expect("txn");
                if ndb.get_note_by_id(&txn, &id).is_ok() {
                    return true;
                }
            }
            if stream.next().await.is_none() {
                return false;
            }
        }
    })
    .await;

    found.unwrap_or(false)
}
