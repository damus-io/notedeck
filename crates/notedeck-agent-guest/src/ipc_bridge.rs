//! notedeck's socket, served inside a sandbox guest.
//!
//! ```text
//!   guest                                              host
//!   nd ──▶ <socket> (Unix, served here) ══ splice ══▶ vsock CID 2:<port> ──▶ notedeck.sock
//! ```
//!
//! A guest's `127.0.0.1` is the guest, and microsandbox's gateway doesn't
//! forward to the host's loopback, so the guest can't reach notedeck by any
//! address. microsandbox can route a vsock port to a host Unix socket
//! instead (`SandboxBuilder::vsock`), and agentium-sandbox's `ipc` module
//! routes one to notedeck's own socket. This module is the guest's end of
//! that route: [`spawn`] binds a Unix socket in the guest and splices each
//! connection to it onto a fresh vsock stream ([`vsock_connect`]), so `nd`
//! needs no change beyond `$NOTEDECK_SOCKET` naming the guest path.
//!
//! Threads, not tasks: the bridge has to outlive nothing but the process,
//! it carries one short connection per `nd` run, and keeping it off the
//! tokio runtime keeps it from ever competing with the tool calls in flight.
//!
//! A connection is accepted before the host is dialled, so when the host
//! end is down (notedeck not running, or no route) the client's connection
//! ends with no answer rather than being refused: `nd` fails with "notedeck
//! hung up without answering", or a connection reset when its request was
//! still unread, instead of exiting 2.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::thread;

/// How much one direction of a splice moves per read.
const SPLICE_BUF: usize = 16 * 1024;

/// Bind a Unix socket at `socket`, replacing a stale one and creating its
/// directory, and serve it on a background thread: each connection is
/// spliced onto a stream from `connect`, in both directions, until both
/// sides have hung up. A failed `connect` closes that one connection.
pub fn spawn<C>(socket: &Path, connect: C) -> io::Result<thread::JoinHandle<()>>
where
    C: Fn() -> io::Result<OwnedFd> + Send + 'static,
{
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(socket)?;
    thread::Builder::new()
        .name("ipc-bridge".to_owned())
        .spawn(move || accept_loop(listener, connect))
}

/// [`spawn`]'s thread: accept, dial, splice, forever.
fn accept_loop<C>(listener: UnixListener, connect: C)
where
    C: Fn() -> io::Result<OwnedFd>,
{
    for client in listener.incoming() {
        let Ok(client) = client else {
            continue;
        };
        let host = match connect() {
            Ok(host) => host,
            Err(e) => {
                eprintln!("notedeck-agent: ipc bridge: can't reach the host: {e}");
                continue;
            }
        };
        if let Err(e) = splice(OwnedFd::from(client), host) {
            eprintln!("notedeck-agent: ipc bridge: {e}");
        }
    }
}

/// Copy `a` to `b` and `b` to `a` on two threads. Each direction, on
/// reaching its source's end, half-closes its destination's write side, so
/// a peer reading to the end sees it while the other direction carries on.
fn splice(a: OwnedFd, b: OwnedFd) -> io::Result<()> {
    let a_reader = File::from(a.try_clone()?);
    let b_reader = File::from(b.try_clone()?);
    let a_writer = File::from(a);
    let b_writer = File::from(b);
    thread::Builder::new()
        .name("ipc-splice".to_owned())
        .spawn(move || pump(a_reader, b_writer))?;
    thread::Builder::new()
        .name("ipc-splice".to_owned())
        .spawn(move || pump(b_reader, a_writer))?;
    Ok(())
}

/// One direction of a [`splice`]. A plain read/write loop rather than
/// `io::copy`, whose Linux fast paths are for files and pipes, not two
/// sockets of different families.
fn pump(mut from: File, mut to: File) {
    let mut buf = vec![0u8; SPLICE_BUF];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if to.write_all(&buf[..n]).is_err() {
            break;
        }
    }
    // SAFETY: `to` owns a live socket descriptor for this whole call.
    unsafe {
        libc::shutdown(to.as_raw_fd(), libc::SHUT_WR);
    }
}

