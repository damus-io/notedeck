use std::time::Instant;

use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::Filter;

use crate::relay::FullHistorySubId;
use crate::relay::SubPassGuardian;
use crate::NoteId;

use super::{
    protocol::{neg_close_msg, neg_msg, neg_open_msg, NegSessionId},
    relay::NegentropyRelay,
    session::{prepare_negentropy, ActiveSession},
    state::{NegentropyData, NegentropyNeed},
};

const MIN_FRAME_SIZE_LIMIT: u64 = 4097;
type TestNegentropy = Negentropy<'static, NegentropyStorageVector>;

fn id_for_test(index: usize) -> Id {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
    Id::from_byte_array(bytes)
}

fn sealed_storage_with_ids(count: usize) -> NegentropyStorageVector {
    let mut storage = NegentropyStorageVector::with_capacity(count);
    for index in 0..count {
        storage
            .insert(index as u64, id_for_test(index))
            .expect("insert test id");
    }
    storage.seal().expect("seal test storage");
    storage
}

fn test_filter() -> Filter {
    Filter::new().kinds(vec![1]).build()
}

fn empty_negentropy() -> TestNegentropy {
    Negentropy::owned(NegentropyStorageVector::new(), 0).unwrap()
}

fn insert_active_session(
    data: &mut NegentropyData,
    guardian: &mut SubPassGuardian,
    session_id: &str,
    opened_at: Instant,
    filter: Filter,
    owner_history_id: FullHistorySubId,
) {
    insert_active_session_with_neg(
        data,
        guardian,
        session_id,
        empty_negentropy(),
        opened_at,
        filter,
        owner_history_id,
    );
}

fn insert_active_session_with_neg(
    data: &mut NegentropyData,
    guardian: &mut SubPassGuardian,
    session_id: &str,
    neg: TestNegentropy,
    opened_at: Instant,
    filter: Filter,
    owner_history_id: FullHistorySubId,
) {
    let pass = guardian.take_pass().unwrap();
    data.active_sessions.insert(
        NegSessionId::new(session_id.to_owned()),
        ActiveSession::new(neg, pass, opened_at, filter, owner_history_id),
    );
}

#[test]
fn neg_open_msg_format() {
    let msg = neg_open_msg(
        &NegSessionId::new("sub-1".to_owned()),
        r#"{"kinds":[1]}"#.to_owned(),
        "abcd",
    );
    assert_eq!(
        msg.to_json().expect("serialize NEG-OPEN"),
        r#"["NEG-OPEN","sub-1",{"kinds":[1]},"abcd"]"#
    );
}

#[test]
fn neg_msg_format() {
    let msg = neg_msg(&NegSessionId::new("sub-1".to_owned()), "deadbeef");
    assert_eq!(
        msg.to_json().expect("serialize NEG-MSG"),
        r#"["NEG-MSG","sub-1","deadbeef"]"#
    );
}

#[test]
fn neg_close_msg_format() {
    let msg = neg_close_msg(&NegSessionId::new("sub-1".to_owned()));
    assert_eq!(
        msg.to_json().expect("serialize NEG-CLOSE"),
        r#"["NEG-CLOSE","sub-1"]"#
    );
}

#[test]
fn prepare_negentropy_produces_hex() {
    let mut storage = NegentropyStorageVector::new();
    storage.seal().unwrap();
    let result = prepare_negentropy(storage);
    assert!(result.is_some());
    assert!(!result.unwrap().1.is_empty());
}

#[test]
fn drain_need_ids_empties() {
    let mut data = NegentropyData::default();
    data.surfaced_need_ids.push(NegentropyNeed {
        owner_history_id: FullHistorySubId(0),
        filter: test_filter(),
        id: NoteId::new([1; 32]),
    });
    assert_eq!(
        data.drain_need_ids(),
        vec![NegentropyNeed {
            owner_history_id: FullHistorySubId(0),
            filter: test_filter(),
            id: NoteId::new([1; 32])
        }]
    );
    assert!(data.drain_need_ids().is_empty());
}

#[test]
fn handle_neg_err_does_not_mark_unsupported() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now(),
        test_filter(),
        FullHistorySubId(0),
    );

    NegentropyRelay::new(None, &mut data, &mut guardian)
        .handle_neg_err("sub-1", "blocked: too many records");

    assert!(!data.is_unsupported());
    assert!(data.is_filter_blocked(&test_filter()));
    assert!(data.drain_retry_neg_sets().is_empty());
    assert_eq!(guardian.available_passes(), 2);
}

#[test]
fn handle_neg_err_closed_does_not_block_filter() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    let filter = test_filter();
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now(),
        filter.clone(),
        FullHistorySubId(0),
    );

    NegentropyRelay::new(None, &mut data, &mut guardian)
        .handle_neg_err("sub-1", "closed: session timeout");

    assert!(!data.is_unsupported());
    assert!(!data.is_filter_blocked(&filter));
    let retries = data.drain_retry_neg_sets();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].owner_history_id, FullHistorySubId(0));
    assert!(retries[0].filter.same_canonical_attributes(&filter));
    assert_eq!(guardian.available_passes(), 2);
}

#[test]
fn handle_relay_disconnect_returns_all_passes() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now(),
        test_filter(),
        FullHistorySubId(0),
    );

    NegentropyRelay::new(None, &mut data, &mut guardian).handle_relay_disconnect();

    assert_eq!(guardian.available_passes(), 2);
}

#[test]
fn handle_timeout_marks_unsupported() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now() - super::session::NEGENTROPY_OPEN_TIMEOUT,
        test_filter(),
        FullHistorySubId(0),
    );

    NegentropyRelay::new(None, &mut data, &mut guardian).handle_timeout(Instant::now());

    assert!(data.is_unsupported());
    assert_eq!(guardian.available_passes(), 2);
}

