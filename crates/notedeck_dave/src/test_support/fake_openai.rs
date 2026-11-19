//! A fake OpenAI-compatible endpoint on a local port, for tests.
//!
//! A test that drives a real request (the native loop's backend, a
//! sub-agent's job) points its [`Endpoint`] at a [`FakeOpenAi`] and gets
//! back the raw request it sent, so the test is the reply it wants and the
//! asserts on what went out:
//!
//! ```text
//! streaming(chunks)  read request ─▶ 200 text/event-stream, one `data:` per chunk, `[DONE]`
//! completion(body)   read request ─▶ 200 application/json, `body`
//! silent()           read request ─▶ drop the connection
//! hanging()          never read: every request stays in flight
//! ```
//!
//! Each answering kind serves exactly one request, on a thread, and
//! [`FakeOpenAi::request`] joins it.

use agentium_core::agent_spec::provider::Endpoint;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

/// An OpenAI-compatible endpoint on `127.0.0.1` that serves one request the
/// way it was built to, or holds every request in flight
/// ([`FakeOpenAi::hanging`]).
///
/// Keep it alive for as long as the request under test runs: dropping a
/// hanging one closes its port, which turns "in flight" into "refused".
pub struct FakeOpenAi {
    addr: SocketAddr,
    base_url: String,
    server: Server,
}

/// What serves a [`FakeOpenAi`]'s port.
enum Server {
    /// A thread serving one request, joining to the raw request it read.
    OneRequest(JoinHandle<String>),
    /// The listener, held and never accepted from. The kernel completes
    /// each connection into its backlog, so the client's request is sent
    /// and its reply never comes.
    Hanging { _listener: TcpListener },
}

/// How one served request is answered, after it is read.
enum Reply {
    /// An HTTP response, written whole.
    Http(String),
    /// None: the connection is dropped.
    Drop,
}

impl FakeOpenAi {
    /// A streamed chat completion: each of `chunks` as one SSE `data:`
    /// event, then `data: [DONE]`.
    pub fn streaming(chunks: Vec<Value>) -> Self {
        let mut response = String::from(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        );
        for chunk in &chunks {
            response.push_str(&format!("data: {chunk}\n\n"));
        }
        response.push_str("data: [DONE]\n\n");
        Self::serve_one(Reply::Http(response))
    }

    /// A whole chat completion: `body` as JSON.
    pub fn completion(body: Value) -> Self {
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        Self::serve_one(Reply::Http(response))
    }

    /// An endpoint that reads one request and drops the connection without
    /// answering: for a test that asserts only on what was sent.
    pub fn silent() -> Self {
        Self::serve_one(Reply::Drop)
    }

    /// An endpoint that never answers, so every request to it stays in
    /// flight until the client gives up or is stopped. It has no request to
    /// hand back: [`Self::request`] panics.
    pub fn hanging() -> Self {
        let listener = bind();
        let addr = listener.local_addr().unwrap();
        Self::at(
            addr,
            Server::Hanging {
                _listener: listener,
            },
        )
    }

    /// Its `http://127.0.0.1:<port>/v1` base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The local address it listens on, for a test that wants a base URL
    /// with another path.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A keyless `local` provider's endpoint at [`Self::base_url`], asking
    /// for `model`. Struct-update it for another provider or a key.
    pub fn endpoint(&self, model: &str) -> Endpoint {
        Endpoint {
            provider: "local".into(),
            base_url: self.base_url.clone(),
            api_key: None,
            model: model.into(),
        }
    }

    /// The raw request it was sent, head and body, once served. Blocks until
    /// it has been: an async test runs it in `spawn_blocking`.
    pub fn request(self) -> String {
        match self.server {
            Server::OneRequest(server) => server.join().expect("the fake endpoint read a request"),
            Server::Hanging { .. } => panic!("a hanging FakeOpenAi never reads a request"),
        }
    }

    /// Serve one request on a thread, answering it with `reply`.
    fn serve_one(reply: Reply) -> Self {
        let listener = bind();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request(&mut conn);
            if let Reply::Http(response) = reply {
                conn.write_all(response.as_bytes()).unwrap();
            }
            request
        });
        Self::at(addr, Server::OneRequest(server))
    }

    fn at(addr: SocketAddr, server: Server) -> Self {
        Self {
            addr,
            base_url: format!("http://{addr}/v1"),
            server,
        }
    }
}

/// A listener on a free local port.
fn bind() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}

/// The request on `conn`, head and body. A chat request's body is JSON, so
/// it is in once the bytes read end with its last brace.
fn read_request(conn: &mut TcpStream) -> String {
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).trim_end().ends_with('}') {
        let n = conn.read(&mut buf).unwrap();
        assert!(n > 0, "the connection closed before the body arrived");
        seen.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&seen).into_owned()
}
