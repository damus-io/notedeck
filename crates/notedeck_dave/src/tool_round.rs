//! One round of a model's tool calls, answered without blocking the frame.
//!
//! A backend that drives the model while Dave runs the tools (the native
//! OpenAI-compatible loop) hands Dave a round of tool calls; Dave answers
//! every one, and the answers go back to the model as the next request. Each
//! call is *started* on the frame thread ([`StartedCall`]): a fast host tool,
//! such as an ndb query, answers right there, and a slow one hands back a
//! [`ToolJob`] that runs on a thread of its own.
//!
//! What a round guarantees, stated here so it doesn't depend on tools
//! having been synchronous:
//!
//! - **All of a round's calls run at once**, and a session has at most one
//!   round outstanding: the model can't ask for its next round until it has
//!   this one's answers.
//! - **The answers enter the chat together, in the order the model made the
//!   calls**, never one at a time and never in the order they finished. A
//!   transcript, and a session reconstructed from it, has the same rows a
//!   synchronous round would have written.
//! - **Every call is answered exactly once.** A job that panics, or that
//!   can't get a thread, answers with an error. A cancelled round answers
//!   each call still running with an error.
//! - **Cancelling reaches the running jobs** through the [`ToolCancel`] each
//!   one is handed. A job that checks it stops early. One that doesn't runs
//!   to the end, and its answer is dropped. An answer that landed before the
//!   round noticed the cancel is kept, since that tool did run.

use crate::{Message, ToolResponse, ToolResponses};
use notedeck::Waker;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

/// The slow work of one call, run on a thread of its own. It is handed the
/// round's [`ToolCancel`] and returns the call's answer, or an error for the
/// model.
pub(crate) type ToolJob = Box<dyn FnOnce(&ToolCancel) -> Result<ToolResponses, String> + Send>;

/// How one call of a round started.
pub(crate) enum StartedCall {
    /// Answered on the spot. A fast host tool, such as an ndb query, runs
    /// inline, so a round of only these is answered on the frame it arrives.
    Done(Box<ToolResponses>),
    /// Work too slow for the frame, such as a build or a network round trip.
    // Every tool Dave runs today answers on the spot. The first slow ones are
    // the Notedeck toolset's app tools (headway:dave/evoke-blanket-little).
    #[cfg_attr(not(test), allow(dead_code))]
    Pending(ToolJob),
}

/// One call of a round: the id the model gave it and how it started.
pub(crate) struct RoundCall {
    pub(crate) id: String,
    pub(crate) started: StartedCall,
}

/// Tells a round's running jobs to stop. Cloned into every job; setting it
/// on one clone sets it on all.
#[derive(Clone, Default)]
pub(crate) struct ToolCancel(Arc<AtomicBool>);

impl ToolCancel {
    /// Ask every job of the round to stop.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the round was cancelled. A long job checks this between steps
    /// and returns early (an error is fine; its answer is dropped).
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A job's answer, sent back from its thread.
struct Landed {
    /// The call's place in the round.
    index: usize,
    response: ToolResponses,
}

/// A round's answers, ready for the chat.
pub(crate) struct AnsweredRound {
    /// One `ToolResponse` row per call, in the order the model made the calls.
    pub(crate) responses: Vec<Message>,
    /// The round was cancelled, so the model isn't asked to go on.
    pub(crate) cancelled: bool,
}

/// What a cancelled call answers.
pub(crate) const CANCELLED: &str =
    "Cancelled: the user stopped the turn before this tool finished.";

/// A session's outstanding round of tool calls (see the module docs).
pub(crate) struct ToolRound {
    /// Each call's id, in the order the model made the calls.
    ids: Vec<String>,
    /// Each call's answer once it has one, by its place in the round.
    answers: Vec<Option<ToolResponses>>,
    /// How many calls have no answer yet.
    outstanding: usize,
    /// Where the jobs' answers arrive.
    landed: mpsc::Receiver<Landed>,
    cancel: ToolCancel,
    /// Wakes the frame loop, so a cancel is seen without waiting on input.
    waker: Waker,
    /// The backend ended its turn while calls were still running, so the
    /// turn's end waits for the round.
    turn_over: bool,
}

impl ToolRound {
    /// Start a round: record the calls answered on the spot, and run each
    /// pending one on a thread of its own, which wakes the frame loop with
    /// `waker` when its answer is in.
    pub(crate) fn start(calls: Vec<RoundCall>, waker: &Waker) -> Self {
        let (tx, landed) = mpsc::channel();
        let cancel = ToolCancel::default();
        let mut ids = Vec::with_capacity(calls.len());
        let mut answers = Vec::with_capacity(calls.len());
        let mut outstanding = 0;

        for (index, call) in calls.into_iter().enumerate() {
            ids.push(call.id);
            let job = match call.started {
                StartedCall::Done(response) => {
                    answers.push(Some(*response));
                    continue;
                }
                StartedCall::Pending(job) => job,
            };
            match spawn_job(index, job, &cancel, &tx, waker) {
                Ok(()) => {
                    answers.push(None);
                    outstanding += 1;
                }
                Err(err) => answers.push(Some(ToolResponses::Error(format!(
                    "no thread to run the tool on: {err}"
                )))),
            }
        }

        Self {
            ids,
            answers,
            outstanding,
            landed,
            cancel,
            waker: waker.clone(),
            turn_over: false,
        }
    }

