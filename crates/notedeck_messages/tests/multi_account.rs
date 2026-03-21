//! Multi-account, bidirectional, and mesh end-to-end tests.

mod harness;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use enostr::FullKeypair;
use harness::fixtures::{
    seed_cluster_dm_relay_list, seed_cluster_known_profiles, seed_local_dm_relay_list,
};
use harness::relay::wait_for_relay_count_at_least;
use harness::ui::{
    build_direct_message_batch, open_conversation_via_ui, send_direct_message, send_message_via_ui,
};
use harness::{
    add_account_to_device, assert_cluster_matches_expected, build_messages_cluster,
    build_messages_device, cluster_actual_sets, cluster_converged_on, init_tracing,
    local_chat_messages, select_account_on_device, step_clusters, step_device_frames,
    step_device_group, step_devices, wait_for_cluster_convergence, wait_for_device_group_messages,
    wait_for_device_messages_while_flushing, wait_for_devices_messages, warm_up_clusters,
    TEST_TIMEOUT,
};
use nostr::{Filter as NostrFilter, Kind as NostrKind};
use nostr_relay_builder::{
    builder::RateLimit,
    prelude::{MemoryDatabase, MemoryDatabaseOptions},
    LocalRelay, RelayBuilder,
};
/// Verifies account switching isolates local Messages state and backfills the newly selected account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_accounts_isolates_messages_state_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let account_a = FullKeypair::generate();
    let account_b = FullKeypair::generate();
    let sender = FullKeypair::generate();

    let mut switching_device = build_messages_device(&relay_url, &account_a);
    add_account_to_device(&mut switching_device, &account_b);
    select_account_on_device(&mut switching_device, &account_b);
    step_device_frames(&mut switching_device, 20);
    std::thread::sleep(Duration::from_millis(200));

    let mut sender_to_a = build_messages_device(&relay_url, &sender);
    let mut sender_to_b = build_messages_device(&relay_url, &sender);
    seed_local_dm_relay_list(&mut sender_to_a, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_b, &sender, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_a, &account_a, &relay_url);
    seed_local_dm_relay_list(&mut sender_to_b, &account_b, &relay_url);

    open_conversation_via_ui(
        &mut sender_to_b,
        &account_b.pubkey.npub().expect("account b npub"),
    );
    send_message_via_ui(&mut sender_to_b, "for-account-b");

    wait_for_device_messages_while_flushing(
        &mut switching_device,
        &BTreeSet::from(["for-account-b".to_owned()]),
        TEST_TIMEOUT,
        "account b message visibility before switching away",
        &mut [&mut sender_to_b],
    );

    select_account_on_device(&mut switching_device, &account_b);
    select_account_on_device(&mut switching_device, &account_a);
    open_conversation_via_ui(
        &mut sender_to_a,
        &account_a.pubkey.npub().expect("account a npub"),
    );
    send_message_via_ui(&mut sender_to_a, "for-account-a");
    wait_for_device_messages_while_flushing(
        &mut switching_device,
        &BTreeSet::from(["for-account-a".to_owned()]),
        TEST_TIMEOUT,
        "account a message visibility after switching back",
        &mut [&mut sender_to_a],
    );

    select_account_on_device(&mut switching_device, &account_b);
    assert_eq!(
        local_chat_messages(&mut switching_device),
        BTreeSet::from(["for-account-b".to_owned()]),
        "expected switching back to account b to restore only account b messages"
    );

    relay.shutdown();
}

