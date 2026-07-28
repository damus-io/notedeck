//! A minimal embedded [NIP-01] nostr relay served over a nostrdb [`Ndb`] handle.
//!
//! This exists so external tooling — CLI utilities for dogfooding Headway — can
//! publish and read nostr events directly against a running app's local
//! nostrdb. It speaks just enough of NIP-01 to be useful:
//!
//! - `["EVENT", {…}]` — ingest the event into ndb, reply with `OK`.
//! - `["REQ", <sub>, <filter>…]` — replay stored matches, send `EOSE`, then
//!   live-stream newly ingested matches until the subscription is closed.
//! - `["CLOSE", <sub>]` — stop a live subscription.
//!
//! It also speaks [NIP-77] negentropy as the responder, so clients can
//! reconcile their set against the relay's in O(difference) rather than
//! re-downloading everything:
//!
//! - `["NEG-OPEN", <sub>, <filter>, <msg-hex>]` — start a reconciliation over a
//!   filter's matches; reply with `NEG-MSG`.
//! - `["NEG-MSG", <sub>, <msg-hex>]` — one reconciliation round; reply with
//!   `NEG-MSG`.
//! - `["NEG-CLOSE", <sub>]` — end a reconciliation.
//!
//! There is deliberately no NIP-11 or NIP-42 auth. Access control is "bind to
//! localhost" — this is a dogfooding port, not a public relay.
//!
//! [NIP-01]: https://github.com/nostr-protocol/nips/blob/master/01.md
//! [NIP-77]: https://github.com/nostr-protocol/nips/blob/master/77.md

use std::collections::HashMap;
use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostrdb::{Filter, Ndb, SubscriptionStream, Transaction};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A negentropy reconciliation in progress. The storage is owned, so the
/// instance is `'static` and can live in the connection's session map across
/// `NEG-MSG` rounds. Each round reuses the same sealed item set.
type NegSession = Negentropy<'static, NegentropyStorageVector>;

/// How many stored events a single `REQ` replays before `EOSE`.
const STORED_QUERY_LIMIT: i32 = 500;
/// How many freshly-ingested notes we drain per subscription wakeup.
const LIVE_BATCH: u32 = 64;
/// Upper bound on the events a single negentropy reconciliation covers. Unlike a
/// `REQ` (which pages with `EOSE`), reconciliation needs the *whole* matching set
/// at once, so this is set high; a board larger than this would reconcile only a
/// truncated prefix.
const NEG_QUERY_LIMIT: i32 = 100_000;
/// Negentropy frame size limit (`0` = unlimited). Localhost + small boards, so we
/// don't bound per-message size; the protocol still recurses across rounds.
const NEG_FRAME_LIMIT: u64 = 0;

/// A running relay. Dropping the handle (or calling [`shutdown`](Self::shutdown))
/// stops the accept loop; in-flight connection tasks then wind down on their own.
pub struct RelayHandle {
    local_addr: SocketAddr,
    shutdown: watch::Sender<bool>,
}

impl RelayHandle {
    /// The address the relay actually bound to (useful when binding to port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The `ws://` URL clients should connect to.
    pub fn url(&self) -> String {
        format!("ws://{}", self.local_addr)
    }

    /// Signal the accept loop to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Bind a NIP-01 relay to `addr` and spawn its accept loop on the current Tokio
/// runtime. Returns immediately with a [`RelayHandle`].
///
/// Binds synchronously (so a port conflict surfaces here, not in a detached
/// task) and must be called from within a Tokio runtime context.
pub fn spawn(ndb: Ndb, addr: SocketAddr) -> std::io::Result<RelayHandle> {
    let std_listener = std::net::TcpListener::bind(addr)?;
    let local_addr = std_listener.local_addr()?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener)?;

    let (shutdown, shutdown_rx) = watch::channel(false);
    tokio::spawn(accept_loop(listener, ndb, shutdown_rx));

