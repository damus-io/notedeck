use crate::{relay::RelayStatus, ClientMessage, Result, Wakeup};

use std::{
    fmt,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use ewebsock::{Options, WsMessage, WsReceiver, WsSender};
use tracing::{debug, error};

/// WebsocketConn owns an outbound websocket connection to a relay.
pub struct WebsocketConn {
    pub url: nostr::RelayUrl,
    pub status: RelayStatus,
    pub sender: WsSender,
    pub receiver: WsReceiver,
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
    pub fn new(
        url: nostr::RelayUrl,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Result<Self> {
        #[derive(Clone)]
        struct TmpWakeup<W>(W);

        impl<W> Wakeup for TmpWakeup<W>
        where
            W: Fn() + Send + Sync + Clone + 'static,
        {
            fn wake(&self) {
                (self.0)()
            }
        }

        WebsocketConn::from_wakeup(url, TmpWakeup(wakeup))
    }

    pub fn from_wakeup<W>(url: nostr::RelayUrl, wakeup: W) -> Result<Self>
    where
        W: Wakeup,
    {
        let status = RelayStatus::Connecting;
        let wake = wakeup;
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(url.as_str(), Options::default(), move || wake.wake())?;

        Ok(Self {
            url,
            sender,
            receiver,
            status,
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

    pub fn connect(&mut self, wakeup: impl Fn() + Send + Sync + 'static) -> Result<()> {
        let (sender, receiver) =
            ewebsock::connect_with_wakeup(self.url.as_str(), Options::default(), wakeup)?;
        self.status = RelayStatus::Connecting;
        self.sender = sender;
        self.receiver = receiver;
        Ok(())
    }

    pub fn ping(&mut self) {
        let msg = WsMessage::Ping(vec![]);
        self.sender.send(msg);
    }

    pub fn set_status(&mut self, status: RelayStatus) {
        self.status = status;
    }
}

/// WebsocketRelay wraps WebsocketConn with reconnect/keepalive metadata.
pub struct WebsocketRelay {
    pub conn: WebsocketConn,
    pub last_ping: Instant,
    pub last_connect_attempt: Instant,
    pub retry_connect_after: Duration,
    /// Number of consecutive failed reconnect attempts. Reset to 0 on successful connection.
    pub reconnect_attempt: u32,
}

impl WebsocketRelay {
    pub fn new(relay: WebsocketConn) -> Self {
        Self {
            conn: relay,
            last_ping: Instant::now(),
            last_connect_attempt: Instant::now(),
            retry_connect_after: Self::initial_reconnect_duration(),
            reconnect_attempt: 0,
        }
    }

    pub fn initial_reconnect_duration() -> Duration {
        Duration::from_secs(5)
    }

    pub fn is_connected(&self) -> bool {
        self.conn.status == RelayStatus::Connected
    }
}
