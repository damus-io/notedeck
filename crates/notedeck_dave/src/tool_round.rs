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
//!
//! ## App tools start on the frame
//!
//! An app tool (the Notedeck toolset's headway and notebook tools) runs
//! through the host's tool registry, which needs the app context only the
//! frame has, and a gated one runs only once the user allows it. So it
//! starts [waiting](StartedCall::Waiting): the round holds it until the
//! frame's [`start_waiting`](ToolRound::start_waiting) pass finds it allowed
//! and starts it there, as a fast answer or a job like any other.
//!
//! ## A job can show its progress before it answers
//!
//! A long job (a sub-agent's research) [reports](JobCtx::report) what it is
//! doing as backend responses, over a channel of the round's own: the frame
//! [drains](ToolRound::next_progress) them each pass and applies them as it
//! would a backend's, so the row they build shows, and publishes, while the
//! job is still running. Nothing is shared but the channel.

use crate::messages::{DaveApiResponse, PermissionResponse};
use crate::tools::AppToolCall;
use crate::{Message, ToolResponse, ToolResponses};
use notedeck::Waker;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use tokio::sync::oneshot;

/// The slow work of one call, run on a thread of its own. It is handed its
/// [`JobCtx`] (the round's stop, and where it reports progress) and returns
/// the call's answer, or an error for the model.
pub(crate) type ToolJob = Box<dyn FnOnce(&JobCtx) -> Result<ToolResponses, String> + Send>;

/// What a running job is handed: the round's [`ToolCancel`], the id of the
/// call it answers, and where it reports progress for the frame to show
/// before its answer lands (see the module docs).
pub(crate) struct JobCtx {
    cancel: ToolCancel,
    call_id: String,
    progress: mpsc::Sender<DaveApiResponse>,
    waker: Waker,
}

impl JobCtx {
    /// The round's stop: a long job checks it between steps.
    pub(crate) fn cancel(&self) -> &ToolCancel {
        &self.cancel
    }

    /// The id the model gave the call this job answers, which names what
    /// the job reports (a sub-agent's row is keyed by it).
    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Report `progress`, applied on the frame as a backend's response would
    /// be, and wake the frame to show it. Dropped once the round is gone.
    pub(crate) fn report(&self, progress: DaveApiResponse) {
        if self.progress.send(progress).is_ok() {
            self.waker.wake();
        }
    }

    /// A context for a job run outside any round, call `call_id`, stopped by
    /// `cancel`: what it reports goes nowhere.
    #[cfg(test)]
    pub(crate) fn detached(call_id: &str, cancel: ToolCancel) -> Self {
        let (progress, _) = mpsc::channel();
        Self::reporting(call_id, cancel, progress)
    }

    /// A context for a job run outside any round, call `call_id`, stopped by
    /// `cancel`, whose reports go to `progress` for a test to read.
    #[cfg(test)]
    pub(crate) fn reporting(
        call_id: &str,
        cancel: ToolCancel,
        progress: mpsc::Sender<DaveApiResponse>,
    ) -> Self {
        Self {
            cancel,
            call_id: call_id.to_owned(),
            progress,
            waker: Waker::noop(),
        }
    }
}

/// How one call of a round started.
pub(crate) enum StartedCall {
    /// Answered on the spot. A fast host tool, such as an ndb query, runs
    /// inline, so a round of only these is answered on the frame it arrives.
    Done(Box<ToolResponses>),
    /// Work too slow for the frame, such as a build or a network round trip.
    Pending(ToolJob),
    /// An app tool, started later by the frame's
    /// [`start_waiting`](ToolRound::start_waiting) pass (see the module
    /// docs).
    Waiting(WaitingCall),
}

/// An app tool call the round holds until the frame starts it.
pub(crate) struct WaitingCall {
    /// The call, as the model made it.
    pub(crate) call: AppToolCall,
    /// What it waits on before it can start.
    pub(crate) gate: Gate,
    /// What runs it once it may start.
    pub(crate) runner: CallRunner,
}

