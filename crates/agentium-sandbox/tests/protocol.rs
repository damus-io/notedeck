//! The host's client against the real guest loop (`notedeck_agent_guest::
//! serve`), over in-memory pipes, with the guest's output cut into chunks
//! that ignore line boundaries the way `ExecEvent::Stdout` does.
//!
//! No microVM here (most CI runners have no KVM), but everything above the
//! transport is the code a guest runs: the same serve loop, executor and
//! process-group cancel, and the same client. `src/exec.rs` only adapts
//! microsandbox's events to the client's two transport traits.
//!
//! The cancel tests assert on elapsed time: a cancel that kills the shell
//! but not what it forked leaves the pipe open and the call running, and
//! still answers "cancelled".
#![cfg(unix)]

use std::io;
use std::time::{Duration, Instant};

use agentium_sandbox::client::{
    connect, connect_with, CallError, CallEvent, ConnectError, ConnectOptions, GuestClient,
    GuestOutput, Pipe,
};
use notedeck_agent_guest::{serve, Executor};
use notedeck_agent_proto::call::{BashInput, CodingCall, ReadInput};
use notedeck_agent_proto::wire::{HandshakeError, OutputStream, Version};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// The build the test guests claim to be.
const BUILD: &str = "test-build";

/// How long a cancelled call may take to answer.
const PROMPTLY: Duration = Duration::from_secs(5);

/// The guest's stdout read in pieces whose sizes cycle through `sizes`,
/// so lines arrive split, merged and one byte at a time.
struct Choppy {
    pipe: DuplexStream,
    sizes: &'static [usize],
    next: usize,
}

impl GuestOutput for Choppy {
    type Chunk = Vec<u8>;

