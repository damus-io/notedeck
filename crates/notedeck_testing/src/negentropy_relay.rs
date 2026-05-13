//! Local test relay that proxies ordinary Nostr traffic to `LocalRelay`
//! while answering `NEG-*` messages itself.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostr::{ClientMessage, Filter, JsonUtil};
use nostr_relay_builder::{
    prelude::{MemoryDatabase, MemoryDatabaseOptions, NostrEventsDatabase},
    LocalRelay, RelayBuilder,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::broadcast};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

use crate::{
    stepping::{assert_device_condition_stable, wait_for_device_condition},
    DeviceHarness,
};

type ProxyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type OwnedNegentropy = Negentropy<'static, NegentropyStorageVector>;

/// Failure injection modes for the negentropy proxy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NegentropyRelayMode {
    /// Normal successful negentropy behavior.
    #[default]
    Normal,
    /// Reply to every `NEG-OPEN` with one `NEG-ERR`.
    NegErrOnOpen(String),
    /// Reply to only the first `NEG-OPEN` with one `NEG-ERR`, then behave
    /// normally on later attempts.
    NegErrOnOpenOnce(String),
    /// Accept `NEG-OPEN` but never answer, forcing the real timeout path.
    SilentOnOpen,
    /// Drop the first `NEG-OPEN` connection before replying, then behave
    /// normally on later attempts.
    DisconnectOnOpenOnce,
}

#[derive(Clone, Default)]
struct RelayProbe {
    captured_text: Arc<Mutex<Vec<String>>>,
    captured_neg_opens: Arc<Mutex<Vec<(String, Filter)>>>,
    did_disconnect_once: Arc<AtomicBool>,
    did_neg_err_once: Arc<AtomicBool>,
}

impl RelayProbe {
    fn capture(&self, text: impl Into<String>) {
        self.captured_text
            .lock()
            .expect("lock negentropy relay capture")
            .push(text.into());
    }

    fn capture_neg_open(&self, subscription_id: String, filter: Filter) {
        self.captured_neg_opens
            .lock()
            .expect("lock negentropy relay capture")
            .push((subscription_id, filter));
    }

    fn captured_neg_opens(&self) -> Vec<(String, Filter)> {
        self.captured_neg_opens
            .lock()
            .expect("lock negentropy relay capture")
            .clone()
    }

    fn captured_text(&self) -> Vec<String> {
        self.captured_text
            .lock()
            .expect("lock negentropy relay capture")
            .clone()
    }

    fn count_prefix(&self, prefix: &str) -> usize {
        self.captured_text
            .lock()
            .expect("lock negentropy relay capture")
            .iter()
            .filter(|text| text.starts_with(prefix))
            .count()
    }
}

/// Local relay plus a websocket proxy that implements NIP-77 `NEG-*` handling.
pub struct NegentropyRelay {
    url: String,
    backend: Option<LocalRelay>,
    shutdown: broadcast::Sender<()>,
    probe: RelayProbe,
}

/// In-memory relay database paired with its negentropy-capable proxy.
pub struct MemoryNegentropyRelay {
    /// Shared relay database used by test fixtures to seed remote events.
    pub db: MemoryDatabase,
    /// Client-facing relay proxy used by the app under test.
    pub relay: NegentropyRelay,
}

impl NegentropyRelay {
    /// Returns the client-facing websocket URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Count captured proxied text frames by prefix for assertions.
    pub fn count_captured_prefix(&self, prefix: &str) -> usize {
        self.probe.count_prefix(prefix)
    }

    /// Returns whether the relay captured this exact client text frame.
    pub fn has_captured_text(&self, expected: &str) -> bool {
        self.probe
            .captured_text()
            .into_iter()
            .any(|text| text == expected)
    }

    /// Returns captured `NEG-OPEN` subscription ids whose filter matches.
    pub fn captured_neg_open_session_ids(
        &self,
        mut filter_matches: impl FnMut(&Filter) -> bool,
    ) -> Vec<String> {
        self.probe
            .captured_neg_opens()
            .into_iter()
            .filter_map(|(subscription_id, filter)| {
                filter_matches(&filter).then_some(subscription_id)
            })
            .collect()
    }

    pub fn neg_open_count(&self, mut filter_matches: impl FnMut(&Filter) -> bool) -> usize {
        self.captured_neg_open_session_ids(|filter| filter_matches(filter))
            .len()
    }

