use crate::{
    relay::{
        ws::{self, WsMessage, WsReceiver, WsSender},
        RelayStatus,
    },
    ClientMessage, Error, Result, WebSocketError,
};
use std::{
    fmt,
    hash::{Hash, Hasher},
};
use tracing::{debug, error};

/// One outbound websocket connection owned by the outbox service.
pub struct WebsocketConn {
    pub url: nostr::RelayUrl,
    pub status: RelayStatus,
    pub sender: WsSender,
    pub receiver: WsReceiver,
    /// Monotonic identifier for the current sender/receiver websocket leg.
    send_generation: u64,
}

impl fmt::Debug for WebsocketConn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("url", &self.url)
            .field("status", &self.status)
            .finish()
    }
}

impl Hash for WebsocketConn {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.url.hash(state);
    }
}

impl PartialEq for WebsocketConn {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
    }
}

impl Eq for WebsocketConn {}

impl WebsocketConn {
    pub fn new(url: nostr::RelayUrl, wakeup: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        require_tokio_websocket_runtime()?;

        let ws::WsChannels { sender, receiver } = ws::connect(url.as_str(), wakeup)?;
        Ok(Self {
            url,
            status: RelayStatus::Connecting,
            sender,
            receiver,
            send_generation: 0,
        })
    }

    #[profiling::function]
    pub fn send(&mut self, msg: &ClientMessage) {
        let json = match msg.to_json() {
            Ok(json) => {
                debug!("sending {} to {}", json, self.url);
                json
            }
            Err(err) => {
                error!("error serializing json for filter: {err}");
                return;
            }
        };

        self.sender.send(WsMessage::Text(json));
    }

    pub(crate) fn set_send_generation(&mut self, send_generation: u64) {
        self.send_generation = send_generation;
    }

    pub fn ping(&mut self) {
        self.sender.send(WsMessage::Ping(Vec::new()));
    }

    pub fn set_status(&mut self, status: RelayStatus) {
        self.status = status;
    }
}

fn require_tokio_websocket_runtime() -> Result<()> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(Error::WebSocket(WebSocketError::new(
            "tokio runtime unavailable for websocket connection",
        )));
    }

    Ok(())
}
