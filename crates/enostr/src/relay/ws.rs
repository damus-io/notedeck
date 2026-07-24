//! Native websocket transport for relay connections.
//!
//! This owns the underlying TCP socket (via `tokio-tungstenite`) so the relay
//! stack can observe connection liveness at the socket level rather than
//! relying solely on application-level ping/pong timeouts. It exposes the same
//! poll-friendly [`WsSender`]/[`WsReceiver`] pair the coordinator drives from
//! the synchronous per-frame render loop: outbound frames are queued to a
//! background tokio task, and inbound [`WsEvent`]s are drained non-blocking via
//! [`WsReceiver::try_recv`].
//!
//! This replaces the previous `ewebsock` dependency. We deliberately do not
//! support a wasm/browser backend — notedeck ships native only, and owning the
//! socket is what makes realtime, socket-level relay status possible.

use crate::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use socket2::{SockRef, TcpKeepalive};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::{client_async_tls, MaybeTlsStream, WebSocketStream};

/// Idle time before the OS starts sending TCP keepalive probes on a relay
/// socket. Kept short so a silently-dropped connection (dead peer, NAT
/// timeout, laptop sleep) is noticed at the socket level rather than waiting
/// on the far slower application-level ping/pong timeout.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(10);
/// Gap between individual keepalive probes once idle.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(3);
/// Number of unanswered probes before the OS declares the socket dead
/// (~KEEPALIVE_IDLE + KEEPALIVE_INTERVAL * KEEPALIVE_RETRIES ≈ 19s to detect).
const KEEPALIVE_RETRIES: u32 = 3;

/// A websocket data frame exchanged with a relay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
}

/// A connection lifecycle or data event surfaced by the transport.
#[derive(Debug)]
pub enum WsEvent {
    /// The websocket handshake completed; the connection is live. Carries the
    /// local socket address, when known, so the pool can tie the connection to
    /// the network interface it is routed through.
    Opened(Option<SocketAddr>),
    /// The peer (or transport) closed the connection.
    Closed,
    /// A data frame arrived.
    Message(WsMessage),
    /// The connection failed at the socket or protocol level.
    Error(String),
}

/// Outbound half of a relay websocket connection.
///
/// Each connection leg owns exactly one sender, mirroring the previous
/// transport contract. Queued frames are drained by the background writer task.
pub struct WsSender {
    tx: UnboundedSender<WsMessage>,
}

impl WsSender {
    /// Queue one frame for the background writer task. Silently dropped if the
    /// connection task has already exited; a subsequent [`WsReceiver::try_recv`]
    /// surfaces the corresponding close/error.
    pub fn send(&mut self, msg: WsMessage) {
        let _ = self.tx.send(msg);
    }
}

/// Inbound half of a relay websocket connection, drained from the render loop.
pub struct WsReceiver {
    rx: UnboundedReceiver<WsEvent>,
}

impl WsReceiver {
    /// Non-blocking poll for the next transport event, or `None` when nothing
    /// is queued yet.
    pub fn try_recv(&mut self) -> Option<WsEvent> {
        self.rx.try_recv().ok()
    }
}

/// Open a websocket connection to `url`, returning the poll-friendly channel
/// halves immediately while the handshake proceeds on a background tokio task.
///
/// `wakeup` is invoked whenever a new [`WsEvent`] is queued so the host can
/// schedule a render-loop drain. Requires an active tokio runtime.
pub fn connect<W>(url: &str, wakeup: W) -> Result<(WsSender, WsReceiver)>
where
    W: Fn() + Send + Sync + 'static,
{
    // Validate the request synchronously so a malformed URL fails on the
    // spot, matching the previous connect contract.
    let request = url
        .into_client_request()
        .map_err(|e| Error::Generic(format!("invalid relay url {url}: {e}")))?;

    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<WsEvent>();
    let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<WsMessage>();

    tokio::spawn(run(request, out_tx, in_rx, wakeup));

    Ok((WsSender { tx: in_tx }, WsReceiver { rx: out_rx }))
}