/// What runs a [`WaitingCall`] once it may start.
pub(crate) enum CallRunner {
    /// The host's tool registry, which only the frame can reach: the
    /// frame's start pass runs it.
    Registry,
    /// Work of Dave's own that needs no frame (a web fetch, a sub-agent's
    /// question), held only for the user's answer: it runs as a job off the
    /// frame.
    Job(ToolJob),
}

/// What a [`WaitingCall`] waits on.
pub(crate) enum Gate {
    /// Nothing: its approval is auto, so the next frame starts it.
    Open,
    /// The user's answer to the permission request shown for it.
    Asked(AskedGate),
}

/// A call's open permission request: which request it is, and where its
/// answer arrives.
pub(crate) struct AskedGate {
    /// The request's id, so a round that ends before the answer can say
    /// which requests nobody will read (see [`AnsweredRound::unanswered`]).
    pub(crate) perm_id: uuid::Uuid,
    pub(crate) answer: oneshot::Receiver<PermissionResponse>,
}

/// What the round does with a waiting call this pass.
enum Verdict {
    /// Leave it: its permission request is unanswered.
    Wait,
    /// Start it.
    Start,
    /// Answer it without running it.
    Answer(Box<ToolResponses>),
    /// The user stopped the turn from its permission request.
    StopTurn,
}

/// A [`WaitingCall`] and its place in the round.
struct Parked {
    index: usize,
    waiting: WaitingCall,
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

    /// Resolves once the round is cancelled, for a job that awaits network
    /// work and races it against the stop. The flag has no waker, so it is
    /// polled every 100ms.
    pub(crate) async fn cancelled(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// Run `work` to the end on a runtime of its own, for a job that awaits
/// network work on its thread. The job's clients are built inside `work`,
/// since their connections belong to the runtime they run on.
pub(crate) fn block_on_own_runtime<T>(
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("no runtime for the tool: {err}"))?
        .block_on(work)
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
    /// The permission requests of the calls still waiting on the user when
    /// the round ended (a cancelled round's): nobody reads their answers
    /// now, so the session withdraws them. One answered just before the
    /// round ended may be among them, and withdrawing it changes nothing.
    pub(crate) unanswered: Vec<uuid::Uuid>,
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
    /// The app tool calls not started yet, for the frame to start.
    waiting: Vec<Parked>,
    /// Where a started job's answer goes; kept for the jobs the frame starts
    /// later.
    tx: mpsc::Sender<Landed>,
    /// What the jobs have reported and the frame hasn't shown yet.
    progress: mpsc::Receiver<DaveApiResponse>,
    /// Handed to each job, in its [`JobCtx`].
    progress_tx: mpsc::Sender<DaveApiResponse>,
}

/// Where a job started by the round sends what it has to say.
struct JobSinks<'a> {
    cancel: &'a ToolCancel,
    landed: &'a mpsc::Sender<Landed>,
    progress: &'a mpsc::Sender<DaveApiResponse>,
    waker: &'a Waker,
}

impl ToolRound {
    /// Start a round: record the calls answered on the spot, and run each
    /// pending one on a thread of its own, which wakes the frame loop with
    /// `waker` when its answer is in.
    pub(crate) fn start(calls: Vec<RoundCall>, waker: &Waker) -> Self {
        let (tx, landed) = mpsc::channel();
        let (progress_tx, progress) = mpsc::channel();
        let cancel = ToolCancel::default();
        let sinks = JobSinks {
            cancel: &cancel,
            landed: &tx,
            progress: &progress_tx,
            waker,
        };
        let mut ids = Vec::with_capacity(calls.len());
        let mut answers = Vec::with_capacity(calls.len());
        let mut outstanding = 0;
        let mut waiting = Vec::new();

        for (index, call) in calls.into_iter().enumerate() {
            let job = match call.started {
                StartedCall::Done(response) => {
                    ids.push(call.id);
                    answers.push(Some(*response));
                    continue;
                }
                StartedCall::Waiting(waiting_call) => {
                    ids.push(call.id);
                    answers.push(None);
                    outstanding += 1;
                    waiting.push(Parked {
                        index,
                        waiting: waiting_call,
                    });
                    continue;
                }
                StartedCall::Pending(job) => job,
            };
            let spawned = spawn_job(index, call.id.clone(), job, &sinks);
            ids.push(call.id);
            match spawned {
                Ok(()) => {
                    answers.push(None);
                    outstanding += 1;
                }
                Err(err) => answers.push(Some(ToolResponses::Error(format!(
                    "no thread to run the tool on: {err}"
                )))),
            }
        }

        // A waiting call starts on a frame, which may not be coming: ask
        // for one. (An answer to its permission request comes from a click
        // or an ingested note, and either brings a frame of its own.)
        if !waiting.is_empty() {
            waker.wake();
        }

        Self {
            ids,
            answers,
            outstanding,
            landed,
            cancel,
            waker: waker.clone(),
            turn_over: false,
            waiting,
            tx,
            progress,
            progress_tx,
        }
    }