    pub fn wait_for_neg_open(
        &self,
        device: &mut DeviceHarness,
        timeout: Duration,
        context: &str,
        mut filter_matches: impl FnMut(&Filter) -> bool,
    ) {
        wait_for_device_condition(device, timeout, context, |_| {
            let open_count = self.neg_open_count(|filter| filter_matches(filter));
            if open_count > 0 {
                Ok(())
            } else {
                Err(format!("captured {open_count} NEG-OPEN frames"))
            }
        });
    }

    pub fn wait_for_neg_open_session_id(
        &self,
        device: &mut DeviceHarness,
        timeout: Duration,
        context: &str,
        mut filter_matches: impl FnMut(&Filter) -> bool,
    ) -> String {
        wait_for_device_condition(device, timeout, context, |_| {
            self.captured_neg_open_session_ids(|filter| filter_matches(filter))
                .into_iter()
                .next()
                .ok_or_else(|| "captured 0 NEG-OPEN frames".to_owned())
        })
    }

    pub fn wait_for_neg_open_count(
        &self,
        device: &mut DeviceHarness,
        expected_count: usize,
        timeout: Duration,
        context: &str,
        mut filter_matches: impl FnMut(&Filter) -> bool,
    ) {
        wait_for_device_condition(device, timeout, context, |_| {
            let open_count = self.neg_open_count(|filter| filter_matches(filter));
            if open_count >= expected_count {
                Ok(())
            } else {
                Err(format!(
                    "expected at least {expected_count}, captured {open_count}"
                ))
            }
        });
    }

    pub fn assert_neg_open_count_stable(
        &self,
        device: &mut DeviceHarness,
        expected_count: usize,
        frames: usize,
        context: &str,
        mut filter_matches: impl FnMut(&Filter) -> bool,
    ) {
        assert_device_condition_stable(device, frames, context, |_| {
            let open_count = self.neg_open_count(|filter| filter_matches(filter));
            if open_count == expected_count {
                Ok(())
            } else {
                Err(format!(
                    "expected NEG-OPEN count to stay at {expected_count}, found {open_count}"
                ))
            }
        });
    }
}

impl Drop for NegentropyRelay {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(backend) = &self.backend {
            backend.shutdown();
        }
    }
}

/// Starts a negentropy-capable local relay with a fresh shared memory database.
pub async fn run_memory_negentropy_relay() -> ProxyResult<MemoryNegentropyRelay> {
    run_memory_negentropy_relay_with_mode(NegentropyRelayMode::Normal).await
}

/// Starts a negentropy-capable local relay with a fresh shared memory database
/// and an explicit failure-injection mode.
pub async fn run_memory_negentropy_relay_with_mode(
    mode: NegentropyRelayMode,
) -> ProxyResult<MemoryNegentropyRelay> {
    let db = MemoryDatabase::with_opts(MemoryDatabaseOptions {
        events: true,
        ..Default::default()
    });
    let relay = run_negentropy_relay_with_mode(db.clone(), mode).await?;
    Ok(MemoryNegentropyRelay { db, relay })
}

/// Starts one negentropy-capable local relay backed by a shared memory
/// database with an explicit failure-injection mode.
async fn run_negentropy_relay_with_mode(
    relay_db: MemoryDatabase,
    mode: NegentropyRelayMode,
) -> ProxyResult<NegentropyRelay> {
    let builder = RelayBuilder::default().database(relay_db.clone());
    let backend = LocalRelay::run(builder).await?;
    let backend_url = backend.url().to_owned();

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    let url = format!("ws://{local_addr}");

    let (shutdown, mut shutdown_rx) = broadcast::channel(1);
    let listener_shutdown = shutdown.clone();
    let probe = RelayProbe::default();
    let listener_probe = probe.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    let Ok((stream, _addr)) = accept_result else {
                        continue;
                    };

                    let backend_url = backend_url.clone();
                    let relay_db = relay_db.clone();
                    let mode = mode.clone();
                    let probe = listener_probe.clone();
                    let conn_shutdown = listener_shutdown.subscribe();
                    tokio::spawn(async move {
                        if let Err(err) = dispatch_connection(
                            stream,
                            &backend_url,
                            relay_db,
                            mode,
                            probe,
                            conn_shutdown,
                        )
                        .await
                        {
                            tracing::debug!("negentropy relay proxy connection ended with error: {err}");
                        }
                    });
                }
                _ = shutdown_rx.recv() => break,
            }
        }
    });

    Ok(NegentropyRelay {
        url,
        backend: Some(backend),
        shutdown,
        probe,
    })
}

