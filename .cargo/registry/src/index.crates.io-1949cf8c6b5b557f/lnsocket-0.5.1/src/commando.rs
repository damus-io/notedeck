//! Commando client over an `LNSocket`.
//!
//! This spawns a background **pump** task that owns the socket, demuxes replies by internal
//! request ID, and (optionally) **reconnects** and **resends** in-flight commands when the
//! underlying TCP stream breaks.
//!
//! ### Policies & timeouts
//! - Defaults (via `CommandoConfig::default()`):
//!   - `timeout`: 30s per call
//!   - `reconnect`: Auto { max_attempts: 10, base_backoff: 200ms, max_backoff: 5s }
//!   - `retry_policy`: Always { max_retries: 3 }  // ← adjust if you prefer Never by default
//! - Per-call overrides via `CallOpts` (`retry()`, `timeout()`, `rune()`).
//!
//! ### Reconnect behavior
//! - On `BrokenPipe`, pending in-flight calls are **classified** by their `RetryPolicy`:
//!   eligible ones are queued (attempts++ and their partial buffers cleared), others
//!   fail immediately with `Error::Io(BrokenPipe)`.
//! - After a successful reconnect, queued calls are **resent FIFO**. On the first resend
//!   failure, the remainder is preserved in order for the next reconnect cycle.
//!
//! ### Error model
//! - `Error::Io(io::ErrorKind)` (incl. `TimedOut`, `BrokenPipe`), `Error::Json`,
//!   `Error::Decode`, `Error::Lightning`, `Error::DnsError`, etc.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::Error;
use crate::LNSocket;
use crate::RpcError;
use crate::ln::msgs;
use crate::ln::msgs::DecodeError;
use crate::ln::wire::{Message, Type};
use crate::util::ser::{LengthLimitedRead, Readable, Writeable, Writer};

pub const COMMANDO_COMMAND: u16 = 0x4c4f;
pub const COMMANDO_REPLY_CONT: u16 = 0x594b;
pub const COMMANDO_REPLY_TERM: u16 = 0x594d;

#[derive(Clone, Copy, Debug)]
pub enum RetryPolicy {
    Never,
    Always { max_retries: usize },
}

// Control messages to the pump task
enum Ctrl {
    Start {
        cmd: CommandoCommand,
        policy: RetryPolicy,
        done_tx: oneshot::Sender<Result<Value, Error>>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum ReconnectMode {
    Never,
    Auto {
        max_attempts: usize,
        base_backoff: Duration,
        max_backoff: Duration,
    },
}

/// Client-wide defaults for timeouts, reconnect, and retry policy.
///
/// Builder-style API:
/// ```
/// use lnsocket::commando::CommandoConfig;
/// use std::time::Duration;
/// let cfg = CommandoConfig::new()
///     .timeout(Some(Duration::from_secs(10)))
///     .reconnect(5, Duration::from_millis(100), Duration::from_secs(2))
///     .retry_policy(lnsocket::commando::RetryPolicy::Always { max_retries: 2 });
/// ```
#[derive(Clone, Debug)]
pub struct CommandoConfig {
    timeout: Option<Duration>,
    reconnect: ReconnectMode,
    retry_policy: RetryPolicy,
}

/// Per-call overrides. Leave fields as `None` to inherit from the client.
///
/// ```
/// use lnsocket::commando::CallOpts;
/// let opts = CallOpts::new()
///     .retry(5)                      // RetryPolicy::Always { 5 }
///     .timeout(std::time::Duration::from_secs(9))
///     .rune("override-rune".into());
/// ```
#[derive(Clone, Debug, Default)]
pub struct CallOpts {
    pub retry_policy: Option<RetryPolicy>,
    pub timeout: Option<Duration>,
    pub rune: Option<String>,
    pub filter: Option<Value>,
}

impl CallOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn retry(mut self, max_retries: usize) -> Self {
        self.retry_policy = Some(RetryPolicy::Always { max_retries });
        self
    }

    pub fn filter(mut self, value: Value) -> Self {
        self.filter = Some(value);
        self
    }

    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    pub fn rune(mut self, rune: String) -> Self {
        self.rune = Some(rune);
        self
    }
}

impl CommandoConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn timeout(mut self, duration: Option<Duration>) -> Self {
        self.timeout = duration;
        self
    }

