//! The listening end, held by the chrome.
//!
//! A background thread accepts connections and reads each one's request line,
//! so the UI thread never waits on a client. A request that doesn't parse is
//! answered there; a good one is handed over as an [`Incoming`], and the host is
//! woken to serve it on its next tick.
//!
//! A subscribe's connection becomes an [`EventQueue`]: the host pushes event
//! lines into a bounded queue without waiting, and a writer thread of the
//! connection's own drains it to the client. A live event that finds the
//! queue full drops the client, and the writer tells it so on its last line,
//! so a stream never waits on a reader that stopped. A burst the producer
//! knows is finite (a follow's opening history) can instead wait for room
//! ([`EventQueue::try_push`]); a reader that stops outright is still dropped
//! once a write times out.

use crate::{Request, Response};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// How long a client gets to send its request line before it's dropped, so a
/// client that connects and says nothing can't hold up the ones behind it.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one write may wait on a client that isn't reading before it
/// fails, so an answer written on the host's loop can stall it no longer
/// than this, and a stream's writer thread gives up on a reader that stopped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a stream's write may wait on its client: shorter than
/// [`WRITE_TIMEOUT`], since nothing else waits on a stream's writer and a
/// reader this far behind has stopped. A write that makes some progress may
/// take two of these to fail.
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often a stream's writer thread, with nothing to write, checks whether
/// its client hung up: the longest a stream outlives its reader.
const HANGUP_CHECK: Duration = Duration::from_millis(250);

/// The last line a client dropped for falling behind reads, so it can tell
/// that from a stream that ended.
pub const DROPPED: &str = "dropped: the reader fell too far behind";

/// The socket notedeck listens on, and the requests waiting to be served.
pub struct Listener {
    incoming: mpsc::Receiver<Incoming>,
    watch: Watch,
}

/// What a notedeck started for `nd` (`--serve`) watches to know when to exit:
/// how long its socket has gone without a request, and whether the socket
/// file is still the one it bound. A notedeck that starts later takes the
/// path (see [`Listener::bind`]), and this one can no longer be reached.
///
/// Cheap to clone and read from any thread; the listener thread stamps it.
#[derive(Clone)]
pub struct Watch {
    path: PathBuf,
    /// The bound socket file, as `(st_dev, st_ino)`.
    file: SocketFile,
    /// When the listener started; [`last_request`](Self::last_request) counts
    /// from here.
    started: Instant,
    /// Milliseconds after `started` that the last connection arrived, or 0
    /// for none yet.
    last_request: Arc<AtomicU64>,
}

/// One socket file's identity, so a replacement at the same path shows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SocketFile {
    dev: u64,
    ino: u64,
}

