//! The host's end of the tool protocol (`notedeck_agent_proto::wire`): one
//! session with a guest's `notedeck-agent`, many calls in flight on it.
//!
//! ```text
//!   GuestClient::call ─┐                 ┌──────────── pump (one task) ─────────────┐
//!   GuestClient::call ─┼─ Command ──────▶│ Call/Ping: note id ─▶ write one line ────│──▶ guest stdin
//!   Call::cancel/drop ─┘                 │                                          │
//!                                        │ LineReassembler ◀── chunk ◀── chunk ◀────│─── guest stdout
//!   Call::next ◀── Output, Output, Done ◀│   route each line by id                  │
//!                                        └──────────────────────────────────────────┘
//! ```
//!
//! Three things the session has to get right, each with a test in
//! `tests/protocol.rs`:
//!
//! - **One session, many calls.** A model asks for parallel tool calls as a
//!   matter of course, so calls are correlated by id and run side by side
//!   on one stream; nothing here opens a stream per call.
//! - **Lines, not chunks.** The transport hands over bytes in whatever
//!   pieces it likes ([`GuestOutput`]); a [`LineReassembler`] turns them
//!   back into messages, so a 512 KB result is one message however it was
//!   cut.
//! - **The handshake.** [`connect`] pings first and refuses a guest that
//!   isn't this build ([`Version::check`]): a stale guest binary is one clear
//!   error instead of a field one side has and the other doesn't.
//!
//! The transport is two small traits, so the same client drives a microVM
//! guest over microsandbox's `exec_stream` ([`crate::exec`]), a child
//! process's pipes, or an in-memory pipe in a test ([`Pipe`]).

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use notedeck_agent_proto::call::CodingCall;
use notedeck_agent_proto::lines::{Line, LineReassembler, MAX_LINE};
use notedeck_agent_proto::wire::{
    GuestMessage, HandshakeError, HostMessage, OutputStream, Version,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// The guest's stdout, as the transport delivers it: pieces of bytes with no
/// promise that a piece is a line.
pub trait GuestOutput: Send + 'static {
    /// One piece of the guest's output.
    type Chunk: AsRef<[u8]> + Send;

    /// The next piece; `None` once the guest has closed its output, an
    /// error if the transport failed.
    fn next_chunk(&mut self) -> impl Future<Output = Option<io::Result<Self::Chunk>>> + Send;
}

/// The guest's stdin.
pub trait GuestInput: Send + 'static {
    /// Write one whole line, its `\n` included.
    fn write_line(&mut self, line: Vec<u8>) -> impl Future<Output = io::Result<()>> + Send;

    /// Close the guest's stdin, which the guest reads as a shutdown.
    fn close(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

/// Either end of a byte pipe (a child's stdio, an in-memory duplex) as a
/// guest transport.
pub struct Pipe<T>(pub T);

impl<R: AsyncRead + Unpin + Send + 'static> GuestOutput for Pipe<R> {
    type Chunk = Vec<u8>;

    async fn next_chunk(&mut self) -> Option<io::Result<Vec<u8>>> {
        let mut buf = vec![0u8; 64 * 1024];
        match self.0.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some(Ok(buf))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> GuestInput for Pipe<W> {
    async fn write_line(&mut self, line: Vec<u8>) -> io::Result<()> {
        self.0.write_all(&line).await?;
        self.0.flush().await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.0.shutdown().await
    }
}

/// How [`connect_with`] sets up a session.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// How long the guest has to answer the handshake ping. A warm guest
    /// answers in about a millisecond; this is for one that never starts.
    pub handshake_timeout: Duration,
    /// The longest line read from the guest; longer ones fail the call they
    /// belong to. See [`MAX_LINE`].
    pub max_line: usize,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            max_line: MAX_LINE,
        }
    }
}

/// Why a session couldn't be set up.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The guest answered, but isn't this build or this protocol.
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    /// The guest didn't answer the ping in time.
    #[error("the agent executor didn't answer within {0:?}")]
    Timeout(Duration),
    /// The session closed before the guest answered.
    #[error("the agent executor went away before answering: {0}")]
    Disconnected(String),
}

