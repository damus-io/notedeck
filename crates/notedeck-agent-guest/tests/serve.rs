//! The executor's stdio loop, driven over an in-memory pipe the way a host
//! drives it over a guest's stdio.
//!
//! The cancel tests assert on elapsed time, not only on the answer: a cancel
//! that kills the shell but not the `sleep` it forked leaves the pipe open
//! and the call running to the end, and still answers "cancelled".
//!
//! Bash's process-group cancel is unix-only, so the tests that run a
//! command are too.
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

use std::time::{Duration, Instant};

use notedeck_agent_guest::{serve, Executor};
use notedeck_agent_proto::call::{BashInput, CodingCall, ReadInput, WriteInput};
use notedeck_agent_proto::wire::{GuestMessage, HostMessage, Version, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::task::JoinHandle;

/// Long enough that a call still running is unmistakable, short enough that
/// a broken cancel fails the test rather than hanging it.
#[cfg(unix)]
const SLOW: &str = "sleep 30; echo finished";

/// How long a cancelled or shut-down call may take to answer.
const PROMPTLY: Duration = Duration::from_secs(5);

/// The host's end of a serving executor.
struct Host {
    to_guest: DuplexStream,
    from_guest: Lines<BufReader<DuplexStream>>,
    served: JoinHandle<std::io::Result<()>>,
    root: tempfile::TempDir,
}

impl Host {
    /// An executor serving a fresh temp dir, built as "test-build".
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (to_guest, guest_in) = tokio::io::duplex(1 << 20);
        let (guest_out, from_guest) = tokio::io::duplex(1 << 20);
        let served = tokio::spawn(serve(
            Executor::new(root.path()),
            Version::new("test-build"),
            BufReader::new(guest_in),
            guest_out,
        ));
        Self {
            to_guest,
            from_guest: BufReader::new(from_guest).lines(),
            served,
            root,
        }
    }

    async fn send_line(&mut self, line: &str) {
        self.to_guest.write_all(line.as_bytes()).await.unwrap();
        self.to_guest.write_all(b"\n").await.unwrap();
    }

    async fn send(&mut self, message: &HostMessage) {
        self.send_line(&serde_json::to_string(message).unwrap())
            .await;
    }

    /// The next message, failing the test if none comes within [`PROMPTLY`].
    async fn recv(&mut self) -> GuestMessage {
        let line = tokio::time::timeout(PROMPTLY, self.from_guest.next_line())
            .await
            .expect("an answer within the deadline")
            .unwrap()
            .expect("the executor still writing");
        serde_json::from_str(&line).unwrap()
    }

    /// The executor's return, once it has stopped.
    async fn finished(self) -> std::io::Result<()> {
        tokio::time::timeout(PROMPTLY, self.served)
            .await
            .expect("serve returns within the deadline")
            .unwrap()
    }
}

#[cfg(unix)]
fn bash(command: &str) -> CodingCall {
    CodingCall::Bash(BashInput {
        command: command.into(),
        description: None,
        timeout: None,
    })
}

/// Give a just-sent Bash call time to be running before it's cancelled.
#[cfg(unix)]
async fn let_it_start() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ping_answers_the_build() {
    let mut host = Host::start();
    host.send(&HostMessage::Ping { id: 0 }).await;
    let GuestMessage::Pong { id: 0, version } = host.recv().await else {
        panic!("expected a pong");
    };
    assert_eq!(version.protocol, PROTOCOL_VERSION);
    assert_eq!(version.check("test-build"), Ok(()));
    assert!(version.check("another-build").is_err());
}

/// A file written by one call is read back by the next, relative to the
/// root, and a call's answer carries its id.
#[tokio::test(flavor = "multi_thread")]
async fn calls_run_against_the_root() {
    let mut host = Host::start();
    let write = CodingCall::Write(WriteInput {
        file_path: "src/a.rs".into(),
        content: "fn a() {}\n".into(),
    });
    host.send(&HostMessage::Call { id: 1, call: write }).await;
    assert!(matches!(
        host.recv().await,
        GuestMessage::Result { id: 1, .. }
    ));

    let read = CodingCall::Read(ReadInput {
        file_path: "src/a.rs".into(),
        offset: None,
        limit: None,
    });
    host.send(&HostMessage::Call { id: 2, call: read }).await;
    assert_eq!(
        host.recv().await,
        GuestMessage::Result {
            id: 2,
            text: "     1\tfn a() {}\n".into()
        }
    );
}

