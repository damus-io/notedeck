//! Shared harness for Headway's networked / two-party SNS integration tests.
//!
//! These tests exercise the *transport* half of shared boards that the
//! single-process `src` tests can't reach: sealed kind-1081 envelopes leaving one
//! member's device, crossing a real in-process relay
//! ([`notedeck_testing::negentropy_relay::MemoryNegentropyRelay`]), and folding
//! into another member's board via [`event::fold_shared_board`]. Everything is
//! driven through full `notedeck_headway::Headway` app instances (one per member
//! device), so the assertions cover the production sync path end to end:
//! [`notedeck::PrivateRelaySync`] inbound, [`notedeck::fan_out_unseen_notes`]
//! outbound (including its `is_rumor` leak guard), and nostrdb's 1059→1082 /
//! 1081→rumor auto-unwrap.
//!
//! The device/relay plumbing is reused from `notedeck_testing` (the same harness
//! the Messages and Dave e2e suites drive); this module only adds the
//! Headway-specific pieces: the app factory, the SNS key-share gift-wrap builder
//! (lifted from `teams.rs`'s unit tests), and the seal-an-edit / read-a-shared-
//! board helpers.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use enostr::{FullKeypair, NoteId, Pubkey};
use nostr::key::PublicKey;
use nostr::nips::nip44;
use nostr::secp256k1::rand::rngs::OsRng;
use nostrdb::{Filter, NoteBuilder, Transaction};
use notedeck::RelayAction;
use notedeck_headway::{
    Headway,
    event::{self, BoardView, ColumnDef},
    store::{self, BoardAction, Signer, SnsChannel},
};
use notedeck_testing::{
    device::{AppFactory, DeviceHarness, build_device_in_tmpdir_with_relays},
    fixtures::process_relay_action_on_device,
};
use tempfile::TempDir;

/// Generous ceiling for the convergence polling loops — long enough for a slow CI
/// runner to complete a NIP-77 reconcile plus a live REQ, short enough that a
/// genuinely stuck sync fails the test rather than hanging forever.
pub const CONVERGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Installs the Headway app on a freshly booted device.
pub fn headway_app_factory() -> AppFactory {
    Box::new(|notedeck, _egui_ctx| {
        notedeck.set_app(Headway::new());
    })
}

/// One full Headway device on `relay_url`, with that relay marked as the account's
/// kind-10013 NIP-37 private-sync relay — the marker
/// [`notedeck::PrivateRelaySync`] keys off, so the board (and its shared-board
/// envelope streams) actually syncs over the wire rather than staying local.
pub fn build_headway_device(relay_url: &str, account: &FullKeypair) -> DeviceHarness {
    let mut device = build_device_in_tmpdir_with_relays(
        &[relay_url],
        account,
        TempDir::new().expect("tmpdir"),
        headway_app_factory(),
    );
    process_relay_action_on_device(&mut device, RelayAction::AddPrivate(relay_url.to_owned()));
    device
}

/// Build a kind-1082 key-share rumor (all notes via nostrdb [`NoteBuilder`]),
/// NIP-59 seal + gift-wrap it to `recipient`, and return the kind-1059 giftwrap
/// JSON — what a member publishes to add another and what nostrdb unwraps back
/// into a queryable `1082` rumor. Lifted verbatim from the `teams.rs` unit tests
/// so both exercise one builder; `nip44` does the crypto layers, every note is a
/// `NoteBuilder`.
pub fn gift_wrapped_keyshare(
    sender: &FullKeypair,
    recipient: &Pubkey,
    root: &[u8; 32],
    board_addr: Option<&str>,
    epoch: Option<u32>,
) -> String {
    let mut rumor = NoteBuilder::new()
        .kind(enostr::sns::KEYSHARE_KIND)
        .content("")
        .start_tag()
        .tag_str("team_root")
        .tag_str(&hex::encode(root));
    if let Some(addr) = board_addr {
        rumor = rumor.start_tag().tag_str("a").tag_str(addr);
    }
    if let Some(epoch) = epoch {
        rumor = rumor
            .start_tag()
            .tag_str("epoch")
            .tag_str(&epoch.to_string());
    }
    let rumor_json = rumor
        .sign(&sender.secret_key.secret_bytes())
        .build()
        .expect("rumor")
        .json()
        .expect("rumor json");

    let recipient_pk = PublicKey::from_slice(recipient.bytes()).expect("recipient pk");
    let mut rng = OsRng;
    // NIP-44 seals the rumor to the recipient, then a random wrap key seals the
    // kind-13 seal into the 1059 (encryption only — the notes are NoteBuilder).
    let encrypted_rumor = nip44::encrypt_with_rng(
        &mut rng,
        &sender.secret_key,
        &recipient_pk,
        &rumor_json,
        nip44::Version::V2,
    )
    .expect("encrypt rumor");
    let seal_json = NoteBuilder::new()
        .kind(13)
        .content(&encrypted_rumor)
        .sign(&sender.secret_key.secret_bytes())
        .build()
        .expect("seal")
        .json()
        .expect("seal json");
    let wrap_keys = FullKeypair::generate();
    let encrypted_seal = nip44::encrypt_with_rng(
        &mut rng,
        &wrap_keys.secret_key,
        &recipient_pk,
        &seal_json,
        nip44::Version::V2,
    )
    .expect("encrypt seal");
    NoteBuilder::new()
        .kind(1059)
        .content(&encrypted_seal)
        .start_tag()
        .tag_str("p")
        .tag_str(&recipient.hex())
        .sign(&wrap_keys.secret_key.secret_bytes())
        .build()
        .expect("giftwrap")
        .json()
        .expect("giftwrap json")
}

