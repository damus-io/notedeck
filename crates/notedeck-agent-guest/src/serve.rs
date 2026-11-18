//! The binary's loop: [`HostMessage`] lines in, [`GuestMessage`] lines out.
//!
//! ```text
//!            ┌──────────── serve (one task) ─────────────┐
//!  stdin ──▶ │ Ping ─▶ Pong                              │ ──▶ stdout
//!            │ Call ─▶ spawn ─▶ Executor::run ─┐ chunks, │
//!            │ Cancel ─▶ in_flight[id].cancel  │ finished│
//!            │                ◀────────────────┘         │
//!            │ Chunk: write it                           │
//!            │ finished: in_flight.remove(id), write it  │
//!            └───────────────────────────────────────────┘
//! ```
//!
//! Every call runs in its own task, so a slow `cargo build` doesn't hold up
//! a Read beside it. The loop alone owns `in_flight`, and a call leaves it
//! only when its answer comes back: a Cancel that arrives mid-run always
//! finds the call it names. (The other order, dropping the entry before the
//! work finishes, turns a mid-run cancel into a no-op that still reports
//! success.)
//!
//! A Bash call's output goes out as [`GuestMessage::Chunk`]s while it runs,
//! on the same channel as its answer and ahead of it, so the host always
//! sees a call's chunks before its result.
//!
//! Shutdown, or the host closing stdin, cancels everything in flight and
//! waits for each answer before returning, so no Bash process group
//! outlives the executor.

use std::collections::HashMap;

use notedeck_agent_proto::call::CodingCall;
use notedeck_agent_proto::wire::{GuestMessage, HostMessage, OutputStream, Version};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::executor::{ExecError, Executor};

/// A call that is running: how to cancel it, until it has been.
struct Running {
    cancel: Option<oneshot::Sender<()>>,
}

impl Running {
    /// Ask the call to stop. A second ask does nothing.
    fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

/// What a call's task sends back to the loop.
enum Event {
    /// Output the call wrote; written out as it is.
    Chunk(GuestMessage),
    /// The call's answer: it leaves `in_flight` and the answer is written.
    Finished { id: u64, answer: GuestMessage },
}

/// Sends a call's answer back to the loop, or, if the task unwinds before
/// it has one, an error in its place, so the call still leaves `in_flight`
/// and a shutdown doesn't wait for it forever.
struct Reply {
    id: u64,
    to_loop: mpsc::UnboundedSender<Event>,
    sent: bool,
}

impl Reply {
    /// Send output the call has written so far.
    fn chunk(&self, stream: OutputStream, data: &str) {
        let _ = self.to_loop.send(Event::Chunk(GuestMessage::Chunk {
            id: self.id,
            stream,
            data: data.to_owned(),
        }));
    }

