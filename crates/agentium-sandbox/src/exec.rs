//! The tool protocol over microsandbox's `exec_stream`: start
//! `notedeck-agent` in a booted guest and talk to it with a
//! [`GuestClient`].
//!
//! ```text
//!   GuestClient ──write_line──▶ ExecSink::write ──vsock──▶ notedeck-agent stdin
//!   GuestClient ◀──next_chunk── ExecEvent::Stdout(Bytes) ◀── notedeck-agent stdout
//!                               ExecEvent::Stderr ─▶ kept, for the error if it dies
//!                               ExecEvent::Exited ─▶ the end of the session
//! ```
//!
//! One exec session per guest carries every call: the client multiplexes
//! by id. `ExecEvent::Stdout` is a byte chunk with no line boundary
//! promised, which is why [`GuestOutput`] hands chunks to a line reassembler
//! instead of reading each as a message.

use std::collections::VecDeque;
use std::io;
use std::path::Path;

use microsandbox::sandbox::exec::{ExecEvent, ExecHandle, ExecSink};
use microsandbox::Sandbox;

use crate::client::{connect, GuestClient, GuestInput, GuestOutput};
use crate::SandboxError;

/// How much of the executor's stderr is kept to explain its exit.
const STDERR_TAIL: usize = 4096;

/// The executor's stdout, from its exec session's events.
pub struct ExecOutput {
    handle: ExecHandle,
    events: Events,
}

/// What [`ExecOutput`] makes of the session's events, apart from the
/// handle so it can be tested without a guest: `ExecHandle` can't be built
/// outside microsandbox, an `ExecEvent` can.
#[derive(Default)]
struct Events {
    /// The end of what the executor wrote to stderr: its panic or its usage
    /// error, kept to explain an exit.
    stderr: VecDeque<u8>,
}

/// What one event means for the stream of stdout chunks.
enum Step {
    /// A piece of stdout.
    Chunk(Vec<u8>),
    /// The stream is over: cleanly (`None`) or with why (`Some`).
    End(Option<io::Error>),
    /// Nothing for the stream; read the next event.
    Skip,
}

impl ExecOutput {
    /// Read the stdout of the exec session `handle`.
    pub fn new(handle: ExecHandle) -> Self {
        Self {
            handle,
            events: Events::default(),
        }
    }
}

impl Events {
    fn step(&mut self, event: ExecEvent) -> Step {
        match event {
            ExecEvent::Stdout(bytes) => Step::Chunk(bytes.to_vec()),
            ExecEvent::Stderr(bytes) => {
                self.stderr.extend(bytes.iter());
                let over = self.stderr.len().saturating_sub(STDERR_TAIL);
                self.stderr.drain(..over);
                Step::Skip
            }
            // A clean exit is the end of the stream; the client knows
            // whether it asked for one.
            ExecEvent::Exited { code: 0 } => Step::End(None),
            ExecEvent::Exited { code } => {
                let message = format!("notedeck-agent exited {code}: {}", self.stderr_text());
                Step::End(Some(io::Error::other(message)))
            }
            ExecEvent::Failed(failed) => {
                let message = format!("notedeck-agent didn't start: {failed:?}");
                Step::End(Some(io::Error::other(message)))
            }
            // A failed stdin write surfaces as the client's own write
            // error; `Started` carries nothing the session needs.
            ExecEvent::Started { .. } | ExecEvent::StdinError(_) => Step::Skip,
        }
    }

    fn stderr_text(&self) -> String {
        let (a, b) = self.stderr.as_slices();
        let mut text = String::from_utf8_lossy(a).into_owned();
        text.push_str(&String::from_utf8_lossy(b));
        text.trim().to_owned()
    }
}

impl GuestOutput for ExecOutput {
    type Chunk = Vec<u8>;

    async fn next_chunk(&mut self) -> Option<io::Result<Vec<u8>>> {
        loop {
            match self.events.step(self.handle.recv().await?) {
                Step::Chunk(bytes) => return Some(Ok(bytes)),
                Step::End(None) => return None,
                Step::End(Some(e)) => return Some(Err(e)),
                Step::Skip => {}
            }
        }
    }
}

impl GuestInput for ExecSink {
    async fn write_line(&mut self, line: Vec<u8>) -> io::Result<()> {
        self.write(line).await.map_err(io::Error::other)
    }

    async fn close(&mut self) -> io::Result<()> {
        ExecSink::close(self).await.map_err(io::Error::other)
    }
}

/// Start the executor at `binary` (its path inside the guest:
/// [`crate::image::AGENT_PATH`] for a guest set up with
/// [`crate::image::GuestSetup`]) in `sandbox`,
/// serving the working tree `root`, and open a session with it. Refused
/// unless the guest is this build ([`crate::guest::HOST_BUILD`]).
pub async fn start(
    sandbox: &Sandbox,
    binary: &str,
    root: &Path,
) -> Result<GuestClient, SandboxError> {
    let root = root.to_string_lossy().into_owned();
    let mut handle = sandbox
        .exec_stream_with(binary, |o| o.args(["--root", root.as_str()]).stdin_pipe())
        .await?;
    let stdin = handle.take_stdin().ok_or(SandboxError::NoStdin)?;
    let client = connect(ExecOutput::new(handle), stdin, crate::guest::HOST_BUILD).await?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// stdout passes through as it came; stderr is kept, not streamed.
    #[test]
    fn stdout_is_the_stream_and_stderr_is_kept() {
        let mut events = Events::default();
        assert!(matches!(
            events.step(ExecEvent::Started { pid: 7 }),
            Step::Skip
        ));
        let Step::Chunk(chunk) = events.step(ExecEvent::Stdout(Bytes::from_static(b"{\"ty")))
        else {
            panic!("stdout wasn't passed on");
        };
        assert_eq!(chunk, b"{\"ty");
        assert!(matches!(
            events.step(ExecEvent::Stderr(Bytes::from_static(b"warming up\n"))),
            Step::Skip
        ));
        assert!(matches!(
            events.step(ExecEvent::Exited { code: 0 }),
            Step::End(None)
        ));
    }

    /// A failing exit says its code and the end of what it wrote to stderr,
    /// which is where a panic message is.
    #[test]
    fn a_failing_exit_carries_its_stderr_tail() {
        let mut events = Events::default();
        let noise = vec![b'.'; STDERR_TAIL * 2];
        events.step(ExecEvent::Stderr(Bytes::from(noise)));
        events.step(ExecEvent::Stderr(Bytes::from_static(
            b"thread 'main' panicked at src/main.rs:1\n",
        )));
        let Step::End(Some(e)) = events.step(ExecEvent::Exited { code: 101 }) else {
            panic!("a failing exit wasn't an error");
        };
        let message = e.to_string();
        assert!(
            message.starts_with("notedeck-agent exited 101: "),
            "{message:.80}"
        );
        assert!(
            message.ends_with("panicked at src/main.rs:1"),
            "{message:.80}"
        );
        assert!(message.len() <= STDERR_TAIL + 40, "{}", message.len());
    }
}