async fn dispatch_connection(
    mut stream: tokio::net::TcpStream,
    backend_url: &str,
    relay_db: MemoryDatabase,
    mode: NegentropyRelayMode,
    probe: RelayProbe,
    shutdown_rx: broadcast::Receiver<()>,
) -> ProxyResult<()> {
    let mut buf = [0u8; 1024];
    let peeked = stream.peek(&mut buf).await?;
    let request = std::str::from_utf8(&buf[..peeked]).unwrap_or_default();

    if request.starts_with("GET ") && !request.to_ascii_lowercase().contains("upgrade: websocket") {
        return respond_to_nip11_probe(&mut stream).await;
    }

    proxy_connection(stream, backend_url, relay_db, mode, probe, shutdown_rx).await
}

async fn proxy_connection(
    stream: tokio::net::TcpStream,
    backend_url: &str,
    relay_db: MemoryDatabase,
    mode: NegentropyRelayMode,
    probe: RelayProbe,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> ProxyResult<()> {
    let client_ws = accept_async(stream).await?;
    let (backend_ws, _) = connect_async(backend_url).await?;

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut backend_tx, mut backend_rx) = backend_ws.split();
    let mut sessions: HashMap<String, OwnedNegentropy> = HashMap::new();

    loop {
        tokio::select! {
            maybe_msg = client_rx.next() => {
                let Some(msg) = maybe_msg else {
                    break;
                };
                let msg = msg?;

                match msg {
                    Message::Text(text) => match handle_client_text(
                        &relay_db,
                        &mut sessions,
                        &mode,
                        &probe,
                        text.as_ref(),
                    )
                    .await? {
                        ClientTextAction::Forward => backend_tx.send(Message::Text(text)).await?,
                        ClientTextAction::Respond(Some(reply)) => {
                            client_tx.send(Message::Text(reply)).await?
                        }
                        ClientTextAction::Respond(None) => {}
                        ClientTextAction::Disconnect => break,
                    },
                    Message::Binary(bytes) => backend_tx.send(Message::Binary(bytes)).await?,
                    Message::Ping(payload) => backend_tx.send(Message::Ping(payload)).await?,
                    Message::Pong(payload) => backend_tx.send(Message::Pong(payload)).await?,
                    Message::Close(frame) => {
                        backend_tx.send(Message::Close(frame)).await?;
                        break;
                    }
                    Message::Frame(_) => {}
                }
            }
            maybe_msg = backend_rx.next() => {
                let Some(msg) = maybe_msg else {
                    break;
                };
                client_tx.send(msg?).await?;
            }
            _ = shutdown_rx.recv() => {
                let _ = client_tx.send(Message::Close(None)).await;
                let _ = backend_tx.send(Message::Close(None)).await;
                break;
            }
        }
    }

    Ok(())
}