    /// Stop the round: every running job is told to stop, and the round
    /// counts as answered, each call still running answering [`CANCELLED`].
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
        self.waker.wake();
    }

    /// Note that the backend ended its turn; the turn's end now waits for
    /// this round.
    pub(crate) fn end_turn(&mut self) {
        self.turn_over = true;
    }

    /// Whether the backend has ended its turn (see [`end_turn`](Self::end_turn)).
    pub(crate) fn turn_over(&self) -> bool {
        self.turn_over
    }

    /// Collect the answers that have landed, and say whether the round is
    /// answered: every call has an answer, or the round was cancelled.
    pub(crate) fn poll(&mut self) -> bool {
        while let Ok(Landed { index, response }) = self.landed.try_recv() {
            if let Some(slot @ None) = self.answers.get_mut(index) {
                *slot = Some(response);
                self.outstanding -= 1;
            }
        }
        self.outstanding == 0 || self.cancel.is_cancelled()
    }

    /// The round's rows, in call order. Call it once [`poll`](Self::poll)
    /// says the round is answered; a call still without an answer then
    /// answers [`CANCELLED`].
    pub(crate) fn into_answers(mut self) -> AnsweredRound {
        let cancelled = self.cancel.is_cancelled();
        let answers = std::mem::take(&mut self.answers);
        let responses = std::mem::take(&mut self.ids)
            .into_iter()
            .zip(answers)
            .map(|(id, answer)| {
                let answer = answer.unwrap_or_else(|| ToolResponses::Error(CANCELLED.to_owned()));
                Message::ToolResponse(ToolResponse::new(id, answer))
            })
            .collect();
        AnsweredRound {
            responses,
            cancelled,
        }
    }
}

