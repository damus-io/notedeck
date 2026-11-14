//! The `notedeck --serve` binary `nd` starts, run for real: it leaves the
//! socket to a notedeck that answers on it, exits once its socket goes idle,
//! and exits when another notedeck takes the socket.
//!
//! Each run gets its own data dir and its own `NOTEDECK_SOCKET` under /tmp,
//! so no test touches the user's notedeck or its socket. `--testrunner` keeps
//! it off the network and the keyring.
#![cfg(unix)]

use notedeck_ipc::{Listener, Reply, Response};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// How long a test waits on the binary, which boots a debug notedeck.
const PATIENCE: Duration = Duration::from_secs(60);

/// *This* worktree's build of the notedeck binary (see
/// [`bin_testing::worktree_bin`]). It's not run to check it: with no
/// arguments it would open a window.
fn notedeck_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_notedeck")),
        "notedeck_chrome",
        |_| true,
    )
}

/// A short temp dir for the socket: a Unix socket path has to fit in about
/// 100 bytes.
fn socket_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("nds")
        .tempdir_in("/tmp")
        .or_else(|_| tempfile::tempdir())
        .expect("temp dir")
}

/// `notedeck --serve` on `socket`, with its data in `data`, exiting after
/// `idle_exit` seconds without a request.
fn serve(socket: &Path, data: &Path, idle_exit: u64) -> Child {
    Command::new(notedeck_bin())
        .args(["--testrunner", "--serve", "--datapath"])
        .arg(data)
        .args(["--idle-exit", &idle_exit.to_string()])
        .env("NOTEDECK_SOCKET", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start notedeck")
}

/// Wait for `child` to exit, or panic after [`PATIENCE`].
fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("notedeck --serve didn't exit within {PATIENCE:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until a notedeck answers on `socket`.
fn wait_served(socket: &Path, child: &mut Child) {
    let deadline = Instant::now() + PATIENCE;
    while !notedeck_ipc::probe(socket, Duration::from_secs(2)) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("notedeck --serve exited ({status}) before it served");
        }
        assert!(
            Instant::now() < deadline,
            "notedeck --serve didn't serve within {PATIENCE:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The socket file's inode, to tell one bind from another.
fn inode(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().ino()
}

/// With a notedeck answering on the socket, a serve leaves it be and exits.
#[test]
fn a_serve_never_takes_a_live_socket() {
    let dir = socket_dir();
    let socket = dir.path().join("s.sock");
    let data = TempDir::new().unwrap();
    let running = Listener::bind(&socket, || {}).unwrap();
    let answering = std::thread::spawn(move || {
        // Answer the serve's probe, as a running notedeck would.
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Some(mut incoming) = running.try_recv() {
                let tools = Response::ok(Reply::Tools { tools: vec![] });
                incoming.connection.reply(&tools).unwrap();
                return running;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the serve never probed");
    });
    let before = inode(&socket);

    let mut child = serve(&socket, data.path(), 600);
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    let running = answering.join().unwrap();
    assert_eq!(inode(&socket), before, "the serve replaced the socket");
    assert!(running.watch().still_ours());
}

/// A serve with nobody calling it exits after its idle time.
#[test]
fn a_serve_exits_when_idle() {
    let dir = socket_dir();
    let socket = dir.path().join("s.sock");
    let data = TempDir::new().unwrap();

    let mut child = serve(&socket, data.path(), 2);
    wait_served(&socket, &mut child);
    let served = Instant::now();
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    assert!(
        served.elapsed() >= Duration::from_secs(1),
        "exited {:?} after its last request, before its idle time",
        served.elapsed()
    );
}

/// A notedeck that starts later takes the socket, and the serve exits.
#[test]
fn a_serve_exits_when_its_socket_is_taken() {
    let dir = socket_dir();
    let socket = dir.path().join("s.sock");
    let data = TempDir::new().unwrap();

    let mut child = serve(&socket, data.path(), 600);
    wait_served(&socket, &mut child);
    let _gui = Listener::bind(&socket, || {}).unwrap();
    let taken = Instant::now();
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    assert!(
        taken.elapsed() < Duration::from_secs(30),
        "took {:?} to notice",
        taken.elapsed()
    );
}