async fn respond_to_nip11_probe(stream: &mut tokio::net::TcpStream) -> ProxyResult<()> {
    let body = r#"{"name":"notedeck-test-relay","description":"negentropy test relay"}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/nostr+json\r\ncontent-length: {}\r\naccess-control-allow-origin: *\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

enum ClientTextAction {
    Forward,
    Respond(Option<String>),
    Disconnect,
}

async fn handle_client_text(
    relay_db: &MemoryDatabase,
    sessions: &mut HashMap<String, OwnedNegentropy>,
    mode: &NegentropyRelayMode,
    probe: &RelayProbe,
    text: &str,
) -> ProxyResult<ClientTextAction> {
    probe.capture(text);

    if !text.starts_with("[\"NEG-") {
        return Ok(ClientTextAction::Forward);
    }

    match ClientMessage::from_json(text)? {
        ClientMessage::NegOpen {
            subscription_id,
            filter,
            initial_message,
            ..
        } => {
            let session_key = subscription_id.to_string();
            let filter = *filter;
            probe.capture_neg_open(session_key.clone(), filter.clone());
            tracing::debug!("proxy NEG-OPEN for session {session_key}");
            match mode {
                NegentropyRelayMode::Normal => {}
                NegentropyRelayMode::NegErrOnOpen(reason) => {
                    let reply = capture_neg_err(probe, &session_key, reason);
                    return Ok(ClientTextAction::Respond(Some(reply)));
                }
                NegentropyRelayMode::NegErrOnOpenOnce(reason) => {
                    if !probe.did_neg_err_once.swap(true, Ordering::SeqCst) {
                        let reply = capture_neg_err(probe, &session_key, reason);
                        return Ok(ClientTextAction::Respond(Some(reply)));
                    }
                }
                NegentropyRelayMode::SilentOnOpen => {
                    return Ok(ClientTextAction::Respond(None));
                }
                NegentropyRelayMode::DisconnectOnOpenOnce => {
                    if !probe.did_disconnect_once.swap(true, Ordering::SeqCst) {
                        tracing::debug!(
                            "proxy NEG-OPEN disconnecting once for session {session_key}"
                        );
                        return Ok(ClientTextAction::Disconnect);
                    }
                }
            }
            let (session, reply_hex) =
                match open_negentropy_session(relay_db, filter, &initial_message).await {
                    Ok(session) => session,
                    Err(err) => {
                        let reason = err.to_string();
                        tracing::warn!("proxy NEG-OPEN failed for session {session_key}: {reason}");
                        return Ok(ClientTextAction::Respond(Some(capture_neg_err(
                            probe,
                            &session_key,
                            &reason,
                        ))));
                    }
                };
            tracing::debug!(
                "proxy NEG-OPEN established session {session_key} with reply size {}",
                reply_hex.len()
            );
            sessions.insert(session_key.clone(), session);
            let response = neg_msg_json(&session_key, &reply_hex);
            probe.capture(response.clone());
            Ok(ClientTextAction::Respond(Some(response)))
        }
        ClientMessage::NegMsg {
            subscription_id,
            message,
        } => {
            let session_key = subscription_id.to_string();
            tracing::debug!("proxy NEG-MSG for session {session_key}");
            let response = match sessions.get_mut(&session_key) {
                Some(session) => match reconcile_server_message(session, &message) {
                    Ok(reply_hex) => {
                        tracing::debug!(
                            "proxy NEG-MSG replied for session {session_key} with size {}",
                            reply_hex.len()
                        );
                        neg_msg_json(&session_key, &reply_hex)
                    }
                    Err(err) => {
                        tracing::warn!("proxy NEG-MSG failed for session {session_key}: {err}");
                        return Ok(ClientTextAction::Respond(Some(capture_neg_err(
                            probe,
                            &session_key,
                            &err.to_string(),
                        ))));
                    }
                },
                None => {
                    tracing::warn!("proxy NEG-MSG for unknown session {session_key}");
                    return Ok(ClientTextAction::Respond(Some(capture_neg_err(
                        probe,
                        &session_key,
                        "unknown negentropy session",
                    ))));
                }
            };
            probe.capture(response.clone());
            Ok(ClientTextAction::Respond(Some(response)))
        }
        ClientMessage::NegClose { subscription_id } => {
            let session_key = subscription_id.to_string();
            tracing::debug!("proxy NEG-CLOSE for session {session_key}");
            sessions.remove(&session_key);
            Ok(ClientTextAction::Respond(None))
        }
        _ => Ok(ClientTextAction::Forward),
    }
}

async fn open_negentropy_session(
    relay_db: &MemoryDatabase,
    filter: Filter,
    initial_message_hex: &str,
) -> ProxyResult<(OwnedNegentropy, String)> {
    let storage = build_negentropy_storage(relay_db, filter).await?;
    let mut session = Negentropy::owned(storage, 0)?;
    let reply_hex = reconcile_server_message(&mut session, initial_message_hex)?;
    Ok((session, reply_hex))
}

fn reconcile_server_message(
    session: &mut OwnedNegentropy,
    message_hex: &str,
) -> ProxyResult<String> {
    let query = hex::decode(message_hex)?;
    let reply = session.reconcile(&query)?;
    Ok(hex::encode(reply))
}

async fn build_negentropy_storage(
    relay_db: &MemoryDatabase,
    filter: Filter,
) -> ProxyResult<NegentropyStorageVector> {
    let mut storage = NegentropyStorageVector::new();
    for (event_id, timestamp) in relay_db.negentropy_items(filter).await? {
        storage.insert(timestamp.as_u64(), Id::from_byte_array(event_id.to_bytes()))?;
    }
    storage.seal()?;
    Ok(storage)
}

fn neg_msg_json(subscription_id: &str, message_hex: &str) -> String {
    format!(r#"["NEG-MSG","{subscription_id}","{message_hex}"]"#)
}

fn capture_neg_err(probe: &RelayProbe, subscription_id: &str, reason: &str) -> String {
    let reply = neg_err_json(subscription_id, reason);
    probe.capture(reply.clone());
    reply
}

fn neg_err_json(subscription_id: &str, reason: &str) -> String {
    let reason = reason.replace('\\', "\\\\").replace('"', "\\\"");
    format!(r#"["NEG-ERR","{subscription_id}","{reason}"]"#)
}
