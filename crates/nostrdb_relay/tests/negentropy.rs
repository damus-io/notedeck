//! NIP-77 reconciliation against the embedded relay, driven by a real
//! negentropy client. The relay is the responder; this test plays the
//! initiator and asserts it learns exactly the set difference both ways.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::{Config, Filter, Ndb, NoteBuilder, SubscriptionStream};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const SECRET: [u8; 32] = [0x42; 32];
const TEST_KIND: u32 = 30_111;

/// A signed test event, kept alongside the `(created_at, id)` the protocol
/// reconciles on so we can seed either side and assert on the diff.
struct Ev {
    frame: String,
    id: [u8; 32],
    created_at: u64,
}

fn make_event(content: &str) -> Ev {
    let note = NoteBuilder::new()
        .kind(TEST_KIND)
        .content(content)
        .sign(&SECRET)
        .build()
        .expect("signed note");
    let id = *note.id();
    let created_at = note.created_at();
    let json = note.json().expect("note json");
    Ev {
        frame: format!(r#"["EVENT",{json}]"#),
        id,
        created_at,
    }
}

fn fresh_ndb(dir: &std::path::Path) -> Ndb {
    Ndb::new(
        dir.to_str().unwrap(),
        &Config::new().set_ingester_threads(1),
    )
    .expect("ndb")
}

/// Ingest `events` into `ndb` and await their commit via a subscription rather
/// than sleeping. We subscribe *before* processing so every ingest wakes the
/// stream; the subscription fires on commit, so once we've seen one key per
/// event they're all queryable.
async fn ingest_and_wait(ndb: &Ndb, events: &[&Ev]) {
    let filter = Filter::new().kinds([TEST_KIND as u64]).build();
    let sub = ndb.subscribe(&[filter]).expect("subscribe");
    let mut stream = SubscriptionStream::new(ndb.clone(), sub);

    for ev in events {
        ndb.process_client_event(&ev.frame).expect("ingest");
    }

    let mut seen = 0;
    while seen < events.len() {
        let keys = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("ingest within timeout")
            .expect("subscription stream open");
        seen += keys.len();
    }
}

async fn next_frame<S>(ws: &mut S) -> Vec<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("frame within timeout")
            .expect("stream open")
            .expect("ws message");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).expect("json array frame");
        }
    }
}

fn id_set(ids: impl IntoIterator<Item = Id>) -> std::collections::BTreeSet<[u8; 32]> {
    ids.into_iter().map(|id| id.to_bytes()).collect()
}

fn byte_set(events: &[Ev]) -> std::collections::BTreeSet<[u8; 32]> {
    events.iter().map(|e| e.id).collect()
}

#[tokio::test]
async fn reconciles_set_difference_both_ways() {
    // Three buckets: events both sides hold, events only the relay has, and
    // events only the client has. Reconciliation must surface the latter two.
    let shared: Vec<Ev> = (0..5).map(|i| make_event(&format!("shared {i}"))).collect();
    let relay_only: Vec<Ev> = (0..3).map(|i| make_event(&format!("relay {i}"))).collect();
    let client_only: Vec<Ev> = (0..2).map(|i| make_event(&format!("client {i}"))).collect();

    // Relay side: shared + relay-only.
    let dir = tempfile::tempdir().expect("tempdir");
    let ndb = fresh_ndb(dir.path());
    let relay_events: Vec<&Ev> = shared.iter().chain(relay_only.iter()).collect();
    ingest_and_wait(&ndb, &relay_events).await;
    let relay = nostrdb_relay::spawn(ndb.clone(), "127.0.0.1:0".parse().unwrap()).expect("relay");

    // Client side: shared + client-only, as a sealed negentropy set.
    let mut local = NegentropyStorageVector::new();
    for ev in shared.iter().chain(client_only.iter()) {
        local
            .insert(ev.created_at, Id::from_byte_array(ev.id))
            .expect("insert");
    }
    local.seal().expect("seal");
    let mut neg = Negentropy::owned(local, 0).expect("negentropy");
    let initial = neg.initiate().expect("initiate");

    let (mut ws, _resp) = tokio_tungstenite::connect_async(relay.url())
        .await
        .expect("connect");

    let filter = json!({ "kinds": [TEST_KIND] });
    ws.send(Message::Text(
        json!(["NEG-OPEN", "s1", filter, hex::encode(&initial)]).to_string(),
    ))
    .await
    .expect("send NEG-OPEN");

    // Drive the rounds: each NEG-MSG from the relay folds in and may produce the
    // next query, until our side has nothing left to ask.
    let mut have_ids: Vec<Id> = Vec::new();
    let mut need_ids: Vec<Id> = Vec::new();
    loop {
        let frame = next_frame(&mut ws).await;
        assert_eq!(
            frame[0].as_str(),
            Some("NEG-MSG"),
            "unexpected frame: {frame:?}"
        );
        assert_eq!(frame[1].as_str(), Some("s1"));
        let msg = hex::decode(frame[2].as_str().unwrap()).expect("hex");

        match neg
            .reconcile_with_ids(&msg, &mut have_ids, &mut need_ids)
            .expect("reconcile")
        {
            Some(reply) => ws
                .send(Message::Text(
                    json!(["NEG-MSG", "s1", hex::encode(&reply)]).to_string(),
                ))
                .await
                .expect("send NEG-MSG"),
            None => break,
        }
    }

    // `need` = on the relay, missing locally = relay-only.
    // `have` = held locally, missing on the relay = client-only.
    assert_eq!(id_set(need_ids), byte_set(&relay_only), "need set");
    assert_eq!(id_set(have_ids), byte_set(&client_only), "have set");

    ws.send(Message::Text(json!(["NEG-CLOSE", "s1"]).to_string()))
        .await
        .expect("send NEG-CLOSE");
}

#[tokio::test]
async fn neg_msg_without_open_session_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ndb = fresh_ndb(dir.path());
    let relay = nostrdb_relay::spawn(ndb, "127.0.0.1:0".parse().unwrap()).expect("relay");

    let (mut ws, _resp) = tokio_tungstenite::connect_async(relay.url())
        .await
        .expect("connect");

    // A NEG-MSG for a sub id we never NEG-OPENed must come back as NEG-ERR, not
    // crash the connection.
    ws.send(Message::Text(
        json!(["NEG-MSG", "ghost", hex::encode([0x61u8])]).to_string(),
    ))
    .await
    .expect("send");

    let frame = next_frame(&mut ws).await;
    assert_eq!(frame[0].as_str(), Some("NEG-ERR"), "frame: {frame:?}");
    assert_eq!(frame[1].as_str(), Some("ghost"));
}