/// Why a call didn't produce an answer. Its text is what the model reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The tool ran and failed (no such file, a timeout, a bad pattern):
    /// the executor's own message.
    #[error("{0}")]
    Tool(String),
    /// The call was cancelled before it finished.
    #[error("cancelled")]
    Cancelled,
    /// The answer was longer than the session reads, so it was dropped.
    #[error("the tool's answer was {len} bytes, more than the {max} the host reads")]
    TooLarge { len: usize, max: usize },
    /// The guest answered with something the host couldn't read.
    #[error("the agent executor sent an answer the host couldn't read: {0}")]
    Unreadable(String),
    /// The session ended before the call was answered: the guest exited,
    /// the transport failed, or the client shut down.
    #[error("the agent executor went away: {0}")]
    Disconnected(String),
}

/// What a running call has to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    /// Output the call wrote, as it wrote it. For watching only: the
    /// answer in [`CallEvent::Done`] is complete without these.
    Output { stream: OutputStream, data: String },
    /// The call's answer. Nothing follows it.
    Done(Result<String, CallError>),
}

/// What the client asks of the pump.
enum Command {
    Ping {
        id: u64,
        pong: oneshot::Sender<Version>,
    },
    Call {
        id: u64,
        call: CodingCall,
        events: mpsc::UnboundedSender<CallEvent>,
    },
    Cancel {
        id: u64,
    },
    Shutdown,
}

/// Someone waiting on an id.
enum Waiter {
    Ping(oneshot::Sender<Version>),
    Call(mpsc::UnboundedSender<CallEvent>),
}

/// A session with a guest's executor. Cheap to share by reference: every
/// call goes through the one pump task.
pub struct GuestClient {
    commands: mpsc::UnboundedSender<Command>,
    next_id: AtomicU64,
    version: Version,
    /// Why the session ended, once it has.
    closed: Arc<OnceLock<String>>,
    /// Taken by [`GuestClient::shutdown`] to wait for the end.
    pump: Option<JoinHandle<()>>,
}

/// One call in flight. Read it with [`Call::next`] or [`Call::result`];
/// dropping it before its answer cancels it in the guest, so an abandoned
/// `cargo build` doesn't run on.
pub struct Call {
    id: u64,
    events: mpsc::UnboundedReceiver<CallEvent>,
    commands: mpsc::UnboundedSender<Command>,
    closed: Arc<OnceLock<String>>,
    done: bool,
}

/// Open a session over `output` and `input` with default options, and
/// refuse it unless the guest is `expected_build` (the host's own,
/// [`crate::guest::HOST_BUILD`]).
pub async fn connect<O: GuestOutput, I: GuestInput>(
    output: O,
    input: I,
    expected_build: &str,
) -> Result<GuestClient, ConnectError> {
    connect_with(output, input, expected_build, ConnectOptions::default()).await
}

/// [`connect`], with `options`.
pub async fn connect_with<O: GuestOutput, I: GuestInput>(
    output: O,
    input: I,
    expected_build: &str,
    options: ConnectOptions,
) -> Result<GuestClient, ConnectError> {
    let (commands, from_client) = mpsc::unbounded_channel();
    let closed = Arc::new(OnceLock::new());
    let pump = tokio::spawn(pump(
        output,
        input,
        from_client,
        closed.clone(),
        options.max_line,
    ));
    // Id 0 is the handshake's; calls count from 1.
    let handshake = ping(&commands, 0, &closed);
    let answered = tokio::time::timeout(options.handshake_timeout, handshake).await;
    let version = match answered {
        Ok(Ok(version)) => version
            .check(expected_build)
            .map(|()| version)
            .map_err(ConnectError::from),
        Ok(Err(reason)) => Err(ConnectError::Disconnected(reason)),
        Err(_) => Err(ConnectError::Timeout(options.handshake_timeout)),
    };
    let version = match version {
        Ok(version) => version,
        Err(e) => {
            let _ = commands.send(Command::Shutdown);
            return Err(e);
        }
    };
    Ok(GuestClient {
        commands,
        next_id: AtomicU64::new(1),
        version,
        closed,
        pump: Some(pump),
    })
}

/// Ping the guest as `id` and wait for its version, or why the session
/// ended.
async fn ping(
    commands: &mpsc::UnboundedSender<Command>,
    id: u64,
    closed: &OnceLock<String>,
) -> Result<Version, String> {
    let (pong, answer) = oneshot::channel();
    if commands.send(Command::Ping { id, pong }).is_err() {
        return Err(closed_reason(closed));
    }
    answer.await.map_err(|_| closed_reason(closed))
}

