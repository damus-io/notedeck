use crate::{relay::RelayStatus, ClientMessage, Error, Result, WebSocketError};

use std::{
    fmt,
    hash::{Hash, Hasher},
    ops::ControlFlow,
    sync::mpsc,
};

use ewebsock::{Options, WsEvent, WsMessage, WsSender};
use tracing::{debug, error};

/// WebsocketConn owns an outbound websocket connection to a relay.
pub struct WebsocketConn {
    pub url: nostr::RelayUrl,
    pub status: RelayStatus,
    pub sender: WsSender,
    pub receiver: WebsocketReceiver,
    /// Monotonic identifier for the current sender/receiver websocket leg.
    send_generation: u64,
}

/// Receiver for one websocket leg's inbound events.
///
/// This mirrors ewebsock's receiver shape but lets enostr enqueue the event
/// before waking the outbox service. The service can then drain the current
/// per-connection queue without racing an empty wake.
pub struct WebsocketReceiver {
    rx: mpsc::Receiver<WsEvent>,
}

impl WebsocketReceiver {
    fn connect(url: &str, wakeup: impl Fn() + Send + Sync + 'static) -> Result<(WsSender, Self)> {
        let (tx, rx) = mpsc::channel();
        let on_event = Box::new(move |event| {
            if tx.send(event).is_err() {
                return ControlFlow::Break(());
            }
            wakeup();
            ControlFlow::Continue(())
        });
        let sender = ewebsock::ws_connect(url.to_owned(), Options::default(), on_event)
            .map_err(|err| Error::WebSocket(WebSocketError::from(err)))?;
        Ok((sender, Self { rx }))
    }

    pub fn try_recv(&self) -> Option<WsEvent> {
        self.rx.try_recv().ok()
    }
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
        // Hashes the Relay by hashing the URL
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

        let status = RelayStatus::Connecting;
        let (sender, receiver) = WebsocketReceiver::connect(url.as_str(), wakeup)?;

        Ok(Self {
            url,
            sender,
            receiver,
            status,
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
            Err(e) => {
                error!("error serializing json for filter: {e}");
                return;
            }
        };

        let txt = WsMessage::Text(json);
        self.sender.send(txt);
    }

    pub(crate) fn set_send_generation(&mut self, send_generation: u64) {
        self.send_generation = send_generation;
    }

    pub fn ping(&mut self) {
        let msg = WsMessage::Ping(vec![]);
        self.sender.send(msg);
    }

    pub fn set_status(&mut self, status: RelayStatus) {
        self.status = status;
    }
}

fn require_tokio_websocket_runtime() -> Result<()> {
    #[cfg(feature = "tokio-websocket")]
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(Error::WebSocket(WebSocketError::new(
            "tokio runtime unavailable for websocket connection",
        )));
    }

    Ok(())
}
