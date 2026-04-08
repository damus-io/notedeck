use crate::{relay::backoff, relay::RelayStatus, ClientMessage, Result, Wakeup};

use std::{
    fmt,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use ewebsock::{Options, WsMessage, WsReceiver, WsSender};
use tracing::{debug, error};

const MAX_BOOTSTRAP_RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

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
    pub last_pong: Instant,
    pub last_connect_attempt: Instant,
    pub retry_connect_after: Duration,
    /// Number of consecutive failed reconnect attempts. Reset to 0 on successful connection.
    pub reconnect_attempt: u32,
}

impl WebsocketRelay {
    pub fn new(relay: WebsocketConn) -> Self {
        let now = Instant::now();
        Self {
            conn: relay,
            last_ping: now,
            last_pong: now,
            last_connect_attempt: now,
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

    /// Enters the connected state for a fresh websocket leg, refreshing
    /// liveness tracking and reconnect metadata.
    pub fn set_connected(&mut self, reconnect_delay: Duration) {
        self.conn.status = RelayStatus::Connected;
        self.last_pong = Instant::now();
        self.reconnect_attempt = 0;
        self.retry_connect_after = reconnect_delay;
    }

    /// Enters the disconnected state and starts reconnect timing from the
    /// moment the live websocket leg ended.
    pub fn set_disconnected_now(&mut self) {
        self.conn.status = RelayStatus::Disconnected;
        self.last_connect_attempt = Instant::now();
    }
}

/// Owns websocket presence and bootstrap-retry state.
pub struct WebsocketSlot {
    relay: Option<WebsocketRelay>,
    restore_attempt: u32,
    retry_after: Duration,
    last_attempt: Instant,
}

impl WebsocketSlot {
    /// Creates a websocket slot and attempts an initial bootstrap connection.
    pub fn from_wakeup<W>(url: nostr::RelayUrl, wakeup: W) -> Self
    where
        W: Wakeup,
    {
        let now = Instant::now();
        let relay = match WebsocketConn::from_wakeup(url.clone(), wakeup) {
            Ok(conn) => Some(WebsocketRelay::new(conn)),
            Err(err) => {
                tracing::error!("could not open websocket to {url:?}: {err}");
                None
            }
        };

        Self {
            relay,
            restore_attempt: 0,
            retry_after: WebsocketRelay::initial_reconnect_duration(),
            last_attempt: now,
        }
    }

    pub fn as_ref(&self) -> Option<&WebsocketRelay> {
        self.relay.as_ref()
    }

    pub fn as_mut(&mut self) -> Option<&mut WebsocketRelay> {
        self.relay.as_mut()
    }

    fn should_attempt_restore(&self, now: Instant) -> bool {
        now > self.last_attempt + self.retry_after
    }

    fn note_restore_failure(&mut self, now: Instant, url: &nostr::RelayUrl) {
        self.last_attempt = now;
        self.restore_attempt = self.restore_attempt.saturating_add(1);
        let seed = backoff::jitter_seed(url, self.restore_attempt);
        self.retry_after =
            backoff::next_duration(self.restore_attempt, seed, MAX_BOOTSTRAP_RETRY_AFTER);
    }

    fn note_restore_success(&mut self, now: Instant, conn: WebsocketConn) {
        self.relay = Some(WebsocketRelay::new(conn));
        self.restore_attempt = 0;
        self.last_attempt = now;
        self.retry_after = WebsocketRelay::initial_reconnect_duration();
    }

    /// Attempts to restore a missing websocket using a `Wakeup` implementation.
    pub fn try_restore_with_wakeup<W>(
        &mut self,
        url: nostr::RelayUrl,
        wakeup: W,
        force: bool,
    ) -> bool
    where
        W: Wakeup,
    {
        self.try_restore_inner(url.clone(), force, || {
            WebsocketConn::from_wakeup(url, wakeup)
        })
    }

    /// Attempts to restore a missing websocket using a closure wakeup callback.
    pub fn try_restore_with_fn(
        &mut self,
        url: nostr::RelayUrl,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
        force: bool,
    ) -> bool {
        self.try_restore_inner(url.clone(), force, || WebsocketConn::new(url, wakeup))
    }

    fn try_restore_inner(
        &mut self,
        url: nostr::RelayUrl,
        force: bool,
        connect: impl FnOnce() -> Result<WebsocketConn>,
    ) -> bool {
        if self.relay.is_some() {
            return true;
        }

        let now = Instant::now();
        if !force && !self.should_attempt_restore(now) {
            return false;
        }

        match connect() {
            Ok(conn) => {
                self.note_restore_success(now, conn);
                tracing::info!("restored websocket for relay {url}");
                true
            }
            Err(err) => {
                self.note_restore_failure(now, &url);
                tracing::warn!("failed to restore websocket for relay {url}: {err}");
                false
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_for_test(&mut self) {
        self.relay = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{WebsocketConn, WebsocketRelay};
    use crate::{relay::test_utils::MockWakeup, RelayStatus};
    use std::time::{Duration, Instant};

    #[test]
    fn set_connected_refreshes_liveness_and_configured_reconnect_delay() {
        let mut websocket = WebsocketRelay::new(
            WebsocketConn::from_wakeup(
                nostr::RelayUrl::parse("wss://relay-websocket-open.example.com").unwrap(),
                MockWakeup::default(),
            )
            .unwrap(),
        );
        websocket.conn.status = RelayStatus::Disconnected;
        websocket.last_pong = Instant::now() - Duration::from_secs(5);
        websocket.reconnect_attempt = 3;
        let before = websocket.last_pong;
        let configured_delay = Duration::from_millis(30);

        websocket.set_connected(configured_delay);

        assert_eq!(websocket.conn.status, RelayStatus::Connected);
        assert!(websocket.last_pong > before);
        assert_eq!(websocket.reconnect_attempt, 0);
        assert_eq!(websocket.retry_connect_after, configured_delay);
    }

    #[test]
    fn set_disconnected_now_starts_reconnect_delay_without_resetting_backoff() {
        let mut websocket = WebsocketRelay::new(
            WebsocketConn::from_wakeup(
                nostr::RelayUrl::parse("wss://relay-websocket-close.example.com").unwrap(),
                MockWakeup::default(),
            )
            .unwrap(),
        );
        websocket.conn.status = RelayStatus::Connected;
        websocket.last_connect_attempt = Instant::now() - Duration::from_secs(5);
        websocket.retry_connect_after = Duration::from_millis(45);
        websocket.reconnect_attempt = 3;
        let before = websocket.last_connect_attempt;

        websocket.set_disconnected_now();

        assert_eq!(websocket.conn.status, RelayStatus::Disconnected);
        assert!(websocket.last_connect_attempt > before);
        assert_eq!(websocket.retry_connect_after, Duration::from_millis(45));
        assert_eq!(websocket.reconnect_attempt, 3);
    }
}