impl GuestClient {
    /// The version the guest answered the handshake with.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Ask the guest for its version: a liveness check. `Err` says why the
    /// session has ended.
    pub async fn ping(&self) -> Result<Version, String> {
        ping(&self.commands, self.next_id(), &self.closed).await
    }

    /// Start `call` in the guest. It runs beside any others in flight.
    pub fn call(&self, call: CodingCall) -> Call {
        let id = self.next_id();
        let (events, receiver) = mpsc::unbounded_channel();
        let sent = self.commands.send(Command::Call {
            id,
            call,
            events: events.clone(),
        });
        if sent.is_err() {
            let reason = closed_reason(&self.closed);
            let _ = events.send(CallEvent::Done(Err(CallError::Disconnected(reason))));
        }
        Call {
            id,
            events: receiver,
            commands: self.commands.clone(),
            closed: self.closed.clone(),
            done: false,
        }
    }

    /// Why the session ended, or `None` while it is open.
    pub fn closed(&self) -> Option<&str> {
        self.closed.get().map(String::as_str)
    }

    /// Shut the guest down: it cancels what is in flight, answers it and
    /// exits. Waits up to `grace` for the session to end.
    pub async fn shutdown(mut self, grace: Duration) {
        let _ = self.commands.send(Command::Shutdown);
        let Some(mut pump) = self.pump.take() else {
            return;
        };
        if tokio::time::timeout(grace, &mut pump).await.is_err() {
            pump.abort();
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl Drop for GuestClient {
    /// Dropping the client ends the session: the guest cancels what is in
    /// flight, and each [`Call`] still held is answered
    /// [`CallError::Cancelled`].
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

impl Call {
    /// The call's id on the wire.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The call's next event; `None` after [`CallEvent::Done`].
    pub async fn next(&mut self) -> Option<CallEvent> {
        if self.done {
            return None;
        }
        let event = self.events.recv().await.unwrap_or_else(|| {
            CallEvent::Done(Err(CallError::Disconnected(closed_reason(&self.closed))))
        });
        self.done = matches!(event, CallEvent::Done(_));
        Some(event)
    }

    /// The call's answer, ignoring its output along the way.
    pub async fn result(mut self) -> Result<String, CallError> {
        while let Some(event) = self.next().await {
            if let CallEvent::Done(result) = event {
                return result;
            }
        }
        Err(CallError::Disconnected(closed_reason(&self.closed)))
    }

    /// Ask the guest to stop the call; it is answered
    /// [`CallError::Cancelled`] unless it finished first.
    pub fn cancel(&self) {
        let _ = self.commands.send(Command::Cancel { id: self.id });
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.done {
            self.cancel();
        }
    }
}

/// Why the session ended, for a waiter that found it gone.
fn closed_reason(closed: &OnceLock<String>) -> String {
    closed
        .get()
        .cloned()
        .unwrap_or_else(|| "the session closed".to_owned())
}

/// The session's one task: writes what the client asks, reads what the
/// guest answers, and routes each answer to whoever is waiting on its id.
/// When it ends, everyone still waiting hears why.
async fn pump<O: GuestOutput, I: GuestInput>(
    mut output: O,
    mut input: I,
    mut from_client: mpsc::UnboundedReceiver<Command>,
    closed: Arc<OnceLock<String>>,
    max_line: usize,
) {
    let mut waiting: HashMap<u64, Waiter> = HashMap::new();
    let mut lines = LineReassembler::new(max_line);
    // After a shutdown the guest is still read to its end, so the calls it
    // cancels are answered.
    let mut shutting_down = false;

    let reason = loop {
        tokio::select! {
            command = from_client.recv(), if !shutting_down => {
                let command = command.unwrap_or(Command::Shutdown);
                shutting_down = matches!(command, Command::Shutdown);
                if let Err(e) = send(&mut input, &mut waiting, command).await {
                    break format!("writing to it failed: {e}");
                }
            }
            chunk = output.next_chunk() => match chunk {
                Some(Ok(bytes)) => {
                    lines.push(bytes.as_ref(), |line| route(&mut waiting, line, max_line));
                }
                Some(Err(e)) => break format!("reading from it failed: {e}"),
                None if lines.pending() > 0 => {
                    break "it closed its output partway through a message".to_owned();
                }
                None if shutting_down => break "it was shut down".to_owned(),
                None => break "it closed its output".to_owned(),
            },
        }
    };

    let _ = closed.set(reason.clone());
    for (_, waiter) in waiting.drain() {
        if let Waiter::Call(events) = waiter {
            let _ = events.send(CallEvent::Done(Err(CallError::Disconnected(
                reason.clone(),
            ))));
        }
    }
    // Anything asked after the end hears it too: the client's sends fail
    // once this receiver is dropped, and it reads `closed`.
}

/// Carry out one client command: note who waits on its id, then write it.
async fn send<I: GuestInput>(
    input: &mut I,
    waiting: &mut HashMap<u64, Waiter>,
    command: Command,
) -> io::Result<()> {
    let message = match command {
        Command::Ping { id, pong } => {
            waiting.insert(id, Waiter::Ping(pong));
            HostMessage::Ping { id }
        }
        Command::Call { id, call, events } => {
            waiting.insert(id, Waiter::Call(events));
            HostMessage::Call { id, call }
        }
        Command::Cancel { id } => {
            if !waiting.contains_key(&id) {
                // Answered already; the cancel raced it.
                return Ok(());
            }
            HostMessage::Cancel { id }
        }
        Command::Shutdown => {
            write(input, &HostMessage::Shutdown).await?;
            return input.close().await;
        }
    };
    write(input, &message).await
}

/// Write `message` as one line.
async fn write<I: GuestInput>(input: &mut I, message: &HostMessage) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).expect("a message serializes");
    line.push(b'\n');
    input.write_line(line).await
}

/// Hand one line from the guest to whoever waits on its id.
fn route(waiting: &mut HashMap<u64, Waiter>, line: Line<'_>, max_line: usize) {
    let message = match line {
        Line::Complete([]) => return,
        Line::Complete(line) => match serde_json::from_slice::<GuestMessage>(line) {
            Ok(message) => message,
            Err(e) => {
                let id = serde_json::from_slice::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("id")?.as_u64());
                finish(waiting, id, Err(CallError::Unreadable(e.to_string())));
                return;
            }
        },
        Line::TooLong { prefix, len } => {
            let error = CallError::TooLarge { len, max: max_line };
            finish(waiting, id_in_prefix(prefix), Err(error));
            return;
        }
    };

    match message {
        GuestMessage::Pong { id, version } => {
            if let Some(Waiter::Ping(pong)) = waiting.remove(&id) {
                let _ = pong.send(version);
            }
        }
        GuestMessage::Chunk { id, stream, data } => {
            if let Some(Waiter::Call(events)) = waiting.get(&id) {
                let _ = events.send(CallEvent::Output { stream, data });
            }
        }
        GuestMessage::Result { id, text } => finish(waiting, Some(id), Ok(text)),
        GuestMessage::Error { id, message } => {
            finish(waiting, Some(id), Err(CallError::Tool(message)))
        }
        GuestMessage::Cancelled { id } => finish(waiting, Some(id), Err(CallError::Cancelled)),
        // The guest couldn't read a line of ours and found no id in it.
        // Every line we write is a serialized HostMessage with an id or a
        // Shutdown, so there is no call to answer.
        GuestMessage::Malformed { .. } => {}
    }
}

/// Answer the call `id`, if one is waiting on it.
fn finish(waiting: &mut HashMap<u64, Waiter>, id: Option<u64>, result: Result<String, CallError>) {
    let Some(id) = id else {
        return;
    };
    if let Some(Waiter::Call(events)) = waiting.remove(&id) {
        let _ = events.send(CallEvent::Done(result));
    }
}

/// The `"id":N` near the start of a refused line. serde writes an
/// internally tagged message as `{"type":…,"id":N,…}`, so the first `"id":`
/// is the message's.
fn id_in_prefix(prefix: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"id\":";
    let at = prefix.windows(KEY.len()).position(|w| w == KEY)? + KEY.len();
    let digits = prefix[at..].iter().take_while(|b| b.is_ascii_digit());
    let mut id: u64 = 0;
    let mut any = false;
    for &d in digits {
        id = id.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
        any = true;
    }
    any.then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_read_from_a_refused_line() {
        assert_eq!(
            id_in_prefix(br#"{"type":"result","id":42,"text":"xxxx"#),
            Some(42)
        );
        assert_eq!(id_in_prefix(br#"{"type":"result","text":"xx"#), None);
        assert_eq!(id_in_prefix(br#"{"type":"result","id":"#), None);
    }
}