    /// Start every waiting app call that may start now, with `start`, which
    /// runs one through the host's tool registry; the frame calls this each
    /// pass. A call whose permission was denied answers so without running,
    /// and one whose request was answered "stop" stops the turn. A cancelled
    /// round starts nothing: its waiting calls answer cancelled.
    pub(crate) fn start_waiting(&mut self, start: &mut dyn FnMut(&AppToolCall) -> StartedCall) {
        let mut i = 0;
        while i < self.waiting.len() && !self.cancel.is_cancelled() {
            let verdict = match &mut self.waiting[i].waiting.gate {
                Gate::Open => Verdict::Start,
                Gate::Asked(asked) => verdict(asked.answer.try_recv()),
            };
            if matches!(verdict, Verdict::Wait) {
                i += 1;
                continue;
            }
            // The answers are kept by index, so the parked calls' order
            // doesn't matter.
            let Parked { index, waiting } = self.waiting.swap_remove(i);
            match verdict {
                Verdict::Wait => unreachable!("handled above"),
                Verdict::Start => {
                    let started = match waiting.runner {
                        CallRunner::Registry => start(&waiting.call),
                        CallRunner::Job(job) => StartedCall::Pending(job),
                    };
                    self.start_late(index, started)
                }
                Verdict::Answer(response) => self.answer(index, *response),
                Verdict::StopTurn => self.cancel(),
            }
        }
    }

    /// Start call `index` the frame has just started late (see
    /// [`start_waiting`](Self::start_waiting)).
    fn start_late(&mut self, index: usize, started: StartedCall) {
        match started {
            StartedCall::Done(response) => self.answer(index, *response),
            StartedCall::Pending(job) => {
                let sinks = JobSinks {
                    cancel: &self.cancel,
                    landed: &self.tx,
                    progress: &self.progress_tx,
                    waker: &self.waker,
                };
                let call_id = self.ids[index].clone();
                if let Err(err) = spawn_job(index, call_id, job, &sinks) {
                    self.answer(
                        index,
                        ToolResponses::Error(format!("no thread to run the tool on: {err}")),
                    );
                }
            }
            StartedCall::Waiting(_) => self.answer(
                index,
                ToolResponses::Error("the tool asked to wait a second time".to_owned()),
            ),
        }
    }

