//! The chrome's end of notedeck's socket (see [`notedeck_ipc`]): `nd` lists
//! the apps' tools and calls them. Every installed app's data tools are
//! listed, running or not, and a call to one of a stopped app's starts it
//! (see [`Chrome::start_tool_owner`]); a UI tool is listed only while its app
//! runs, unless it [starts the app](ToolSpec::starts_app).
//!
//! The chrome owns the socket, not an app, because only the chrome can both
//! dispatch a tool to the app that owns it and carry out the intent a tool
//! answers with. Requests are served in [`update`](notedeck::App::update), so
//! a `--headless` notedeck answers them too; an intent that needs the window
//! waits for [`render`](notedeck::App::render), and is refused headless.
//!
//! A [slow tool](notedeck::SlowTool) answers later: its work runs on a worker
//! thread while the loop goes on, and the client's answer is written on the
//! tick after it finishes, so a `git fetch` never stalls the window.
//!
//! A [streaming tool](notedeck::StreamTool) is subscribed to, not called: its
//! job runs as a tokio task, emitting into the connection's bounded
//! [`EventQueue`](notedeck_ipc::EventQueue), which a writer thread of its own
//! drains. A client too slow to keep up with live events is dropped rather
//! than waited on, and told so; a stream's opening history waits for it
//! instead ([`notedeck::Emitter::emit_waiting`]), so a long one isn't cut
//! short. One that hangs up ends the task.
//!
//! An edit answers once its write reads back. A data tool that wrote notes
//! names them (see [`Rendered::written`](notedeck::Rendered::written)), and
//! since nostrdb ingests, and unwraps a sealed board's envelopes,
//! asynchronously, the answer is held until `get_note_by_id` finds every one,
//! or [`SETTLE_DEADLINE`] passes. So `nd headway add`, then an edit of the new
//! ref, never folds a board without the card. The wait is checked each tick,
//! never blocked on: a subscription on the ids wakes the loop when they land.
//!
//! Only the real notedeck serves: [`Chrome::serve_ipc`] is called by its
//! entry points, never by a constructor, so a test chrome leaves the socket of
//! the notedeck running on the same machine alone.

use super::actions::chrome_handle_app_action;
use super::Chrome;
use nostrdb_net::NoteId;
use nostrdb_net::{FullKeypair, SecretKey};
use notedeck::{
    AppAction, AppContext, Finished, PendingCall, Started, StreamJob, ToolOutcome, ToolSpec,
};
use notedeck_ipc::{IntentStatus, Reply, Request, Response, Signer, ToolInfo};

/// How long an edit's answer waits for its write to read back before it
/// answers anyway, with a notice. Ingest takes milliseconds; this is for a
/// note nostrdb refused, which would otherwise hold the client forever.
#[cfg(unix)]
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// How many events a stream holds for a client that hasn't read them yet
/// before a live event drops the client: a burst of a session's messages,
/// with room. An opening history waits on it instead of dropping.
#[cfg(unix)]
const STREAM_QUEUE: usize = 256;

/// The socket and the requests waiting on the window. Empty until
/// [`Chrome::serve_ipc`].
#[derive(Default)]
pub(super) struct IpcServer {
    #[cfg(unix)]
    listener: Option<notedeck_ipc::Listener>,
    /// Intents waiting on the next render, with the client to tell once each
    /// is carried out. Empty but for the frame a request arrives on.
    #[cfg(unix)]
    applying: Vec<PendingIntent>,
    /// Slow tools' work running on worker threads, with the client to answer
    /// once each finishes. Empty but while one runs.
    #[cfg(unix)]
    running: Vec<RunningCall>,
    /// Edits answered but not yet told, waiting for their writes to read
    /// back. Empty but for the few milliseconds an ingest takes.
    #[cfg(unix)]
    settling: Vec<SettlingCall>,
}

/// An edit's answer, held until the notes it wrote read back.
#[cfg(unix)]
struct SettlingCall {
    /// The notes to find before answering.
    written: Vec<NoteId>,
    /// A subscription on `written`, only so nostrdb's callback wakes the
    /// loop when they land; nothing polls it. `None` if it couldn't be made,
    /// when the idle tick finds them instead.
    sub: Option<nostrdb::Subscription>,
    /// When to answer even if they haven't.
    deadline: std::time::Instant,
    response: Response,
    connection: notedeck_ipc::Connection,
}

/// A slow tool's work on its worker thread, and the client waiting on it.
#[cfg(unix)]
struct RunningCall {
    /// Where the worker sends what the work came to.
    done: std::sync::mpsc::Receiver<Finished>,
    connection: notedeck_ipc::Connection,
}

/// An intent a socket request raised, held for the window.
#[cfg(unix)]
struct PendingIntent {
    action: AppAction,
    connection: notedeck_ipc::Connection,
}

/// What a request comes to: an answer now, an answer once the notes it
/// wrote read back, an intent for the window to carry out before answering,
/// a slow tool's work to run before answering, or a stream to run for as
/// long as the client listens.
pub(super) enum Served {
    Reply(Response),
    Settle {
        response: Response,
        written: Vec<NoteId>,
    },
    Apply(AppAction),
    Run(PendingCall),
    Stream(StreamJob),
}

impl Chrome {
    /// Listen on [`notedeck_ipc::socket_path`], taking it from any notedeck
    /// that had it. `waker` brings on the tick that serves a request. Returns
    /// the socket's [`Watch`](notedeck_ipc::Watch) when it is up, which a
    /// `--serve` notedeck reads to know when to exit; a failure is logged and
    /// notedeck runs on without it.
    #[cfg(unix)]
    pub fn serve_ipc(&mut self, waker: notedeck::Waker) -> Option<notedeck_ipc::Watch> {
        let path = notedeck_ipc::socket_path();
        match notedeck_ipc::Listener::bind(&path, move || waker.wake()) {
            Ok(listener) => {
                tracing::info!("serving tools on {}", path.display());
                let watch = listener.watch();
                self.ipc.listener = Some(listener);
                Some(watch)
            }
            Err(err) => {
                tracing::warn!("no tool socket at {}: {err}", path.display());
                None
            }
        }
    }

