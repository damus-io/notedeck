//! The listening end, held by the chrome.
//!
//! A background thread accepts connections and reads each one's request line,
//! so the UI thread never waits on a client. A request that doesn't parse is
//! answered there; a good one is handed over as an [`Incoming`], and the host is
//! woken to serve it on its next tick.

use crate::{Request, Response};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long a client gets to send its request line before it's dropped, so a
/// client that connects and says nothing can't hold up the ones behind it.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The socket notedeck listens on, and the requests waiting to be served.
pub struct Listener {
    incoming: mpsc::Receiver<Incoming>,
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
}

impl Connection {
    /// Answer the request with one line. The client reads one, so a second
    /// reply goes nowhere.
    pub fn reply(&mut self, response: &Response) -> io::Result<()> {
        writeln!(self.stream, "{}", response.to_line())?;
        self.stream.flush()
    }
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

        let (sender, incoming) = mpsc::channel();
        thread::Builder::new()
            .name("ipc-listener".to_owned())
            .spawn(move || accept_loop(listener, sender, wake))?;

        Ok(Self { incoming })
    }

    /// The next request waiting to be served, if any. Never blocks.
    pub fn try_recv(&self) -> Option<Incoming> {
        self.incoming.try_recv().ok()
    }
}

/// Accept connections until the [`Listener`] is dropped, handing each good
/// request to it.
fn accept_loop(listener: UnixListener, sender: mpsc::Sender<Incoming>, wake: impl Fn()) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("notedeck ipc: accept failed: {e}");
                continue;
            }
        };
        let mut connection = Connection { stream };
        let request = match read_request(&connection.stream) {
            Ok(request) => request,
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
fn read_request(stream: &UnixStream) -> Result<Request, String> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("reading the request: {e}"))?;
    Request::from_line(&line)
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
    }
}
