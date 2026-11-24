//! Bash: `bash -c <command>` as a child process, in the root.
//!
//! On unix the command runs in a process group of its own, so a timeout or
//! a cancel kills everything it started, not just the shell. A command that
//! exits leaving something in the background (`server &`) isn't killed: its
//! answer is what was written by the time the shell exited, after a short
//! wait for the pipes to drain, and the background process keeps running.

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

use super::{ChildEnv, ExecError, OnOutput, STREAM_BUDGET};
use notedeck_agent_proto::call::BashInput;
use notedeck_agent_proto::wire::OutputStream;

/// How long a command may run when the call names no timeout, and the most
/// it may name: Claude Code's two minutes and ten.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;

/// Bytes of a stream's start, and of its end, the answer keeps; what falls
/// between is counted and cut. A test run's verdict is at its end.
const KEEP_HEAD: usize = 15_000;
const KEEP_TAIL: usize = 15_000;

/// How long the pipes may keep draining after the shell has gone, before
/// whatever it left behind is no longer waited for.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(200);

/// One output stream as kept: its head, its tail, and how much fell
/// between.
#[derive(Default)]
struct Captured {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    cut: usize,
}

impl Captured {
    fn push(&mut self, mut bytes: &[u8]) {
        let room = KEEP_HEAD - self.head.len();
        if room > 0 {
            let take = room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend(bytes);
        let over = self.tail.len().saturating_sub(KEEP_TAIL);
        self.tail.drain(..over);
        self.cut += over;
    }

    /// The stream as text, its cut middle marked.
    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if self.cut > 0 {
            text.push_str(&format!("\n[… {} bytes cut …]\n", self.cut));
        }
        let (a, b) = self.tail.as_slices();
        text.push_str(&String::from_utf8_lossy(a));
        text.push_str(&String::from_utf8_lossy(b));
        text
    }
}

/// One stream's live view: hands what the command writes to the watcher as
/// text, holding back a character split across two reads until the rest of
/// it arrives, and stopping at [`STREAM_BUDGET`].
struct Streamer<'a> {
    stream: OutputStream,
    on_output: OnOutput<'a>,
    /// Bytes read but not yet handed over: at most an incomplete character.
    carry: Vec<u8>,
    sent: usize,
}

impl<'a> Streamer<'a> {
    fn new(stream: OutputStream, on_output: OnOutput<'a>) -> Self {
        Self {
            stream,
            on_output,
            carry: Vec::new(),
            sent: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.sent >= STREAM_BUDGET {
            return;
        }
        let bytes = &bytes[..bytes.len().min(STREAM_BUDGET - self.sent)];
        self.sent += bytes.len();
        self.carry.extend_from_slice(bytes);
        let text = take_text(&mut self.carry);
        if !text.is_empty() {
            (self.on_output)(self.stream, &text);
        }
    }
}

/// The text at the front of `bytes`, removed from it: everything but an
/// incomplete character at the very end, which stays for the next read.
/// An invalid sequence anywhere else becomes U+FFFD, as lossy decoding does.
fn take_text(bytes: &mut Vec<u8>) -> String {
    let mut text = String::new();
    let mut rest: &[u8] = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                rest = &[];
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                text.push_str(std::str::from_utf8(valid).expect("valid prefix"));
                let Some(bad) = e.error_len() else {
                    rest = after;
                    break;
                };
                text.push(char::REPLACEMENT_CHARACTER);
                rest = &after[bad..];
            }
        }
    }
    let keep = rest.len();
    bytes.drain(..bytes.len() - keep);
    text
}

/// Read `pipe` to its end into `into`, showing it to `live` as it comes.
async fn drain(pipe: Option<impl AsyncRead + Unpin>, into: &mut Captured, live: &mut Streamer<'_>) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut chunk = [0u8; 8192];
    while let Ok(n @ 1..) = pipe.read(&mut chunk).await {
        into.push(&chunk[..n]);
        live.push(&chunk[..n]);
    }
}

/// How the command's run ended.
enum Ended {
    Exited(std::io::Result<ExitStatus>),
    TimedOut,
    Cancelled,
}

