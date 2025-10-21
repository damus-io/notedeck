// Copyright (c) 2023 Yuki Kishimoto
// Distributed under the MIT software license

use negentropy::{Bytes, Negentropy};

fn main() {
    // Client
    let mut client = Negentropy::new(16, None).unwrap();
    client
        .add_item(
            0,
            Bytes::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
        .unwrap();
    client
        .add_item(
            1,
            Bytes::from_hex("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
        )
        .unwrap();
    client.seal().unwrap();
    let init_output = client.initiate().unwrap();
    println!("Initiator Output: {}", init_output.as_hex());

    // Relay
    let mut relay = Negentropy::new(16, None).unwrap();
    relay
        .add_item(
            0,
            Bytes::from_hex("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        )
        .unwrap();
    relay
        .add_item(
            2,
            Bytes::from_hex("cccccccccccccccccccccccccccccccc").unwrap(),
        )
        .unwrap();
    relay
        .add_item(
            3,
            Bytes::from_hex("11111111111111111111111111111111").unwrap(),
        )
        .unwrap();
    relay
        .add_item(
            5,
            Bytes::from_hex("22222222222222222222222222222222").unwrap(),
        )
        .unwrap();
    relay
        .add_item(
            10,
            Bytes::from_hex("33333333333333333333333333333333").unwrap(),
        )
        .unwrap();
    relay.seal().unwrap();
    let reconcile_output = relay.reconcile(&init_output).unwrap();
    println!("Reconcile Output: {}", reconcile_output.as_hex());

    // Client
    let mut have_ids = Vec::new();
    let mut need_ids = Vec::new();
    let reconcile_output_with_ids = client
        .reconcile_with_ids(&reconcile_output, &mut have_ids, &mut need_ids)
        .unwrap();
    println!(
        "Reconcile Output with IDs: {}",
        reconcile_output_with_ids.unwrap().as_hex()
    );
    println!(
        "Have IDs: {}",
        have_ids
            .into_iter()
            .map(|b| b.to_hex())
            .collect::<Vec<_>>()
            .join("")
    );
    println!(
        "Need IDs: {}",
        need_ids
            .into_iter()
            .map(|b| b.to_hex())
            .collect::<Vec<_>>()
            .join("")
    );
}