/// Verifies concurrent sends from two same-account devices converge inside one DM thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_account_sender_devices_converge_after_concurrent_same_thread_sends_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let sender = FullKeypair::generate();
    let recipient = FullKeypair::generate();
    let recipient_npub = recipient.pubkey.npub().expect("recipient npub");

    let mut sender_device_a = build_messages_device(&relay_url, &sender);
    let mut sender_device_b = build_messages_device(&relay_url, &sender);
    let mut recipient_devices = vec![
        build_messages_device(&relay_url, &recipient),
        build_messages_device(&relay_url, &recipient),
    ];

    for sender_device in [&mut sender_device_a, &mut sender_device_b] {
        seed_local_dm_relay_list(sender_device, &sender, &relay_url);
        seed_local_dm_relay_list(sender_device, &recipient, &relay_url);
    }
    for recipient_device in &mut recipient_devices {
        seed_local_dm_relay_list(recipient_device, &recipient, &relay_url);
    }

    step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
    step_devices(&mut recipient_devices);
    std::thread::sleep(Duration::from_millis(100));
    step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
    step_devices(&mut recipient_devices);

    open_conversation_via_ui(&mut sender_device_a, &recipient_npub);
    open_conversation_via_ui(&mut sender_device_b, &recipient_npub);

    let batch_a = build_direct_message_batch("same-account-a", "thread", 4);
    let batch_b = build_direct_message_batch("same-account-b", "thread", 4);
    let mut expected = BTreeSet::new();

    for round in 0..4 {
        let message_a = batch_a[round].clone();
        let message_b = batch_b[round].clone();
        expected.insert(message_a.clone());
        expected.insert(message_b.clone());

        send_message_via_ui(&mut sender_device_a, &message_a);
        send_message_via_ui(&mut sender_device_b, &message_b);
        step_device_group(&mut [&mut sender_device_a, &mut sender_device_b]);
        step_devices(&mut recipient_devices);
        std::thread::sleep(Duration::from_millis(20));
    }

    wait_for_device_group_messages(
        &mut [&mut sender_device_a, &mut sender_device_b],
        &expected,
        TEST_TIMEOUT,
        "same-account sender devices to converge after concurrent same-thread sends",
    );
    wait_for_devices_messages(
        &mut recipient_devices,
        &expected,
        TEST_TIMEOUT,
        "recipient devices to converge after same-account concurrent sends",
    );

    relay.shutdown();
}

/// Verifies both sides of a DM thread see the full interleaved history when both parties reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_dm_thread_converges_across_devices_e2e() {
    init_tracing();

    let relay_db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = LocalRelay::run(RelayBuilder::default().database(relay_db.clone()))
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);

    // Cross-seed: alice's devices need bob's relay list for sending, and vice versa
    for device in &mut alice.devices {
        seed_local_dm_relay_list(device, &bob.account, &relay_url);
    }
    for device in &mut bob.devices {
        seed_local_dm_relay_list(device, &alice.account, &relay_url);
    }

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob]);

    let mut expected = BTreeSet::new();

    for round in 1..=4 {
        let alice_msg = format!("alice->bob:{round:02}");
        expected.insert(alice_msg.clone());
        if round == 1 {
            // First round: open conversation via profile search + send
            send_direct_message(alice.device(0), &bob.npub, &alice_msg);
        } else {
            // Subsequent rounds: conversation already open, just send
            send_message_via_ui(alice.device(0), &alice_msg);
            alice.device(0).step();
            std::thread::sleep(Duration::from_millis(25));
        }
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len() * 2,
            TEST_TIMEOUT,
            "alice giftwraps to land on relay",
            &mut [],
        )
        .await;
        step_clusters(&mut [&mut alice, &mut bob]);

        let bob_msg = format!("bob->alice:{round:02}");
        expected.insert(bob_msg.clone());
        if round == 1 {
            send_direct_message(bob.device(0), &alice.npub, &bob_msg);
        } else {
            send_message_via_ui(bob.device(0), &bob_msg);
            bob.device(0).step();
            std::thread::sleep(Duration::from_millis(25));
        }
        wait_for_relay_count_at_least(
            &relay_db,
            NostrFilter::new().kind(NostrKind::GiftWrap),
            expected.len() * 2,
            TEST_TIMEOUT,
            "bob giftwraps to land on relay",
            &mut [],
        )
        .await;
        step_clusters(&mut [&mut alice, &mut bob]);

        std::thread::sleep(Duration::from_millis(25));
    }

    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        step_clusters(&mut [&mut alice, &mut bob]);

        if cluster_converged_on(&mut alice, &expected) && cluster_converged_on(&mut bob, &expected)
        {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for bidirectional DM convergence; alice {:?}, bob {:?}",
            cluster_actual_sets(&mut alice),
            cluster_actual_sets(&mut bob)
        );

        std::thread::sleep(Duration::from_millis(20));
    }

    assert_cluster_matches_expected(&mut alice, &expected);
    assert_cluster_matches_expected(&mut bob, &expected);

    relay.shutdown();
}