    async fn next_chunk(&mut self) -> Option<io::Result<Vec<u8>>> {
        let size = self.sizes[self.next % self.sizes.len()];
        self.next += 1;
        let mut buf = vec![0u8; size];
        match self.pipe.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some(Ok(buf))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

/// Odd sizes, a one, and a big one: no chunk lines up with a message.
const RAGGED: &[usize] = &[1, 7, 3, 4096, 2, 13, 65536];

/// A guest serving a temp dir, and the host's session with it.
struct Session {
    client: GuestClient,
    root: tempfile::TempDir,
}

/// Serve a fresh temp dir with the real guest loop, built as `build`.
async fn session_with(build: &str, sizes: &'static [usize]) -> Result<Session, ConnectError> {
    let root = tempfile::tempdir().unwrap();
    let (host_out, guest_in) = tokio::io::duplex(1 << 16);
    let (guest_out, host_in) = tokio::io::duplex(1 << 16);
    tokio::spawn(serve(
        Executor::new(root.path()),
        Version::new(build),
        BufReader::new(guest_in),
        guest_out,
    ));
    let output = Choppy {
        pipe: host_in,
        sizes,
        next: 0,
    };
    let client = connect(output, Pipe(host_out), BUILD).await?;
    Ok(Session { client, root })
}

async fn session() -> Session {
    session_with(BUILD, RAGGED).await.unwrap()
}

fn bash(command: &str) -> CodingCall {
    CodingCall::Bash(BashInput {
        command: command.into(),
        description: None,
        timeout: None,
    })
}

/// Whether process `pid` is gone (or a zombie on its way) within a second.
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

/// The pid a test command wrote to `name` in the root, once it has.
async fn pid_in(root: &std::path::Path, name: &str) -> libc::pid_t {
    for _ in 0..100 {
        if let Ok(pid) = std::fs::read_to_string(root.join(name)) {
            if let Ok(pid) = pid.trim().parse() {
                return pid;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{name} was never written");
}

/// The session opens against the build the host expects, and not another.
#[tokio::test(flavor = "multi_thread")]
async fn the_handshake_holds_the_guest_to_this_build() {
    let s = session().await;
    assert_eq!(s.client.version().build, BUILD);
    assert_eq!(s.client.ping().await.unwrap().build, BUILD);

    let Err(err) = session_with("someone-elses-build", RAGGED).await else {
        panic!("a guest of another build was accepted");
    };
    assert!(
        matches!(err, ConnectError::Handshake(HandshakeError::Build { .. })),
        "{err}"
    );
    assert!(
        err.to_string().contains("scripts/build-guest-agent"),
        "{err}"
    );
}

/// A guest that never answers fails the handshake on its deadline instead
/// of hanging the host.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_guest_times_out() {
    let (host_out, _guest_in) = tokio::io::duplex(1024);
    let (_guest_out, host_in) = tokio::io::duplex(1024);
    let options = ConnectOptions {
        handshake_timeout: Duration::from_millis(200),
        ..ConnectOptions::default()
    };
    let started = Instant::now();
    let Err(err) = connect_with(Pipe(host_in), Pipe(host_out), BUILD, options).await else {
        panic!("connected to nothing");
    };
    assert!(matches!(err, ConnectError::Timeout(_)), "{err}");
    assert!(started.elapsed() < PROMPTLY);
}

/// Three calls on one session run side by side: together they take about
/// as long as one, not three.
#[tokio::test(flavor = "multi_thread")]
async fn calls_on_one_session_run_side_by_side() {
    let s = session().await;
    let started = Instant::now();
    let calls = [1, 2, 3].map(|n| s.client.call(bash(&format!("sleep 0.5; echo {n}"))));
    let ids: Vec<u64> = calls.iter().map(|c| c.id()).collect();
    assert_eq!(ids.len(), 3);
    let mut answers = Vec::new();
    for call in calls {
        answers.push(call.result().await.unwrap());
    }
    let elapsed = started.elapsed();
    assert_eq!(answers, ["1\n", "2\n", "3\n"]);
    assert!(
        elapsed < Duration::from_millis(1300),
        "three 500 ms calls took {elapsed:?}: they ran one after another"
    );
}

/// A command's output reaches the host while it runs, spread over its
/// run, and adds up to the answer.
#[tokio::test(flavor = "multi_thread")]
async fn output_streams_in_while_the_command_runs() {
    let s = session().await;
    let started = Instant::now();
    let mut call = s
        .client
        .call(bash("for i in 1 2 3 4 5; do echo tick $i; sleep 0.2; done"));
    let mut arrivals = Vec::new();
    let mut streamed = String::new();
    let answer = loop {
        match call.next().await.expect("an event") {
            CallEvent::Output {
                stream: OutputStream::Stdout,
                data,
            } => {
                arrivals.push(started.elapsed());
                streamed.push_str(&data);
            }
            CallEvent::Output { stream, data } => panic!("unexpected {stream:?} {data:?}"),
            CallEvent::Done(result) => break result.unwrap(),
        }
    };
    let total = started.elapsed();
    assert_eq!(streamed, answer);
    assert!(
        arrivals.len() >= 3,
        "only {} chunks: {arrivals:?}",
        arrivals.len()
    );
    assert!(
        arrivals[0] + Duration::from_millis(500) < total,
        "the first output came at {:?} of {total:?}: it waited for the exit",
        arrivals[0]
    );
    assert!(call.next().await.is_none(), "an event after Done");
}

/// A 512 KB single-line answer survives the trip byte for byte, read one
/// byte at a time in places and 8 KB in others.
#[tokio::test(flavor = "multi_thread")]
async fn a_large_answer_arrives_byte_exact() {
    const SIZES: &[usize] = &[8192, 1, 13, 8192, 3];
    let s = session_with(BUILD, SIZES).await.unwrap();
    // 2000 characters is the longest line Read returns whole; 262 of them
    // make a 512 KB answer, sent as one JSON line.
    let line: String = (0..2000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    let file: String = (0..262).map(|_| format!("{line}\n")).collect();
    std::fs::write(s.root.path().join("big.txt"), &file).unwrap();
    let read = CodingCall::Read(ReadInput {
        file_path: "big.txt".into(),
        offset: None,
        limit: None,
    });

    let started = Instant::now();
    let over_the_wire = s.client.call(read.clone()).result().await.unwrap();
    let elapsed = started.elapsed();
    let direct = Executor::new(s.root.path())
        .run(&read, std::future::pending())
        .await
        .unwrap();
    assert!(direct.len() > 512 * 1024, "{}", direct.len());
    assert!(over_the_wire == direct, "the answer changed in transit");
    assert!(elapsed < PROMPTLY, "{elapsed:?}");
}

/// Cancelling a 30-second command answers promptly and kills what it
/// forked, not only the shell.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_stops_a_long_call_promptly() {
    let s = session().await;
    let call = s.client.call(bash(
        "sh -c 'echo $$ > grandchild; exec sleep 30' & wait; echo finished",
    ));
    let pid = pid_in(s.root.path(), "grandchild").await;
    let started = Instant::now();
    call.cancel();
    assert_eq!(call.result().await, Err(CallError::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the cancel took {:?}: the command ran on",
        started.elapsed()
    );
    assert!(gone(pid).await, "the forked sleep outlived the cancel");
}

/// Dropping a call the host no longer wants cancels it in the guest, so an
/// abandoned build doesn't run on.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_call_cancels_it() {
    let s = session().await;
    let call = s
        .client
        .call(bash("sh -c 'echo $$ > dropped; exec sleep 30' & wait"));
    let pid = pid_in(s.root.path(), "dropped").await;
    drop(call);
    assert!(gone(pid).await, "the dropped call's command ran on");
    // The session is still good for the next call.
    assert_eq!(
        s.client.call(bash("echo after")).result().await.unwrap(),
        "after\n"
    );
}

/// Shutting down cancels what is in flight and ends the session.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_answers_what_is_in_flight() {
    let s = session().await;
    let call = s.client.call(bash("sleep 30"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    s.client.shutdown(PROMPTLY).await;
    assert_eq!(call.result().await, Err(CallError::Cancelled));
    assert!(started.elapsed() < PROMPTLY);
}

/// A guest that goes away fails every call waiting on it with why, and
/// later calls fail at once instead of hanging.
#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_goes_away_fails_its_calls() {
    let (host_out, mut guest_in) = tokio::io::duplex(1 << 16);
    let (mut guest_out, host_in) = tokio::io::duplex(1 << 16);
    // A hand-rolled guest: answers the handshake, takes one call, and dies
    // partway through answering it.
    let guest = tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut buf = [0u8; 4096];
        let mut answered_ping = false;
        loop {
            let n = guest_in.read(&mut buf).await.unwrap();
            seen.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&seen);
            if !answered_ping && text.contains("\"ping\"") {
                let pong = serde_json::json!({"type": "pong", "id": 0,
                    "version": Version::new(BUILD)});
                guest_out
                    .write_all(format!("{pong}\n").as_bytes())
                    .await
                    .unwrap();
                answered_ping = true;
            }
            if text.contains("\"call\"") {
                guest_out
                    .write_all(br#"{"type":"result","id":1,"te"#)
                    .await
                    .unwrap();
                return;
            }
        }
    });
    let client = connect(Pipe(host_in), Pipe(host_out), BUILD).await.unwrap();
    let call = client.call(bash("echo never"));
    guest.await.unwrap();
    let Err(CallError::Disconnected(why)) = call.result().await else {
        panic!("the call outlived its guest");
    };
    assert!(why.contains("partway through a message"), "{why}");
    assert!(client.closed().is_some());

    let Err(CallError::Disconnected(_)) = client.call(bash("echo late")).result().await else {
        panic!("a call on a closed session didn't fail");
    };
}

/// An answer longer than the host reads fails that call alone; the
/// session and the calls beside it carry on.
#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_answer_fails_only_its_call() {
    let root = tempfile::tempdir().unwrap();
    let (host_out, guest_in) = tokio::io::duplex(1 << 16);
    let (guest_out, host_in) = tokio::io::duplex(1 << 16);
    tokio::spawn(serve(
        Executor::new(root.path()),
        Version::new(BUILD),
        BufReader::new(guest_in),
        guest_out,
    ));
    let options = ConnectOptions {
        max_line: 4096,
        ..ConnectOptions::default()
    };
    let client = connect_with(Pipe(host_in), Pipe(host_out), BUILD, options)
        .await
        .unwrap();

    let lines: String = (0..10).map(|_| format!("{}\n", "x".repeat(1000))).collect();
    std::fs::write(root.path().join("big.txt"), lines).unwrap();
    let big = client.call(CodingCall::Read(ReadInput {
        file_path: "big.txt".into(),
        offset: None,
        limit: None,
    }));
    let Err(CallError::TooLarge { len, max: 4096 }) = big.result().await else {
        panic!("an oversized answer wasn't refused");
    };
    assert!(len > 4096, "{len}");
    assert_eq!(
        client.call(bash("echo still here")).result().await.unwrap(),
        "still here\n"
    );
}
