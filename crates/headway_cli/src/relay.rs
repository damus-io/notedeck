//! Minimal async NIP-01 client for a running notedeck's embedded relay.
//!
//! Two directions, mirroring [`headway::store`]'s `Publisher` seam:
//! - [`Relay::sync_into`] `REQ`s the board's events and processes every stored
//!   one into a local nostrdb, so the CLI can fold the board locally.
//! - [`Relay::publish`] forwards the `["EVENT", {...}]` frames produced by an
//!   edit back to the relay, so the running app sees the change.

use enostr::NoteId;
use futures_util::{SinkExt, StreamExt};
use nostrdb::Ndb;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::Result;

/// The subscription id used for the one-shot board sync.
const SUB: &str = "headway";

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
