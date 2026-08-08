//! Networked / two-party integration tests for SNS shared boards.
//!
//! The single-process tests in `src` cover the db-level mechanics (wrap/unwrap,
//! the `is_rumor` leak guard, apply-over-channel → `fold_shared_board`, the roster
//! derived from nostrdb). What they can't reach — and what this file adds — is the
//! *transport*: sealed kind-1081 envelopes actually leaving one member's Headway
//! device, crossing a real in-process relay, and folding into another member's
//! board. Two full `notedeck_headway::Headway` app instances are driven against
//! one `MemoryNegentropyRelay`, so every assertion rides the production sync path
//! (`PrivateRelaySync` inbound, `fan_out_unseen_notes` outbound, nostrdb's
//! 1081→rumor auto-unwrap).
//!
//! Scope note: this is the "arrival + convergence" slice of
//! `headway:headway/unfold-kiwi-bonus`. Rotation (multiple `team_root` epochs) and
//! the reject-share-not-addressed-to-me branch are deferred to follow-up commits.

mod common;

use std::time::{Duration, Instant};

use common::{
    CONVERGE_TIMEOUT, apply_sealed, build_headway_device, join_shared_board, seal_board_definition,
    shared_board, shared_card_titles, step_for, wait_for_convergence,
};
use enostr::FullKeypair;
use nostrdb::{Filter, Transaction};
use notedeck_headway::{
    event::{self, ColumnDef, HeadwayEvent},
    store::{BoardAction, SnsChannel},
};
use notedeck_testing::{device::DeviceHarness, init_tracing, stepping::wait_for_device_condition};

/// The shared 32-byte `team_root` every member of the test board holds. Fixed
/// (not random) so a failure reproduces; distinctive bytes so a stray all-zero
/// root can't accidentally match.
fn team_root() -> [u8; 32] {
    let mut root = [0u8; 32];
    root[0] = 0x11;
    root[31] = 0x42;
    root
}

/// The columns the shared board is seeded with.
fn shared_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef::new("backlog", "Backlog"),
        ColumnDef::new("todo", "Todo"),
        ColumnDef::new("done", "Done"),
    ]
}

const BOARD_ID: &str = "headway";
const BOARD_TITLE: &str = "Two-party shared board";

/// The subjects of every kind-1621 issue rumor in a device's local nostrdb,
/// regardless of which board coordinate they're anchored to. Used to prove a
/// co-member's sealed edit *arrived* (its rumor was unwrapped and stored), which
/// is a separate question from whether the shared fold *counts* it.
fn local_issue_subjects(device: &mut DeviceHarness) -> Vec<String> {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let Ok(results) = app_ctx.ndb.query(
        &txn,
        &[Filter::new().kinds([event::KIND_ISSUE as u64]).build()],
        4096,
    ) else {
        return Vec::new();
    };
    results
        .into_iter()
        .filter_map(|r| match event::parse(&r.note) {
            Some(HeadwayEvent::Issue(issue)) => Some(issue.subject),
            _ => None,
        })
        .collect()
}

/// Bring up a two-member shared board over a live relay: both members join the
/// same `team_root`, the owner seals the board *definition* into the channel, and
/// we wait until it has folded in on the owner's own device (so a later sealed
/// edit has a view to apply against). Returns the two devices, the shared board
/// coordinate, and the channel.
///
/// Both members mark the relay private, so their Headway instances subscribe to
/// the channel's kind-1081 envelope stream and fan their own sealed edits out to
/// it — the wiring the convergence assertions depend on.
struct SharedBoardFixture {
    owner: FullKeypair,
    member: FullKeypair,
    owner_device: DeviceHarness,
    member_device: DeviceHarness,
    board_addr: String,
    channel: SnsChannel,
    /// Kept alive so the in-process relay proxy (shuts down on drop) outlives the
    /// devices syncing through it.
    _relay: notedeck_testing::negentropy_relay::MemoryNegentropyRelay,
}

