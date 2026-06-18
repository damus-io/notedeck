//! Minimal async NIP-01 + NIP-77 client for a running notedeck's embedded relay.
//!
//! Mirrors [`headway::store`]'s `Publisher` seam, plus a reconciliation step:
//! - [`Relay::reconcile`] runs a NIP-77 negentropy session as the initiator to
//!   learn the set difference both ways — which events the relay has that we
//!   don't, and which we have that it doesn't — without transferring the board.
//! - [`Relay::sync_into`] `REQ`s events (by id, after reconcile) and processes
//!   each stored one into a local nostrdb, so the CLI can fold the board locally.
//! - [`Relay::publish`] forwards the `["EVENT", {...}]` frames produced by an
//!   edit (or surfaced by reconcile) back to the relay, so the app sees them.

use enostr::NoteId;
use futures_util::{SinkExt, StreamExt};
use negentropy::{Negentropy, NegentropyStorageVector};
use nostrdb::Ndb;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::Result;

/// The subscription id used for the one-shot board sync and reconciliation.
const SUB: &str = "headway";

/// The set difference a [`Relay::reconcile`] uncovers, as raw event ids.
pub struct Diff {
    /// Ids the relay holds that the local cache is missing — pull these down.
    pub need: Vec<[u8; 32]>,
    /// Ids the local cache holds that the relay is missing — push these up.
    pub have: Vec<[u8; 32]>,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Relay {
    ws: Ws,
    url: String,
}

impl Relay {
    pub async fn connect(url: &str) -> Result<Self> {
        let (ws, _resp) = connect_async(url)
            .await
            .map_err(|e| format!("connecting to {url}: {e}"))?;
        Ok(Relay {
            ws,
            url: url.to_string(),
        })
    }

    /// Run a NIP-77 negentropy reconciliation as the initiator over the events
    /// matching `filter_json`. `storage` is the sealed local set `(created_at,
    /// id)`. Returns the [`Diff`]: the ids each side is missing, computed in
    /// O(difference) without transferring the matching events themselves.
    pub async fn reconcile(
        &mut self,
        filter_json: &str,
        storage: NegentropyStorageVector,
    ) -> Result<Diff> {
        // frame_size_limit 0 = unlimited; the board is small and this is
        // localhost, so we don't bound per-message size.
        let mut neg = Negentropy::owned(storage, 0)?;
        let initial = neg.initiate()?;
        self.ws
            .send(Message::Text(format!(
                r#"["NEG-OPEN","{SUB}",{filter_json},"{}"]"#,
                hex::encode(&initial)
            )))
            .await?;

        let mut diff = Diff {
            need: Vec::new(),
            have: Vec::new(),
        };
        loop {
            let text = self.next_text().await?;
            let frame: Vec<Value> = serde_json::from_str(&text)?;
            match frame.first().and_then(Value::as_str) {
                Some("NEG-MSG") => {
                    let msg = frame
                        .get(2)
                        .and_then(Value::as_str)
                        .ok_or("NEG-MSG missing payload")?;
                    let msg = hex::decode(msg).map_err(|e| format!("bad NEG-MSG hex: {e}"))?;

                    // `have`/`need` are from our (the initiator's) perspective:
                    // have = we hold, relay lacks; need = relay holds, we lack.
                    let mut have = Vec::new();
                    let mut need = Vec::new();
                    let reply = neg.reconcile_with_ids(&msg, &mut have, &mut need)?;
                    diff.have.extend(have.iter().map(|id| *id.as_bytes()));
                    diff.need.extend(need.iter().map(|id| *id.as_bytes()));

                    match reply {
                        // More ranges to probe — answer and keep going.
                        Some(reply) => {
                            self.ws
                                .send(Message::Text(format!(
                                    r#"["NEG-MSG","{SUB}","{}"]"#,
                                    hex::encode(&reply)
                                )))
                                .await?;
                        }
                        // Nothing left to ask: reconciliation is complete.
                        None => break,
                    }
                }
                Some("NEG-ERR") => {
                    let reason = frame.get(2).and_then(Value::as_str).unwrap_or("");
                    return Err(format!("relay refused reconciliation: {reason}").into());
                }
                _ => {}
            }
        }
        self.ws
            .send(Message::Text(format!(r#"["NEG-CLOSE","{SUB}"]"#)))
            .await?;
        Ok(diff)
    }

    /// `REQ` `filter_json`, processing every stored EVENT into `ndb`. Returns the
    /// ids of the events received (so the caller can wait for the async ingest to
    /// land) once the relay sends EOSE.
    pub async fn sync_into(&mut self, ndb: &Ndb, filter_json: &str) -> Result<Vec<[u8; 32]>> {
        self.ws
            .send(Message::Text(format!(r#"["REQ","{SUB}",{filter_json}]"#)))
            .await?;

        let mut received = Vec::new();
        loop {
            let text = self.next_text().await?;
            let frame: Vec<Value> = serde_json::from_str(&text)?;
            match frame.first().and_then(Value::as_str) {
                // Relay form is ["EVENT", <sub>, <note>]; hand nostrdb the whole
                // message verbatim so it ingests the note as received.
                Some("EVENT") => {
                    ndb.process_event(&text)
                        .map_err(|e| format!("ingesting event: {e}"))?;
                    if let Some(id) = frame
                        .get(2)
                        .and_then(|note| note.get("id"))
                        .and_then(Value::as_str)
                        .and_then(|hex| NoteId::from_hex(hex).ok())
                    {
                        received.push(*id.bytes());
                    }
                }
                Some("EOSE") => break,
                Some("CLOSED") => return Err(format!("relay closed subscription: {text}").into()),
                _ => {}
            }
        }
        self.ws
            .send(Message::Text(format!(r#"["CLOSE","{SUB}"]"#)))
            .await?;
        Ok(received)
    }

    /// Send each `["EVENT", {...}]` frame and wait for its `OK`, erroring if the
    /// relay rejects one.
    pub async fn publish(&mut self, frames: &[String]) -> Result<()> {
        for frame in frames {
            self.ws.send(Message::Text(frame.clone())).await?;
        }
        let mut acked = 0;
        while acked < frames.len() {
            let text = self.next_text().await?;
            let frame: Vec<Value> = serde_json::from_str(&text)?;
            if frame.first().and_then(Value::as_str) == Some("OK") {
                let accepted = frame.get(2).and_then(Value::as_bool).unwrap_or(false);
                if !accepted {
                    let reason = frame.get(3).and_then(Value::as_str).unwrap_or("");
                    return Err(format!("relay rejected event: {reason}").into());
                }
                acked += 1;
            }
        }
        Ok(())
    }

    /// Await the next text frame, skipping pings/binary.
    async fn next_text(&mut self) -> Result<String> {
        loop {
            let msg = self
                .ws
                .next()
                .await
                .ok_or_else(|| format!("{} closed the connection", self.url))??;
            if let Message::Text(text) = msg {
                return Ok(text);
            }
        }
    }
}