/// A cancel stops a running command and everything it forked, promptly.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_stops_a_running_command_promptly() {
    let mut host = Host::start();
    let started = Instant::now();
    // The grandchild writes its pid and becomes the sleep, so the test can
    // see whether the cancel reached past the shell.
    let command = "sh -c 'echo $$ > grandchild; exec sleep 30' & wait; echo finished";
    host.send(&HostMessage::Call {
        id: 1,
        call: bash(command),
    })
    .await;
    let_it_start().await;
    host.send(&HostMessage::Cancel { id: 1 }).await;
    assert_eq!(host.recv().await, GuestMessage::Cancelled { id: 1 });
    assert!(
        started.elapsed() < PROMPTLY,
        "the cancel answered after {:?}: the command ran on",
        started.elapsed()
    );
    let pid = std::fs::read_to_string(host.root.path().join("grandchild")).unwrap();
    assert!(
        gone(pid.trim().parse().unwrap()).await,
        "the forked sleep outlived the cancel"
    );
}

/// Whether process `pid` is gone (or a zombie on its way) within a second.
#[cfg(unix)]
async fn gone(pid: libc::pid_t) -> bool {
    for _ in 0..50 {
        // SAFETY: kill(2) takes no pointers; signal 0 only probes.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| stat.contains(") Z "));
        if !alive || zombie {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Calls in flight don't wait for each other: a Read beside a slow command
/// answers first, and a cancel reaches the slow one while the other runs.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn calls_in_flight_run_side_by_side() {
    let mut host = Host::start();
    host.send(&HostMessage::Call {
        id: 1,
        call: bash(SLOW),
    })
    .await;
    host.send(&HostMessage::Call {
        id: 2,
        call: bash("echo quick"),
    })
    .await;
    assert!(
        matches!(host.recv().await, GuestMessage::Result { id: 2, ref text } if text.contains("quick"))
    );
    host.send(&HostMessage::Cancel { id: 1 }).await;
    assert_eq!(host.recv().await, GuestMessage::Cancelled { id: 1 });
}

/// A cancel for a call that has finished, or never ran, raced its answer:
/// nothing is written for it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_late_cancel_is_ignored() {
    let mut host = Host::start();
    host.send(&HostMessage::Call {
        id: 1,
        call: bash("true"),
    })
    .await;
    assert!(matches!(
        host.recv().await,
        GuestMessage::Result { id: 1, .. }
    ));
    host.send(&HostMessage::Cancel { id: 1 }).await;
    host.send(&HostMessage::Cancel { id: 99 }).await;
    host.send(&HostMessage::Ping { id: 2 }).await;
    assert!(matches!(
        host.recv().await,
        GuestMessage::Pong { id: 2, .. }
    ));
}

/// A second call under an id still running is refused, and the first runs
/// on undisturbed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_running_id_is_not_reused() {
    let mut host = Host::start();
    host.send(&HostMessage::Call {
        id: 1,
        call: bash(SLOW),
    })
    .await;
    host.send(&HostMessage::Call {
        id: 1,
        call: bash("echo second"),
    })
    .await;
    assert!(matches!(
        host.recv().await,
        GuestMessage::Error { id: 1, .. }
    ));
    host.send(&HostMessage::Cancel { id: 1 }).await;
    assert_eq!(host.recv().await, GuestMessage::Cancelled { id: 1 });
}

/// A call this build can't read is answered by its id, so the host isn't
/// left waiting; a line with no id is answered as malformed.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_line_is_answered() {
    let mut host = Host::start();
    host.send_line(r#"{"type":"call","id":7,"call":{"name":"NotebookEdit","input":{}}}"#)
        .await;
    let GuestMessage::Error { id: 7, message } = host.recv().await else {
        panic!("expected an error for call 7");
    };
    assert!(message.contains("NotebookEdit"), "{message}");

    host.send_line("").await;
    host.send_line("not json").await;
    assert!(matches!(host.recv().await, GuestMessage::Malformed { .. }));
}

/// Shutdown cancels what's in flight, answers it, and returns: no process
/// group outlives the executor.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_cancels_what_is_in_flight() {
    let mut host = Host::start();
    let started = Instant::now();
    host.send(&HostMessage::Call {
        id: 1,
        call: bash(SLOW),
    })
    .await;
    let_it_start().await;
    host.send(&HostMessage::Shutdown).await;
    assert_eq!(host.recv().await, GuestMessage::Cancelled { id: 1 });
    host.finished().await.unwrap();
    assert!(started.elapsed() < PROMPTLY);
}

/// The host closing the executor's stdin is a shutdown.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn closing_stdin_shuts_down() {
    let mut host = Host::start();
    host.send(&HostMessage::Call {
        id: 1,
        call: bash(SLOW),
    })
    .await;
    let_it_start().await;
    host.to_guest.shutdown().await.unwrap();
    assert_eq!(host.recv().await, GuestMessage::Cancelled { id: 1 });
    host.finished().await.unwrap();
}
