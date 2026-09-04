//! Regression: a board created in the Headway app fans its kind-1059 self-share
//! out to the account's private relays, not just its kind-1081 content.
//!
//! A team-of-one board seals its definition into an SNS channel (kind-1081, fanned
//! by the notedeck host's private `Session`) and self-shares the channel root to
//! the account's own NIP-59 inbox (kind-1059) — the key another of the account's
//! devices needs to discover the board and register its root before nostrdb will
//! unwrap the channel's envelopes. The host leg carries the 1081s, but nothing
//! carried the 1059: it's authored by an ephemeral gift-wrap key, so it matches
//! neither the app's plaintext author poll nor the host's team-envelope filter. A
//! board made in the app therefore synced its *content* while staying invisible on
//! every other device — the sealed envelopes arrive but fold nothing without the
//! root. This drives the default board's auto-seed on one device and asserts the
//! self-share actually reaches the relay.

mod common;

use std::time::{Duration, Instant};

use common::{CONVERGE_TIMEOUT, build_headway_device};
use enostr::FullKeypair;
use notedeck_testing::init_tracing;

/// Kind-1059 is the NIP-59 gift wrap the self-share rides in; kind-1081 is the
/// sealed board-definition envelope. Matched as substrings of the captured EVENT
/// frames (see `count_captured_events_containing`).
const GIFTWRAP: &str = "\"kind\":1059";
const SNS_ENVELOPE: &str = "\"kind\":1081";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn created_board_self_share_reaches_the_private_relay() {
    init_tracing();

    let account = FullKeypair::generate();
    let relay = notedeck_testing::negentropy_relay::run_memory_negentropy_relay()
        .await
        .expect("start relay");
    let relay_url = relay.relay.url().to_owned();

    // A single device with the relay marked as its NIP-37 private-sync relay. On an
    // early frame it auto-seeds its default board as a team-of-one SNS channel,
    // sealing the definition (kind-1081) and self-sharing the root (kind-1059).
    let mut device = build_headway_device(&relay_url, &account);

    // Step until the self-share reaches the relay, or fail after the convergence
    // budget. The self-share is captured locally at creation and fanned out on a
    // following frame once the private relay set resolves.
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        device.run_ok();
        if relay.relay.count_captured_events_containing(GIFTWRAP) >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "auto-seeded board never fanned its kind-1059 self-share to the private relay \
             (captured locally at creation but no outbound leg published it)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Sanity: the sealed board definition rode its (separate, host-owned) leg to the
    // relay too, so the self-share assertion isn't passing in a world where the
    // device simply published everything or nothing.
    assert!(
        relay.relay.count_captured_events_containing(SNS_ENVELOPE) >= 1,
        "the board-definition envelope never reached the relay, so the sync path was dead"
    );
}