/// Ingest a kind-1059 gift-wrap into a device's local nostrdb — how an SNS
/// key-share reaches a member. nostrdb auto-unwraps it to the durable `1082`
/// rumor (the account's key is registered at boot), which the device's Headway
/// polls to join the board (see [`teams`](notedeck_headway) — the roster lives in
/// the db, not on disk).
pub fn ingest_giftwrap(device: &mut DeviceHarness, giftwrap_json: &str) {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    app_ctx
        .ndb
        .process_event(&format!("[\"EVENT\",\"kg\",{giftwrap_json}]"))
        .expect("ingest giftwrap");
}

/// Deliver a shared board to `device` by gift-wrapping it a key-share for `root`
/// naming `board_addr`, then stepping until its Headway has joined the team —
/// proven by the device declaring the channel's kind-1081 envelope filter to the
/// relay, which is what makes co-members' sealed edits reach (and fan out from)
/// this device.
///
/// The join is asynchronous: nostrdb unwraps the 1059 on its writer thread, the
/// live key-share subscription reports the 1082 on a later frame, and only then
/// does `update` register the root and widen the private-sync subscription. We
/// step generously rather than guess a frame count.
pub fn join_shared_board(
    device: &mut DeviceHarness,
    sharer: &FullKeypair,
    recipient: &Pubkey,
    root: &[u8; 32],
    board_addr: &str,
) {
    let giftwrap = gift_wrapped_keyshare(sharer, recipient, root, Some(board_addr), None);
    ingest_giftwrap(device, &giftwrap);
    // Let the writer commit the unwrapped 1082 and the app pick it up over a few
    // update frames (derive roster → register root → declare the envelope sub).
    step_for(device, Duration::from_millis(400));
}

/// Seal a fresh board *definition* (kind-30619 with `columns`) into `channel` and
/// ingest it on `device`, authored by `owner`. Shared boards have no plaintext
/// leg the members subscribe to, so the definition itself must travel sealed for a
/// co-member's [`event::fold_shared_board`] to ever resolve the board — this is
/// what a member subscribes to a coordinate *for*. The device's `update` fans the
/// resulting 1081 envelope out to the relay on a following frame.
pub fn seal_board_definition(
    device: &mut DeviceHarness,
    owner_secret: &[u8; 32],
    channel: &SnsChannel,
    board_id: &str,
    title: &str,
    columns: &[ColumnDef],
) {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    store::ingest_signed(
        app_ctx.ndb,
        event::build_board(board_id, title, "", columns),
        &Signer::shared(owner_secret, channel),
        &mut store::NoPublish,
    );
}

/// Apply a [`BoardAction`] on `device` as `author`, sealed into `channel`, against
/// the shared board folded at `shared_addr` — the production render-path shape: the
/// acting member folds the *shared* coordinate's view (columns, cards) and writes
/// a sealed edit off it. The device's `update` fans the resulting 1081 envelope out
/// to the relay next frame.
///
/// `author` is what stamps the edit's board coordinate (`store::apply` derives it
/// as `board_address(author, board_id)`), which is the acting member's own pubkey
/// on the render path — deliberately faithful, since that coordinate choice is
/// exactly what the two-party tests probe.
pub fn apply_sealed(
    device: &mut DeviceHarness,
    board_id: &str,
    shared_addr: &str,
    author: &Pubkey,
    author_secret: &[u8; 32],
    channel: &SnsChannel,
    action: BoardAction,
) {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    let view = event::load_shared_board(
        app_ctx.ndb,
        &txn,
        shared_addr,
        std::slice::from_ref(&channel.keys.team_keypair.pubkey),
    )
    .expect("shared board folds before an edit is applied to it");
    store::apply(
        app_ctx.ndb,
        board_id,
        &view,
        author,
        &Signer::shared(author_secret, channel),
        action,
        &mut store::NoPublish,
    );
}