/// Drive one connection leg to completion: perform the handshake, then pump
/// outbound frames and inbound events until either side closes or errors.
async fn run<W>(
    request: Request,
    out_tx: UnboundedSender<WsEvent>,
    mut in_rx: UnboundedReceiver<WsMessage>,
    wakeup: W,
) where
    W: Fn() + Send + Sync + 'static,
{
    // Queue an inbound event and wake the host. Returns false once the
    // receiver half has been dropped, signalling the task to wind down.
    let emit = |ev: WsEvent| {
        if out_tx.send(ev).is_err() {
            return false;
        }
        wakeup();
        true
    };

    let (stream, local_addr) = match connect_stream(request).await {
        Ok(opened) => opened,
        Err(err) => {
            emit(WsEvent::Error(err.to_string()));
            return;
        }
    };

    if !emit(WsEvent::Opened(local_addr)) {
        return;
    }

    let (mut write, mut read) = stream.split();

    loop {
        tokio::select! {
            outbound = in_rx.recv() => {
                let Some(msg) = outbound else {
                    // All senders dropped: the app tore this leg down (e.g. a
                    // reconnect). Close cleanly and exit without an event.
                    let _ = write.close().await;
                    break;
                };
                if write.send(msg.into()).await.is_err() {
                    emit(WsEvent::Closed);
                    break;
                }
            }
            inbound = read.next() => {
                match inbound {
                    Some(Ok(frame)) => {
                        if let Some(ev) = inbound_event(frame) {
                            if !emit(ev) {
                                break;
                            }
                        }
                    }
                    Some(Err(err)) => {
                        emit(WsEvent::Error(err.to_string()));
                        break;
                    }
                    None => {
                        emit(WsEvent::Closed);
                        break;
                    }
                }
            }
        }
    }
}

/// Establish the underlying TCP connection ourselves so we can enable
/// aggressive TCP keepalive, then complete the websocket (and, for `wss://`,
/// TLS) handshake over it. Owning the socket is what lets the relay stack
/// observe connection liveness at the socket level.
#[allow(clippy::type_complexity)]
async fn connect_stream(
    request: Request,
) -> Result<(
    WebSocketStream<MaybeTlsStream<TcpStream>>,
    Option<SocketAddr>,
)> {
    let uri = request.uri();
    let host = uri
        .host()
        .ok_or_else(|| Error::Generic(format!("relay url has no host: {uri}")))?
        .to_string();
    let port = uri.port_u16().unwrap_or_else(|| match uri.scheme_str() {
        Some("wss") | Some("https") => 443,
        _ => 80,
    });

    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| Error::Generic(format!("tcp connect to {host}:{port} failed: {e}")))?;

    // The local address ties this connection to the interface it is routed
    // through, so the pool can drop it the instant that interface goes away.
    let local_addr = tcp.local_addr().ok();

    // Best-effort: a socket without keepalive still works, just with slower
    // dead-connection detection, so a config failure is logged, not fatal.
    let keepalive = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    if let Err(err) = SockRef::from(&tcp).set_tcp_keepalive(&keepalive) {
        tracing::warn!("failed to enable tcp keepalive on {host}:{port}: {err}");
    }

    let (stream, _resp) = client_async_tls(request, tcp)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
    Ok((stream, local_addr))
}

/// Map an inbound tungstenite frame to a transport event.
///
/// Ping frames are answered automatically by tungstenite, so we swallow them
/// and only surface Pong (for liveness tracking), Text, and Binary payloads. A
/// Close frame maps to [`WsEvent::Closed`]; raw frames are internal and ignored.
fn inbound_event(frame: TMessage) -> Option<WsEvent> {
    match frame {
        TMessage::Text(text) => Some(WsEvent::Message(WsMessage::Text(text))),
        TMessage::Binary(bytes) => Some(WsEvent::Message(WsMessage::Binary(bytes))),
        TMessage::Pong(bytes) => Some(WsEvent::Message(WsMessage::Pong(bytes))),
        TMessage::Ping(_) => None,
        TMessage::Close(_) => Some(WsEvent::Closed),
        TMessage::Frame(_) => None,
    }
}

impl From<WsMessage> for TMessage {
    fn from(msg: WsMessage) -> TMessage {
        match msg {
            WsMessage::Text(text) => TMessage::Text(text),
            WsMessage::Binary(bytes) => TMessage::Binary(bytes),
            WsMessage::Ping(bytes) => TMessage::Ping(bytes),
            WsMessage::Pong(bytes) => TMessage::Pong(bytes),
        }
    }
}