/// Run the call's command in `root` until it exits, its timeout passes or
/// `cancelled` resolves, and answer its output and exit status. What it
/// writes reaches `on_output` as it is written. Its environment is this
/// process's, changed as `env` says.
pub(super) async fn bash(
    root: &Path,
    env: &ChildEnv,
    input: &BashInput,
    cancelled: impl Future<Output = ()>,
    on_output: OnOutput<'_>,
) -> Result<String, ExecError> {
    let timeout_ms = input
        .timeout
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);
    // `/bin/sh`, not `bash`: the guest image only promises a POSIX shell
    // (see agentium-sandbox's `image` module), and `alpine/git` has no bash.
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&input.command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in &env.remove {
        command.env_remove(name);
    }
    command.envs(&env.set);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(ExecError::Spawn)?;

    let mut stdout = Captured::default();
    let mut stderr = Captured::default();
    let mut live_out = Streamer::new(OutputStream::Stdout, on_output);
    let mut live_err = Streamer::new(OutputStream::Stderr, on_output);
    let ended = {
        let out_pipe = child.stdout.take();
        let err_pipe = child.stderr.take();
        let reads = async {
            tokio::join!(
                drain(out_pipe, &mut stdout, &mut live_out),
                drain(err_pipe, &mut stderr, &mut live_err)
            );
        };
        tokio::pin!(reads, cancelled);
        let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(deadline);
        let mut read_all = false;
        let ended = loop {
            tokio::select! {
                () = &mut reads, if !read_all => read_all = true,
                status = child.wait() => break Ended::Exited(status),
                () = &mut deadline => break Ended::TimedOut,
                () = &mut cancelled => break Ended::Cancelled,
            }
        };
        if !matches!(ended, Ended::Exited(_)) {
            kill_group(&mut child).await;
        }
        if !read_all {
            let _ = tokio::time::timeout(DRAIN_AFTER_EXIT, &mut reads).await;
        }
        ended
    };

    match ended {
        Ended::Exited(status) => Ok(answer(&stdout, &stderr, status.map_err(ExecError::Spawn)?)),
        Ended::TimedOut => Err(ExecError::TimedOut {
            ms: timeout_ms,
            output: output(&stdout, &stderr),
        }),
        Ended::Cancelled => Err(ExecError::Cancelled),
    }
}

/// Kill the command and everything it started, and reap the shell.
async fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // The shell leads its own group (`process_group(0)`), so its pid is
        // the group's id. SAFETY: kill(2) takes no pointers.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

/// stdout, then stderr after it when there is any.
fn output(stdout: &Captured, stderr: &Captured) -> String {
    let mut text = stdout.text();
    let err = stderr.text();
    if !err.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    text
}