    /// There is no socket off Unix.
    #[cfg(not(unix))]
    pub fn serve_ipc(&mut self, waker: notedeck::Waker) -> Option<()> {
        let _ = waker;
        tracing::info!("the tool socket is unix only");
        None
    }

    /// Serve every request that has arrived. An intent is held for the next
    /// render, or refused if there is no window to render it.
    #[cfg(unix)]
    pub(super) fn poll_ipc(&mut self, ctx: &mut AppContext) {
        self.answer_finished_calls();
        loop {
            let Some(incoming) = self.ipc.listener.as_ref().and_then(|l| l.try_recv()) else {
                break;
            };
            let notedeck_ipc::Incoming {
                request,
                mut connection,
            } = incoming;
            let response = match serve(self, ctx, request) {
                Served::Reply(response) => response,
                Served::Settle { response, written } => {
                    let call = SettlingCall::new(ctx.ndb, written, response, connection);
                    self.ipc.settling.push(call);
                    continue;
                }
                Served::Apply(action) => {
                    if let Some(egui) = ctx.egui {
                        egui.request_repaint();
                        self.ipc.applying.push(PendingIntent { action, connection });
                        continue;
                    }
                    Response::error("the tool acts on the window, and this notedeck is headless")
                }
                Served::Run(pending) => {
                    match run_off_loop(pending, ctx.waker.clone()) {
                        Ok(done) => self.ipc.running.push(RunningCall { done, connection }),
                        Err(err) => {
                            let response = Response::error(format!("no worker thread: {err}"));
                            if let Err(err) = connection.reply(&response) {
                                tracing::debug!("ipc: the client left before its answer: {err}");
                            }
                        }
                    }
                    continue;
                }
                Served::Stream(job) => {
                    start_stream(job, connection);
                    continue;
                }
            };
            if let Err(err) = connection.reply(&response) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
        }
        // After the requests, so an edit whose write already landed is
        // answered on the tick it arrived.
        self.answer_settled_calls(ctx);
    }

    /// Answer each held edit whose writes have read back, or whose deadline
    /// has passed, and in the GUI schedule a frame for the next deadline.
    /// Allocates nothing.
    #[cfg(unix)]
    fn answer_settled_calls(&mut self, ctx: &mut AppContext) {
        if self.ipc.settling.is_empty() {
            return;
        }
        let ndb = &mut *ctx.ndb;
        let now = std::time::Instant::now();
        self.ipc.settling.retain_mut(|call| {
            let landed = nostrdb::Transaction::new(ndb).is_ok_and(|txn| {
                call.written
                    .iter()
                    .all(|id| ndb.get_note_by_id(&txn, id.bytes()).is_ok())
            });
            if !landed && now < call.deadline {
                return true;
            }
            if !landed {
                tracing::warn!(
                    "ipc: an edit's {} notes hadn't read back after {SETTLE_DEADLINE:?}",
                    call.written.len()
                );
                mark_unsettled(&mut call.response);
            }
            if let Some(sub) = call.sub.take() {
                let _ = ndb.unsubscribe(sub);
            }
            if let Err(err) = call.connection.reply(&call.response) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
            false
        });
        let next = self.ipc.settling.iter().map(|call| call.deadline).min();
        if let (Some(egui), Some(next)) = (ctx.egui, next) {
            egui.request_repaint_after(next.saturating_duration_since(now));
        }
    }

    /// Answer each client whose slow tool has finished. Allocates nothing.
    #[cfg(unix)]
    fn answer_finished_calls(&mut self) {
        if self.ipc.running.is_empty() {
            return;
        }
        self.ipc.running.retain_mut(|call| {
            let response = match call.done.try_recv() {
                Ok(done) => finished_response(done),
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Response::error("the tool's work stopped without an answer")
                }
            };
            if let Err(err) = call.connection.reply(&response) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
            false
        });
    }

    /// Carry out the intents socket requests raised, and tell each client.
    /// Allocates nothing when none are waiting.
    #[cfg(unix)]
    pub(super) fn apply_ipc_intents(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) {
        if self.ipc.applying.is_empty() {
            return;
        }
        for PendingIntent {
            action,
            mut connection,
        } in std::mem::take(&mut self.ipc.applying)
        {
            chrome_handle_app_action(self, ctx, action, ui);
            let applied = Response::ok(Reply::Intent {
                intent: IntentStatus::Applied,
            });
            if let Err(err) = connection.reply(&applied) {
                tracing::debug!("ipc: the client left before its answer: {err}");
            }
        }
    }
}

/// What `request` comes to. Runs tools; carries out no intent.
pub(super) fn serve(chrome: &mut Chrome, ctx: &mut AppContext, request: Request) -> Served {
    match request {
        Request::ListTools => Served::Reply(Response::ok(Reply::Tools {
            tools: ctx.tool_specs().iter().map(tool_info).collect(),
        })),
        Request::CallTool { name, args, signer } => {
            call_tool(chrome, ctx, &name, &args, signer.as_ref())
        }
        Request::Subscribe { name, args } => {
            chrome.start_tool_owner(&ctx.registries.tools, &name);
            match ctx.subscribe_tool(&name, &args) {
                Ok(job) => Served::Stream(job),
                Err(err) => Served::Reply(Response::error(err)),
            }
        }
    }
}

