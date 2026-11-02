//! An encrypted-only private relay gets the account's envelopes and never its
//! plaintext.
//!
//! The account's kind-10013 lists two private relays: its own (full) and a
//! relay it shares with other people, marked `["relay", url, "encrypted"]`.
//! A plaintext board edit must fan out to the full relay only, while the board
//! key's self-share (a kind-1059 gift-wrap) still reaches both. The shared relay
//! being sent anything but kind 1059/1080/1081 is the leak this guards.

mod common;

use std::time::{Duration, Instant};

use common::{CONVERGE_TIMEOUT, headway_app_factory};
use enostr::{FullKeypair, NormRelayUrl};
use nostrdb::IngestMetadata;
use notedeck::{
    PrivateRelay, PrivateRelayRole, PrivateRelaySet, construct_private_relay_list_note,
};
use notedeck_headway::store;
use notedeck_testing::device::{DeviceHarness, build_device_in_tmpdir_with_relays};
use notedeck_testing::init_tracing;
use notedeck_testing::negentropy_relay::{MemoryNegentropyRelay, run_memory_negentropy_relay};
use tempfile::TempDir;

/// The envelope kinds an encrypted-only relay may be sent, matched as substrings
/// of the captured EVENT frames.
const ENVELOPES: [&str; 3] = ["\"kind\":1059", "\"kind\":1080", "\"kind\":1081"];

/// The slug of the plaintext board the test writes, found in its `d` tag.
const PLAIN_BOARD: &str = "plaintext-board-under-test";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encrypted_only_private_relay_never_gets_plaintext() {
    init_tracing();

    let account = FullKeypair::generate();
    let full = run_memory_negentropy_relay().await.expect("full relay");
    let sealed = run_memory_negentropy_relay().await.expect("sealed relay");

    let mut device = build_device_in_tmpdir_with_relays(
        &[full.relay.url()],
        &account,
        TempDir::new().expect("tmpdir"),
        headway_app_factory(),
    );

    // Land the role-carrying 10013 as if another device had written it, so the
    // shared relay is encrypted-only from the first frame it is private at all.
    // (Adding it here and then toggling would first send it the 10013 itself,
    // while it was still a full relay.)
    let relays: PrivateRelaySet = [
        PrivateRelay {
            url: NormRelayUrl::new(full.relay.url()).expect("full url"),
            role: PrivateRelayRole::Full,
        },
        PrivateRelay {
            url: NormRelayUrl::new(sealed.relay.url()).expect("sealed url"),
            role: PrivateRelayRole::EncryptedOnly,
        },
    ]
    .into_iter()
    .collect();
    ingest_private_relay_list(&mut device, &account, &relays);
    step_until(
        &mut device,
        |device| {
            device
                .state_mut()
                .notedeck
                .app_context()
                .accounts
                .selected_account_private_relay_set()
                == &relays
        },
        "the ingested 10013 never became the account's private relay set",
    );

    // A plaintext board edit, written straight into nostrdb the way the CLI
    // does; the app's poll fans it out to the private relays it may go to.
    {
        let app_ctx = device.state_mut().notedeck.app_context();
        let secret = account.secret_key.secret_bytes();
        let seeded = store::seed_board(
            app_ctx.ndb,
            &account.pubkey,
            &secret,
            PLAIN_BOARD,
            "Plaintext board",
            &mut store::NoPublish,
        );
        assert_eq!(seeded, 1, "the plaintext board didn't ingest");
    }

    step_until(
        &mut device,
        |_| full.relay.count_captured_events_containing(PLAIN_BOARD) >= 1,
        "the plaintext board never reached the full private relay",
    );
    // Envelopes still go to the encrypted-only relay: the auto-seeded default
    // board's self-share is a kind-1059.
    step_until(
        &mut device,
        |_| sealed.relay.count_captured_events_containing(ENVELOPES[0]) >= 1,
        "the board self-share (kind-1059) never reached the encrypted-only relay",
    );

    // Give any stray plaintext fan-out a few more frames to show itself.
    let settle = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settle {
        device.run_ok();
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(
        sealed.relay.count_captured_events_containing(PLAIN_BOARD),
        0,
        "the plaintext board was sent to the encrypted-only relay"
    );
    assert_only_envelopes(&sealed);
}

/// Sign the account's kind-10013 for `relays` and ingest it locally, as an
/// arriving note would be. Nothing is published.
fn ingest_private_relay_list(
    device: &mut DeviceHarness,
    account: &FullKeypair,
    relays: &PrivateRelaySet,
) {
    let keypair = account.clone().to_keypair();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let note = construct_private_relay_list_note(relays, &keypair, created_at).expect("10013");
    let frame = format!("[\"EVENT\",{}]", note.json().expect("10013 json"));
    let app_ctx = device.state_mut().notedeck.app_context();
    app_ctx
        .ndb
        .process_event_with(&frame, IngestMetadata::new().client(true))
        .expect("ingest 10013");
}

/// Every EVENT the relay captured is an envelope.
fn assert_only_envelopes(relay: &MemoryNegentropyRelay) {
    let all = relay.relay.count_captured_events_containing("");
    let envelopes: usize = ENVELOPES
        .iter()
        .map(|kind| relay.relay.count_captured_events_containing(kind))
        .sum();
    assert_eq!(
        all,
        envelopes,
        "the encrypted-only relay was sent {} non-envelope event(s)",
        all - envelopes
    );
}

/// Step `device` until `done` holds, or panic with `message` after
/// [`CONVERGE_TIMEOUT`].
fn step_until(
    device: &mut DeviceHarness,
    mut done: impl FnMut(&mut DeviceHarness) -> bool,
    message: &str,
) {
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        device.run_ok();
        if done(device) {
            return;
        }
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