/// Verifies that three accounts can exchange direct messages pairwise and still converge per account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_accounts_pairwise_mesh_converges_across_devices_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default())
        .await
        .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);
    let mut carol = build_messages_cluster("carol", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);
    seed_cluster_dm_relay_list(&mut carol, &relay_url);

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
        (carol.account.clone(), carol.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);
    seed_cluster_known_profiles(&mut carol, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    let alice_to_bob = "alice->bob:01".to_owned();
    let bob_to_alice = "bob->alice:01".to_owned();
    let alice_to_carol = "alice->carol:01".to_owned();
    let carol_to_alice = "carol->alice:01".to_owned();
    let bob_to_carol = "bob->carol:01".to_owned();
    let carol_to_bob = "carol->bob:01".to_owned();

    send_direct_message(alice.device(0), &bob.npub, &alice_to_bob);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(alice.device(1), &carol.npub, &alice_to_carol);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(bob.device(0), &alice.npub, &bob_to_alice);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(bob.device(1), &carol.npub, &bob_to_carol);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(carol.device(0), &alice.npub, &carol_to_alice);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
    send_direct_message(carol.device(1), &bob.npub, &carol_to_bob);
    step_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    let expected_alice = BTreeSet::from([
        alice_to_bob.clone(),
        alice_to_carol.clone(),
        bob_to_alice.clone(),
        carol_to_alice.clone(),
    ]);
    let expected_bob = BTreeSet::from([
        alice_to_bob.clone(),
        bob_to_alice.clone(),
        bob_to_carol.clone(),
        carol_to_bob.clone(),
    ]);
    let expected_carol = BTreeSet::from([
        alice_to_carol.clone(),
        bob_to_carol.clone(),
        carol_to_alice.clone(),
        carol_to_bob.clone(),
    ]);

    wait_for_cluster_convergence(
        &mut alice,
        &expected_alice,
        &mut bob,
        &expected_bob,
        &mut carol,
        &expected_carol,
        TEST_TIMEOUT,
    );

    assert_cluster_matches_expected(&mut alice, &expected_alice);
    assert_cluster_matches_expected(&mut bob, &expected_bob);
    assert_cluster_matches_expected(&mut carol, &expected_carol);

    relay.shutdown();
}

