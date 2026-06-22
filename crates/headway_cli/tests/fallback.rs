//! The CLI syncs over NIP-77 negentropy, but a relay that doesn't speak it must
//! degrade gracefully — fall back to a plain NIP-01 sync — rather than hang.
//!
//! This drives the real `headway` binary against a hand-rolled mock relay that
//! handles EVENT/REQ but answers NEG-OPEN with a NOTICE, the way a pre-NIP-77
//! relay would.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const SECRET: [u8; 32] = [0x42; 32];

fn nsec() -> String {
    let hrp = bech32::Hrp::parse("nsec").expect("hrp");
    bech32::encode::<bech32::Bech32>(hrp, &SECRET).expect("encode nsec")
}

/// A minimal NIP-01 relay that stores posted events and replays them on REQ, but
/// has no idea what negentropy is — it NOTICEs any NEG-* frame, exactly the
/// failure mode the fallback must survive.
async fn spawn_nip01_only_relay() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, store.clone()));
        }
    });

    format!("ws://{addr}")
}

async fn serve(stream: TcpStream, store: Arc<Mutex<Vec<String>>>) {
    let Ok(ws) = accept_async(stream).await else {
        return;
    };
    let (mut tx, mut rx) = ws.split();

    while let Some(Ok(Message::Text(text))) = rx.next().await {
        let frame: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        match frame.first().and_then(Value::as_str) {
            Some("EVENT") => {
                let note = frame.get(1).cloned().unwrap_or(Value::Null);
                let id = note.get("id").and_then(Value::as_str).unwrap_or("");
                let ack = json!(["OK", id, true, ""]).to_string();
                store.lock().unwrap().push(note.to_string());
                let _ = tx.send(Message::Text(ack)).await;
            }
            Some("REQ") => {
                let sub = frame
                    .get(1)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let notes: Vec<String> = store.lock().unwrap().clone();
                for note in notes {
                    let _ = tx
                        .send(Message::Text(format!(r#"["EVENT","{sub}",{note}]"#)))
                        .await;
                }
                let _ = tx
                    .send(Message::Text(json!(["EOSE", sub]).to_string()))
                    .await;
            }
            // The whole point: this relay does not understand negentropy.
            Some("NEG-OPEN") | Some("NEG-MSG") => {
                let _ = tx
                    .send(Message::Text(
                        json!(["NOTICE", "unrecognized message"]).to_string(),
                    ))
                    .await;
            }
            _ => {}
        }
    }
}

/// A relay that rejects every EVENT the way strfry rejects a superseded
/// addressable event: `OK: false, "replaced: have newer event"`. Headway's
/// board and placement events are replaceable, so reconcile routinely flushes a
/// cached id the relay has already replaced — that rejection is benign and must
/// not abort the command.
async fn spawn_replacing_relay() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_replacing(stream));
        }
    });
    format!("ws://{addr}")
}

async fn serve_replacing(stream: TcpStream) {
    let Ok(ws) = accept_async(stream).await else {
        return;
    };
    let (mut tx, mut rx) = ws.split();

    while let Some(Ok(Message::Text(text))) = rx.next().await {
        let frame: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        match frame.first().and_then(Value::as_str) {
            Some("EVENT") => {
                let note = frame.get(1).cloned().unwrap_or(Value::Null);
                let id = note.get("id").and_then(Value::as_str).unwrap_or("");
                let ack = json!(["OK", id, false, "replaced: have newer event"]).to_string();
                let _ = tx.send(Message::Text(ack)).await;
            }
            Some("REQ") => {
                let sub = frame
                    .get(1)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let _ = tx
                    .send(Message::Text(json!(["EOSE", sub]).to_string()))
                    .await;
            }
            Some("NEG-OPEN") | Some("NEG-MSG") => {
                let _ = tx
                    .send(Message::Text(
                        json!(["NOTICE", "unrecognized message"]).to_string(),
                    ))
                    .await;
            }
            _ => {}
        }
    }
}

/// Run the `headway` binary, failing the test if it doesn't exit within `secs`
/// (a hang is the exact regression we're guarding against).
fn run_timed(url: &str, db: &str, args: &[&str], secs: u64) -> std::process::Output {
    let nsec = nsec();
    let mut full = vec!["--nsec", nsec.as_str(), "--relay", url, "--db", db];
    full.extend_from_slice(args);

    let mut child = Command::new(env!("CARGO_BIN_EXE_headway"))
        .args(&full)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn headway");

    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("`headway {args:?}` hung against a non-negentropy relay");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("output")
}

/// Column count is how we tell the seeded board has materialised: the default
/// board seeds its columns but no cards.
fn total_cols(board: &Value) -> usize {
    board["columns"].as_array().map_or(0, Vec::len)
}

#[test]
fn falls_back_to_nip01_when_relay_lacks_negentropy() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let url = rt.block_on(spawn_nip01_only_relay());

    // Seed against the negentropy-less relay: reconcile fails over to a full
    // NIP-01 sync, and the seed's events still publish up to the relay's store.
    let seed_dir = tempfile::tempdir().expect("seed dir");
    let out = run_timed(&url, seed_dir.path().to_str().unwrap(), &["seed"], 15);
    assert!(
        out.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("negentropy reconcile unavailable"),
        "expected a fallback warning, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A fresh cache must reconstruct the board purely through the fallback pull.
    let show_dir = tempfile::tempdir().expect("show dir");
    let db = show_dir.path().to_str().unwrap();
    let mut cols = 0;
    for _ in 0..30 {
        let out = run_timed(&url, db, &["show", "--json"], 15);
        if let Ok(board) = serde_json::from_slice::<Value>(&out.stdout) {
            cols = total_cols(&board);
        }
        if cols == 5 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(cols, 5, "fallback show should reconstruct the seeded board");
}

#[test]
fn benign_replaced_rejection_does_not_abort() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _guard = rt.enter();
    let url = rt.block_on(spawn_replacing_relay());

    // `seed` publishes its events; the relay rejects each as "replaced". That's
    // benign — the relay's state is already at-or-ahead — so the command must
    // succeed and seed the local board rather than erroring out.
    let dir = tempfile::tempdir().expect("dir");
    let out = run_timed(&url, dir.path().to_str().unwrap(), &["seed"], 15);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "seed should survive a 'replaced' rejection, got: {stderr}"
    );
    assert!(
        !stderr.contains("rejected"),
        "a benign 'replaced' must not surface as a rejection: {stderr}"
    );

    // And the board really landed in the local cache.
    let out = run_timed(&url, dir.path().to_str().unwrap(), &["show", "--json"], 15);
    let board: Value = serde_json::from_slice(&out.stdout).expect("board json");
    assert_eq!(
        total_cols(&board),
        5,
        "seed must populate the local board despite the rejections"
    );
}