impl SocketFile {
    /// The file at `path` now, or `None` when there is none.
    fn at(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
}

impl Watch {
    /// How long since the last connection arrived, or since the listener
    /// started when none has.
    pub fn idle_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_request.load(Ordering::Relaxed));
        self.started.elapsed().saturating_sub(last)
    }

    /// Whether the socket file at the path is still the one this listener
    /// bound: `false` once it's removed or another notedeck replaced it.
    pub fn still_ours(&self) -> bool {
        SocketFile::at(&self.path) == Some(self.file)
    }

    /// Mark a connection as just arrived.
    fn stamp(&self) {
        let millis = self.started.elapsed().as_millis().max(1);
        self.last_request
            .store(u64::try_from(millis).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

/// A parsed request and the connection its answer goes back on.
pub struct Incoming {
    pub request: Request,
    pub connection: Connection,
}

/// The client's end, held open until it's answered. Dropping it unanswered
/// hangs up on the client.
pub struct Connection {
    stream: UnixStream,
    /// The version the request was spelled in, which every answer carries.
    version: u32,
}

impl Connection {
    /// Answer the request with one line, in the version it was spelled in.
    /// The client reads one, so a second reply goes nowhere. A client that
    /// doesn't read it holds this up to [`WRITE_TIMEOUT`], then it fails.
    pub fn reply(&mut self, response: &Response) -> io::Result<()> {
        writeln!(self.stream, "{}", response.line_as(self.version))?;
        self.stream.flush()
    }

    /// Turn the connection into a stream's: an [`EventQueue`] holding up to
    /// `capacity` lines, drained by a writer thread of its own.
    ///
    /// Both hooks run on that thread. `on_room` runs each time it takes a
    /// line off the queue, and once more as the stream ends, so a producer
    /// waiting on a full queue ([`EventQueue::try_push`]) wakes to find room
    /// or the end; it should only nudge the producer. `on_close` runs once
    /// the stream is over for any reason (the client hung up or fell behind,
    /// or the queue was dropped and drained), so the producer can stop
    /// without waiting for its next event.
    pub fn into_stream(
        self,
        capacity: usize,
        on_room: impl Fn() + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> io::Result<EventQueue> {
        self.stream.set_write_timeout(Some(STREAM_WRITE_TIMEOUT))?;
        let (lines, queued) = mpsc::sync_channel(capacity);
        let state = Arc::new(StreamState::default());
        let writer = StreamWriter {
            stream: self.stream,
            queued,
            state: Arc::clone(&state),
            version: self.version,
        };
        thread::Builder::new()
            .name("ipc-stream".to_owned())
            .spawn(move || {
                writer.run(&on_room);
                on_room();
                on_close();
            })?;
        Ok(EventQueue {
            lines,
            state,
            version: self.version,
        })
    }
}

/// The host's end of a stream (see [`Connection::into_stream`]): push an
/// event line and go on. Dropping it ends the stream once what is queued
/// has been written.
pub struct EventQueue {
    lines: mpsc::SyncSender<String>,
    state: Arc<StreamState>,
    /// The version the subscribe was spelled in.
    version: u32,
}

/// What [`EventQueue::try_push`] did with a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// It's queued for the client.
    Queued,
    /// The queue has no room yet. The stream is still open: try again once
    /// `on_room` runs.
    Full,
    /// The stream is over: stop producing.
    Closed,
}

/// How a stream stands, shared by its [`EventQueue`] and its writer.
#[derive(Default)]
struct StreamState {
    /// Set when the stream is over, by either end.
    closed: AtomicBool,
    /// Set when a push found the queue full and dropped the client, so the
    /// writer's last line says why.
    behind: AtomicBool,
}

impl StreamState {
    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::Relaxed)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

impl EventQueue {
    /// Queue `response` as the next line, without waiting. `false` when the
    /// stream is over: the client hung up, or reads so slowly that the queue
    /// is full, which drops it (its last line says so). Stop producing then.
    pub fn push(&self, response: &Response) -> bool {
        match self.try_push(response) {
            Pushed::Queued => true,
            Pushed::Full => {
                // Too slow to keep up: the writer hangs up on it.
                self.state.behind.store(true, Ordering::Relaxed);
                self.state.close();
                false
            }
            Pushed::Closed => false,
        }
    }

    /// Queue `response` as the next line if there's room, without waiting
    /// and without dropping the client when there isn't: a producer with a
    /// finite burst to send waits for `on_room` and tries again, so a slow
    /// reader gets all of it. One that stops reading is still dropped once
    /// the writer's write to it times out.
    pub fn try_push(&self, response: &Response) -> Pushed {
        if !self.is_open() {
            return Pushed::Closed;
        }
        match self.lines.try_send(response.line_as(self.version)) {
            Ok(()) => Pushed::Queued,
            Err(mpsc::TrySendError::Full(_)) => Pushed::Full,
            Err(mpsc::TrySendError::Disconnected(_)) => Pushed::Closed,
        }
    }

    /// Whether the stream still has a client to write to.
    pub fn is_open(&self) -> bool {
        self.state.is_open()
    }
}

/// A stream's writer thread: what it writes, and where.
struct StreamWriter {
    stream: UnixStream,
    queued: mpsc::Receiver<String>,
    state: Arc<StreamState>,
    /// The version the subscribe was spelled in, for the [`DROPPED`] line.
    version: u32,
}

impl StreamWriter {
    /// Write each queued line, running `on_room` after taking each, until
    /// the queue is dropped and drained, the client hangs up, a write fails,
    /// or the queue is closed for a slow client; then hang up. A client
    /// dropped for falling behind reads [`DROPPED`] last, unless the line
    /// before it failed to write, which leaves no line to follow.
    fn run(self, on_room: &dyn Fn()) {
        let mut torn = false;
        while self.state.is_open() {
            match self.queued.recv_timeout(HANGUP_CHECK) {
                Ok(line) => {
                    on_room();
                    if self.write_line(line).is_err() {
                        torn = true;
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if hung_up(&self.stream) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        self.state.close();
        if !torn && self.state.behind.load(Ordering::Relaxed) {
            let _ = self.write_line(Response::error(DROPPED).line_as(self.version));
        }
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Write `line` and its newline.
    fn write_line(&self, mut line: String) -> io::Result<()> {
        line.push('\n');
        (&self.stream).write_all(line.as_bytes())
    }
}

/// Whether the client on `stream` has hung up, without waiting: it sends
/// nothing after its request, so a read that finds the end means it's gone.
/// Anything it does send is read and dropped.
fn hung_up(stream: &UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut buf = [0u8; 64];
    let gone = match (&*stream).read(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != io::ErrorKind::WouldBlock,
    };
    gone || stream.set_nonblocking(false).is_err()
}

impl Listener {
    /// Listen on `path`, replacing whatever socket is there (the last notedeck
    /// to start takes it), readable and writable by this user only: a client
    /// can call every tool, writes included. `wake` runs on the listener
    /// thread after each request arrives; it should only nudge the host.
    pub fn bind(path: &Path, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let file = SocketFile::at(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the socket vanished as it was bound",
            )
        })?;
        let watch = Watch {
            path: path.to_owned(),
            file,
            started: Instant::now(),
            last_request: Arc::new(AtomicU64::new(0)),
        };

        let (sender, incoming) = mpsc::channel();
        let stamp = watch.clone();
        thread::Builder::new()
            .name("ipc-listener".to_owned())
            .spawn(move || accept_loop(listener, sender, stamp, wake))?;

        Ok(Self { incoming, watch })
    }

    /// The idle clock and ownership check of this socket.
    pub fn watch(&self) -> Watch {
        self.watch.clone()
    }

    /// The next request waiting to be served, if any. Never blocks.
    pub fn try_recv(&self) -> Option<Incoming> {
        self.incoming.try_recv().ok()
    }
}

/// Accept connections until the [`Listener`] is dropped, handing each good
/// request to it.
///
/// Every connection, even one whose request doesn't parse, stamps `watch`:
/// somebody is using this notedeck.
fn accept_loop(
    listener: UnixListener,
    sender: mpsc::Sender<Incoming>,
    watch: Watch,
    wake: impl Fn(),
) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("notedeck ipc: accept failed: {e}");
                continue;
            }
        };
        watch.stamp();
        if let Err(e) = stream.set_write_timeout(Some(WRITE_TIMEOUT)) {
            eprintln!("notedeck ipc: no write timeout: {e}");
            continue;
        }
        let mut connection = Connection { stream, version: 1 };
        let request = match read_request(&connection.stream) {
            Ok(versioned) => {
                connection.version = versioned.v;
                versioned.request
            }
            Err(message) => {
                let _ = connection.reply(&Response::error(message));
                continue;
            }
        };
        if sender
            .send(Incoming {
                request,
                connection,
            })
            .is_err()
        {
            // The listener is gone; so is notedeck's interest in the socket.
            return;
        }
        wake();
    }
}

/// Read and parse the one request line a client sends.
fn read_request(stream: &UnixStream) -> Result<crate::Versioned, String> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("reading the request: {e}"))?;
    Request::versioned(&line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, Outcome, Reply};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    /// Wait for the listener thread to hand over a request.
    fn next(listener: &Listener) -> Incoming {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(incoming) = listener.try_recv() {
                return incoming;
            }
            assert!(Instant::now() < deadline, "no request arrived");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_request_is_handed_over_and_answered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("notedeck.sock");
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = wakes.clone();
        let listener = Listener::bind(&path, move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "only this user may call tools");

        let client = thread::spawn({
            let path = path.clone();
            move || Client::connect(&path).unwrap().request(&Request::ListTools)
        });
        let mut incoming = next(&listener);
        assert_eq!(incoming.request, Request::ListTools);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        incoming
            .connection
            .reply(&Response::ok(Reply::Tools { tools: vec![] }))
            .unwrap();

        let response = client.join().unwrap().unwrap();
        assert_eq!(
            response.outcome,
            Outcome::Ok(Reply::Tools { tools: vec![] })
        );
    }

    /// A subscriber and its stream: the client's connection, and the
    /// server's queue with channels that hear when there's room in it and
    /// when the stream closes.
    struct Subscribed {
        client: Client,
        queue: EventQueue,
        room: mpsc::Receiver<()>,
        closed: mpsc::Receiver<()>,
    }

    /// Subscribe on a fresh listener at `path` and turn the server's end into
    /// a stream holding `capacity` lines.
    fn subscribe(path: &Path, capacity: usize) -> Subscribed {
        let listener = Listener::bind(path, || {}).unwrap();
        let mut client = Client::connect(path).unwrap();
        client
            .send(&Request::Subscribe {
                name: "test_stream".to_owned(),
                args: serde_json::Value::Null,
            })
            .unwrap();
        let incoming = next(&listener);
        assert!(matches!(incoming.request, Request::Subscribe { .. }));
        let (roomy, room) = mpsc::channel();
        let (tx, closed) = mpsc::channel();
        let queue = incoming
            .connection
            .into_stream(
                capacity,
                move || {
                    let _ = roomy.send(());
                },
                move || {
                    let _ = tx.send(());
                },
            )
            .unwrap();
        Subscribed {
            client,
            queue,
            room,
            closed,
        }
    }

    /// An event `n` padded to `pad` bytes, so a few fill the socket's buffer.
    fn padded(n: usize, pad: usize) -> Response {
        Response::ok(Reply::Event {
            event: serde_json::json!({ "n": n, "pad": "x".repeat(pad) }),
            text: None,
        })
    }

    /// The `n` of an event line `response`, failing on anything else.
    fn event_n(response: &Response) -> u64 {
        match &response.outcome {
            Outcome::Ok(Reply::Event { event, .. }) => event["n"].as_u64().unwrap(),
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn event(n: usize) -> Response {
        Response::ok(Reply::Event {
            event: serde_json::json!({ "n": n }),
            text: Some(format!("{n}\n")),
        })
    }

    /// A subscribe streams each event as a v2 line, in order; dropping the
    /// queue ends the stream after what is queued, and the client reads the
    /// end. A client that hangs up mid-stream closes it on the server.
    #[test]
    fn a_subscribe_streams_events_and_closes_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let Subscribed {
            mut client,
            queue,
            closed,
            ..
        } = subscribe(&path, 16);
        for n in 0..5 {
            assert!(queue.push(&event(n)));
        }
        drop(queue);
        for n in 0..5 {
            let response = client.next_response().unwrap().expect("an event");
            assert_eq!(response.v, 2, "answered in the subscribe's version");
            assert_eq!(response.outcome, event(n).outcome);
        }
        assert!(client.next_response().unwrap().is_none(), "the end");
        closed.recv_timeout(Duration::from_secs(5)).unwrap();

        let path = dir.path().join("second.sock");
        let Subscribed {
            client,
            queue,
            closed,
            ..
        } = subscribe(&path, 16);
        assert!(queue.push(&event(0)));
        drop(client);
        closed
            .recv_timeout(Duration::from_secs(5))
            .expect("the hang-up is heard without another event");
        assert!(!queue.is_open());
        assert!(!queue.push(&event(1)), "nothing more goes out");
    }

    /// A client that never reads fills its queue and is dropped: no push
    /// ever waits on it, and the stream closes.
    #[test]
    fn a_slow_client_is_dropped_without_stalling() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let Subscribed {
            client,
            queue,
            closed,
            ..
        } = subscribe(&path, 4);
        // Big enough lines that the socket's buffer fills long before the
        // pushes run out.
        let big = Response::ok(Reply::Event {
            event: serde_json::json!("x".repeat(64 * 1024)),
            text: None,
        });
        let started = Instant::now();
        let mut pushed = 0;
        while queue.push(&big) {
            pushed += 1;
            assert!(pushed < 10_000, "the queue never filled");
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a push never waits on the reader: {:?}",
            started.elapsed()
        );
        assert!(!queue.is_open());
        closed
            .recv_timeout(STREAM_WRITE_TIMEOUT * 3)
            .expect("the writer gives up on the reader");
        drop(client);
    }

    /// A client dropped for falling behind reads why on its last line,
    /// after the events it was sent and before the end, so `nd` can tell
    /// that from a stream that ended.
    #[test]
    fn a_dropped_client_reads_why_before_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let Subscribed {
            mut client,
            queue,
            closed,
            ..
        } = subscribe(&path, 4);
        let mut pushed = 0;
        while queue.push(&padded(pushed, 64 * 1024)) {
            pushed += 1;
            assert!(pushed < 10_000, "the queue never filled");
        }

        let mut expected = 0;
        let message = loop {
            let response = client
                .next_response()
                .unwrap()
                .expect("the reason before the end");
            if let Outcome::Error { message } = &response.outcome {
                break message.clone();
            }
            assert_eq!(event_n(&response), expected, "in order, none skipped");
            expected += 1;
        };
        assert_eq!(message, DROPPED);
        assert!(
            expected > 0 && expected < pushed as u64,
            "{expected} of {pushed}"
        );
        assert!(client.next_response().unwrap().is_none(), "then the end");
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    /// A burst pushed with [`EventQueue::try_push`], waiting on `on_room`
    /// whenever the queue is full, reaches a client that reads slowly whole
    /// and in order, though it is many times the queue.
    #[test]
    fn a_back_pressured_burst_reaches_a_slow_reader_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let Subscribed {
            mut client,
            queue,
            room,
            closed,
        } = subscribe(&path, 4);
        const BURST: usize = 40;
        let producer = thread::spawn(move || {
            for n in 0..BURST {
                loop {
                    match queue.try_push(&padded(n, 16 * 1024)) {
                        Pushed::Queued => break,
                        Pushed::Full => room
                            .recv_timeout(Duration::from_secs(5))
                            .expect("the writer makes room"),
                        Pushed::Closed => panic!("the slow reader was dropped at {n}"),
                    }
                }
            }
            // Dropped here: the stream ends once the burst is written.
        });
        for n in 0..BURST {
            thread::sleep(Duration::from_millis(5));
            let response = client.next_response().unwrap().expect("the whole burst");
            assert_eq!(event_n(&response), n as u64);
        }
        assert!(client.next_response().unwrap().is_none(), "then the end");
        producer.join().unwrap();
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_bad_request_is_answered_on_the_listener_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let listener = Listener::bind(&path, || {}).unwrap();

        let mut client = Client::connect(&path).unwrap();
        client.send_line(r#"{"v":9,"type":"list_tools"}"#).unwrap();
        let response = Response::from_line(&client.recv_line().unwrap()).unwrap();
        match response.outcome {
            Outcome::Error { message } => assert!(message.contains("version 9"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert!(
            listener.try_recv().is_none(),
            "nothing for the host to serve"
        );
    }

    #[test]
    fn binding_replaces_a_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let first = Listener::bind(&path, || {}).unwrap();
        let second = Listener::bind(&path, || {}).unwrap();

        let client = thread::spawn({
            let path = path.clone();
            move || Client::connect(&path).unwrap().request(&Request::ListTools)
        });
        let mut incoming = next(&second);
        incoming
            .connection
            .reply(&Response::error("second"))
            .unwrap();
        assert!(client.join().unwrap().is_ok());
        assert!(first.try_recv().is_none(), "the old listener lost the path");
        assert!(
            !first.watch().still_ours(),
            "the first sees it lost the path"
        );
        assert!(second.watch().still_ours());
    }

    #[test]
    fn a_removed_socket_is_no_longer_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let listener = Listener::bind(&path, || {}).unwrap();
        assert!(listener.watch().still_ours());
        std::fs::remove_file(&path).unwrap();
        assert!(!listener.watch().still_ours());
    }

    #[test]
    fn a_connection_resets_the_idle_clock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let listener = Listener::bind(&path, || {}).unwrap();
        let watch = listener.watch();
        thread::sleep(Duration::from_millis(60));
        assert!(watch.idle_for() >= Duration::from_millis(50));

        // A bad request is still somebody using the socket.
        let mut client = Client::connect(&path).unwrap();
        client.send_line("not json").unwrap();
        client.recv_line().unwrap();
        assert!(
            watch.idle_for() < Duration::from_millis(50),
            "{:?}",
            watch.idle_for()
        );
    }

    #[test]
    fn a_probe_tells_a_live_listener_from_a_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notedeck.sock");
        let timeout = Duration::from_secs(2);
        assert!(!crate::probe(&path, timeout), "no file");

        // A file nobody listens on: bound, then the listener dropped.
        drop(UnixListener::bind(&path).unwrap());
        assert!(!crate::probe(&path, timeout), "a stale file");

        let listener = Listener::bind(&path, || {}).unwrap();
        let answer = thread::spawn(move || {
            let mut incoming = next(&listener);
            incoming
                .connection
                .reply(&Response::ok(Reply::Tools { tools: vec![] }))
                .unwrap();
        });
        assert!(crate::probe(&path, timeout), "a live listener");
        answer.join().unwrap();
    }
}
