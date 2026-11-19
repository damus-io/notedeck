//! Shared helpers for the crate's hermetic end-to-end tests.
//!
//! These build throwaway nostrdbs, signed events, and an in-process
//! [`nostrdb_net`] relay ([`spawn_relay`]) so the engine's real remote flows
//! (publish, live subscribe, NIP-77 backfill) can be exercised deterministically
//! with no network or fixtures. Test-only.

#![cfg(test)]

use std::time::Duration;

use futures_util::StreamExt;
use nostrdb::{Filter, Ndb, NoteBuildOptions, NoteBuilder, SubscriptionStream, Transaction};
use nostrdb_net::relay::server::{self, RelayHandle};

use crate::session_events::BuiltEvent;

pub(crate) use ndb_testing::{test_config, TempNdb};

/// A fixed, deterministic secret key for signing test events. Doubles as the
/// engine device key in tests, so signed events author-match the engine.
pub(crate) const TEST_SECKEY: [u8; 32] = [7u8; 32];

/// The pubkey `sk` signs as, for the author-scoped loaders.
pub(crate) fn pubkey_of(sk: &[u8; 32]) -> nostrdb_net::Pubkey {
    nostrdb_net::Pubkey::new(
        *NoteBuilder::new()
            .kind(1)
            .content("")
            .options(NoteBuildOptions::default())
            .sign(sk)
            .build()
            .unwrap()
            .pubkey(),
    )
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

/// A [`TempNdb`] that decrypts [`TEST_SECKEY`]'s PNS envelopes, so a
/// [`pns_seed`]ed or [`pns_ingest`]ed note becomes queryable in it.
pub(crate) fn pns_ndb() -> TempNdb {
    let db = TempNdb::new();
    db.ndb.add_key(&TEST_SECKEY);
    db
}

/// PNS-wrap `built` under [`TEST_SECKEY`] into its kind-1080 envelope JSON,
/// as it would go on the wire.
pub(crate) fn pns_wrap(built: &BuiltEvent) -> String {
    let pns = nostrdb_net::pns::derive_pns_keys(&TEST_SECKEY);
    crate::session_events::wrap_pns(&built.note_json, &pns).expect("wrap pns")
}

/// PNS-wrap `built` and ingest the envelope, as an inbound relay event arrives,
/// **without** waiting for ndb to decrypt it. Returns the envelope JSON.
///
/// For a test that subscribes first and proves the subscription wakes (a
/// watch), or one that awaits the note later; otherwise use [`pns_ingest`].
/// `ndb` must hold [`TEST_SECKEY`] (see [`pns_ndb`]) or the inner note never
/// appears.
pub(crate) fn pns_seed(ndb: &Ndb, built: &BuiltEvent) -> String {
    let wrapped = pns_wrap(built);
    ndb.process_event(&format!(r#"["EVENT","_pns",{wrapped}]"#))
        .expect("ingest pns envelope");
    wrapped
}

/// [`pns_seed`] `built`, then wait (5 s) for ndb to decrypt the inner note,
/// failing the test if it never does. Returns the envelope JSON.
pub(crate) async fn pns_ingest(ndb: &Ndb, built: &BuiltEvent) -> String {
    let wrapped = pns_seed(ndb, built);
    assert!(
        await_note(
            ndb,
            built.note_id,
            built.kind as u64,
            Duration::from_secs(5)
        )
        .await,
        "inner kind-{} note never decrypted",
        built.kind
    );
    wrapped
}