/// Run a stream's `job` as a tokio task, emitting into `connection` until
/// the job ends or the client hangs up or falls behind. With no runtime to
/// run it on (a test chrome off tokio), the client is told so.
#[cfg(unix)]
fn start_stream(job: StreamJob, mut connection: notedeck_ipc::Connection) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        let response = Response::error("this notedeck has no runtime to run a stream on");
        if let Err(err) = connection.reply(&response) {
            tracing::debug!("ipc: the client left before its answer: {err}");
        }
        return;
    };
    let (hang_up, gone) = tokio::sync::watch::channel(false);
    let room = std::sync::Arc::new(tokio::sync::Notify::new());
    let roomy = std::sync::Arc::clone(&room);
    let queue = match connection.into_stream(
        STREAM_QUEUE,
        move || roomy.notify_one(),
        move || {
            let _ = hang_up.send(true);
        },
    ) {
        Ok(queue) => queue,
        Err(err) => {
            tracing::warn!("ipc: no writer thread for a stream: {err}");
            return;
        }
    };
    runtime.spawn(job(notedeck::Emitter::new(QueueSink { queue, room }, gone)));
}

/// A stream's events, on their way into its connection's queue.
#[cfg(unix)]
struct QueueSink {
    queue: notedeck_ipc::EventQueue,
    /// Notified by the writer thread each time it takes a line off the
    /// queue, and as the stream ends, so a waiting send tries again.
    room: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(unix)]
impl notedeck::EventSink for QueueSink {
    fn send(&mut self, event: serde_json::Value, text: Option<String>) -> bool {
        self.queue.push(&Response::ok(Reply::Event { event, text }))
    }

    /// Queue the event once there's room. `notify_one` keeps a permit when
    /// nobody waits yet, so a line taken between a full push and the wait
    /// still wakes it.
    fn send_waiting<'s>(
        &'s mut self,
        event: serde_json::Value,
        text: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 's>> {
        use notedeck_ipc::Pushed;

        let response = Response::ok(Reply::Event { event, text });
        Box::pin(async move {
            loop {
                match self.queue.try_push(&response) {
                    Pushed::Queued => return true,
                    Pushed::Closed => return false,
                    Pushed::Full => self.room.notified().await,
                }
            }
        })
    }

    fn fail(&mut self, message: String) {
        self.queue.push(&Response::error(message));
    }
}

/// Run the tool `name`: a data tool here, a UI tool in the app that owns it.
/// What either answers with becomes the reply, or an intent for the window.
///
/// `signer` is the caller's comment key, which a data tool that accepts one
/// signs its comments with. A key that doesn't parse refuses the call rather
/// than letting it sign as the account.
fn call_tool(
    chrome: &mut Chrome,
    ctx: &mut AppContext,
    name: &str,
    args: &serde_json::Value,
    signer: Option<&Signer>,
) -> Served {
    let comment_signer = match signer.map(comment_key).transpose() {
        Ok(key) => key,
        Err(err) => return Served::Reply(Response::error(err)),
    };
    // A stopped app's data tools are listed too, and its app starters; a
    // call to one starts its app, which updates from this tick on.
    chrome.start_tool_owner(&ctx.registries.tools, name);
    // A person reads what nd prints, so ask a data tool for its text too. A
    // slow tool's work is left for a worker thread.
    let notedeck::Rendered {
        outcome,
        text,
        notice,
        written,
    } = match ctx.start_tool_rendered(name, args, comment_signer.as_ref()) {
        Started::Done(rendered) => rendered,
        Started::Pending(pending) => return Served::Run(pending),
    };
    let outcome = match outcome {
        ToolOutcome::Intent(AppAction::CallTool(invocation)) => {
            chrome.run_ui_tool(ctx, &invocation)
        }
        outcome => outcome,
    };
    let response = |result| {
        Response::ok(Reply::Result {
            result,
            text,
            notice,
        })
    };
    match outcome {
        ToolOutcome::Data(result) if written.is_empty() => Served::Reply(response(result)),
        ToolOutcome::Data(result) => Served::Settle {
            response: response(result),
            written,
        },
        ToolOutcome::Error(err) => Served::Reply(Response::error(err)),
        ToolOutcome::Intent(action) => Served::Apply(action),
    }
}

#[cfg(unix)]
impl SettlingCall {
    /// Hold `response` for `connection` until `written` reads back from
    /// `ndb`, subscribing on the ids so their arrival wakes the loop. The
    /// subscription comes before the first check, so a note landing between
    /// the two still wakes it.
    fn new(
        ndb: &nostrdb::Ndb,
        written: Vec<NoteId>,
        response: Response,
        connection: notedeck_ipc::Connection,
    ) -> Self {
        let filter = nostrdb::Filter::new()
            .ids(written.iter().map(NoteId::bytes))
            .build();
        Self {
            sub: ndb.subscribe(&[filter]).ok(),
            written,
            deadline: std::time::Instant::now() + SETTLE_DEADLINE,
            response,
            connection,
        }
    }
}

/// Give `response` a notice that its write hadn't read back yet, so the
/// person knows a command right after it may not see it.
#[cfg(unix)]
fn mark_unsettled(response: &mut Response) {
    let notedeck_ipc::Outcome::Ok(Reply::Result { notice, .. }) = &mut response.outcome else {
        return;
    };
    *notice = Some(format!(
        "the edit was written, but hadn't read back after {SETTLE_DEADLINE:?}; \
         a command right after it may not see it yet"
    ));
}

/// Run a slow tool's work on a thread of its own, waking the loop when it
/// is done so [`Chrome::answer_finished_calls`] answers on the next tick. Its
/// own thread, not the job pool's two, since the work waits on the network.
#[cfg(unix)]
fn run_off_loop(
    pending: PendingCall,
    waker: notedeck::Waker,
) -> std::io::Result<std::sync::mpsc::Receiver<Finished>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("ipc-tool".to_owned())
        .spawn(move || {
            // The receiver is gone only when the notedeck is shutting down.
            let _ = tx.send(pending.run());
            waker.wake();
        })?;
    Ok(rx)
}

/// What a finished slow tool's client is told.
fn finished_response(done: Finished) -> Response {
    match done.result {
        Ok(result) => Response::ok(Reply::Result {
            result,
            text: done.text,
            notice: done.notice,
        }),
        Err(err) => Response::error(err),
    }
}