/// Verifies sustained high-volume pairwise traffic delivers every expected message to every device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_accounts_high_volume_pairwise_mesh_delivers_every_message_e2e() {
    init_tracing();

    let relay = LocalRelay::run(RelayBuilder::default().rate_limit(RateLimit {
        max_reqs: 2_000,
        notes_per_minute: 10_000,
    }))
    .await
    .expect("start local relay");
    let relay_url = relay.url().to_owned();

    let mut alice = build_messages_cluster("alice", &relay_url, 2);
    let mut bob = build_messages_cluster("bob", &relay_url, 2);
    let mut carol = build_messages_cluster("carol", &relay_url, 2);

    seed_cluster_dm_relay_list(&mut alice, &relay_url);
    seed_cluster_dm_relay_list(&mut bob, &relay_url);
    seed_cluster_dm_relay_list(&mut carol, &relay_url);

    let profiles = vec![
        (alice.account.clone(), alice.name),
        (bob.account.clone(), bob.name),
        (carol.account.clone(), carol.name),
    ];
    seed_cluster_known_profiles(&mut alice, &profiles);
    seed_cluster_known_profiles(&mut bob, &profiles);
    seed_cluster_known_profiles(&mut carol, &profiles);

    warm_up_clusters(&mut [&mut alice, &mut bob, &mut carol]);

    open_conversation_via_ui(alice.device(0), &bob.npub);
    open_conversation_via_ui(alice.device(1), &carol.npub);
    open_conversation_via_ui(bob.device(0), &alice.npub);
    open_conversation_via_ui(bob.device(1), &carol.npub);
    open_conversation_via_ui(carol.device(0), &alice.npub);
    open_conversation_via_ui(carol.device(1), &bob.npub);

    const MESSAGES_PER_DIRECTION: usize = 40;
    const CONVERGENCE_BATCH: usize = 2;

    let alice_to_bob = build_direct_message_batch("alice", "bob", MESSAGES_PER_DIRECTION);
    let alice_to_carol = build_direct_message_batch("alice", "carol", MESSAGES_PER_DIRECTION);
    let bob_to_alice = build_direct_message_batch("bob", "alice", MESSAGES_PER_DIRECTION);
    let bob_to_carol = build_direct_message_batch("bob", "carol", MESSAGES_PER_DIRECTION);
    let carol_to_alice = build_direct_message_batch("carol", "alice", MESSAGES_PER_DIRECTION);
    let carol_to_bob = build_direct_message_batch("carol", "bob", MESSAGES_PER_DIRECTION);

    for idx in 0..MESSAGES_PER_DIRECTION {
        send_message_via_ui(alice.device(0), &alice_to_bob[idx]);
        send_message_via_ui(alice.device(1), &alice_to_carol[idx]);
        send_message_via_ui(bob.device(0), &bob_to_alice[idx]);
        send_message_via_ui(bob.device(1), &bob_to_carol[idx]);
        send_message_via_ui(carol.device(0), &carol_to_alice[idx]);
        send_message_via_ui(carol.device(1), &carol_to_bob[idx]);

        step_clusters(&mut [&mut alice, &mut bob, &mut carol]);
        std::thread::sleep(Duration::from_millis(10));
        step_clusters(&mut [&mut alice, &mut bob, &mut carol]);

        if (idx + 1) % CONVERGENCE_BATCH == 0 {
            let expected_alice = alice_to_bob[..=idx]
                .iter()
                .chain(alice_to_carol[..=idx].iter())
                .chain(bob_to_alice[..=idx].iter())
                .chain(carol_to_alice[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            let expected_bob = alice_to_bob[..=idx]
                .iter()
                .chain(bob_to_alice[..=idx].iter())
                .chain(bob_to_carol[..=idx].iter())
                .chain(carol_to_bob[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            let expected_carol = alice_to_carol[..=idx]
                .iter()
                .chain(bob_to_carol[..=idx].iter())
                .chain(carol_to_alice[..=idx].iter())
                .chain(carol_to_bob[..=idx].iter())
                .cloned()
                .collect::<BTreeSet<_>>();

            wait_for_cluster_convergence(
                &mut alice,
                &expected_alice,
                &mut bob,
                &expected_bob,
                &mut carol,
                &expected_carol,
                TEST_TIMEOUT,
            );
        }
    }

    let expected_alice = alice_to_bob
        .iter()
        .chain(alice_to_carol.iter())
        .chain(bob_to_alice.iter())
        .chain(carol_to_alice.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_bob = alice_to_bob
        .iter()
        .chain(bob_to_alice.iter())
        .chain(bob_to_carol.iter())
        .chain(carol_to_bob.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_carol = alice_to_carol
        .iter()
        .chain(bob_to_carol.iter())
        .chain(carol_to_alice.iter())
        .chain(carol_to_bob.iter())
        .cloned()
        .collect::<BTreeSet<_>>();

    wait_for_cluster_convergence(
        &mut alice,
        &expected_alice,
        &mut bob,
        &expected_bob,
        &mut carol,
        &expected_carol,
        TEST_TIMEOUT,
    );

    assert_cluster_matches_expected(&mut alice, &expected_alice);
    assert_cluster_matches_expected(&mut bob, &expected_bob);
    assert_cluster_matches_expected(&mut carol, &expected_carol);

    relay.shutdown();
}
