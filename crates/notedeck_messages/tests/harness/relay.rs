//! Relay-level polling helpers for Messages end-to-end tests.

use std::time::{Duration, Instant};

use enostr::FullKeypair;
use nostr::{nips::nip17, Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::prelude::{MemoryDatabase, NostrEventsDatabase};

use super::fixtures::nostr_pubkey;
use super::DeviceHarness;

/// Waits until the relay database stores the expected number of giftwrap events.
///
/// Any `senders` are stepped each iteration so their outbox continues
/// flushing events to the relay while we poll.
pub async fn wait_for_relay_giftwrap_count(
    relay_db: &MemoryDatabase,
    expected_count: usize,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;
    let filter = NostrFilter::new().kind(NostrKind::GiftWrap);

    loop {
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = relay_db
            .count(vec![filter.clone()])
            .await
            .expect("query relay giftwrap count");
        if actual == expected_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected relay giftwrap count {}, actual {}",
            expected_count,
            actual
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the relay database stores at least the expected number of events matching `filter`.
///
/// Any `senders` are stepped each iteration so their outbox continues
/// flushing events to the relay while we poll.
pub async fn wait_for_relay_count_at_least(
    relay_db: &MemoryDatabase,
    filter: NostrFilter,
    expected_min_count: usize,
    timeout: Duration,
    context: &str,
    senders: &mut [&mut DeviceHarness],
) {
    let deadline = Instant::now() + timeout;

    loop {
        for sender in senders.iter_mut() {
            sender.step();
        }

        let actual = relay_db
            .count(vec![filter.clone()])
            .await
            .expect("query relay giftwrap count");
        if actual >= expected_min_count {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; expected relay giftwrap count at least {}, actual {}",
            expected_min_count,
            actual
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Returns the latest remote DM relay-list relay URLs stored for one account.
pub async fn relay_dm_relay_list_relays(
    relay_db: &MemoryDatabase,
    account: &FullKeypair,
) -> Vec<String> {
    let filter = NostrFilter::new()
        .authors([nostr_pubkey(&account.pubkey)])
        .kind(NostrKind::Custom(10050))
        .limit(1);
    let events = relay_db
        .query(vec![filter])
        .await
        .expect("query relay dm relay list");

    let Some(event) = events.first() else {
        return Vec::new();
    };

    nip17::extract_relay_list(event)
        .map(|relay| relay.to_string().trim_end_matches('/').to_owned())
        .collect()
}