/// Fold the shared board at `board_addr` out of a device's local nostrdb, if its
/// definition has arrived. The read half of convergence — every member resolves a
/// shared board through [`event::fold_shared_board`], gathering all authors'
/// events by coordinate.
pub fn shared_board(
    device: &mut DeviceHarness,
    board_addr: &str,
    team_pubkey: &Pubkey,
) -> Option<BoardView> {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    event::load_shared_board(
        app_ctx.ndb,
        &txn,
        board_addr,
        std::slice::from_ref(team_pubkey),
    )
}

/// The titles of every card on a device's view of the shared board at
/// `board_addr`, across all live columns (empty if the board hasn't folded yet).
pub fn shared_card_titles(
    device: &mut DeviceHarness,
    board_addr: &str,
    team_pubkey: &Pubkey,
) -> Vec<String> {
    shared_board(device, board_addr, team_pubkey)
        .map(|v| {
            v.columns
                .iter()
                .flat_map(|c| c.cards.iter())
                .map(|c| c.title.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The id of the card titled `title` on a device's view of the shared board at
/// `board_addr`, if present. Lets a test address an existing shared card (e.g. to
/// edit or move a co-member's card) without hard-coding note ids.
pub fn shared_card_id(
    device: &mut DeviceHarness,
    board_addr: &str,
    team_pubkey: &Pubkey,
    title: &str,
) -> Option<NoteId> {
    shared_board(device, board_addr, team_pubkey).and_then(|v| {
        v.columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|c| c.title == title)
            .map(|c| c.id)
    })
}

/// Count local notes of `kind` in a device's nostrdb — used to assert that a
/// sealed edit's unwrapped rumor actually reached the receiver (propagation),
/// independent of whether the fold counts it.
pub fn local_note_count(device: &mut DeviceHarness, kind: u64) -> usize {
    let egui_ctx = device.ctx.clone();
    let app_ctx = &mut device.state_mut().notedeck.app_context(&egui_ctx);
    let txn = Transaction::new(app_ctx.ndb).expect("txn");
    app_ctx
        .ndb
        .query(&txn, &[Filter::new().kinds([kind]).build()], 4096)
        .map(|r| r.len())
        .unwrap_or(0)
}

/// Step one device for a wall-clock `duration`, pumping its event loop so async
/// ingests, relay traffic, and repaint bursts make progress. `run_ok` (never
/// `run`) because Headway self-schedules repaint bursts that trip `run`'s
/// max-steps panic.
pub fn step_for(device: &mut DeviceHarness, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        device.run_ok();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Step both devices until `receiver`'s view of the shared board at `board_addr`
/// has folded in (its definition arrived), or panic after [`CONVERGE_TIMEOUT`].
/// Used to make a fixture ready for *either* member to edit: an edit folds the
/// shared view first, so the def must have converged before one can be applied.
pub fn wait_for_shared_board_ready(
    publisher: &mut DeviceHarness,
    receiver: &mut DeviceHarness,
    board_addr: &str,
    team_pubkey: &Pubkey,
    context: &str,
) {
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        publisher.run_ok();
        receiver.run_ok();
        if shared_board(receiver, board_addr, team_pubkey).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {context}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Step both devices until `done` holds against each member's folded shared board,
/// or panic after [`CONVERGE_TIMEOUT`]. Both devices step every iteration so the
/// publisher keeps flushing its outbox while the receiver polls its inbound sync.
pub fn wait_for_convergence(
    publisher: &mut DeviceHarness,
    receiver: &mut DeviceHarness,
    board_addr: &str,
    team_pubkey: &Pubkey,
    context: &str,
    done: impl Fn(&[String]) -> bool,
) {
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        publisher.run_ok();
        receiver.run_ok();

        let titles = shared_card_titles(receiver, board_addr, team_pubkey);
        if done(&titles) {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {context}; receiver saw shared cards {:?}",
            titles
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