    fn send(mut self, answer: GuestMessage) {
        self.sent = true;
        let _ = self.to_loop.send(Event::Finished {
            id: self.id,
            answer,
        });
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        let _ = self.to_loop.send(Event::Finished {
            id: self.id,
            answer: GuestMessage::Error {
                id: self.id,
                message: "the executor failed while running this call".to_owned(),
            },
        });
    }
}

/// Serve `input` until the host sends Shutdown or closes it, answering on
/// `output`. `version` is what a Ping is answered with.
///
/// Returns once every call in flight has been answered, or the first error
/// reading `input` or writing `output`. After a write fails the host can't
/// hear answers any more, so the calls in flight are cancelled and waited
/// for before the error is returned.
pub async fn serve<R, W>(
    executor: Executor,
    version: Version,
    input: R,
    mut output: W,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = input.lines();
    let (to_loop, mut events) = mpsc::unbounded_channel::<Event>();
    let mut in_flight: HashMap<u64, Running> = HashMap::new();
    let mut reading = true;
    let mut failure: Option<std::io::Error> = None;

    while reading || !in_flight.is_empty() {
        let reply = tokio::select! {
            line = lines.next_line(), if reading => match line {
                Ok(Some(line)) => {
                    let Some(message) = read_line(&line) else {
                        continue;
                    };
                    match message {
                        Ok(HostMessage::Ping { id }) => Some(GuestMessage::Pong {
                            id,
                            version: version.clone(),
                        }),
                        Ok(HostMessage::Call { id, call }) => {
                            start(&executor, &mut in_flight, &to_loop, id, call)
                        }
                        Ok(HostMessage::Cancel { id }) => {
                            if let Some(running) = in_flight.get_mut(&id) {
                                running.cancel();
                            }
                            None
                        }
                        Ok(HostMessage::Shutdown) => {
                            reading = false;
                            in_flight.values_mut().for_each(Running::cancel);
                            None
                        }
                        Err(refusal) => Some(refusal),
                    }
                }
                Ok(None) => {
                    reading = false;
                    in_flight.values_mut().for_each(Running::cancel);
                    None
                }
                Err(e) => {
                    reading = false;
                    in_flight.values_mut().for_each(Running::cancel);
                    failure.get_or_insert(e);
                    None
                }
            },
            Some(event) = events.recv() => match event {
                Event::Chunk(chunk) => Some(chunk),
                Event::Finished { id, answer } => {
                    in_flight.remove(&id);
                    Some(answer)
                }
            },
        };

        let Some(reply) = reply else {
            continue;
        };
        if failure.is_some() {
            continue;
        }
        if let Err(e) = write(&mut output, &reply).await {
            reading = false;
            in_flight.values_mut().for_each(Running::cancel);
            failure = Some(e);
        }
    }

    failure.map_or(Ok(()), Err)
}

/// One input line as a message, or the answer refusing it; `None` for a
/// blank line, which is skipped.
fn read_line(line: &str) -> Option<Result<HostMessage, GuestMessage>> {
    if line.trim().is_empty() {
        return None;
    }
    let error = match serde_json::from_str::<HostMessage>(line) {
        Ok(message) => return Some(Ok(message)),
        Err(error) => error,
    };
    // A call this build can't read (a tool or argument it doesn't know,
    // say) is still answered by its id, so the host's turn doesn't wait
    // for an answer that never comes.
    let id = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("id")?.as_u64());
    Some(Err(match id {
        Some(id) => GuestMessage::Error {
            id,
            message: format!("the agent executor couldn't read this call: {error}"),
        },
        None => GuestMessage::Malformed {
            message: format!("the agent executor couldn't read a line: {error}"),
        },
    }))
}

/// Start the call `id` in its own task, or refuse it if `id` is already
/// running.
fn start(
    executor: &Executor,
    in_flight: &mut HashMap<u64, Running>,
    to_loop: &mpsc::UnboundedSender<Event>,
    id: u64,
    call: CodingCall,
) -> Option<GuestMessage> {
    if in_flight.contains_key(&id) {
        return Some(GuestMessage::Error {
            id,
            message: format!("a call with id {id} is already running"),
        });
    }
    let (cancel, cancelled) = oneshot::channel::<()>();
    in_flight.insert(
        id,
        Running {
            cancel: Some(cancel),
        },
    );
    let executor = executor.clone();
    let reply = Reply {
        id,
        to_loop: to_loop.clone(),
        sent: false,
    };
    tokio::spawn(async move {
        let cancelled = async move {
            let _ = cancelled.await;
        };
        let on_output = |stream: OutputStream, data: &str| reply.chunk(stream, data);
        let answer = match executor.run_streaming(&call, cancelled, &on_output).await {
            Ok(text) => GuestMessage::Result { id, text },
            Err(ExecError::Cancelled) => GuestMessage::Cancelled { id },
            Err(e) => GuestMessage::Error {
                id,
                message: e.to_string(),
            },
        };
        reply.send(answer);
    });
    None
}

/// Write `message` as one line and flush it, so the host sees it now.
async fn write(
    output: &mut (impl AsyncWrite + Unpin),
    message: &GuestMessage,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).expect("a message serializes");
    line.push(b'\n');
    output.write_all(&line).await?;
    output.flush().await
}