/// Open a vsock stream to `port` on `cid` (the host is
/// `libc::VMADDR_CID_HOST`, 2).
#[cfg(target_os = "linux")]
pub fn vsock_connect(cid: u32, port: u32) -> io::Result<OwnedFd> {
    use std::os::fd::FromRawFd;

    // SAFETY: plain socket(2); the descriptor is owned at once below.
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };

    // SAFETY: an all-zero sockaddr_vm is valid; the fields that matter are
    // set next.
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = cid;
    addr.svm_port = port;
    // SAFETY: `addr` is a sockaddr_vm of the size passed.
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&addr as *const libc::sockaddr_vm).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    /// A stand-in for the host: a Unix listener that answers each line
    /// with `echo: <line>`, then hangs up when the client half-closes.
    fn echo_host(dir: &Path) -> PathBuf {
        let path = dir.join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let conn = conn.unwrap();
                thread::spawn(move || {
                    let mut writer = conn.try_clone().unwrap();
                    for line in BufReader::new(conn).lines() {
                        writeln!(writer, "echo: {}", line.unwrap()).unwrap();
                    }
                });
            }
        });
        path
    }

    /// Dial the stand-in host the way the real bridge dials vsock.
    fn dial(path: PathBuf) -> impl Fn() -> io::Result<OwnedFd> + Send + 'static {
        move || UnixStream::connect(&path).map(OwnedFd::from)
    }

    /// What `nd` does: one request line, a half-close, read to the end.
    fn request(socket: &Path, line: &str) -> String {
        let mut conn = UnixStream::connect(socket).unwrap();
        writeln!(conn, "{line}").unwrap();
        conn.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = String::new();
        conn.read_to_string(&mut answer).unwrap();
        answer
    }

    #[test]
    fn a_request_crosses_the_bridge_and_its_answer_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let host = echo_host(dir.path());
        let socket = dir.path().join("run/notedeck/notedeck.sock");

        spawn(&socket, dial(host)).unwrap();

        assert_eq!(request(&socket, "ping"), "echo: ping\n");
    }

    /// Each `nd` run is its own connection, and so its own host stream.
    #[test]
    fn connections_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let host = echo_host(dir.path());
        let socket = dir.path().join("guest.sock");
        spawn(&socket, dial(host)).unwrap();

        let threads: Vec<_> = (0..4)
            .map(|i| {
                let socket = socket.clone();
                thread::spawn(move || request(&socket, &format!("call {i}")))
            })
            .collect();
        for (i, t) in threads.into_iter().enumerate() {
            assert_eq!(t.join().unwrap(), format!("echo: call {i}\n"));
        }
    }

    /// A large answer arrives whole: the pump loops, it doesn't stop at
    /// one buffer.
    #[test]
    fn an_answer_bigger_than_the_buffer_arrives_whole() {
        let dir = tempfile::tempdir().unwrap();
        let host = echo_host(dir.path());
        let socket = dir.path().join("guest.sock");
        spawn(&socket, dial(host)).unwrap();

        let big = "x".repeat(SPLICE_BUF * 5 + 17);
        assert_eq!(request(&socket, &big), format!("echo: {big}\n"));
    }

    /// The host being down ends the guest's connection with no answer
    /// (closed, or reset when the request was still unread), and the bridge
    /// keeps serving: once the host is back, the next connection works.
    #[test]
    fn an_unreachable_host_gets_no_answer_and_the_bridge_survives() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let host = echo_host(dir.path());
        let socket = dir.path().join("guest.sock");
        let host_up = std::sync::Arc::new(AtomicBool::new(false));
        let up = host_up.clone();
        let dial_host = dial(host);
        spawn(&socket, move || {
            if !up.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::ConnectionRefused.into());
            }
            dial_host()
        })
        .unwrap();

        let mut conn = UnixStream::connect(&socket).unwrap();
        let _ = writeln!(conn, "ping");
        let mut answer = String::new();
        let read = conn.read_to_string(&mut answer);
        assert!(
            read.is_err() || answer.is_empty(),
            "an answer with no host: {answer:?}"
        );

        host_up.store(true, Ordering::SeqCst);
        assert_eq!(request(&socket, "ping"), "echo: ping\n");
    }

    /// A socket file left by an earlier bridge is replaced, not an error.
    #[test]
    fn a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let host = echo_host(dir.path());
        let socket = dir.path().join("guest.sock");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());

        spawn(&socket, dial(host)).unwrap();

        assert_eq!(request(&socket, "ping"), "echo: ping\n");
    }

    /// The real dial: a vsock listener on this machine, on whatever port
    /// the kernel gives it, reached by [`vsock_connect`] to the host CID
    /// through the bridge. A kernel with no vsock transport (most CI
    /// containers) can't run it, and says so rather than failing.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_bridge_reaches_a_vsock_listener_on_the_host_cid() {
        use std::os::fd::FromRawFd;

        // SAFETY: plain socket(2), owned at once.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            eprintln!("skipped: no AF_VSOCK here: {}", io::Error::last_os_error());
            return;
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let listener = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: zeroed sockaddr_vm is valid; family, CID and port set next.
        let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_cid = libc::VMADDR_CID_ANY;
        addr.svm_port = libc::VMADDR_PORT_ANY;
        let mut len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        // SAFETY: `addr` is a sockaddr_vm of `len` bytes throughout.
        let bound = unsafe {
            libc::bind(
                listener.as_raw_fd(),
                (&addr as *const libc::sockaddr_vm).cast(),
                len,
            ) == 0
                && libc::listen(listener.as_raw_fd(), 4) == 0
                && libc::getsockname(
                    listener.as_raw_fd(),
                    (&mut addr as *mut libc::sockaddr_vm).cast(),
                    &mut len,
                ) == 0
        };
        if !bound {
            eprintln!(
                "skipped: can't listen on vsock: {}",
                io::Error::last_os_error()
            );
            return;
        }
        let port = addr.svm_port;
        if let Err(e) = vsock_connect(libc::VMADDR_CID_HOST, port) {
            eprintln!("skipped: no vsock transport to the host CID: {e}");
            return;
        }
        // The probe above took the first accept; answer the next one.
        thread::spawn(move || {
            for _ in 0..2 {
                // SAFETY: accept(2) on a live listener; owned at once.
                let conn = unsafe {
                    libc::accept(
                        listener.as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                assert!(conn >= 0, "{}", io::Error::last_os_error());
                // SAFETY: a fresh descriptor nothing else owns.
                let conn = File::from(unsafe { OwnedFd::from_raw_fd(conn) });
                let mut writer = conn.try_clone().unwrap();
                if let Some(Ok(line)) = BufReader::new(conn).lines().next() {
                    writeln!(writer, "host: {line}").unwrap();
                }
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("n.sock");
        spawn(&socket, move || vsock_connect(libc::VMADDR_CID_HOST, port)).unwrap();

        assert_eq!(request(&socket, "ping"), "host: ping\n");
    }
}