/// What the model reads of a command that exited: its output, and its exit
/// status when that isn't success.
fn answer(stdout: &Captured, stderr: &Captured, status: ExitStatus) -> String {
    let mut text = output(stdout, stderr);
    let trailer = match status.code() {
        Some(0) => None,
        Some(code) => Some(format!("Exit code {code}")),
        None => Some("Killed by a signal".to_owned()),
    };
    if let Some(trailer) = trailer {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&trailer);
    }
    if text.is_empty() {
        text.push_str("(no output)");
    }
    text
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::future::pending;
    use std::time::Instant;

    fn call(command: &str, timeout: Option<u64>) -> BashInput {
        BashInput {
            command: command.into(),
            description: None,
            timeout,
        }
    }

    async fn run(root: &Path, command: &str) -> Result<String, ExecError> {
        bash(
            root,
            &ChildEnv::default(),
            &call(command, None),
            pending(),
            &|_, _| {},
        )
        .await
    }

    /// stdout and stderr both reach the model, and a failing exit says its
    /// code; the command starts in the root.
    #[tokio::test]
    async fn output_and_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            run(&root, "pwd").await.unwrap().trim(),
            root.display().to_string()
        );
        assert_eq!(
            run(&root, "echo out; echo err >&2; exit 3").await.unwrap(),
            "out\nerr\nExit code 3"
        );
        assert_eq!(run(&root, "true").await.unwrap(), "(no output)");
    }

    /// A command past its timeout is killed with everything it started,
    /// and the answer carries what it wrote first.
    #[tokio::test]
    async fn a_timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let command = format!(
            "echo started; sh -c 'echo $$ > {}; exec sleep 30' & wait",
            pidfile.display()
        );
        let begun = Instant::now();
        let err = bash(
            dir.path(),
            &ChildEnv::default(),
            &call(&command, Some(500)),
            pending(),
            &|_, _| {},
        )
        .await
        .unwrap_err();
        assert!(begun.elapsed() < Duration::from_secs(10));
        let ExecError::TimedOut { ms: 500, output } = err else {
            panic!("expected a timeout, got {err}");
        };
        assert_eq!(output, "started\n");
        let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The grandchild is gone (or a zombie on its way): signal 0 fails.
        let mut gone = false;
        for _ in 0..50 {
            // SAFETY: kill(2) takes no pointers.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.contains(") Z "));
            if !alive || zombie {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(gone, "the backgrounded sleep outlived the timeout");
    }

    /// A cancel stops the command at once.
    #[tokio::test]
    async fn a_cancel_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let begun = Instant::now();
        let cancel = tokio::time::sleep(Duration::from_millis(100));
        let err = bash(
            dir.path(),
            &ChildEnv::default(),
            &call("sleep 30", None),
            cancel,
            &|_, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ExecError::Cancelled));
        assert!(begun.elapsed() < Duration::from_secs(10));
    }

    /// A command that leaves something running in the background answers
    /// when the shell exits, not when the background process does.
    #[tokio::test]
    async fn a_background_process_doesnt_hold_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let begun = Instant::now();
        let out = run(dir.path(), "sleep 5 & echo hi").await.unwrap();
        assert_eq!(out, "hi\n");
        assert!(begun.elapsed() < Duration::from_secs(4));
    }

    /// A flood of output keeps its start and end and counts the middle.
    #[tokio::test]
    async fn a_long_output_keeps_its_ends() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            dir.path(),
            "echo first; head -c 100000 /dev/zero | tr '\\0' x; echo; echo last",
        )
        .await
        .unwrap();
        assert!(out.starts_with("first\n"));
        assert!(out.ends_with("x\nlast\n"));
        let cut = 6 + 100_000 + 1 + 5 - KEEP_HEAD - KEEP_TAIL;
        assert!(
            out.contains(&format!("[… {cut} bytes cut …]")),
            "{out:.200}"
        );
    }

    /// Output reaches the watcher while the command runs, not at exit, on
    /// the stream it was written to.
    #[tokio::test]
    async fn output_streams_while_the_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        let begun = Instant::now();
        let seen = std::sync::Mutex::new(Vec::<(OutputStream, String, Duration)>::new());
        let watch = |stream: OutputStream, text: &str| {
            seen.lock()
                .unwrap()
                .push((stream, text.to_owned(), begun.elapsed()));
        };
        let command = "for i in 1 2 3 4 5; do echo $i; sleep 0.2; done; echo oops >&2";
        let out = bash(
            dir.path(),
            &ChildEnv::default(),
            &call(command, None),
            pending(),
            &watch,
        )
        .await
        .unwrap();
        assert_eq!(out, "1\n2\n3\n4\n5\noops\n");
        let seen = seen.into_inner().unwrap();
        let stdout: String = seen
            .iter()
            .filter(|(s, ..)| *s == OutputStream::Stdout)
            .map(|(_, t, _)| t.as_str())
            .collect();
        assert_eq!(stdout, "1\n2\n3\n4\n5\n");
        assert!(seen
            .iter()
            .any(|(s, t, _)| *s == OutputStream::Stderr && t == "oops\n"));
        let first = seen[0].2;
        let total = begun.elapsed();
        assert!(
            first + Duration::from_millis(500) < total,
            "first output at {first:?} of {total:?}: it waited for exit"
        );
    }

    /// A character split across two reads is held until it is whole, and
    /// a bad byte becomes U+FFFD instead of stalling the stream.
    #[test]
    fn a_split_character_waits_for_its_end() {
        let snowman = "☃".as_bytes();
        let mut carry = snowman[..2].to_vec();
        assert_eq!(take_text(&mut carry), "");
        assert_eq!(carry.len(), 2);
        carry.extend_from_slice(&snowman[2..]);
        carry.extend_from_slice(b"a\xffb");
        assert_eq!(take_text(&mut carry), "☃a\u{fffd}b");
        assert!(carry.is_empty());
    }

    /// The live view stops at its budget; the answer still has both ends.
    #[tokio::test]
    async fn the_live_view_stops_at_its_budget() {
        let dir = tempfile::tempdir().unwrap();
        let streamed = std::sync::atomic::AtomicUsize::new(0);
        let watch = |_: OutputStream, text: &str| {
            streamed.fetch_add(text.len(), std::sync::atomic::Ordering::Relaxed);
        };
        let command = format!(
            "head -c {} /dev/zero | tr '\\0' x; echo; echo last",
            STREAM_BUDGET * 3
        );
        let out = bash(
            dir.path(),
            &ChildEnv::default(),
            &call(&command, None),
            pending(),
            &watch,
        )
        .await
        .unwrap();
        assert!(out.ends_with("x\nlast\n"));
        assert_eq!(streamed.into_inner(), STREAM_BUDGET);
    }
}