impl Drop for ToolRound {
    /// A round dropped unanswered (its session closed) tells its jobs to stop.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Run `job` on a thread of its own, sending its answer back as call
/// `index` and waking the frame loop.
fn spawn_job(
    index: usize,
    job: ToolJob,
    cancel: &ToolCancel,
    tx: &mpsc::Sender<Landed>,
    waker: &Waker,
) -> std::io::Result<()> {
    let cancel = cancel.clone();
    let tx = tx.clone();
    let waker = waker.clone();
    std::thread::Builder::new()
        .name("dave-tool".to_owned())
        .spawn(move || {
            let response = match catch_unwind(AssertUnwindSafe(|| job(&cancel))) {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => ToolResponses::Error(err),
                Err(_) => ToolResponses::Error("the tool panicked".to_owned()),
            };
            // The round is gone only if its session closed; nobody is waiting.
            if tx.send(Landed { index, response }).is_ok() {
                waker.wake();
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Poll `round` until it is answered, failing after a few seconds.
    fn wait_answered(round: &mut ToolRound) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !round.poll() {
            assert!(Instant::now() < deadline, "the round never got its answers");
            std::thread::yield_now();
        }
    }

    /// A pending call whose job waits for `go`, then answers `n` notes.
    fn gated(id: &str, go: mpsc::Receiver<()>, n: i32) -> RoundCall {
        RoundCall {
            id: id.to_owned(),
            started: StartedCall::Pending(Box::new(move |_cancel| {
                go.recv().map_err(|e| e.to_string())?;
                Ok(ToolResponses::PresentNotes(n))
            })),
        }
    }

    fn done(id: &str, n: i32) -> RoundCall {
        RoundCall {
            id: id.to_owned(),
            started: StartedCall::Done(Box::new(ToolResponses::PresentNotes(n))),
        }
    }

    /// Each row's call id and what it answered, in chat order.
    fn rows(answered: &AnsweredRound) -> Vec<String> {
        answered
            .responses
            .iter()
            .map(|m| match m {
                Message::ToolResponse(r) => format!("{}={:?}", r.id(), r.responses()),
                other => panic!("not a tool response: {other:?}"),
            })
            .collect()
    }

    /// A round of only fast calls is answered as soon as it starts: no
    /// thread, no frame of latency.
    #[test]
    fn a_round_of_fast_calls_is_answered_at_once() {
        let mut round = ToolRound::start(vec![done("a", 1), done("b", 2)], &Waker::noop());
        assert!(round.poll());
        let answered = round.into_answers();
        assert!(!answered.cancelled);
        assert_eq!(rows(&answered), ["a=PresentNotes(1)", "b=PresentNotes(2)"]);
    }

    /// The answers come back in the order the model made the calls, not the
    /// order the jobs finished in, and the round waits for its slowest call.
    #[test]
    fn answers_keep_call_order_whatever_order_jobs_finish_in() {
        let (go_a, a) = mpsc::channel();
        let (go_c, c) = mpsc::channel();
        let mut round = ToolRound::start(
            vec![gated("a", a, 1), done("b", 2), gated("c", c, 3)],
            &Waker::noop(),
        );
        assert!(!round.poll(), "two calls are still running");

        go_c.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while round.outstanding > 1 {
            assert!(Instant::now() < deadline, "c never landed");
            round.poll();
        }
        assert!(!round.poll(), "a is still running");

        go_a.send(()).unwrap();
        wait_answered(&mut round);
        let answered = round.into_answers();
        assert_eq!(
            rows(&answered),
            [
                "a=PresentNotes(1)",
                "b=PresentNotes(2)",
                "c=PresentNotes(3)"
            ]
        );
    }

    /// Cancelling answers the round at once: each running call answers
    /// cancelled, and the running job sees the flag.
    #[test]
    fn a_cancelled_round_is_answered_and_its_jobs_told_to_stop() {
        let (saw_tx, saw_cancel) = mpsc::channel();
        // Held until the test is done, so the job's own answer can't land
        // first and be kept.
        let (_hold, held) = mpsc::channel::<()>();
        let job: ToolJob = Box::new(move |cancel| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let _ = saw_tx.send(cancel.is_cancelled());
            let _ = held.recv();
            Err("stopped".to_owned())
        });
        let mut round = ToolRound::start(
            vec![
                done("a", 1),
                RoundCall {
                    id: "slow".to_owned(),
                    started: StartedCall::Pending(job),
                },
            ],
            &Waker::noop(),
        );
        assert!(!round.poll());

        round.cancel();
        assert!(round.poll(), "a cancelled round needs no more answers");
        let answered = round.into_answers();
        assert!(answered.cancelled);
        assert_eq!(
            rows(&answered),
            [
                "a=PresentNotes(1)".to_owned(),
                format!("slow=Error({CANCELLED:?})")
            ]
        );
        assert_eq!(
            saw_cancel.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the running job saw the cancel"
        );
    }

    /// A job that fails or panics still answers its call, as an error.
    #[test]
    fn a_failing_or_panicking_job_answers_with_an_error() {
        let fails: ToolJob = Box::new(|_| Err("no such board".to_owned()));
        let panics: ToolJob = Box::new(|_| panic!("boom"));
        let mut round = ToolRound::start(
            vec![
                RoundCall {
                    id: "f".to_owned(),
                    started: StartedCall::Pending(fails),
                },
                RoundCall {
                    id: "p".to_owned(),
                    started: StartedCall::Pending(panics),
                },
            ],
            &Waker::noop(),
        );
        wait_answered(&mut round);
        let answered = round.into_answers();
        assert_eq!(
            rows(&answered),
            [
                "f=Error(\"no such board\")",
                "p=Error(\"the tool panicked\")"
            ]
        );
    }

    /// A round dropped with its session tells its running jobs to stop.
    #[test]
    fn dropping_a_round_cancels_its_jobs() {
        let (saw_tx, saw_cancel) = mpsc::channel();
        let job: ToolJob = Box::new(move |cancel| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let _ = saw_tx.send(cancel.is_cancelled());
            Ok(ToolResponses::PresentNotes(0))
        });
        let round = ToolRound::start(
            vec![RoundCall {
                id: "slow".to_owned(),
                started: StartedCall::Pending(job),
            }],
            &Waker::noop(),
        );
        drop(round);
        assert_eq!(saw_cancel.recv_timeout(Duration::from_secs(5)), Ok(true));
    }
}