    /// Record call `index`'s answer, if it has none yet.
    fn answer(&mut self, index: usize, response: ToolResponses) {
        if let Some(slot @ None) = self.answers.get_mut(index) {
            *slot = Some(response);
            self.outstanding -= 1;
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

    /// The next thing a job reported that the frame hasn't shown yet (see
    /// [`JobCtx::report`]). A job reports before it answers, so once
    /// [`poll`](Self::poll) has seen a call's answer, everything that call
    /// reported is here to drain.
    pub(crate) fn next_progress(&mut self) -> Option<DaveApiResponse> {
        self.progress.try_recv().ok()
    }

    /// Collect the answers that have landed, and say whether the round is
    /// answered: every call has an answer, or the round was cancelled.
    pub(crate) fn poll(&mut self) -> bool {
        while let Ok(Landed { index, response }) = self.landed.try_recv() {
            self.answer(index, response);
        }
        self.outstanding == 0 || self.cancel.is_cancelled()
    }

    /// The round's rows, in call order. Call it once [`poll`](Self::poll)
    /// says the round is answered; a call still without an answer then
    /// answers [`CANCELLED`].
    pub(crate) fn into_answers(mut self) -> AnsweredRound {
        let cancelled = self.cancel.is_cancelled();
        let unanswered = self
            .waiting
            .iter()
            .filter_map(|parked| match &parked.waiting.gate {
                Gate::Asked(asked) => Some(asked.perm_id),
                Gate::Open => None,
            })
            .collect();
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
            unanswered,
        }
    }
}

impl Drop for ToolRound {
    /// A round dropped unanswered (its session closed) tells its jobs to stop.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// What the user's `answer` to a waiting call's permission request means
/// for the call.
fn verdict(answer: Result<PermissionResponse, oneshot::error::TryRecvError>) -> Verdict {
    match answer {
        Err(oneshot::error::TryRecvError::Empty) => Verdict::Wait,
        Ok(PermissionResponse::Allow { .. }) => Verdict::Start,
        Ok(PermissionResponse::Deny { reason }) => Verdict::Answer(Box::new(ToolResponses::Error(
            format!("The user denied this call: {reason}"),
        ))),
        Ok(PermissionResponse::Cancel { .. }) => Verdict::StopTurn,
        Err(oneshot::error::TryRecvError::Closed) => {
            Verdict::Answer(Box::new(ToolResponses::Error(
                "The permission request was dropped before anyone answered it.".to_owned(),
            )))
        }
    }
}

/// Run `job`, the answer to call `call_id`, on a thread of its own, sending
/// its answer back as call `index` and waking the frame loop.
fn spawn_job(
    index: usize,
    call_id: String,
    job: ToolJob,
    sinks: &JobSinks<'_>,
) -> std::io::Result<()> {
    let ctx = JobCtx {
        cancel: sinks.cancel.clone(),
        call_id,
        progress: sinks.progress.clone(),
        waker: sinks.waker.clone(),
    };
    let tx = sinks.landed.clone();
    let waker = sinks.waker.clone();
    std::thread::Builder::new()
        .name("dave-tool".to_owned())
        .spawn(move || {
            let response = match catch_unwind(AssertUnwindSafe(|| job(&ctx))) {
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
        let job: ToolJob = Box::new(move |ctx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ctx.cancel().is_cancelled() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let _ = saw_tx.send(ctx.cancel().is_cancelled());
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
        let job: ToolJob = Box::new(move |ctx| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ctx.cancel().is_cancelled() && Instant::now() < deadline {
                std::thread::yield_now();
            }
            let _ = saw_tx.send(ctx.cancel().is_cancelled());
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

    /// An app tool call waiting on `gate`.
    fn waiting(id: &str, gate: Gate) -> RoundCall {
        RoundCall {
            id: id.to_owned(),
            started: StartedCall::Waiting(WaitingCall {
                call: AppToolCall {
                    name: format!("tool_{id}"),
                    args: serde_json::json!({}),
                },
                gate,
                runner: CallRunner::Registry,
            }),
        }
    }

    /// A gate on a fresh permission request, answered on `answer`.
    fn ask(answer: oneshot::Receiver<PermissionResponse>) -> Gate {
        Gate::Asked(AskedGate {
            perm_id: uuid::Uuid::new_v4(),
            answer,
        })
    }

    /// What the frame's start pass runs here: a job off the frame for
    /// `tool_slow`, an answer on the spot for anything else.
    fn start(call: &AppToolCall) -> StartedCall {
        if call.name == "tool_slow" {
            return StartedCall::Pending(Box::new(|_| Ok(ToolResponses::PresentNotes(9))));
        }
        StartedCall::Done(Box::new(ToolResponses::PresentNotes(1)))
    }

    /// An open call waits for the frame, then starts, fast or slow; one
    /// whose request is unanswered keeps waiting, and the round with it.
    #[test]
    fn waiting_calls_start_on_the_frame_once_allowed() {
        let (allow, asked) = oneshot::channel();
        let mut round = ToolRound::start(
            vec![
                waiting("fast", Gate::Open),
                waiting("slow", Gate::Open),
                waiting("asked", ask(asked)),
            ],
            &Waker::noop(),
        );
        assert!(!round.poll(), "nothing has started");

        round.start_waiting(&mut start);
        assert_eq!(round.waiting.len(), 1, "the asked call still waits");
        let deadline = Instant::now() + Duration::from_secs(5);
        while round.outstanding > 1 {
            assert!(Instant::now() < deadline, "the slow job never landed");
            round.poll();
        }
        assert!(!round.poll(), "the asked call holds the round");

        allow
            .send(PermissionResponse::Allow { message: None })
            .unwrap();
        round.start_waiting(&mut start);
        assert!(round.poll());
        assert_eq!(
            rows(&round.into_answers()),
            [
                "fast=PresentNotes(1)",
                "slow=PresentNotes(9)",
                "asked=PresentNotes(1)"
            ]
        );
    }

    /// A gated call Dave runs itself (a web fetch, a sub-agent's question)
    /// waits for the user like an app tool, then runs as a job: the frame's
    /// start pass never sees it.
    #[test]
    fn a_gated_job_runs_off_the_frame_once_allowed() {
        let (allow, asked) = oneshot::channel();
        let mut round = ToolRound::start(
            vec![RoundCall {
                id: "ask".to_owned(),
                started: StartedCall::Waiting(WaitingCall {
                    call: AppToolCall {
                        name: "ask_researcher".to_owned(),
                        args: serde_json::json!({ "question": "who?" }),
                    },
                    gate: ask(asked),
                    runner: CallRunner::Job(Box::new(|_| Ok(ToolResponses::PresentNotes(7)))),
                }),
            }],
            &Waker::noop(),
        );
        round.start_waiting(&mut |_| panic!("a job never goes through the registry"));
        assert!(!round.poll(), "it waits for the user");

        allow
            .send(PermissionResponse::Allow { message: None })
            .unwrap();
        round.start_waiting(&mut |_| panic!("a job never goes through the registry"));
        wait_answered(&mut round);
        assert_eq!(rows(&round.into_answers()), ["ask=PresentNotes(7)"]);
    }

    /// "Stop" on a call's permission request stops the round, and a
    /// request dropped unanswered answers its call with an error; neither
    /// call runs.
    #[test]
    fn a_stopped_or_dropped_request_runs_nothing() {
        let (stop, asked) = oneshot::channel();
        let (dropped, never) = oneshot::channel::<PermissionResponse>();
        let mut round = ToolRound::start(
            vec![waiting("gone", ask(never)), waiting("stop", ask(asked))],
            &Waker::noop(),
        );
        drop(dropped);
        stop.send(PermissionResponse::Cancel {
            reason: "enough".into(),
        })
        .unwrap();
        round.start_waiting(&mut |_| panic!("nothing runs"));
        assert!(round.poll());
        let answered = round.into_answers();
        assert!(answered.cancelled, "the turn stops");
        let rows = rows(&answered);
        assert!(rows[0].starts_with("gone=Error(\"The permission request was dropped"));
        assert_eq!(rows[1], format!("stop=Error({CANCELLED:?})"));
    }

    /// "Stop" on one of two requests ends the round with the other still
    /// open: the round names it, so the session can withdraw the request
    /// nobody will read. An answered request isn't named.
    #[test]
    fn a_cancelled_round_names_its_unanswered_requests() {
        let (stop, stopped) = oneshot::channel();
        let (_open, open) = oneshot::channel::<PermissionResponse>();
        let left = uuid::Uuid::new_v4();
        let mut round = ToolRound::start(
            vec![
                waiting("stop", ask(stopped)),
                RoundCall {
                    id: "left".to_owned(),
                    started: StartedCall::Waiting(WaitingCall {
                        call: AppToolCall {
                            name: "tool_left".to_owned(),
                            args: serde_json::json!({}),
                        },
                        gate: Gate::Asked(AskedGate {
                            perm_id: left,
                            answer: open,
                        }),
                        runner: CallRunner::Registry,
                    }),
                },
            ],
            &Waker::noop(),
        );
        stop.send(PermissionResponse::Cancel {
            reason: "enough".into(),
        })
        .unwrap();
        round.start_waiting(&mut |_| panic!("nothing runs"));
        assert!(round.poll());
        let answered = round.into_answers();
        assert!(answered.cancelled);
        assert_eq!(answered.unanswered, [left]);
    }
}