    pub fn retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn no_reconnect(mut self) -> Self {
        self.reconnect = ReconnectMode::Never;
        self
    }

    pub fn reconnect(
        mut self,
        max_attempts: usize,
        base_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        self.reconnect = ReconnectMode::Auto {
            max_attempts,
            base_backoff,
            max_backoff,
        };
        self
    }
}

impl Default for CommandoConfig {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(30)),
            reconnect: ReconnectMode::Auto {
                max_attempts: 10,
                base_backoff: Duration::from_millis(200),
                max_backoff: Duration::from_secs(5),
            },
            retry_policy: RetryPolicy::Always { max_retries: 3 },
        }
    }
}

impl CommandoCommand {
    pub fn new(
        id: u64,
        method: String,
        rune: String,
        params: Value,
        filter: Option<Value>,
    ) -> Self {
        Self {
            id,
            method,
            rune,
            params,
            filter,
        }
    }
    pub fn req_id(&self) -> u64 {
        self.id
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn rune(&self) -> &str {
        &self.rune
    }
    pub fn params(&self) -> &Value {
        &self.params
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandoCommand {
    id: u64,
    method: String,
    params: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Value>,
    rune: String,
}

struct InProgress {
    cmd: CommandoCommand,
    done_tx: oneshot::Sender<Result<Value, Error>>,
    policy: RetryPolicy,
    attempts: usize,
    buf: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CommandoReplyChunk {
    pub req_id: u64,
    pub chunk: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum IncomingCommandoMessage {
    Chunk(CommandoReplyChunk),
    Done(CommandoReplyChunk),
}

pub fn read_incoming_commando_message<R: LengthLimitedRead>(
    typ: u16,
    buf: &mut R,
) -> Result<Option<IncomingCommandoMessage>, DecodeError> {
    if typ == COMMANDO_REPLY_CONT {
        let req_id: u64 = Readable::read(buf)?;
        let mut chunk = Vec::with_capacity(buf.remaining_bytes() as usize);
        buf.read_to_end(&mut chunk)?;
        Ok(Some(IncomingCommandoMessage::Chunk(CommandoReplyChunk {
            req_id,
            chunk,
        })))
    } else if typ == COMMANDO_REPLY_TERM {
        let req_id: u64 = Readable::read(buf)?;
        let mut chunk = Vec::with_capacity(buf.remaining_bytes() as usize);
        buf.read_to_end(&mut chunk)?;
        Ok(Some(IncomingCommandoMessage::Done(CommandoReplyChunk {
            req_id,
            chunk,
        })))
    } else {
        Ok(None)
    }
}

impl Writeable for CommandoCommand {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), std::io::Error> {
        self.id.write(writer)?;
        writer.write_all(
            &serde_json::to_string(self)
                .expect("commando command json")
                .into_bytes(),
        )?;
        Ok(())
    }
}

impl Type for CommandoCommand {
    fn type_id(&self) -> u16 {
        COMMANDO_COMMAND
    }
}

impl Type for IncomingCommandoMessage {
    fn type_id(&self) -> u16 {
        match self {
            IncomingCommandoMessage::Chunk(_) => COMMANDO_REPLY_CONT,
            IncomingCommandoMessage::Done(_) => COMMANDO_REPLY_TERM,
        }
    }
}

/// Public client for Core Lightning Commando over an `LNSocket`.
///
/// Spawns a background task to:
/// - write requests with generated IDs,
/// - read fragments (`COMMANDO_REPLY_CONT`) and accumulate bytes,
/// - parse JSON on terminal chunk (`COMMANDO_REPLY_TERM`),
/// - optionally reconnect and resend per `RetryPolicy`.
///
/// ### Usage
/// ```no_run
/// # use lnsocket::{LNSocket, CommandoClient};
/// # use bitcoin::secp256k1::{SecretKey, PublicKey, rand};
/// # use serde_json::json;
/// # async fn ex(pk: PublicKey, rune: &str) -> Result<(), lnsocket::Error> {
/// let key = SecretKey::new(&mut rand::thread_rng());
/// let sock = LNSocket::connect_and_init(key, pk, "ln.example.com:9735").await?;
/// let client = CommandoClient::spawn(sock, rune);
///
/// // Default policy (see `CommandoConfig::default()`):
/// let v = client.call("listpeers", json!({})).await?;
///
/// // Per-call overrides:
/// use lnsocket::commando::CallOpts;
/// let opts = CallOpts::new().retry(5).timeout(std::time::Duration::from_secs(10));
/// let v2 = client.call_with_opts("getchaninfo", json!({"channel": "..." }), opts).await?;
/// # Ok(()) }
/// ```
pub struct CommandoClient {
    tx: mpsc::Sender<Ctrl>,
    next_id: AtomicU64,
    config: CommandoConfig,
    rune: String,
}

impl CommandoClient {
    /// Spawn the background pump that owns the LNSocket.
    pub fn spawn_with_config(
        sock: LNSocket,
        rune: impl Into<String>,
        config: CommandoConfig,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Ctrl>(128);
        // move everything into the task
        tokio::spawn(pump(sock, rx, config.clone()));

        Self {
            tx,
            rune: rune.into(),
            next_id: AtomicU64::new(1),
            config,
        }
    }

    pub fn spawn(sock: LNSocket, rune: impl Into<String>) -> Self {
        Self::spawn_with_config(sock, rune, CommandoConfig::default())
    }

    #[inline]
    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn call(&self, method: impl Into<String>, params: Value) -> Result<Value, Error> {
        self.call_with_opts(method, params, CallOpts::default())
            .await
    }

    pub async fn call_with_opts(
        &self,
        method: impl Into<String>,
        params: Value,
        opts: CallOpts,
    ) -> Result<Value, Error> {
        let (done_tx, done_rx) = oneshot::channel();
        let cmd = CommandoCommand::new(
            self.alloc_id(),
            method.into(),
            opts.rune.clone().unwrap_or_else(|| self.rune.clone()),
            params,
            opts.filter.clone(),
        );

        self.tx
            .send(Ctrl::Start {
                policy: opts.retry_policy.unwrap_or(self.config.retry_policy),
                cmd,
                done_tx,
            })
            .await
            .map_err(|_| Error::Io(std::io::ErrorKind::BrokenPipe))?;

        match self.config.timeout {
            Some(d) => timeout(d, async { done_rx.await })
                .await
                .map_err(|_| Error::Io(std::io::ErrorKind::TimedOut))?
                .map_err(|_| Error::Io(std::io::ErrorKind::BrokenPipe))?,
            None => done_rx
                .await
                .map_err(|_| Error::Io(std::io::ErrorKind::BrokenPipe))?,
        }
    }
}

// Background task: single reader + demux per internal req_id.
async fn pump(mut sock: LNSocket, mut rx: mpsc::Receiver<Ctrl>, cfg: CommandoConfig) {
    let mut pending: HashMap<u64, InProgress> = HashMap::new();
    let mut queue: Vec<InProgress> = Vec::new();

    loop {
        tokio::select! {
            maybe_ctrl = rx.recv() => {
                let Some(Ctrl::Start { cmd, policy, done_tx }) = maybe_ctrl else {
                    // channel closed; if nothing is pending, we can end. Otherwise, keep reading until we fail.
                    if pending.is_empty() { break; }
                    continue;
                };

                let req_id = cmd.req_id();
                let ip = InProgress { cmd, policy, attempts: 0, done_tx, buf: Vec::new() };
                pending.insert(req_id, ip);

                if let Err(_e) = sock.write(&pending[&req_id].cmd).await {
                    if handle_broken_pipe(&cfg, &mut sock, &mut pending, &mut queue).await.is_err() {
                        break;
                    }
                }
            }

            res = sock.read_custom(|typ, buf| read_incoming_commando_message(typ, buf)) => {
                match res {
                    Err(_e) => {
                        if handle_broken_pipe(&cfg, &mut sock, &mut pending, &mut queue).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Ping(ping)) => {
                        tracing::trace!("pump: pingpong {}", ping.ponglen);
                        let _ = sock.write(&msgs::Pong { byteslen: ping.ponglen }).await;
                    }
                    Ok(Message::Custom(IncomingCommandoMessage::Chunk(chunk))) => {
                        tracing::trace!("pump: [{}] chunk_partial {}", chunk.req_id, chunk.chunk.len());
                        if let Some(p) = pending.get_mut(&chunk.req_id) {
                            p.buf.extend_from_slice(&chunk.chunk);
                        }
                    }
                    Ok(Message::Custom(IncomingCommandoMessage::Done(chunk))) => {
                        tracing::trace!("pump: [{}] chunk_done {}", chunk.req_id, chunk.chunk.len());
                        if let Some(mut p) = pending.remove(&chunk.req_id) {
                            p.buf.extend_from_slice(&chunk.chunk);
                            let parsed = parse_commando_response(&p.buf);
                            let _ = p.done_tx.send(parsed);
                        }
                    }
                    Ok(other) => {
                        tracing::trace!("pump: other_msg {}", other.type_id());
                    }
                }
            }
        }
    }
}

fn parse_commando_response(buf: &[u8]) -> Result<Value, Error> {
    let value = serde_json::from_slice::<Value>(buf).map_err(|_| Error::Json)?;
    let obj = value.as_object().ok_or(Error::Json)?;

    if let Some(error) = obj.get("error") {
        let rpc_err: RpcError =
            serde_json::from_value(error.clone()).unwrap_or_else(|_| RpcError {
                code: -1,
                message: serde_json::to_string(error).unwrap(),
            });
        return Err(Error::Rpc(rpc_err));
    }

    match obj.get("result") {
        None => Err(Error::Json),
        Some(res) => Ok(res.clone()),
    }
}

async fn reconnect(
    sock: &mut LNSocket,
    max_attempts: usize,
    base_backoff: Duration,
    max_backoff: Duration,
    pending: &mut HashMap<u64, InProgress>,
    queued_while_down: &mut Vec<InProgress>,
) -> Result<(), ()> {
    // Decide what to retry (respect per-request policy)
    let mut to_retry = Vec::new();
    for (_id, mut p) in pending.drain() {
        match p.policy {
            RetryPolicy::Always { max_retries } if p.attempts < max_retries => {
                p.attempts += 1;
                p.buf.clear();
                to_retry.push(p);
            }
            _ => {
                let _ = p
                    .done_tx
                    .send(Err(Error::Io(std::io::ErrorKind::BrokenPipe)));
            }
        }
    }
    queued_while_down.extend(to_retry);

    // Exponential backoff (no RNG jitter here to keep deps minimal)
    let mut attempt = 0usize;
    let mut delay = base_backoff.min(max_backoff);

    loop {
        match sock.reconnect_fresh().await {
            Ok(new_sock) => {
                tracing::info!("reconnected!");
                *sock = new_sock;
                break;
            }
            Err(err) => {
                attempt += 1;
                if attempt >= max_attempts {
                    tracing::error!("reconnect exhausted after {attempt} attempts: {err}");
                    // Fail any still-queued items
                    for p in queued_while_down.drain(..) {
                        let _ = p
                            .done_tx
                            .send(Err(Error::Io(std::io::ErrorKind::BrokenPipe)));
                    }
                    return Err(());
                }
                tracing::warn!("reconnect failed: {err}; retrying in {:?}", delay);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(max_backoff);
            }
        }
    }

    // Resend queued (preserve remainder on early failure)
    if !queued_while_down.is_empty() {
        tracing::info!("attempting to retry {} commands", queued_while_down.len());
    }

    // Take ownership of queued items so we can iterate by value.
    // If we fail partway, we’ll put the current and remaining items back.
    let mut rest = std::mem::take(queued_while_down).into_iter();

    while let Some(p) = rest.next() {
        if sock.write(&p.cmd).await.is_ok() {
            pending.insert(p.cmd.req_id(), p);
        } else {
            // Put back the current item and all remaining ones, preserving order.
            queued_while_down.push(p);
            queued_while_down.extend(rest); // moves the remaining items
            return Err(());
        }
    }

    Ok(())
}

async fn handle_broken_pipe(
    cfg: &CommandoConfig,
    sock: &mut LNSocket,
    pending: &mut HashMap<u64, InProgress>,
    queue: &mut Vec<InProgress>,
) -> Result<(), ()> {
    match cfg.reconnect {
        ReconnectMode::Never => {
            for (_id, p) in pending.drain() {
                let _ = p
                    .done_tx
                    .send(Err(Error::Io(std::io::ErrorKind::BrokenPipe)));
            }
            for p in queue.drain(..) {
                let _ = p
                    .done_tx
                    .send(Err(Error::Io(std::io::ErrorKind::BrokenPipe)));
            }
            Err(())
        }
        ReconnectMode::Auto {
            max_attempts,
            base_backoff,
            max_backoff,
        } => {
            reconnect(
                sock,
                max_attempts,
                base_backoff,
                max_backoff,
                pending,
                queue,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn mk_cmd(id: u64) -> CommandoCommand {
        CommandoCommand::new(
            id,
            format!("m{id}"),
            "rune".to_string(),
            serde_json::Value::Null,
            None,
        )
    }

    /// Build an InProgress with a fresh oneshot so callers can assert how it completes.
    fn mk_ip(
        id: u64,
        policy: RetryPolicy,
        attempts: usize,
    ) -> (InProgress, oneshot::Receiver<Result<Value, Error>>) {
        let (tx, rx) = oneshot::channel();
        let ip = InProgress {
            cmd: mk_cmd(id),
            done_tx: tx,
            policy,
            attempts,
            buf: Vec::new(),
        };
        (ip, rx)
    }

    /// This mirrors the resend loop in `reconnect`, but takes a closure for "socket write".
    /// It preserves FIFO, moves successes to `pending`, and on first failure puts the current
    /// plus the remainder back into `queue`.
    fn drain_resend_sim<F>(
        queue: &mut Vec<InProgress>,
        pending: &mut HashMap<u64, InProgress>,
        mut write_ok: F,
    ) -> Result<(), ()>
    where
        F: FnMut(&CommandoCommand) -> bool,
    {
        let mut rest = std::mem::take(queue).into_iter();
        while let Some(p) = rest.next() {
            if write_ok(&p.cmd) {
                pending.insert(p.cmd.req_id(), p);
            } else {
                queue.push(p);
                queue.extend(rest);
                return Err(());
            }
        }
        Ok(())
    }

    /// Helper that performs just the "classification" part of reconnect:
    /// drain `pending`, moving retry-eligible items to `queue`, failing others.
    fn classify_for_retry(pending: &mut HashMap<u64, InProgress>, queue: &mut Vec<InProgress>) {
        let mut to_retry = Vec::new();
        for (_id, mut p) in pending.drain() {
            match p.policy {
                RetryPolicy::Always { max_retries } if p.attempts < max_retries => {
                    p.attempts += 1;
                    p.buf.clear();
                    to_retry.push(p);
                }
                _ => {
                    let _ = p
                        .done_tx
                        .send(Err(Error::Io(std::io::ErrorKind::BrokenPipe)));
                }
            }
        }
        queue.extend(to_retry);
    }

    #[tokio::test]
    async fn retry_classification_honors_policy_and_bumps_attempts() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        // will be retried (attempts < max)
        let (ip1, _rx1) = mk_ip(1, RetryPolicy::Always { max_retries: 2 }, 0);
        // at limit -> fail
        let (ip2, rx2) = mk_ip(2, RetryPolicy::Always { max_retries: 2 }, 2);
        // never retry -> fail
        let (ip3, rx3) = mk_ip(3, RetryPolicy::Never, 0);

        pending.insert(1, ip1);
        pending.insert(2, ip2);
        pending.insert(3, ip3);

        classify_for_retry(&mut pending, &mut queue);

        // pending should be empty after classification
        assert!(pending.is_empty());
        // queue should contain only id=1
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].cmd.req_id(), 1);
        assert_eq!(queue[0].attempts, 1, "attempts should increment on retry");

        // The others must have been failed with BrokenPipe
        let err2 = rx2.await.expect("sender must send an error");
        match err2 {
            Err(Error::Io(kind)) => assert_eq!(kind, std::io::ErrorKind::BrokenPipe),
            other => panic!("expected BrokenPipe error, got {:?}", other),
        }

        let err3 = rx3.await.expect("sender must send an error");
        match err3 {
            Err(Error::Io(kind)) => assert_eq!(kind, std::io::ErrorKind::BrokenPipe),
            other => panic!("expected BrokenPipe error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn drain_resend_moves_all_on_success_and_empties_queue() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        let (ip1, _r1) = mk_ip(10, RetryPolicy::Always { max_retries: 3 }, 1);
        let (ip2, _r2) = mk_ip(11, RetryPolicy::Always { max_retries: 3 }, 0);
        let (ip3, _r3) = mk_ip(12, RetryPolicy::Always { max_retries: 3 }, 2);

        queue.extend([ip1, ip2, ip3]);

        // All writes succeed
        let res = drain_resend_sim(&mut queue, &mut pending, |_cmd| true);
        assert!(res.is_ok());
        assert!(queue.is_empty());
        assert_eq!(pending.len(), 3);
        assert!(pending.contains_key(&10));
        assert!(pending.contains_key(&11));
        assert!(pending.contains_key(&12));
    }

    #[tokio::test]
    async fn drain_resend_stops_on_first_failure_and_preserves_remainder_order() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        let (ip1, _r1) = mk_ip(20, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip2, _r2) = mk_ip(21, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip3, _r3) = mk_ip(22, RetryPolicy::Always { max_retries: 5 }, 0);

        queue.extend([ip1, ip2, ip3]);

        // First write ok, second fails -> should put back 21 and 22 (in order).
        let mut call = 0usize;
        let res = drain_resend_sim(&mut queue, &mut pending, |_cmd| {
            call += 1;
            call != 2 // succeed on #1, fail on #2
        });

        assert!(res.is_err());
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&20));

        // Queue should now have the failed current (21) followed by the remainder (22)
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].cmd.req_id(), 21);
        assert_eq!(queue[1].cmd.req_id(), 22);
    }

    // --- API surface & wiring -------------------------------------------------

    #[test]
    fn commando_command_getters_and_type_id() {
        let c = CommandoCommand::new(
            42,
            "listpeers".to_string(),
            "rune-abc".to_string(),
            serde_json::json!({"id": "0299..."}),
            None,
        );

        assert_eq!(c.req_id(), 42);
        assert_eq!(c.method(), "listpeers");
        assert_eq!(c.rune(), "rune-abc");
        assert_eq!(c.params(), &serde_json::json!({"id": "0299..."}));
        assert_eq!(c.type_id(), COMMANDO_COMMAND);
    }

    #[test]
    fn incoming_message_type_ids_match_constants() {
        let chunk = CommandoReplyChunk {
            req_id: 7,
            chunk: vec![1, 2, 3],
        };
        let cont = IncomingCommandoMessage::Chunk(chunk.clone());
        let done = IncomingCommandoMessage::Done(chunk);

        assert_eq!(cont.type_id(), COMMANDO_REPLY_CONT);
        assert_eq!(done.type_id(), COMMANDO_REPLY_TERM);
    }

    #[test]
    fn callopts_builders_override_values() {
        let opts = CallOpts::new()
            .retry(5)
            .timeout(Duration::from_secs(9))
            .rune("override-rune".to_string());

        match opts.retry_policy {
            Some(RetryPolicy::Always { max_retries }) => assert_eq!(max_retries, 5),
            _ => panic!("retry policy should be Always(5)"),
        }
        assert_eq!(opts.timeout, Some(Duration::from_secs(9)));
        assert_eq!(opts.rune.as_deref(), Some("override-rune"));
    }

    #[test]
    fn commando_config_default_and_builders() {
        // Defaults
        let d = CommandoConfig::default();
        assert_eq!(d.timeout, Some(Duration::from_secs(30)));
        match d.reconnect {
            ReconnectMode::Auto {
                max_attempts,
                base_backoff,
                max_backoff,
            } => {
                assert_eq!(max_attempts, 10);
                assert_eq!(base_backoff, Duration::from_millis(200));
                assert_eq!(max_backoff, Duration::from_secs(5));
            }
            _ => panic!("default should be Auto reconnect"),
        }

        // Builder overrides
        let c = CommandoConfig::new()
            .timeout(None)
            .retry_policy(RetryPolicy::Always { max_retries: 3 })
            .reconnect(4, Duration::from_millis(50), Duration::from_secs(1))
            .no_reconnect(); // final call wins

        assert_eq!(c.timeout, None);
        match c.retry_policy {
            RetryPolicy::Always { max_retries } => assert_eq!(max_retries, 3),
            _ => panic!("retry policy should be Always(3)"),
        }
        match c.reconnect {
            ReconnectMode::Never => {}
            _ => panic!("explicit no_reconnect should win"),
        }
    }

    // --- Retry/queue semantics edge cases -------------------------------------

    #[tokio::test]
    async fn classify_for_retry_clears_buf_on_retry() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        // eligible for retry and has stale bytes to clear
        let (mut ip, _rx) = mk_ip(100, RetryPolicy::Always { max_retries: 2 }, 0);
        ip.buf.extend_from_slice(b"stale-partial-json");
        pending.insert(100, ip);

        classify_for_retry(&mut pending, &mut queue);

        assert!(pending.is_empty());
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].cmd.req_id(), 100);
        assert_eq!(queue[0].attempts, 1);
        assert!(queue[0].buf.is_empty(), "buf must be cleared before retry");
    }

    #[tokio::test]
    async fn drain_resend_fails_immediately_and_preserves_all_on_first_failure() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        let (ip1, _r1) = mk_ip(30, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip2, _r2) = mk_ip(31, RetryPolicy::Always { max_retries: 5 }, 0);
        queue.extend([ip1, ip2]);

        // First write fails outright.
        let res = drain_resend_sim(&mut queue, &mut pending, |_cmd| false);
        assert!(res.is_err());

        // Nothing moved to pending; entire original order preserved in queue.
        assert!(pending.is_empty());
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].cmd.req_id(), 30);
        assert_eq!(queue[1].cmd.req_id(), 31);
    }

    #[tokio::test]
    async fn drain_resend_third_fails_preserves_remaining_order() {
        let mut pending: HashMap<u64, InProgress> = HashMap::new();
        let mut queue: Vec<InProgress> = Vec::new();

        let (ip1, _r1) = mk_ip(40, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip2, _r2) = mk_ip(41, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip3, _r3) = mk_ip(42, RetryPolicy::Always { max_retries: 5 }, 0);
        let (ip4, _r4) = mk_ip(43, RetryPolicy::Always { max_retries: 5 }, 0);
        queue.extend([ip1, ip2, ip3, ip4]);

        let mut call = 0usize;
        let res = drain_resend_sim(&mut queue, &mut pending, |_cmd| {
            call += 1;
            call < 3 // succeed for #1 and #2, fail on #3
        });

        assert!(res.is_err());
        assert_eq!(pending.len(), 2);
        assert!(pending.contains_key(&40));
        assert!(pending.contains_key(&41));

        // Queue holds failed current (42) followed by remainder (43), FIFO preserved.
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].cmd.req_id(), 42);
        assert_eq!(queue[1].cmd.req_id(), 43);
    }
}