    tracing::info!("nostrdb_relay listening on ws://{local_addr}");
    Ok(RelayHandle {
        local_addr,
        shutdown,
    })
}

async fn accept_loop(listener: TcpListener, ndb: Ndb, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                let ndb = ndb.clone();
                let shutdown_rx = shutdown_rx.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_connection(stream, ndb, shutdown_rx).await {
                        tracing::debug!("nostrdb_relay connection ended: {err}");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
}

async fn serve_connection(
    stream: TcpStream,
    ndb: Ndb,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BoxError> {
    let ws = accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Subscription tasks push frames here; the connection drains them to the
    // socket. Keeping the original `out_tx` alive means `recv()` never returns
    // `None` while the connection lives.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    // subscription id -> cancel signal for its live-streaming task.
    let mut subs: HashMap<String, oneshot::Sender<()>> = HashMap::new();
    // subscription id -> in-progress negentropy reconciliation.
    let mut neg_sessions: HashMap<String, NegSession> = HashMap::new();

    loop {
        tokio::select! {
            outgoing = out_rx.recv() => {
                if let Some(msg) = outgoing {
                    ws_tx.send(msg).await?;
                }
            }
            incoming = ws_rx.next() => {
                let Some(msg) = incoming else { break };
                match msg? {
                    Message::Text(text) => {
                        handle_client_frame(&text, &ndb, &out_tx, &mut subs, &mut neg_sessions);
                    }
                    Message::Ping(payload) => ws_tx.send(Message::Pong(payload)).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }

    // Dropping the cancel senders stops every live subscription task, which then
    // unsubscribes from ndb.
    subs.clear();
    Ok(())
}

/// Parse and act on one client text frame. Errors are reported to the client as
/// `NOTICE` rather than dropping the connection.
fn handle_client_frame(
    text: &str,
    ndb: &Ndb,
    out_tx: &mpsc::UnboundedSender<Message>,
    subs: &mut HashMap<String, oneshot::Sender<()>>,
    neg_sessions: &mut HashMap<String, NegSession>,
) {
    let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(text) else {
        let _ = out_tx.send(notice("could not parse message"));
        return;
    };

    match frame.first().and_then(Value::as_str) {
        Some("EVENT") => handle_event(text, &frame, ndb, out_tx),
        Some("REQ") => handle_req(&frame, ndb, out_tx, subs),
        Some("CLOSE") => {
            if let Some(sub_id) = frame.get(1).and_then(Value::as_str) {
                subs.remove(sub_id);
            }
        }
        Some("NEG-OPEN") => handle_neg_open(&frame, ndb, out_tx, neg_sessions),
        Some("NEG-MSG") => handle_neg_msg(&frame, out_tx, neg_sessions),
        Some("NEG-CLOSE") => {
            if let Some(sub_id) = frame.get(1).and_then(Value::as_str) {
                neg_sessions.remove(sub_id);
            }
        }
        _ => {
            let _ = out_tx.send(notice("unrecognized message"));
        }
    }
}

fn handle_event(text: &str, frame: &[Value], ndb: &Ndb, out_tx: &mpsc::UnboundedSender<Message>) {
    let event_id = frame
        .get(1)
        .and_then(|e| e.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // The client frame is already `["EVENT", {…}]`, exactly what
    // `process_client_event` expects, so we hand it the raw text verbatim.
    match ndb.process_client_event(text) {
        Ok(()) => {
            let _ = out_tx.send(ok(event_id, true, ""));
        }
        Err(err) => {
            let _ = out_tx.send(ok(event_id, false, &format!("error: {err}")));
        }
    }
}

fn handle_req(
    frame: &[Value],
    ndb: &Ndb,
    out_tx: &mpsc::UnboundedSender<Message>,
    subs: &mut HashMap<String, oneshot::Sender<()>>,
) {
    let Some(sub_id) = frame.get(1).and_then(Value::as_str) else {
        let _ = out_tx.send(notice("REQ missing subscription id"));
        return;
    };
    let sub_id = sub_id.to_owned();

    let filters: Vec<Filter> = frame[2..]
        .iter()
        .filter_map(|f| Filter::from_json(&f.to_string()).ok())
        .collect();

    // Stored phase, run synchronously here: everything already in ndb that
    // matches, then EOSE. Doing it before we spawn keeps the non-`Send` `Filter`
    // and `Transaction` off the awaiting task entirely.
    if let Ok(txn) = Transaction::new(ndb)
        && let Ok(results) = ndb.query(&txn, &filters, STORED_QUERY_LIMIT)
    {
        for result in results {
            if let Ok(note_json) = result.note.json()
                && out_tx.send(event(&sub_id, &note_json)).is_err()
            {
                return;
            }
        }
    }
    if out_tx.send(eose(&sub_id)).is_err() {
        return;
    }

    // Live phase: a fresh subscription only reports future ingests. Subscribe
    // here (still synchronous) so the spawned task captures only `Send` values.
    let Ok(sub) = ndb.subscribe(&filters) else {
        return;
    };

    // A re-REQ of an existing id replaces the old subscription: inserting drops
    // the previous cancel sender, which stops the old streaming task.
    let (cancel_tx, cancel_rx) = oneshot::channel();
    subs.insert(sub_id.clone(), cancel_tx);

    tokio::spawn(stream_subscription(
        ndb.clone(),
        sub,
        sub_id,
        out_tx.clone(),
        cancel_rx,
    ));
}

/// Live-stream newly ingested matches for one subscription until it's cancelled
/// (CLOSE, re-REQ, or connection drop) or the client's outgoing channel closes.
/// Captures only `Send` values so it can live on a spawned task.
///
/// Holds a single long-lived [`SubscriptionStream`] for the subscription's whole
/// life and polls it in the loop. Dropping the stream on exit unsubscribes from
/// ndb. (The deprecated `wait_for_notes` can't be used here: it builds a stream,
/// awaits one batch, then drops it — unsubscribing after the first note.)
async fn stream_subscription(
    ndb: Ndb,
    sub: nostrdb::Subscription,
    sub_id: String,
    out_tx: mpsc::UnboundedSender<Message>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    let mut stream = SubscriptionStream::new(ndb.clone(), sub).notes_per_await(LIVE_BATCH);
    loop {
        tokio::select! {
            _ = &mut cancel_rx => break,
            next = stream.next() => {
                let Some(keys) = next else { break };
                let Ok(txn) = Transaction::new(&ndb) else { break };
                for key in keys {
                    if let Ok(note) = ndb.get_note_by_key(&txn, key)
                        && let Ok(note_json) = note.json()
                        && out_tx.send(event(&sub_id, &note_json)).is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}

/// Start a negentropy reconciliation: `["NEG-OPEN", <sub>, <filter>, <msg-hex>]`.
///
/// We're the responder. Build the reconciliation set from everything in ndb that
/// matches `filter`, run the client's opening message through it, and reply with
/// our `NEG-MSG`. The session is kept so subsequent `NEG-MSG` rounds reuse the
/// same item set. A re-`NEG-OPEN` of a live id replaces the prior session.
fn handle_neg_open(
    frame: &[Value],
    ndb: &Ndb,
    out_tx: &mpsc::UnboundedSender<Message>,
    neg_sessions: &mut HashMap<String, NegSession>,
) {
    let Some(sub_id) = frame.get(1).and_then(Value::as_str) else {
        let _ = out_tx.send(notice("NEG-OPEN missing subscription id"));
        return;
    };

    let Some(filter_json) = frame.get(2).map(Value::to_string) else {
        let _ = out_tx.send(neg_err(sub_id, "NEG-OPEN missing filter"));
        return;
    };
    let Ok(filter) = Filter::from_json(&filter_json) else {
        let _ = out_tx.send(neg_err(sub_id, "NEG-OPEN filter is invalid"));
        return;
    };

    let Some(Ok(query)) = frame.get(3).and_then(Value::as_str).map(hex::decode) else {
        let _ = out_tx.send(neg_err(sub_id, "NEG-OPEN message is not valid hex"));
        return;
    };

    let mut session = match build_neg_session(ndb, filter) {
        Ok(session) => session,
        Err(err) => {
            let _ = out_tx.send(neg_err(sub_id, &format!("could not build set: {err}")));
            return;
        }
    };

    match session.reconcile(&query) {
        Ok(reply) => {
            let _ = out_tx.send(neg_msg(sub_id, &reply));
            neg_sessions.insert(sub_id.to_owned(), session);
        }
        Err(err) => {
            let _ = out_tx.send(neg_err(sub_id, &format!("reconcile failed: {err}")));
        }
    }
}

/// One reconciliation round: `["NEG-MSG", <sub>, <msg-hex>]`. Looks up the open
/// session, folds the client's message in, and replies with the next `NEG-MSG`.
fn handle_neg_msg(
    frame: &[Value],
    out_tx: &mpsc::UnboundedSender<Message>,
    neg_sessions: &mut HashMap<String, NegSession>,
) {
    let Some(sub_id) = frame.get(1).and_then(Value::as_str) else {
        let _ = out_tx.send(notice("NEG-MSG missing subscription id"));
        return;
    };
    let Some(session) = neg_sessions.get_mut(sub_id) else {
        let _ = out_tx.send(neg_err(sub_id, "no open negentropy session for this id"));
        return;
    };
    let Some(Ok(query)) = frame.get(2).and_then(Value::as_str).map(hex::decode) else {
        let _ = out_tx.send(neg_err(sub_id, "NEG-MSG message is not valid hex"));
        return;
    };

    match session.reconcile(&query) {
        Ok(reply) => {
            let _ = out_tx.send(neg_msg(sub_id, &reply));
        }
        Err(err) => {
            let _ = out_tx.send(neg_err(sub_id, &format!("reconcile failed: {err}")));
            neg_sessions.remove(sub_id);
        }
    }
}

/// Build a sealed negentropy reconciliation set from every event in ndb matching
/// `filter`, keyed by `(created_at, id)` as the protocol requires.
fn build_neg_session(ndb: &Ndb, filter: Filter) -> Result<NegSession, BoxError> {
    let txn = Transaction::new(ndb)?;
    let results = ndb.query(&txn, &[filter], NEG_QUERY_LIMIT)?;

    let mut storage = NegentropyStorageVector::with_capacity(results.len());
    for result in results {
        storage.insert(
            result.note.created_at(),
            Id::from_byte_array(*result.note.id()),
        )?;
    }
    storage.seal()?;

    Ok(Negentropy::owned(storage, NEG_FRAME_LIMIT)?)
}

fn ok(event_id: &str, status: bool, message: &str) -> Message {
    Message::Text(json!(["OK", event_id, status, message]).to_string())
}

/// `["NEG-MSG", <sub>, <msg-hex>]` — one reconciliation round back to the client.
fn neg_msg(sub_id: &str, msg: &[u8]) -> Message {
    Message::Text(json!(["NEG-MSG", sub_id, hex::encode(msg)]).to_string())
}

/// `["NEG-ERR", <sub>, <reason>]` — abort a reconciliation with a reason.
fn neg_err(sub_id: &str, reason: &str) -> Message {
    Message::Text(json!(["NEG-ERR", sub_id, reason]).to_string())
}

fn eose(sub_id: &str) -> Message {
    Message::Text(json!(["EOSE", sub_id]).to_string())
}

fn notice(message: &str) -> Message {
    Message::Text(json!(["NOTICE", message]).to_string())
}

/// `["EVENT", <sub>, <note>]`. The note is already serialized JSON, so we splice
/// it in rather than parse-and-reserialize.
fn event(sub_id: &str, note_json: &str) -> Message {
    Message::Text(format!(r#"["EVENT",{},{}]"#, json!(sub_id), note_json))
}