async fn setup_shared_board() -> SharedBoardFixture {
    let root = team_root();
    let channel = SnsChannel {
        keys: enostr::sns::derive_sns_keys(&root).expect("derive sns keys"),
    };

    let owner = FullKeypair::generate();
    let member = FullKeypair::generate();
    let board_addr = event::board_address(&owner.pubkey, BOARD_ID);

    let relay = notedeck_testing::negentropy_relay::run_memory_negentropy_relay()
        .await
        .expect("start relay");
    let relay_url = relay.relay.url().to_owned();

    let mut owner_device = build_headway_device(&relay_url, &owner);
    let mut member_device = build_headway_device(&relay_url, &member);

    // Both members join the board (the owner self-shares so its own Headway
    // registers the root and fans out its sealed edits; the member is invited).
    join_shared_board(&mut owner_device, &owner, &owner.pubkey, &root, &board_addr);
    join_shared_board(
        &mut member_device,
        &owner,
        &member.pubkey,
        &root,
        &board_addr,
    );

    // Seal the board definition into the channel: shared boards have no plaintext
    // leg the members subscribe to, so the definition must travel sealed for
    // either side's `fold_shared_board` to resolve the board at all.
    seal_board_definition(
        &mut owner_device,
        &owner.secret_key.secret_bytes(),
        &channel,
        BOARD_ID,
        BOARD_TITLE,
        &shared_columns(),
    );
    wait_for_device_condition(
        &mut owner_device,
        CONVERGE_TIMEOUT,
        "owner's own shared board definition folds in",
        |device| {
            if shared_board(device, &board_addr).is_some() {
                Ok(())
            } else {
                Err("board definition not folded yet".to_owned())
            }
        },
    );
    // The member must also have the definition before it can apply an edit off the
    // shared view — wait for the sealed def to cross the relay to the member.
    common::wait_for_shared_board_ready(
        &mut owner_device,
        &mut member_device,
        &board_addr,
        "the member to receive the sealed board definition over the relay",
    );

    SharedBoardFixture {
        owner,
        member,
        owner_device,
        member_device,
        board_addr,
        channel,
        _relay: relay,
    }
}

/// The owner's sealed edits reach the member over the relay and fold into the
/// member's shared board — and the plaintext rumors nostrdb unwraps from them
/// never cross the wire (the `is_rumor` fan-out guard, proven end to end).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owner_sealed_edits_converge_to_member_over_relay() {
    init_tracing();
    let mut fx = setup_shared_board().await;

    // The owner adds a card, sealed into the channel. Its distinctive title is a
    // sentinel: it lives only inside the encrypted 1081 envelope, so if it ever
    // appears in a plaintext EVENT frame the rumor leaked.
    apply_sealed(
        &mut fx.owner_device,
        BOARD_ID,
        &fx.board_addr,
        &fx.owner.pubkey,
        &fx.owner.secret_key.secret_bytes(),
        &fx.channel,
        BoardAction::AddCard {
            col: 0,
            title: "owner-sentinel-card".to_owned(),
            labels: vec![],
            parent: None,
        },
    );

    // The card folds into the member's view of the shared board — proof the sealed
    // 1081 crossed the relay, nostrdb unwrapped it, and fold_shared_board gathered
    // the owner-authored rumor by coordinate.
    wait_for_convergence(
        &mut fx.owner_device,
        &mut fx.member_device,
        &fx.board_addr,
        "owner's sealed card to fold into the member's shared board",
        |titles| titles.iter().any(|t| t == "owner-sentinel-card"),
    );
}

/// The `is_rumor` fan-out guard, over the wire: with everything sealed, the owner
/// publishes only kind-1081 envelopes; the plaintext rumors nostrdb unwraps (and
/// which match the owner's own board filter) are never fanned out, so the card's
/// and board's cleartext never touch the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sealed_rumors_never_cross_the_wire_in_plaintext() {
    init_tracing();

    // Re-run the relay locally so we hold its capture handle (the fixture forgets
    // its own to keep it alive; here we need to assert on captured frames).
    let root = team_root();
    let channel = SnsChannel {
        keys: enostr::sns::derive_sns_keys(&root).expect("derive sns keys"),
    };
    let owner = FullKeypair::generate();
    let member = FullKeypair::generate();
    let board_addr = event::board_address(&owner.pubkey, BOARD_ID);

    let relay = notedeck_testing::negentropy_relay::run_memory_negentropy_relay()
        .await
        .expect("start relay");
    let relay_url = relay.relay.url().to_owned();

    let mut owner_device = build_headway_device(&relay_url, &owner);
    let mut member_device = build_headway_device(&relay_url, &member);
    join_shared_board(&mut owner_device, &owner, &owner.pubkey, &root, &board_addr);
    join_shared_board(
        &mut member_device,
        &owner,
        &member.pubkey,
        &root,
        &board_addr,
    );

    seal_board_definition(
        &mut owner_device,
        &owner.secret_key.secret_bytes(),
        &channel,
        BOARD_ID,
        "plaintext-sentinel-board",
        &shared_columns(),
    );
    wait_for_device_condition(
        &mut owner_device,
        CONVERGE_TIMEOUT,
        "owner's own shared board definition folds in",
        |device| {
            if shared_board(device, &board_addr).is_some() {
                Ok(())
            } else {
                Err("board definition not folded yet".to_owned())
            }
        },
    );
    apply_sealed(
        &mut owner_device,
        BOARD_ID,
        &board_addr,
        &owner.pubkey,
        &owner.secret_key.secret_bytes(),
        &channel,
        BoardAction::AddCard {
            col: 0,
            title: "plaintext-sentinel-card".to_owned(),
            labels: vec![],
            parent: None,
        },
    );
    wait_for_convergence(
        &mut owner_device,
        &mut member_device,
        &board_addr,
        "the sealed card to converge before checking the wire",
        |titles| titles.iter().any(|t| t == "plaintext-sentinel-card"),
    );
    // A few more frames so any (erroneous) plaintext fan-out would have flushed.
    step_for(&mut owner_device, Duration::from_millis(200));
    step_for(&mut member_device, Duration::from_millis(200));

    // Sealed envelopes did cross the wire...
    assert!(
        relay
            .relay
            .count_captured_events_containing("\"kind\":1081")
            > 0,
        "expected sealed kind-1081 envelopes to be published to the relay"
    );
    // ...but neither sentinel's cleartext ever did. The titles live only inside the
    // encrypted envelope payload; their appearance in a plaintext EVENT frame would
    // mean fan_out_unseen_notes published an unwrapped rumor — exactly what the
    // is_rumor guard exists to prevent.
    assert_eq!(
        relay
            .relay
            .count_captured_events_containing("plaintext-sentinel-card"),
        0,
        "a sealed card's cleartext leaked onto the wire"
    );
    assert_eq!(
        relay
            .relay
            .count_captured_events_containing("plaintext-sentinel-board"),
        0,
        "a sealed board definition's cleartext leaked onto the wire"
    );
}

