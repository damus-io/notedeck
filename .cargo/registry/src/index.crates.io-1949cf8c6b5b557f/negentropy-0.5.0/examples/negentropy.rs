// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use negentropy::{Id, Negentropy, NegentropyStorageVector};

fn main() {
    // Client
    let mut storage_client = NegentropyStorageVector::new();
    storage_client
        .insert(
            0,
            Id::from_slice(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
        .unwrap();
    storage_client
        .insert(
            1,
            Id::from_slice(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
        )
        .unwrap();
    storage_client.seal().unwrap();
    let mut client = Negentropy::borrowed(&storage_client, 0).unwrap();
    let init_output = client.initiate().unwrap();
    println!("Initiator Output: {:x?}", init_output.clone());

    // Relay
    let mut storage_relay = NegentropyStorageVector::new();
    storage_relay
        .insert(
            0,
            Id::from_slice(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
        .unwrap();
    storage_relay
        .insert(
            2,
            Id::from_slice(b"cccccccccccccccccccccccccccccccc").unwrap(),
        )
        .unwrap();
    storage_relay
        .insert(
            3,
            Id::from_slice(b"11111111111111111111111111111111").unwrap(),
        )
        .unwrap();
    storage_relay
        .insert(
            5,
            Id::from_slice(b"22222222222222222222222222222222").unwrap(),
        )
        .unwrap();
    storage_relay
        .insert(
            10,
            Id::from_slice(b"33333333333333333333333333333333").unwrap(),
        )
        .unwrap();
    storage_relay.seal().unwrap();
    let mut relay = Negentropy::borrowed(&storage_relay, 0).unwrap();
    let reconcile_output = relay.reconcile(&init_output).unwrap();
    println!("Reconcile Output: {:x?}", reconcile_output.clone());

    // Client
    let mut have_ids = Vec::new();
    let mut need_ids = Vec::new();
    client
        .reconcile_with_ids(&reconcile_output, &mut have_ids, &mut need_ids)
        .unwrap();
    println!(
        "Have IDs: {}",
        have_ids
            .into_iter()
            .map(|b| format!("{:x?}", b))
            .collect::<Vec<_>>()
            .join("")
    );
    println!(
        "Need IDs: {}",
        need_ids
            .into_iter()
            .map(|b| format!("{:x?}", b))
            .collect::<Vec<_>>()
            .join("")
    );
}
