//! End-to-end check: a websocket client publishes a signed event into the
//! embedded relay and reads it back both live and from stored history.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nostrdb::{Config, Ndb, NoteBuilder};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

const SECRET: [u8; 32] = [0x42; 32];
const TEST_KIND: u32 = 30_100;

fn fresh_ndb(dir: &std::path::Path) -> Ndb {
    Ndb::new(
        dir.to_str().unwrap(),
        &Config::new().set_ingester_threads(1),
    )
    .expect("ndb")
}

fn signed_event_frame(content: &str) -> (String, String) {
    let note = NoteBuilder::new()
        .kind(TEST_KIND)
        .content(content)
        .sign(&SECRET)
        .build()
        .expect("signed note");
    let json = note.json().expect("note json");
    let id = serde_json::from_str::<Value>(&json).expect("note json")["id"]
        .as_str()
        .expect("event id")
        .to_owned();
    (id, format!(r#"["EVENT",{json}]"#))
}

/// Await the next text frame, parsed as a JSON array, failing on timeout.
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
            return serde_json::from_str::<Vec<Value>>(&text).expect("json array frame");
        }
    }
}

#[tokio::test]
async fn publishes_and_reads_back_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ndb = fresh_ndb(dir.path());
    let relay = nostrdb_relay::spawn(ndb, "127.0.0.1:0".parse().unwrap()).expect("spawn relay");

    let (mut ws, _resp) = tokio_tungstenite::connect_async(relay.url())
        .await
        .expect("connect");

    // Open a live subscription against an empty db: immediate EOSE.
    ws.send(Message::Text(
        r#"["REQ","live",{"kinds":[30100]}]"#.to_string(),
    ))
    .await
    .expect("send REQ");
    let eose = next_frame(&mut ws).await;
    assert_eq!(eose[0], "EOSE", "first frame is EOSE on empty db");
    assert_eq!(eose[1], "live");

    // Publish an event; expect an OK and a live EVENT on the open subscription.
    let (event_id, frame) = signed_event_frame("hello headway");
    ws.send(Message::Text(frame)).await.expect("send EVENT");

    let mut saw_ok = false;
    let mut saw_live_event = false;
    for _ in 0..2 {
        let f = next_frame(&mut ws).await;
        match f[0].as_str().unwrap() {
            "OK" => {
                assert_eq!(f[1], event_id, "OK references our event id");
                assert_eq!(f[2], true, "ingest accepted");
                saw_ok = true;
            }
            "EVENT" => {
                assert_eq!(f[1], "live", "live EVENT on our subscription");
                assert_eq!(f[2]["id"], event_id);
                saw_live_event = true;
            }
            other => panic!("unexpected frame {other}"),
        }
    }
    assert!(saw_ok && saw_live_event, "got both OK and live EVENT");

    // A second REQ now replays the same event from stored history, then EOSE.
    ws.send(Message::Text(
        r#"["REQ","replay",{"kinds":[30100]}]"#.to_string(),
    ))
    .await
    .expect("send replay REQ");

    let stored = next_frame(&mut ws).await;
    assert_eq!(stored[0], "EVENT");
    assert_eq!(stored[1], "replay");
    assert_eq!(stored[2]["id"], event_id);

    let stored_eose = next_frame(&mut ws).await;
    assert_eq!(stored_eose[0], "EOSE");
    assert_eq!(stored_eose[1], "replay");
}