/// A co-member's sealed edit propagates over the relay and its rumor lands in the
/// owner's nostrdb — but it does **not** fold into the owner's shared board. This
/// is the concrete face of the epic's OPEN note: transport-sharing ≠
/// collaboration. Two gaps compound here, both gated on
/// `headway:headway/purchase-arch-since` (the G6 roster-authority gate):
///
///  1. **Write-path coordinate.** `store::apply` anchors an edit at
///     `board_address(author, board_id)`, and the render path passes the acting
///     member's *own* pubkey — so the member's card is tagged
///     `30619:<member>:headway`, not the owner's coordinate, and
///     `fold_shared_board(30619:<owner>:headway)` never gathers it.
///  2. **Reducer authority.** Even re-addressed to the owner's coordinate, the
///     reducer only counts an edit authored by the card's author or the board
///     owner (`event.rs`'s `authorised` gate), so a member editing the board still
///     wouldn't count until an admin-signed roster defines who may.
///
/// Until that gate lands, a member can *read* a shared board but not *write* to
/// it. This test pins that boundary: the rumor arrives (propagation works), the
/// fold excludes it (collaboration doesn't yet). When G6 lands, this assertion
/// should flip — flag it there rather than treating today's behavior as correct.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_edit_propagates_but_does_not_count_toward_shared_board() {
    init_tracing();
    let mut fx = setup_shared_board().await;

    // The member adds a card off the shared board's view, sealed into the channel,
    // exactly as the render path would (author = the member's own pubkey).
    apply_sealed(
        &mut fx.member_device,
        BOARD_ID,
        &fx.board_addr,
        &fx.member.pubkey,
        &fx.member.secret_key.secret_bytes(),
        &fx.channel,
        BoardAction::AddCard {
            col: 0,
            title: "member-card".to_owned(),
            labels: vec![],
            parent: None,
        },
    );

    // Propagation works: the sealed 1081 crosses the relay and nostrdb unwraps the
    // member's issue rumor into the owner's db.
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        fx.member_device.run_ok();
        fx.owner_device.run_ok();
        if local_issue_subjects(&mut fx.owner_device)
            .iter()
            .any(|s| s == "member-card")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "member's sealed edit never propagated to the owner's db over the relay"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // ...but collaboration does not: the owner's fold of the shared board excludes
    // it, because the edit is anchored at the member's own coordinate (and the
    // reducer's author-or-owner authority gate would drop it even if it weren't).
    // See the doc comment: this flips once headway:headway/purchase-arch-since lands.
    let owner_titles = shared_card_titles(&mut fx.owner_device, &fx.board_addr);
    assert!(
        !owner_titles.iter().any(|t| t == "member-card"),
        "member's card unexpectedly counted toward the shared board — has the G6 \
         roster-authority gate (headway:headway/purchase-arch-since) landed? Update \
         this assertion if so. Owner saw: {owner_titles:?}"
    );
}