/// The keypair `signer` names. The error never quotes the key.
fn comment_key(signer: &Signer) -> Result<FullKeypair, String> {
    SecretKey::parse(signer.comment_nsec.trim())
        .ok()
        .and_then(|secret| FullKeypair::from_secret_bytes(&secret.secret_bytes()))
        .ok_or_else(|| {
            "the call's comment signer is not a valid key (expected an nsec1… or a 64-char hex \
             secret key); refusing to sign as the account instead"
                .to_owned()
        })
}

/// `spec` as `list_tools` lists it.
fn tool_info(spec: &ToolSpec) -> ToolInfo {
    ToolInfo {
        name: spec.name().to_owned(),
        title: spec.display_title().to_owned(),
        description: spec.description().to_owned(),
        category: spec.category().map(str::to_owned),
        kind: match spec.kind() {
            notedeck::ToolKind::Data => notedeck_ipc::ToolKind::Data,
            notedeck::ToolKind::Ui => notedeck_ipc::ToolKind::Ui,
        },
        read_only: spec.is_read_only(),
        input_schema: spec.json_schema(),
        command: spec
            .command_path()
            .iter()
            .map(|word| (*word).to_owned())
            .collect(),
        accepts_signer: spec.takes_comment_signer(),
        streams: spec.stream_mode().map(|mode| match mode {
            notedeck::StreamMode::Append => notedeck_ipc::StreamMode::Append,
            notedeck::StreamMode::Redraw => notedeck_ipc::StreamMode::Redraw,
        }),
        model_reach: Some(match spec.model_reach() {
            notedeck::ModelReach::Never => notedeck_ipc::ModelReach::Never,
            notedeck::ModelReach::Data => notedeck_ipc::ModelReach::Data,
            notedeck::ModelReach::Window => notedeck_ipc::ModelReach::Window,
        }),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use notedeck::{
        AppResponse, AppTool, Notedeck, RegisteredTool, SlowTool, ToolContext, ToolInvocation,
        ToolJob,
    };
    use notedeck_ipc::{Outcome, ToolKind};
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A data tool: answers with its args.
    const ECHO: &str = "test_echo";
    /// A UI tool answering with data.
    pub(in crate::chrome) const PEEK: &str = "test_peek";
    /// A UI tool answering with an intent to toggle the chrome's drawer.
    pub(in crate::chrome) const TOGGLE: &str = "test_toggle_chrome";
    /// A UI tool answering with a call to [`PEEK`], which is never made.
    pub(in crate::chrome) const CALL_AGAIN: &str = "test_call_again";
    /// A data tool that accepts a comment signer and answers with its pubkey.
    const WHO_SIGNS: &str = "test_who_signs";
    /// A slow tool: its work waits for the file its `gate` arg names.
    const SLOW: &str = "test_slow";
    /// A stream that emits its `to` numbers, then ends.
    const COUNT: &str = "test_count";
    /// A data tool that writes a note and names it in
    /// [`ToolContext::written`]: ingested unless `lost` is set, so the note
    /// a `lost` call names never lands.
    const WRITES: &str = "test_writes";

    struct Writes;

    impl AppTool for Writes {
        type Args = Value;
        type Output = String;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(WRITES, "Writes a note", Vec::new())
        }

        fn call(&self, cx: &mut ToolContext, args: Value) -> Result<String, String> {
            let key = nostrdb_net::FullKeypair::generate();
            let note = nostrdb::NoteBuilder::new()
                .kind(1)
                .content("written")
                .sign(&key.secret_key.secret_bytes())
                .build()
                .ok_or("no note")?;
            let id = NoteId::new(*note.id());
            if !args["lost"].as_bool().unwrap_or(false) {
                let frame = nostrdb_net::ClientMessage::event(&note)
                    .map_err(|err| err.to_string())?
                    .to_json()
                    .map_err(|err| err.to_string())?;
                cx.ndb
                    .process_event_with(&frame, nostrdb::IngestMetadata::new().client(true))
                    .map_err(|err| err.to_string())?;
            }
            cx.written.push(id);
            Ok(id.hex())
        }
    }

    struct Count;

    impl notedeck::StreamTool for Count {
        type Args = Value;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(COUNT, "Counts up", Vec::new())
                .read_only()
                .streams(notedeck::StreamMode::Redraw)
        }

        fn prepare(&self, _cx: &mut ToolContext, args: Value) -> Result<StreamJob, String> {
            let to = args["to"].as_u64().ok_or("no to")?;
            Ok(Box::new(move |mut emitter: notedeck::Emitter| {
                Box::pin(async move {
                    for n in 0..to {
                        if !emitter.emit(&n, Some(format!("{n}\n"))) {
                            return;
                        }
                    }
                })
            }))
        }
    }

    struct Slow;

    impl SlowTool for Slow {
        type Args = Value;
        type Output = String;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(SLOW, "Waits for its gate file", Vec::new()).read_only()
        }

        fn prepare(&self, _cx: &mut ToolContext, args: Value) -> Result<ToolJob<String>, String> {
            let gate = std::path::PathBuf::from(args["gate"].as_str().ok_or("no gate")?);
            Ok(Box::new(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !gate.exists() {
                    if std::time::Instant::now() > deadline {
                        return Err("the gate never opened".to_owned());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok("through".to_owned())
            }))
        }

        fn render(&self, out: &String) -> Option<String> {
            Some(format!("{out}\n"))
        }

        fn notice(&self, _out: &String) -> Option<String> {
            Some("waited for the gate".to_owned())
        }
    }

    struct WhoSigns;

    impl AppTool for WhoSigns {
        type Args = Value;
        type Output = Option<String>;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(WHO_SIGNS, "Says who signs", Vec::new()).accepts_comment_signer()
        }

        fn call(&self, cx: &mut ToolContext, _args: Value) -> Result<Option<String>, String> {
            Ok(cx.comment_signer.map(|key| key.pubkey.hex()))
        }
    }

    struct Echo;

    impl AppTool for Echo {
        type Args = Value;
        type Output = Value;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(ECHO, "Answers with its args", Vec::new()).read_only()
        }

        fn call(&self, _cx: &mut ToolContext, args: Value) -> Result<Value, String> {
            Ok(args)
        }

        fn render(&self, out: &Value) -> Option<String> {
            Some(format!("echoed {out}\n"))
        }
    }

    /// An app with one data tool and UI tools answering each way a tool can,
    /// recording every UI call that reaches it.
    struct ToolApp {
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl notedeck::App for ToolApp {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn tools(&self) -> Vec<RegisteredTool> {
            vec![
                RegisteredTool::new(Echo),
                RegisteredTool::new(WhoSigns),
                RegisteredTool::slow(Slow),
                RegisteredTool::stream(Count),
                RegisteredTool::new(Writes),
            ]
        }

        fn ui_tools(&self) -> Vec<ToolSpec> {
            [PEEK, TOGGLE, CALL_AGAIN]
                .into_iter()
                .map(|name| ToolSpec::new(name, "a test tool", Vec::new()).ui())
                .collect()
        }

        fn call_ui_tool(
            &mut self,
            _ctx: &mut AppContext<'_>,
            name: &str,
            _args: &Value,
        ) -> ToolOutcome {
            self.calls.borrow_mut().push(name.to_owned());
            match name {
                TOGGLE => ToolOutcome::Intent(AppAction::ToggleChrome),
                CALL_AGAIN => {
                    ToolOutcome::Intent(AppAction::CallTool(ToolInvocation::new(PEEK, Value::Null)))
                }
                _ => ToolOutcome::Data(json!({"peeked": true})),
            }
        }
    }

    /// [`IdleApp`]'s data tool.
    const IDLE_ECHO: &str = "test_idle_echo";
    /// [`IdleApp`]'s UI tool, listed only while it runs.
    const IDLE_PEEK: &str = "test_idle_peek";
    /// [`IdleApp`]'s UI tool that [starts it](ToolSpec::starts_app).
    const IDLE_START: &str = "test_idle_start";

    struct IdleEcho;

    impl AppTool for IdleEcho {
        type Args = Value;
        type Output = Value;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(IDLE_ECHO, "Answers with its args", Vec::new())
        }

        fn call(&self, _cx: &mut ToolContext, args: Value) -> Result<Value, String> {
            Ok(args)
        }
    }

    /// An app the fixture installs but never opens, with a data tool, a UI
    /// tool and an app starter.
    struct IdleApp;

    impl notedeck::App for IdleApp {
        fn render(&mut self, _ctx: &mut AppContext<'_>, _ui: &mut egui::Ui) -> AppResponse {
            AppResponse::none()
        }

        fn tools(&self) -> Vec<RegisteredTool> {
            vec![RegisteredTool::new(IdleEcho)]
        }

        fn ui_tools(&self) -> Vec<ToolSpec> {
            vec![
                ToolSpec::new(IDLE_PEEK, "a test tool", Vec::new()).ui(),
                ToolSpec::new(IDLE_START, "a test tool", Vec::new())
                    .ui()
                    .starts_app(),
            ]
        }

        fn call_ui_tool(
            &mut self,
            _ctx: &mut AppContext<'_>,
            name: &str,
            _args: &Value,
        ) -> ToolOutcome {
            ToolOutcome::Data(json!({ "ran": name }))
        }
    }

    /// The slot the fixture installs [`IdleApp`] in.
    const IDLE_SLOT: usize = 1;

    pub(in crate::chrome) struct Fixture {
        _dir: tempfile::TempDir,
        pub(in crate::chrome) notedeck: Notedeck,
        pub(in crate::chrome) chrome: Chrome,
        pub(in crate::chrome) calls: Rc<RefCell<Vec<String>>>,
    }

    /// A chrome running only [`ToolApp`], with [`IdleApp`] installed behind
    /// it and never opened, the tools registered. It never serves the
    /// socket.
    pub(in crate::chrome) fn fixture() -> Fixture {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let calls = Rc::default();
        let mut chrome = Chrome::new_headless_with_apps(
            &mut notedeck,
            [
                crate::app::NotedeckApp::Other(
                    "test".to_owned(),
                    Box::new(ToolApp {
                        calls: Rc::clone(&calls),
                    }),
                ),
                crate::app::NotedeckApp::Other("idle".to_owned(), Box::new(IdleApp)),
            ],
        );
        let update = notedeck::App::take_tool_update(&mut chrome).expect("running set changed");
        notedeck.apply_tool_update(update);
        Fixture {
            _dir: dir,
            notedeck,
            chrome,
            calls,
        }
    }

    fn call(f: &mut Fixture, name: &str, args: Value) -> Served {
        let mut ctx = f.notedeck.app_context();
        let request = Request::CallTool {
            name: name.to_owned(),
            args,
            signer: None,
        };
        serve(&mut f.chrome, &mut ctx, request)
    }

    /// Call [`WHO_SIGNS`] with `signer`, answering with what it saw.
    fn call_signed(f: &mut Fixture, signer: Option<&str>) -> Outcome {
        let mut ctx = f.notedeck.app_context();
        let request = Request::CallTool {
            name: WHO_SIGNS.to_owned(),
            args: json!({}),
            signer: signer.map(|key| Signer {
                comment_nsec: key.to_owned(),
            }),
        };
        reply(serve(&mut f.chrome, &mut ctx, request))
    }

    /// The names `list_tools` answers with.
    fn listed(f: &mut Fixture) -> Vec<String> {
        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        tools.into_iter().map(|tool| tool.name).collect()
    }

    /// Apply the registry update the chrome has, as the next frame would.
    fn next_frame(f: &mut Fixture) {
        if let Some(update) = notedeck::App::take_tool_update(&mut f.chrome) {
            f.notedeck.apply_tool_update(update);
        }
    }

    fn reply(served: Served) -> Outcome {
        match served {
            Served::Reply(response) | Served::Settle { response, .. } => response.outcome,
            Served::Apply(_) => panic!("expected a reply, got an intent"),
            Served::Run(pending) => finished_response(pending.run()).outcome,
            Served::Stream(_) => panic!("expected a reply, got a stream"),
        }
    }

    #[tokio::test]
    async fn list_tools_names_data_ui_and_chrome_tools_with_schemas() {
        let mut f = fixture();
        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is listed"))
        };

        let echo = find(ECHO);
        assert_eq!(echo.kind, ToolKind::Data);
        assert!(echo.read_only);
        assert_eq!(echo.input_schema["type"], json!("object"));
        assert_eq!(find(PEEK).kind, ToolKind::Ui);
        let shell = find(super::super::OPEN_COMMAND_BAR);
        assert_eq!(shell.category.as_deref(), Some(super::super::TOOL_CATEGORY));
    }

    #[tokio::test]
    async fn a_data_tool_answers_with_its_result() {
        let mut f = fixture();
        let served = call(&mut f, ECHO, json!({"a": 1}));
        assert!(
            matches!(served, Served::Reply(_)),
            "a call that writes nothing answers on its own tick"
        );
        assert_eq!(
            reply(served),
            Outcome::Ok(Reply::Result {
                result: json!({"a": 1}),
                text: Some("echoed {\"a\":1}\n".to_owned()),
                notice: None,
            })
        );
        assert!(f.calls.borrow().is_empty(), "no UI tool ran");
    }

    /// A call's signer reaches the context of a tool that accepts one, as a
    /// keypair; a call without one leaves it `None`; and a key that doesn't
    /// parse refuses the call without echoing the key.
    #[tokio::test]
    async fn a_call_tool_signer_reaches_the_tool_context() {
        let mut f = fixture();
        let key = nostrdb_net::FullKeypair::generate();
        let Outcome::Ok(Reply::Result { result, .. }) =
            call_signed(&mut f, Some(&key.secret_key.to_secret_hex()))
        else {
            panic!("the signed call answers");
        };
        assert_eq!(result, json!(key.pubkey.hex()));

        let Outcome::Ok(Reply::Result { result, .. }) = call_signed(&mut f, None) else {
            panic!("the unsigned call answers");
        };
        assert_eq!(result, Value::Null);

        let Outcome::Error { message } = call_signed(&mut f, Some("nsec1notakey")) else {
            panic!("a bad key is refused");
        };
        assert!(message.contains("not a valid key"), "{message}");
        assert!(!message.contains("nsec1notakey"), "{message}");

        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        let accepts = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap()
                .accepts_signer
        };
        assert!(accepts(WHO_SIGNS));
        assert!(!accepts(ECHO));
    }

    /// An app that isn't running lists its data tool and its app starter,
    /// not its other UI tools, and the list is rebuilt only when something
    /// changed.
    #[tokio::test]
    async fn a_stopped_app_lists_its_data_tools_and_starters() {
        let mut f = fixture();
        assert!(!f.chrome.is_running(IDLE_SLOT));
        let names = listed(&mut f);
        assert!(names.iter().any(|n| n == IDLE_ECHO), "{names:?}");
        assert!(names.iter().any(|n| n == IDLE_START), "{names:?}");
        assert!(!names.iter().any(|n| n == IDLE_PEEK), "{names:?}");
        assert!(
            notedeck::App::take_tool_update(&mut f.chrome).is_none(),
            "nothing changed, so nothing is rebuilt"
        );
    }

    /// A call to a stopped app's data tool starts the app and answers at
    /// once; from the next frame its UI tools are listed too.
    #[tokio::test]
    async fn a_data_tool_call_starts_its_app() {
        let mut f = fixture();
        assert_eq!(
            reply(call(&mut f, IDLE_ECHO, json!({"b": 2}))),
            Outcome::Ok(Reply::Result {
                result: json!({"b": 2}),
                text: None,
                notice: None,
            })
        );
        assert!(f.chrome.is_running(IDLE_SLOT), "the call started its app");

        next_frame(&mut f);
        let names = listed(&mut f);
        assert!(names.iter().any(|n| n == IDLE_PEEK), "{names:?}");
    }

    /// A call to a stopped app's app starter starts the app and runs there.
    #[tokio::test]
    async fn an_app_starter_starts_its_app_and_runs_there() {
        let mut f = fixture();
        assert_eq!(
            reply(call(&mut f, IDLE_START, Value::Null)),
            Outcome::Ok(Reply::Result {
                result: json!({ "ran": IDLE_START }),
                text: None,
                notice: None,
            })
        );
        assert!(f.chrome.is_running(IDLE_SLOT));
    }

    /// A stopped app's other UI tools aren't there to call, and calling one
    /// leaves the app stopped.
    #[tokio::test]
    async fn a_stopped_apps_ui_tool_is_unknown() {
        let mut f = fixture();
        assert!(matches!(
            reply(call(&mut f, IDLE_PEEK, Value::Null)),
            Outcome::Error { .. }
        ));
        assert!(!f.chrome.is_running(IDLE_SLOT));
    }

    #[tokio::test]
    async fn a_ui_tool_runs_in_its_app_and_answers_with_its_data() {
        let mut f = fixture();
        assert_eq!(
            reply(call(&mut f, PEEK, Value::Null)),
            Outcome::Ok(Reply::Result {
                result: json!({"peeked": true}),
                text: None,
                notice: None,
            })
        );
        assert_eq!(*f.calls.borrow(), vec![PEEK]);
    }

    #[tokio::test]
    async fn an_intent_is_held_for_the_window() {
        let mut f = fixture();
        let served = call(&mut f, TOGGLE, Value::Null);
        assert!(matches!(served, Served::Apply(AppAction::ToggleChrome)));
        assert!(
            !f.chrome.nav.navigating,
            "serving carries out nothing itself"
        );
    }

    #[tokio::test]
    async fn a_tool_call_is_one_hop() {
        let mut f = fixture();
        let Outcome::Error { message } = reply(call(&mut f, CALL_AGAIN, Value::Null)) else {
            panic!("expected an error");
        };
        assert!(message.contains("one hop"), "{message}");
        assert_eq!(*f.calls.borrow(), vec![CALL_AGAIN], "PEEK never ran");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_error() {
        let mut f = fixture();
        assert!(matches!(
            reply(call(&mut f, "no_such_tool", Value::Null)),
            Outcome::Error { .. }
        ));
        assert!(f.calls.borrow().is_empty());
    }

    /// A call of `dave_spawn_session`, as `nd spawn` sends it, starts Dave,
    /// unopened until now, and opens a session in the directory it names:
    /// the spawn is an [app starter](ToolSpec::starts_app).
    #[cfg(feature = "dave")]
    #[tokio::test]
    async fn spawn_session_starts_dave_and_opens_a_session() {
        let dir = tempfile::TempDir::new().expect("tmp dir");
        let args: Vec<String> = vec!["notedeck-test".into(), "--testrunner".into()];
        let mut notedeck = Notedeck::init(&egui::Context::default(), dir.path(), &args);
        let mut chrome = Chrome::new_headless(&args, &mut notedeck).expect("chrome");
        let dave = chrome
            .apps
            .iter()
            .position(|app| matches!(app, crate::app::NotedeckApp::Dave(_)))
            .expect("dave in the roster");
        assert!(!chrome.is_running(dave), "Dave starts closed");
        let update = notedeck::App::take_tool_update(&mut chrome).expect("a first registry");
        notedeck.apply_tool_update(update);

        let mut ctx = notedeck.app_context();
        let spawn = |chrome: &mut Chrome, ctx: &mut AppContext, cwd: std::path::PathBuf| {
            let request = Request::CallTool {
                name: notedeck_dave::SPAWN_SESSION.to_owned(),
                args: serde_json::json!({ "cwd": cwd }),
                signer: None,
            };
            reply(serve(chrome, ctx, request))
        };
        let Outcome::Ok(Reply::Result { result, .. }) =
            spawn(&mut chrome, &mut ctx, dir.path().to_owned())
        else {
            panic!("the spawn answers with data");
        };
        assert!(result["session_id"].is_u64(), "{result}");
        assert!(chrome.is_running(dave), "the spawn started Dave");

        let Outcome::Error { message } = spawn(&mut chrome, &mut ctx, dir.path().join("missing"))
        else {
            panic!("a missing directory is refused");
        };
        assert!(message.contains("does not exist"), "{message}");
    }

    /// The whole trip: a client on a temp socket, the chrome polling it in
    /// `update` and carrying out the intent in `render`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_client_gets_its_intent_applied() {
        let mut f = fixture();
        let sock = f._dir.path().join("notedeck.sock");
        f.chrome.ipc.listener = Some(notedeck_ipc::Listener::bind(&sock, || {}).unwrap());

        let client = std::thread::spawn(move || {
            notedeck_ipc::Client::connect(&sock)
                .unwrap()
                .request(&Request::CallTool {
                    name: TOGGLE.to_owned(),
                    args: Value::Null,
                    signer: None,
                })
        });

        let egui_ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while f.chrome.ipc.applying.is_empty() {
            assert!(std::time::Instant::now() < deadline, "no request arrived");
            let mut ctx = f.notedeck.app_context();
            ctx.egui = Some(&egui_ctx);
            f.chrome.poll_ipc(&mut ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let mut ctx = f.notedeck.app_context();
        egui_ctx
            .run_ui(egui::RawInput::default(), |ui| {
                f.chrome.apply_ipc_intents(&mut ctx, ui);
            })
            .drop_without_applying_deltas();
        assert!(f.chrome.nav.navigating, "the drawer is opening");

        let response = client.join().unwrap().unwrap();
        assert_eq!(
            response.outcome,
            Outcome::Ok(Reply::Intent {
                intent: IntentStatus::Applied
            })
        );
    }

    /// The whole trip for a stream: a client subscribes on a temp socket,
    /// `poll_ipc` starts the stream, and the client reads every event, in
    /// the subscribe's v2, then the end. The tool lists as a stream; a call
    /// of it, and a subscribe with a bad arg, answer one error each.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_socket_client_follows_a_stream_to_its_end() {
        let mut f = fixture();
        let mut ctx = f.notedeck.app_context();
        let Outcome::Ok(Reply::Tools { tools }) =
            reply(serve(&mut f.chrome, &mut ctx, Request::ListTools))
        else {
            panic!("expected the tool list");
        };
        drop(ctx);
        let count = tools.iter().find(|t| t.name == COUNT).unwrap();
        assert_eq!(count.streams, Some(notedeck_ipc::StreamMode::Redraw));
        assert_eq!(
            count.model_reach,
            Some(notedeck_ipc::ModelReach::Never),
            "notedeck sends the spec's reach, so `nd mcp` reads it rather than rederiving it"
        );
        let Outcome::Error { message } = reply(call(&mut f, COUNT, json!({ "to": 1 }))) else {
            panic!("a stream can't be called");
        };
        assert!(message.contains("subscribe"), "{message}");
        let mut ctx = f.notedeck.app_context();
        let bad = Request::Subscribe {
            name: COUNT.to_owned(),
            args: json!({}),
        };
        assert!(matches!(
            serve(&mut f.chrome, &mut ctx, bad),
            Served::Reply(Response {
                outcome: Outcome::Error { .. },
                ..
            })
        ));
        drop(ctx);

        let sock = f._dir.path().join("notedeck.sock");
        f.chrome.ipc.listener = Some(notedeck_ipc::Listener::bind(&sock, || {}).unwrap());
        let client = std::thread::spawn(move || {
            let mut client = notedeck_ipc::Client::connect(&sock).unwrap();
            client
                .send(&Request::Subscribe {
                    name: COUNT.to_owned(),
                    args: json!({ "to": 3 }),
                })
                .unwrap();
            let mut got = Vec::new();
            while let Some(response) = client.next_response().unwrap() {
                assert_eq!(response.v, 2);
                got.push(response.outcome);
            }
            got
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !client.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the stream never ended"
            );
            let mut ctx = f.notedeck.app_context();
            f.chrome.poll_ipc(&mut ctx);
            drop(ctx);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let event = |n: u64| {
            Outcome::Ok(Reply::Event {
                event: json!(n),
                text: Some(format!("{n}\n")),
            })
        };
        assert_eq!(client.join().unwrap(), vec![event(0), event(1), event(2)]);
    }

    /// A slow tool's work runs off the loop: `poll_ipc` keeps returning while
    /// it waits, and the client is answered, text and notice and all, on the
    /// tick after it finishes.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_tool_answers_after_its_work_without_holding_the_loop() {
        let mut f = fixture();
        let sock = f._dir.path().join("notedeck.sock");
        let gate = f._dir.path().join("gate");
        f.chrome.ipc.listener = Some(notedeck_ipc::Listener::bind(&sock, || {}).unwrap());

        let args = json!({ "gate": gate.to_string_lossy() });
        let client = std::thread::spawn(move || {
            notedeck_ipc::Client::connect(&sock)
                .unwrap()
                .request(&Request::CallTool {
                    name: SLOW.to_owned(),
                    args,
                    signer: None,
                })
        });

        let tick = |f: &mut Fixture| {
            let mut ctx = f.notedeck.app_context();
            f.chrome.poll_ipc(&mut ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while f.chrome.ipc.running.is_empty() {
            assert!(std::time::Instant::now() < deadline, "no request arrived");
            tick(&mut f);
        }
        // The loop ticks on while the work waits.
        for _ in 0..5 {
            tick(&mut f);
        }
        assert_eq!(f.chrome.ipc.running.len(), 1, "still running");

        std::fs::write(&gate, "").unwrap();
        while !f.chrome.ipc.running.is_empty() {
            assert!(std::time::Instant::now() < deadline, "never answered");
            tick(&mut f);
        }
        let response = client.join().unwrap().unwrap();
        assert_eq!(
            response.outcome,
            Outcome::Ok(Reply::Result {
                result: json!("through"),
                text: Some("through\n".to_owned()),
                notice: Some("waited for the gate".to_owned()),
            })
        );
    }

    /// Call [`WRITES`] over a temp socket with `args`, ticking the chrome
    /// until the client is answered; the client looks its note up the moment
    /// the answer arrives. Answers with the reply, whether the note was there
    /// then, and how many ticks the answer was held.
    #[cfg(unix)]
    fn write_over_socket(f: &mut Fixture, args: Value) -> WroteOverSocket {
        let sock = f._dir.path().join("notedeck.sock");
        f.chrome.ipc.listener = Some(notedeck_ipc::Listener::bind(&sock, || {}).unwrap());
        let ndb = f.notedeck.app_context().ndb.clone();
        let client = std::thread::spawn(move || {
            let response = notedeck_ipc::Client::connect(&sock)
                .unwrap()
                .request(&Request::CallTool {
                    name: WRITES.to_owned(),
                    args,
                    signer: None,
                })
                .unwrap();
            let Outcome::Ok(Reply::Result { result, .. }) = &response.outcome else {
                panic!("expected a result, got {:?}", response.outcome);
            };
            let id = NoteId::from_hex(result.as_str().unwrap()).unwrap();
            let txn = nostrdb::Transaction::new(&ndb).unwrap();
            let landed = ndb.get_note_by_id(&txn, id.bytes()).is_ok();
            (response, landed)
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut held = 0;
        while !client.is_finished() {
            assert!(std::time::Instant::now() < deadline, "never answered");
            let mut ctx = f.notedeck.app_context();
            f.chrome.poll_ipc(&mut ctx);
            held += usize::from(!f.chrome.ipc.settling.is_empty());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (response, landed) = client.join().unwrap();
        WroteOverSocket {
            response,
            landed,
            held,
        }
    }

    /// What [`write_over_socket`] saw.
    #[cfg(unix)]
    struct WroteOverSocket {
        response: Response,
        /// Was the note in nostrdb when the client was answered?
        landed: bool,
        /// Ticks the answer was held for.
        held: usize,
    }

    /// An edit's answer waits for its write: when the client is told, the
    /// note it names is already in nostrdb, so its next command sees it.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_answers_once_its_write_reads_back() {
        let mut f = fixture();
        for _ in 0..20 {
            let wrote = write_over_socket(&mut f, json!({}));
            assert!(wrote.landed, "answered before the write read back");
            let Outcome::Ok(Reply::Result { notice, .. }) = wrote.response.outcome else {
                panic!("expected a result");
            };
            assert_eq!(notice, None, "settled well inside the deadline");
        }
        assert!(f.chrome.ipc.settling.is_empty());
    }

    /// A write that never lands holds its answer only until the deadline,
    /// then answers with a notice saying so, rather than hanging the client.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_whose_write_never_lands_answers_at_the_deadline() {
        let mut f = fixture();
        let started = std::time::Instant::now();
        let wrote = write_over_socket(&mut f, json!({ "lost": true }));
        assert!(!wrote.landed);
        assert!(wrote.held > 1, "held across ticks");
        assert!(started.elapsed() >= SETTLE_DEADLINE, "answered early");
        let Outcome::Ok(Reply::Result { notice, .. }) = wrote.response.outcome else {
            panic!("expected a result");
        };
        let notice = notice.expect("a notice");
        assert!(notice.contains("hadn't read back"), "{notice}");
        assert!(f.chrome.ipc.settling.is_empty());
    }
}