#[test]
fn handle_timeout_retries_expired_session_after_capability_is_known() {
    let mut data = NegentropyData {
        capability: Some(true),
        ..Default::default()
    };
    let mut guardian = SubPassGuardian::new(3);
    let expired_filter = test_filter();
    insert_active_session(
        &mut data,
        &mut guardian,
        "expired",
        Instant::now() - super::session::NEGENTROPY_OPEN_TIMEOUT,
        expired_filter.clone(),
        FullHistorySubId(10),
    );
    insert_active_session(
        &mut data,
        &mut guardian,
        "fresh",
        Instant::now(),
        test_filter(),
        FullHistorySubId(11),
    );

    NegentropyRelay::new(None, &mut data, &mut guardian).handle_timeout(Instant::now());

    assert!(!data.is_unsupported());
    assert_eq!(data.active_session_count(), 1);
    assert!(data
        .active_sessions
        .contains_key(&NegSessionId::new("fresh".to_owned())));
    assert_eq!(guardian.available_passes(), 2);

    let retries = data.drain_retry_neg_sets();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].owner_history_id, FullHistorySubId(10));
    assert!(retries[0].filter.same_canonical_attributes(&expired_filter));
}

#[test]
fn handle_neg_msg_continue_refreshes_timeout_clock() {
    let mut client_storage = NegentropyStorageVector::new();
    client_storage.seal().unwrap();
    let mut client_neg = Negentropy::owned(client_storage, MIN_FRAME_SIZE_LIMIT).unwrap();
    let init_msg = client_neg.initiate().unwrap();

    let relay_storage = sealed_storage_with_ids(200);
    let mut relay_neg = Negentropy::borrowed(&relay_storage, MIN_FRAME_SIZE_LIMIT).unwrap();
    let relay_msg = relay_neg.reconcile(&init_msg).unwrap();

    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    insert_active_session_with_neg(
        &mut data,
        &mut guardian,
        "sub-1",
        client_neg,
        Instant::now() - super::session::NEGENTROPY_OPEN_TIMEOUT,
        test_filter(),
        FullHistorySubId(0),
    );

    let result = NegentropyRelay::new(None, &mut data, &mut guardian)
        .handle_neg_msg("sub-1", &hex::encode(relay_msg));

    assert!(result
        .expect("expected follow-up NEG-MSG")
        .to_json()
        .expect("serialize NEG-MSG")
        .starts_with(r#"["NEG-MSG","sub-1","#));

    NegentropyRelay::new(None, &mut data, &mut guardian).handle_timeout(Instant::now());

    assert!(!data.is_unsupported());
    assert_eq!(data.active_session_count(), 1);
    assert_eq!(guardian.available_passes(), 1);
}

#[test]
fn handle_neg_msg_invalid_hex_marks_relay_unsupported() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(2);
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now(),
        test_filter(),
        FullHistorySubId(0),
    );

    let result =
        NegentropyRelay::new(None, &mut data, &mut guardian).handle_neg_msg("sub-1", "not-hex");

    assert!(result.is_none());
    assert!(data.is_unsupported());
    assert_eq!(data.active_session_count(), 0);
    assert_eq!(guardian.available_passes(), 2);
}

#[test]
fn handle_neg_msg_invalid_hex_retries_session_after_capability_is_known() {
    let mut data = NegentropyData {
        capability: Some(true),
        ..Default::default()
    };
    let mut guardian = SubPassGuardian::new(2);
    let filter = test_filter();
    insert_active_session(
        &mut data,
        &mut guardian,
        "sub-1",
        Instant::now(),
        filter.clone(),
        FullHistorySubId(10),
    );

    let result =
        NegentropyRelay::new(None, &mut data, &mut guardian).handle_neg_msg("sub-1", "not-hex");

    assert!(result.is_none());
    assert!(!data.is_unsupported());
    assert_eq!(data.active_session_count(), 0);
    assert_eq!(guardian.available_passes(), 2);

    let retries = data.drain_retry_neg_sets();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].owner_history_id, FullHistorySubId(10));
    assert!(retries[0].filter.same_canonical_attributes(&filter));
}

#[test]
fn cancel_owner_clears_active_sessions_and_surfaced_need_ids() {
    let mut data = NegentropyData::default();
    let mut guardian = SubPassGuardian::new(4);

    for (session_id, owner_history_id) in [
        ("sub-1", FullHistorySubId(1)),
        ("sub-2", FullHistorySubId(2)),
    ] {
        insert_active_session(
            &mut data,
            &mut guardian,
            session_id,
            Instant::now(),
            test_filter(),
            owner_history_id,
        );
    }

    data.surfaced_need_ids.push(NegentropyNeed {
        owner_history_id: FullHistorySubId(1),
        filter: test_filter(),
        id: NoteId::new([1; 32]),
    });
    data.surfaced_need_ids.push(NegentropyNeed {
        owner_history_id: FullHistorySubId(2),
        filter: test_filter(),
        id: NoteId::new([2; 32]),
    });

    NegentropyRelay::new(None, &mut data, &mut guardian).cancel_owner(FullHistorySubId(1));

    assert_eq!(data.active_sessions.len(), 1);
    assert!(data.active_sessions.contains_key("sub-2"));
    assert_eq!(
        data.drain_need_ids(),
        vec![NegentropyNeed {
            owner_history_id: FullHistorySubId(2),
            filter: test_filter(),
            id: NoteId::new([2; 32])
        }]
    );
    assert_eq!(guardian.available_passes(), 3);
}
