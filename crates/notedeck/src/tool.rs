//! Browser-level, statically-typed agent tools.
//!
//! An "agent tool" is a capability an AI backend can invoke — most importantly
//! a full-text nostrdb query. These are *browser*-owned: they operate on shared
//! state the host holds (the db, caches, accounts), not on any one app's private
//! state, so notedeck models them as a single statically-typed [`ToolCall`]
//! enum rather than a bag of dynamically-dispatched handlers. A dynamic
//! per-app handler could only ever touch this same browser state (it never sees
//! the app's `&mut self`), so it would buy indirection without capability.
//!
//! Tools come in two [`ToolKind`]s that share one name space:
//!
//! - **Data** tools — the browser's [`ToolCall`]s and the apps' [`AppTool`]s —
//!   run over that shared state and return JSON.
//! - **UI** tools run against one app's own `&mut self` (move a cursor, open a
//!   pane). An app advertises them with [`App::ui_tools`](crate::App::ui_tools)
//!   and runs them in [`App::call_ui_tool`](crate::App::call_ui_tool). The
//!   registry only knows each one's spec and owning app: calling one through it
//!   yields an [`Intent`](ToolOutcome::Intent) carrying
//!   [`AppAction::CallTool`], which the shell routes to the owner next. That is
//!   how an app gets an intent of its own; the browser still owns every other
//!   kind.
//!
//! A tool is described in two portable halves so one declaration serves every
//! backend:
//!
//! - the **spec** ([`ToolSpec`]) — a backend-agnostic description (name, docs,
//!   argument schema) reused to build an OpenAI `FunctionObject` or an MCP
//!   `tools/list` entry, and
//! - the **execution** ([`ToolCall::call`]) — runs the parsed, typed call
//!   against a [`ToolContext`] and returns a [`ToolOutcome`].
//!
//! A backend advertises [`ToolCall::specs`], parses an incoming call with
//! [`ToolCall::parse`], and dispatches it with [`ToolCall::call`] over a
//! [`ToolContext`] built from an [`AppContext`](crate::AppContext) via
//! [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
//! `note_context()`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use nostrdb::{Ndb, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::{Accounts, AppAction, AppId, DataPath, ExplicitPublishApi, NoteCache};

/// The portable tool *type* layer — argument types, args, and the tool spec —
/// lives in [`agentium_core::tool`] so the engine owns it and any platform can
/// share it. notedeck re-exports it here so `notedeck::tool::ToolSpec` (and the
/// crate-root `notedeck::ToolSpec`) keep resolving for the browser-level tool
/// machinery below, which builds specs from these types.
pub use agentium_core::tool::{Capability, StreamMode, ToolArg, ToolArgType, ToolKind, ToolSpec};

/// The canonical [`QueryCall`] — the single nostrdb query tool — is likewise
/// owned by the engine and re-exported here, so `notedeck::tool::QueryCall`
/// keeps resolving for the browser [`ToolCall`] below. The browser and the
/// engine's own dave tools share one struct, `to_filter`, `spec`, and
/// note-key execution rather than each maintaining a copy.
pub use agentium_core::tools::QueryCall;

/// The note shape the engine hands a model ([`SimpleNote`]), and the
/// conversion into it, re-exported so an app's nostr tools answer notes the
/// way Dave's `query` tool and thread summaries do.
pub use agentium_core::tools::{note_to_simple, SimpleNote};

/// The browser state a tool executes against, reborrowed from an
/// [`AppContext`](crate::AppContext) via
/// [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
/// [`NoteContext`](crate::NoteContext). Tools open their own short-lived
/// [`Transaction`]s from [`ndb`](Self::ndb) as needed.
pub struct ToolContext<'a> {
    pub ndb: &'a Ndb,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a Accounts,
    /// Explicit-relay publisher, for tools that *mutate* nostr-backed state
    /// (e.g. an app editing its own board) and must fan the resulting events out
    /// to relays the way the app's own edits do. `None` for a read-only context,
    /// where a mutating tool should report an error rather than silently drop the
    /// change. Relay targets and the signer come from [`accounts`](Self::accounts)
    /// (see [`Accounts::selected_account_private_relays`] and
    /// [`Accounts::selected_filled`]); this supplies the remote publish path
    /// used by [`fan_out_event_frame`](crate::fan_out_event_frame).
    pub publish: Option<ExplicitPublishApi<'a>>,
    /// The caller's comment key, to sign comments with in place of the
    /// selected account: an agent's own key, which `nd` sends beside a socket
    /// call (see [`ToolSpec::accepts_comment_signer`]). Only a tool whose
    /// spec opts in ever sees it; [`RegisteredTool`] clears it for any
    /// other. `None` for every model-driven call (Dave, MCP).
    pub comment_signer: Option<&'a nostrdb_net::FullKeypair>,
    /// Where the host keeps its files, for a tool that caches on disk (a
    /// git fetch's bare repos). `None` in a context with no host, where such
    /// a tool reports an error.
    pub path: Option<&'a DataPath>,
    /// The ids of the notes this call wrote, for a caller that answers only
    /// once they read back from [`ndb`](Self::ndb): ingest is asynchronous,
    /// so a client that edits what it just made (`nd headway add`, then a
    /// `title` of the new ref) would otherwise fold a board without it. A
    /// writing tool pushes each id here; the registry hands them on as
    /// [`Rendered::written`], and the socket holds the
    /// answer until they land. Empty for a read, and a backend that answers
    /// a model (Dave) ignores them.
    pub written: Vec<nostrdb_net::NoteId>,
}

/// The result of running a [`ToolCall`].
///
/// [`Data`](Self::Data) is a pure JSON value with no UI side effects — safe to
/// hand straight back to the backend. [`Intent`](Self::Intent) is a UI side
/// effect the caller marshals as a next-frame [`AppAction`] (e.g. routing a
/// column). [`Error`](Self::Error) reports a failure to relay to the backend.
pub enum ToolOutcome {
    Data(Value),
    Intent(AppAction),
    Error(String),
}

/// A [`ToolOutcome`] with the human text a data tool rendered beside its
/// value, for a caller that prints to a person (`nd headway show`). Only
/// [`ToolRegistry::call_rendered`] asks for the text; a backend that hands
/// the value to a model (Dave, `nd mcp`) calls [`ToolRegistry::call`] and
/// never pays for the rendering.
pub struct Rendered {
    pub outcome: ToolOutcome,
    /// What [`AppTool::render`] made of a [`Data`](ToolOutcome::Data) value;
    /// `None` for a tool that renders nothing, and for any other outcome.
    pub text: Option<String>,
    /// A line for the person beside the answer, not part of it: what `nd`
    /// prints to stderr, so piping the text stays clean (see
    /// [`SlowTool::notice`]). `None` for most tools.
    pub notice: Option<String>,
    /// The notes the call wrote (see [`ToolContext::written`]), which a
    /// socket client's answer waits on. Empty for a read.
    pub written: Vec<nostrdb_net::NoteId>,
}

impl From<ToolOutcome> for Rendered {
    /// An outcome with no text.
    fn from(outcome: ToolOutcome) -> Self {
        Self {
            outcome,
            text: None,
            notice: None,
            written: Vec::new(),
        }
    }
}

/// What a [`PendingCall`] came to once its work ran: a [`Rendered`] that can
/// cross threads, since it carries no [`AppAction`].
pub struct Finished {
    /// The tool's value, or its error.
    pub result: Result<Value, String>,
    /// Its text, when asked for and the tool renders one.
    pub text: Option<String>,
    /// Its notice (see [`Rendered::notice`]).
    pub notice: Option<String>,
}

impl From<Finished> for Rendered {
    fn from(done: Finished) -> Self {
        let outcome = match done.result {
            Ok(value) => ToolOutcome::Data(value),
            Err(err) => ToolOutcome::Error(err),
        };
        Self {
            outcome,
            text: done.text,
            notice: done.notice,
            written: Vec::new(),
        }
    }
}

/// A [`SlowTool`] call whose slow half hasn't run yet: owned, so a caller with
/// a render loop runs it on another thread and answers when it
/// [finishes](Finished).
pub struct PendingCall(Box<dyn FnOnce() -> Finished + Send>);

impl PendingCall {
    /// Run the work, blocking until it is done.
    pub fn run(self) -> Finished {
        (self.0)()
    }
}

/// A started call (see [`ToolRegistry::start_rendered`]): answered, or
/// waiting on work to run off the render loop.
pub enum Started {
    Done(Rendered),
    Pending(PendingCall),
}

impl Started {
    /// The answer, running any pending work here and now.
    pub fn finish(self) -> Rendered {
        match self {
            Self::Done(rendered) => rendered,
            Self::Pending(pending) => pending.run().into(),
        }
    }
}

/// A statically-typed, browser-provided tool call, parsed from a backend's
/// request. Each variant owns a concrete argument struct (so a query still calls
/// [`QueryCall::to_filter`] on a real type), and the whole set is what a backend
/// advertises via [`specs`](Self::specs) and dispatches via [`call`](Self::call).
#[derive(Debug, Clone)]
pub enum ToolCall {
    /// A full-text / filtered nostrdb query.
    Query(QueryCall),
}

impl ToolCall {
    /// The specs for every browser-provided tool — what a backend advertises
    /// (OpenAI `tools`, MCP `tools/list`).
    pub fn specs() -> Vec<ToolSpec> {
        vec![QueryCall::spec()]
    }

    /// The registered name of this tool call.
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::Query(_) => QueryCall::NAME,
        }
    }

    /// Parse a backend's tool call by `name` and raw JSON `args` into a typed
    /// [`ToolCall`]. Returns a human-readable error for an unknown tool or
    /// arguments that don't fit the tool's schema.
    pub fn parse(name: &str, args: &Value) -> Result<ToolCall, String> {
        match name {
            QueryCall::NAME => serde_json::from_value::<QueryCall>(args.clone())
                .map(ToolCall::Query)
                .map_err(|e| format!("failed to parse '{name}' arguments: {e}")),
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    /// Execute the call against `cx`, returning what to hand back to the backend
    /// or apply to the UI.
    pub fn call(self, cx: &mut ToolContext) -> ToolOutcome {
        match self {
            ToolCall::Query(query) => {
                let txn = match Transaction::new(cx.ndb) {
                    Ok(txn) => txn,
                    Err(err) => return ToolOutcome::Error(format!("failed to open db: {err}")),
                };
                // Local nostrdb note keys (not portable hex ids) — the engine's
                // single [`QueryCall`] executes to keys.
                ToolOutcome::Data(json!({ "note_keys": query.execute(&txn, cx.ndb).notes }))
            }
        }
    }
}

/// A typed, per-app agent tool contributed by an [`App`](crate::App) over its
/// nostr-backed data.
///
/// Where [`ToolCall`] is the browser's *privileged* built-in tool set — it can
/// emit an [`Intent`](ToolOutcome::Intent) and may grow wider host access — an
/// `AppTool` is an app extension. It sees only a [`ToolContext`] (the same
/// shared db/caches/accounts every tool gets), never the app's private
/// `&mut self`, yet that is enough to do real work: most app data *is* nostr
/// data (a headway board or a notebook node is a nostr event), so the tool's
/// value is its app-specific schema and query logic, not privileged state
/// access. A tool that needs an app's in-memory state is a [`ToolKind::Ui`]
/// tool instead (see [`App::ui_tools`](crate::App::ui_tools)).
///
/// The trait is *typed*: [`Args`](Self::Args) and [`Output`](Self::Output) are
/// plain serde types, so an implementor never juggles [`Value`] by hand. The
/// registry erases those types into a [`RegisteredTool`], which performs the
/// `Value` ⇆ typed conversion at the boundary.
pub trait AppTool: Send + Sync {
    /// The tool's typed arguments, deserialized from the backend's JSON.
    type Args: DeserializeOwned;

    /// The tool's typed result, serialized back to the backend as JSON.
    type Output: Serialize;

    /// The portable [`ToolSpec`] advertised for this tool. Keep it in sync with
    /// [`Args`](Self::Args) by hand (a future revision may derive it).
    fn spec(&self) -> ToolSpec;

    /// Run the tool against `cx` with typed `args`, returning typed output or a
    /// human-readable error to relay to the backend. An `AppTool` produces data
    /// only; a tool with UI side effects is a [`ToolKind::Ui`] tool instead (see
    /// [`App::call_ui_tool`](crate::App::call_ui_tool)).
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String>;

    /// `out` as text for a person to read, beside the JSON value a model
    /// gets: what `nd` prints unless asked for `--json`. `None`, the default,
    /// prints the value instead.
    ///
    /// Render with the muted parts colored (ANSI); the caller strips the
    /// color when its output isn't a terminal, since only it knows.
    fn render(&self, _out: &Self::Output) -> Option<String> {
        None
    }
}

/// The slow half of a [`SlowTool`] call, owned so it runs on any thread.
pub type ToolJob<O> = Box<dyn FnOnce() -> Result<O, String> + Send>;

/// A data tool whose work is too slow for a render loop: a `git fetch`, a
/// network round trip. It runs in two halves. [`prepare`](Self::prepare)
/// reads what the work needs from the [`ToolContext`], quickly, on the
/// caller's thread, and hands back a [`ToolJob`] that owns it; the job does
/// the slow part.
///
/// A caller that can wait for it ([`ToolRegistry::start_rendered`], the
/// socket) runs the job on a worker thread and answers when it is done. One
/// that can't ([`ToolRegistry::call`]: Dave, a key binding) runs it inline,
/// as it runs any [`AppTool`].
pub trait SlowTool: Send + Sync {
    /// The tool's typed arguments, deserialized from the backend's JSON.
    type Args: DeserializeOwned;

    /// The tool's typed result, serialized back to the backend as JSON.
    type Output: Serialize;

    /// The portable [`ToolSpec`] advertised for this tool.
    fn spec(&self) -> ToolSpec;

    /// Read what the work needs from `cx` and return the work, or an error
    /// for arguments that name nothing.
    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<Self::Output>, String>;

    /// `out` as text for a person to read, as [`AppTool::render`].
    fn render(&self, _out: &Self::Output) -> Option<String> {
        None
    }

    /// A line for the person beside the text, which `nd` prints to stderr
    /// so the text pipes clean: where a commit was found, say. `None`, the
    /// default, prints nothing.
    fn notice(&self, _out: &Self::Output) -> Option<String> {
        None
    }
}

/// Where a [`StreamTool`]'s events go: the subscriber's end, as the socket
/// holds it. [`send`](Self::send) never waits: an event it can't take ends
/// the stream. [`send_waiting`](Self::send_waiting) waits for room, for a
/// burst the stream knows is finite.
pub trait EventSink: Send {
    /// Hand on one event, with its text for a person. `false` when the
    /// subscriber is gone, or too far behind to keep: stop emitting.
    fn send(&mut self, event: Value, text: Option<String>) -> bool;

    /// Hand on one event, waiting while the subscriber catches up rather
    /// than dropping it: for a stream's opening history, which can outrun
    /// any queue, but ends. `false` when the subscriber is gone, including
    /// one dropped for not reading at all. The default can't wait, and
    /// [`send`](Self::send)s.
    fn send_waiting<'s>(
        &'s mut self,
        event: Value,
        text: Option<String>,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 's>> {
        Box::pin(std::future::ready(self.send(event, text)))
    }

    /// End the stream with an error for the subscriber.
    fn fail(&mut self, message: String);
}

/// What a running [`StreamJob`] emits through, and how it hears that its
/// subscriber has left.
pub struct Emitter {
    sink: Box<dyn EventSink>,
    /// Turns `true` once the subscriber hangs up.
    gone: tokio::sync::watch::Receiver<bool>,
}

impl Emitter {
    /// Emit into `sink` until `gone` turns `true` (or its sender drops).
    pub fn new(sink: impl EventSink + 'static, gone: tokio::sync::watch::Receiver<bool>) -> Self {
        Self {
            sink: Box::new(sink),
            gone,
        }
    }

    /// Send `event`, with `text` for a person: the lines to append, or the
    /// whole screen, as the spec's [`StreamMode`] says. `false` when the
    /// subscriber is gone: stop then.
    pub fn emit(&mut self, event: &impl Serialize, text: Option<String>) -> bool {
        match serde_json::to_value(event) {
            Ok(event) => self.sink.send(event, text),
            Err(err) => {
                self.sink
                    .fail(format!("failed to serialize an event: {err}"));
                false
            }
        }
    }

    /// [`emit`](Self::emit), waiting for the subscriber to make room rather
    /// than dropping it (see [`EventSink::send_waiting`]): for an opening
    /// burst, such as a follow's history. Keep [`emit`](Self::emit) for live
    /// events, so a subscriber that falls behind them is still dropped.
    ///
    /// The event is serialized before the returned future first runs, so it
    /// needn't outlive the call.
    pub fn emit_waiting(
        &mut self,
        event: &impl Serialize,
        text: Option<String>,
    ) -> impl Future<Output = bool> + Send + '_ {
        let event = serde_json::to_value(event);
        async move {
            match event {
                Ok(event) => self.sink.send_waiting(event, text).await,
                Err(err) => {
                    self.sink
                        .fail(format!("failed to serialize an event: {err}"));
                    false
                }
            }
        }
    }

    /// End the stream with `message`, as a failed call would answer.
    pub fn fail(&mut self, message: String) {
        self.sink.fail(message);
    }

    /// Resolves once the subscriber has hung up, so a stream waiting on its
    /// source stops without emitting first. Cancel-safe, for a `select!`.
    pub async fn gone(&mut self) {
        let _ = self.gone.wait_for(|gone| *gone).await;
    }
}

/// The running half of a [`StreamTool`] call: owned, so the caller spawns it
/// as a task of its own, and it emits until its source ends or its
/// subscriber leaves.
pub type StreamJob = Box<dyn FnOnce(Emitter) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// A data tool that streams: `agentium log --follow`, a live dashboard. A
/// caller subscribes rather than calling, with the socket's `subscribe`
/// request, and gets events until it hangs up.
///
/// Like a [`SlowTool`] it runs in two halves. [`prepare`](Self::prepare)
/// checks the arguments and reads what the stream needs from the
/// [`ToolContext`] (an [`Ndb`] handle, the account), quickly, on the
/// caller's thread; a bad argument fails there, before anything streams. The
/// [`StreamJob`] it returns owns all of that and runs as an async task, so it
/// awaits nostrdb subscriptions and timers without holding up the loop.
///
/// Its spec must say how its events are shown ([`ToolSpec::streams`]);
/// [`RegisteredTool::stream`] marks one that doesn't as
/// [`StreamMode::Append`]. A model, a command bar or a key binding can't
/// wait on a stream, so a [`call`](RegisteredTool::call) of one is an error.
pub trait StreamTool: Send + Sync {
    /// The tool's typed arguments, deserialized from the subscriber's JSON.
    type Args: DeserializeOwned;

    /// The portable [`ToolSpec`] advertised for this tool.
    fn spec(&self) -> ToolSpec;

    /// Check `args` and read what the stream needs from `cx`, returning the
    /// stream, or an error for arguments that name nothing.
    fn prepare(&self, cx: &mut ToolContext, args: Self::Args) -> Result<StreamJob, String>;
}

/// A type-erased [`AppTool`], ready to store in a [`ToolRegistry`] next to tools
/// with different `Args`/`Output` types.
///
/// `AppTool`'s associated types make it non-object-safe, so it can't be held as
/// `dyn AppTool`. Rather than introduce a second object-safe trait, this struct
/// captures the concrete tool in a boxed closure that does the `Value` ⇆ typed
/// serde conversion, and caches the tool's [`ToolSpec`] up front. Build one with
/// [`RegisteredTool::new`].
/// The erased body of a [`RegisteredTool`]: takes raw JSON args, does the
/// `Value` ⇆ typed serde conversion around the concrete [`AppTool::call`] (or
/// [`SlowTool::prepare`]), and returns what it [started](Started), with
/// [`AppTool::render`]'s text when the `bool` asks for it.
type ErasedHandler = Box<dyn Fn(&mut ToolContext, &Value, bool) -> Started + Send + Sync>;

/// The erased body of a streaming [`RegisteredTool`]: raw JSON args in, the
/// [`StreamJob`] [`StreamTool::prepare`] made of them out.
type ErasedStream =
    Box<dyn Fn(&mut ToolContext, &Value) -> Result<StreamJob, String> + Send + Sync>;

/// How a [`RegisteredTool`] runs: called for one answer, or subscribed to.
enum Handler {
    Call(ErasedHandler),
    Stream(ErasedStream),
}

pub struct RegisteredTool {
    spec: ToolSpec,
    handler: Handler,
    /// The app that contributed it, once the shell has said (see
    /// [`owned_by`](Self::owned_by)).
    owner: Option<AppId>,
}

impl RegisteredTool {
    /// Erase a concrete [`AppTool`] into a registrable tool. The returned tool
    /// deserializes incoming JSON into `T::Args`, runs `tool.call`, and
    /// serializes the output into [`ToolOutcome::Data`] — reporting any serde or
    /// execution failure as [`ToolOutcome::Error`]. Asked for text, it renders
    /// the same output with [`AppTool::render`].
    pub fn new<T: AppTool + 'static>(tool: T) -> Self {
        let spec = tool.spec();
        let handler = Box::new(move |cx: &mut ToolContext, args: &Value, text: bool| {
            let args = match serde_json::from_value::<T::Args>(args.clone()) {
                Ok(args) => args,
                Err(err) => return error(format!("invalid arguments: {err}")),
            };
            let output = match tool.call(cx, args) {
                Ok(output) => output,
                Err(err) => return error(err),
            };
            Started::Done(
                Finished {
                    result: serialize(&output),
                    text: text.then(|| tool.render(&output)).flatten(),
                    notice: None,
                }
                .into(),
            )
        });
        Self {
            spec,
            handler: Handler::Call(handler),
            owner: None,
        }
    }

    /// Erase a [`SlowTool`]: its [`prepare`](SlowTool::prepare) runs on the
    /// caller's thread, and its job is handed back as a [`PendingCall`] that
    /// serializes and renders the output once it has run.
    pub fn slow<T: SlowTool + 'static>(tool: T) -> Self {
        let spec = tool.spec();
        let tool = Arc::new(tool);
        let handler = Box::new(move |cx: &mut ToolContext, args: &Value, text: bool| {
            let args = match serde_json::from_value::<T::Args>(args.clone()) {
                Ok(args) => args,
                Err(err) => return error(format!("invalid arguments: {err}")),
            };
            let job = match tool.prepare(cx, args) {
                Ok(job) => job,
                Err(err) => return error(err),
            };
            let tool = Arc::clone(&tool);
            Started::Pending(PendingCall(Box::new(move || match job() {
                Ok(output) => Finished {
                    result: serialize(&output),
                    text: text.then(|| tool.render(&output)).flatten(),
                    notice: tool.notice(&output),
                },
                Err(err) => Finished {
                    result: Err(err),
                    text: None,
                    notice: None,
                },
            })))
        });
        Self {
            spec,
            handler: Handler::Call(handler),
            owner: None,
        }
    }

    /// Erase a [`StreamTool`]. Its spec is marked [`StreamMode::Append`]
    /// unless it already says how it streams.
    pub fn stream<T: StreamTool + 'static>(tool: T) -> Self {
        let spec = tool.spec();
        let spec = match spec.stream_mode() {
            Some(_) => spec,
            None => spec.streams(StreamMode::Append),
        };
        let handler = Box::new(move |cx: &mut ToolContext, args: &Value| {
            let args = serde_json::from_value::<T::Args>(args.clone())
                .map_err(|err| format!("invalid arguments: {err}"))?;
            tool.prepare(cx, args)
        });
        Self {
            spec,
            handler: Handler::Stream(handler),
            owner: None,
        }
    }

    /// Start the stream of a tool registered with [`stream`](Self::stream),
    /// for the caller to spawn with the subscriber's [`Emitter`]. Any other
    /// tool answers once, so subscribing to it is an error.
    pub fn subscribe(&self, cx: &mut ToolContext, args: &Value) -> Result<StreamJob, String> {
        match &self.handler {
            Handler::Stream(prepare) => {
                cx.comment_signer = None;
                prepare(cx, args)
            }
            Handler::Call(_) => Err(format!(
                "'{}' answers once and doesn't stream; call it instead",
                self.name()
            )),
        }
    }

    /// Tag the tool with the app slot that contributed it, so a command bar
    /// can list it with that app's tools. The shell does this as it collects
    /// the running apps' tools; an app never needs to.
    pub fn owned_by(mut self, owner: AppId) -> Self {
        self.owner = Some(owner);
        self
    }

    /// The app slot that contributed it, if the shell has said.
    pub fn owner(&self) -> Option<AppId> {
        self.owner
    }

    /// The portable spec advertised for this tool.
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// The tool's registered name (shorthand for `self.spec().name()`).
    pub fn name(&self) -> &'static str {
        self.spec.name()
    }

    /// Run the erased tool with raw JSON `args`. A [`SlowTool`]'s work runs
    /// here too, blocking until it is done.
    pub fn call(&self, cx: &mut ToolContext, args: &Value) -> ToolOutcome {
        self.run(cx, args, false).finish().outcome
    }

    /// Run it as [`call`](Self::call) does, and render its value as text too
    /// (see [`AppTool::render`]).
    pub fn call_rendered(&self, cx: &mut ToolContext, args: &Value) -> Rendered {
        self.run(cx, args, true).finish()
    }

    /// Start it, rendering its value as text, leaving a [`SlowTool`]'s work
    /// [pending](Started::Pending) for the caller to run where it won't
    /// block.
    pub fn start_rendered(&self, cx: &mut ToolContext, args: &Value) -> Started {
        self.run(cx, args, true)
    }

    /// Run the handler, first taking the caller's comment key away from a
    /// tool that didn't ask for one, so no other tool can sign with it. The
    /// notes the call wrote ride out on its answer as
    /// [`Rendered::written`].
    fn run(&self, cx: &mut ToolContext, args: &Value, text: bool) -> Started {
        if !self.spec.takes_comment_signer() {
            cx.comment_signer = None;
        }
        match &self.handler {
            Handler::Call(handler) => match handler(cx, args, text) {
                Started::Done(mut rendered) => {
                    rendered.written = std::mem::take(&mut cx.written);
                    Started::Done(rendered)
                }
                pending => pending,
            },
            Handler::Stream(_) => error(format!(
                "'{}' streams, so it can't be called for one answer; subscribe to it (`nd {}`)",
                self.name(),
                self.spec.command_path().join(" ")
            )),
        }
    }
}

/// A call that failed before it ran, as a handler answers it.
fn error(err: String) -> Started {
    Started::Done(ToolOutcome::Error(err).into())
}

/// A tool's output as the JSON it answers with.
fn serialize(output: &impl Serialize) -> Result<Value, String> {
    serde_json::to_value(output).map_err(|err| format!("failed to serialize output: {err}"))
}

/// The shell's UI tool that saves the account's keymap overrides, called with
/// `{ "toml": "<the user keymap>" }`. Named here rather than in the chrome so an
/// app with a keymap editor (Notebook's) can raise it as an
/// [`AppAction::CallTool`](crate::AppAction::CallTool) without depending on the
/// chrome.
pub const SAVE_KEYMAP_TOOL: &str = "chrome_save_keymap";

/// A call to a [`ToolKind::Ui`] tool, on its way from whoever raised it (a key
/// binding, a macro step, an agent) to the app that owns the tool. Carried by
/// [`AppAction::CallTool`]; the shell looks the owner up with
/// [`ToolRegistry::ui_owner`] and hands it to
/// [`App::call_ui_tool`](crate::App::call_ui_tool).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    /// The UI tool's registered name, e.g. `headway_cursor_down`.
    pub name: String,
    /// The tool's arguments, `Value::Null` when it takes none.
    pub args: Value,
}

impl ToolInvocation {
    /// A call to the UI tool `name` with `args`.
    pub fn new(name: impl Into<String>, args: Value) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

/// Who runs a [`ToolKind::Ui`] tool: an app, or the shell around the apps
/// (the chrome's own tools, like toggling its which-key strip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOwner {
    /// The app in this slot.
    App(AppId),
    /// The shell itself.
    Shell,
}

/// A registered [`ToolKind::Ui`] tool: its spec and who runs it. The registry
/// can't call a UI tool itself (it needs its owner's `&mut self`), so it keeps
/// only enough to advertise it and to tell the shell where it goes.
#[derive(Debug, Clone)]
pub struct UiToolEntry {
    spec: ToolSpec,
    owner: ToolOwner,
}

impl UiToolEntry {
    /// Register `spec` as a UI tool run by the app in slot `owner`. The spec is
    /// marked [`ToolKind::Ui`] whether or not the app already did.
    pub fn new(spec: ToolSpec, owner: AppId) -> Self {
        Self {
            spec: spec.ui(),
            owner: ToolOwner::App(owner),
        }
    }

    /// Register `spec` as a UI tool the shell runs itself.
    pub fn shell(spec: ToolSpec) -> Self {
        Self {
            spec: spec.ui(),
            owner: ToolOwner::Shell,
        }
    }

    /// The UI tool's spec, always [`ToolKind::Ui`].
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Who runs this tool.
    pub fn owner(&self) -> ToolOwner {
        self.owner
    }
}

/// A fresh set of app-contributed tools for the host's [`ToolRegistry`], built
/// by the shell from its apps (see
/// [`App::take_tool_update`](crate::App::take_tool_update)) and applied with
/// [`ToolRegistry::reset`].
#[derive(Default)]
pub struct ToolUpdate {
    /// Data tools, from every installed app's
    /// [`App::tools`](crate::App::tools), running or not: a caller that
    /// calls one of a stopped app's has the shell start the app.
    pub tools: Vec<RegisteredTool>,
    /// UI tools, from each running app's
    /// [`App::ui_tools`](crate::App::ui_tools), tagged with the app's slot,
    /// and the [app starters](ToolSpec::starts_app) of the apps that aren't
    /// running. A UI tool acts on its app's view, which a stopped app hasn't
    /// got.
    pub ui_tools: Vec<UiToolEntry>,
}

/// One registered tool as [`ToolRegistry::entries`] lists it: its spec and
/// who it belongs to.
#[derive(Clone, Copy)]
pub struct ToolEntry<'a> {
    /// The tool's spec; its [`kind`](ToolSpec::kind) tells data from UI.
    pub spec: &'a ToolSpec,
    /// Who it belongs to: for a UI tool who runs it, for a data tool the app
    /// that contributed it. `None` for a data tool registered without one
    /// (see [`RegisteredTool::owned_by`]).
    pub owner: Option<ToolOwner>,
}

/// Where a registered name points: into the data tools or the UI tools.
#[derive(Clone, Copy)]
enum ToolSlot {
    Data(usize),
    Ui(usize),
}

/// A set of app-contributed tools, data and UI, indexed by one shared name
/// space.
///
/// Rebuilt by the shell from its apps and exposed read-only on
/// [`AppContext`](crate::AppContext), so a backend can advertise
/// ([`specs`](Self::specs)) and dispatch ([`call`](Self::call)) app tools
/// uniformly. Mirrors [`KindRendererRegistry`](crate::KindRendererRegistry).
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<RegisteredTool>,
    ui_tools: Vec<UiToolEntry>,
    by_name: HashMap<String, ToolSlot>,
}

impl ToolRegistry {
    /// Register an app data tool. A name already taken, by either kind, is
    /// logged and skipped, so the first registration for a name wins
    /// (registration order is app-startup order).
    pub fn register(&mut self, tool: RegisteredTool) {
        let name = tool.name();
        if self.by_name.contains_key(name) {
            tracing::warn!("ignoring duplicate agent tool '{name}'");
            return;
        }
        self.by_name
            .insert(name.to_owned(), ToolSlot::Data(self.tools.len()));
        self.tools.push(tool);
    }

    /// Register an app UI tool, with the same first-wins rule across both kinds
    /// as [`register`](Self::register).
    pub fn register_ui(&mut self, entry: UiToolEntry) {
        let name = entry.spec.name();
        if self.by_name.contains_key(name) {
            tracing::warn!("ignoring duplicate UI tool '{name}'");
            return;
        }
        self.by_name
            .insert(name.to_owned(), ToolSlot::Ui(self.ui_tools.len()));
        self.ui_tools.push(entry);
    }

    /// Replace the whole registry with `update`, data tools first, keeping the
    /// duplicate-skipping rules of [`register`](Self::register). The chrome shell
    /// calls this to re-derive the app-tool set when the running apps change:
    /// every installed app's data tools, whose call starts the app if it isn't
    /// running, and the running apps' UI tools (see
    /// [`ToolUpdate`]).
    pub fn reset(&mut self, update: ToolUpdate) {
        self.tools.clear();
        self.ui_tools.clear();
        self.by_name.clear();
        for tool in update.tools {
            self.register(tool);
        }
        for entry in update.ui_tools {
            self.register_ui(entry);
        }
    }

    /// The specs for every registered app tool, data tools then UI tools. A
    /// backend that only wants what a model can use filters on
    /// [`ToolSpec::kind`].
    pub fn specs(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools
            .iter()
            .map(RegisteredTool::spec)
            .chain(self.ui_tools.iter().map(UiToolEntry::spec))
    }

    /// Every registered tool with its owner, in [`specs`](Self::specs)' order:
    /// what a command bar lists, before it asks which can run now.
    pub fn entries(&self) -> impl Iterator<Item = ToolEntry<'_>> {
        let data = self.tools.iter().map(|tool| ToolEntry {
            spec: tool.spec(),
            owner: tool.owner.map(ToolOwner::App),
        });
        let ui = self.ui_tools.iter().map(|entry| ToolEntry {
            spec: entry.spec(),
            owner: Some(entry.owner),
        });
        data.chain(ui)
    }

    /// The tool registered as `name`, of either kind, with its owner.
    pub fn entry(&self, name: &str) -> Option<ToolEntry<'_>> {
        Some(match *self.by_name.get(name)? {
            ToolSlot::Data(idx) => ToolEntry {
                spec: self.tools[idx].spec(),
                owner: self.tools[idx].owner.map(ToolOwner::App),
            },
            ToolSlot::Ui(idx) => ToolEntry {
                spec: self.ui_tools[idx].spec(),
                owner: Some(self.ui_tools[idx].owner),
            },
        })
    }

    /// Whether a tool with `name` is registered, of either kind.
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Who runs the UI tool `name`, or `None` when `name` is not a registered
    /// UI tool.
    pub fn ui_owner(&self, name: &str) -> Option<ToolOwner> {
        match self.by_name.get(name)? {
            ToolSlot::Ui(idx) => Some(self.ui_tools[*idx].owner),
            ToolSlot::Data(_) => None,
        }
    }

    /// Dispatch a tool call by `name` with raw JSON `args`. A data tool runs
    /// here and returns its result. A UI tool can't run here, so it returns an
    /// [`Intent`](ToolOutcome::Intent) carrying [`AppAction::CallTool`] for the
    /// shell to route to its owner. An unknown name is an
    /// [`Error`](ToolOutcome::Error).
    pub fn call(&self, cx: &mut ToolContext, name: &str, args: &Value) -> ToolOutcome {
        match self.by_name.get(name) {
            Some(&ToolSlot::Data(idx)) => self.tools[idx].call(cx, args),
            _ => self.not_here(name, args),
        }
    }

    /// Dispatch as [`call`](Self::call) does, with a data tool's rendered text
    /// beside its value (see [`AppTool::render`]). Only a data tool renders.
    pub fn call_rendered(&self, cx: &mut ToolContext, name: &str, args: &Value) -> Rendered {
        self.start_rendered(cx, name, args).finish()
    }

    /// Start a call as [`call_rendered`](Self::call_rendered) does, leaving a
    /// [`SlowTool`]'s work [pending](Started::Pending) for the caller to run
    /// off its render loop.
    pub fn start_rendered(&self, cx: &mut ToolContext, name: &str, args: &Value) -> Started {
        match self.by_name.get(name) {
            Some(&ToolSlot::Data(idx)) => self.tools[idx].start_rendered(cx, args),
            _ => Started::Done(self.not_here(name, args).into()),
        }
    }

    /// Start the stream of the streaming data tool `name` (see
    /// [`StreamTool`]), for the caller to spawn. A name that isn't a
    /// registered streaming tool is an error.
    pub fn subscribe(
        &self,
        cx: &mut ToolContext,
        name: &str,
        args: &Value,
    ) -> Result<StreamJob, String> {
        match self.by_name.get(name) {
            Some(&ToolSlot::Data(idx)) => self.tools[idx].subscribe(cx, args),
            Some(&ToolSlot::Ui(_)) => Err(format!(
                "'{name}' acts on the window and doesn't stream; call it instead"
            )),
            None => Err(format!("unknown tool '{name}'")),
        }
    }

    /// What a call of `name` is when no data tool by that name runs here: an
    /// intent for a UI tool's owner, else an unknown-tool error.
    fn not_here(&self, name: &str, args: &Value) -> ToolOutcome {
        match self.by_name.get(name) {
            Some(&ToolSlot::Ui(_)) => {
                ToolOutcome::Intent(AppAction::CallTool(ToolInvocation::new(name, args.clone())))
            }
            _ => ToolOutcome::Error(format!("unknown tool '{name}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_util::TempNdb, UnknownIds, FALLBACK_PUBKEY};
    use serde::Deserialize;

    /// An app tool whose typed `Args`/`Output` exercise the serde boundary. It
    /// ignores the [`ToolContext`], so registry dispatch is testable without
    /// touching the db.
    struct Echo;

    #[derive(Deserialize)]
    struct EchoArgs {
        message: String,
    }

    #[derive(Serialize)]
    struct EchoOutput {
        echoed: String,
    }

    impl AppTool for Echo {
        type Args = EchoArgs;
        type Output = EchoOutput;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new(
                "echo",
                "Echo a message back.",
                vec![ToolArg::new("message", ToolArgType::String, "the message").required(true)],
            )
        }

        fn call(&self, _cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
            Ok(EchoOutput {
                echoed: args.message,
            })
        }

        fn render(&self, out: &Self::Output) -> Option<String> {
            Some(format!("echo: {}\n", out.echoed))
        }
    }

    /// The owned pieces a [`ToolContext`] borrows, over a throwaway db. The
    /// temporary directory is kept so it outlives the borrows.
    struct ToolEnv {
        db: TempNdb,
        note_cache: NoteCache,
        accounts: Accounts,
    }

    /// A live [`ToolContext`]'s pieces over a throwaway db, for exercising
    /// dispatch.
    fn test_tool_env() -> ToolEnv {
        let TempNdb { dir, mut ndb } = TempNdb::new();
        let txn = Transaction::new(&ndb).expect("txn");
        let mut unknown_ids = UnknownIds::default();
        let accounts = Accounts::new(
            None,
            Vec::new(),
            Vec::new(),
            FALLBACK_PUBKEY(),
            &mut ndb,
            &txn,
            &mut unknown_ids,
        );
        drop(txn);
        ToolEnv {
            db: TempNdb { ndb, dir },
            note_cache: NoteCache::default(),
            accounts,
        }
    }

    /// A stream that counts from 0 to its `to`, one event each, then ends.
    struct Count;

    #[derive(Deserialize)]
    struct CountArgs {
        to: u32,
    }

    impl StreamTool for Count {
        type Args = CountArgs;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new("count", "Count up.", Vec::new()).command(&["test", "count"])
        }

        fn prepare(&self, _cx: &mut ToolContext, args: CountArgs) -> Result<StreamJob, String> {
            if args.to > 10 {
                return Err("too far".to_owned());
            }
            Ok(Box::new(move |mut emitter: Emitter| {
                Box::pin(async move {
                    for n in 0..args.to {
                        if !emitter.emit(&n, Some(format!("{n}\n"))) {
                            return;
                        }
                    }
                })
            }))
        }
    }

    /// An [`EventSink`] that records what it's sent, and stops taking events
    /// after `room` of them, as a full queue does.
    struct Recorder {
        events: std::sync::mpsc::Sender<(Value, Option<String>)>,
        room: usize,
    }

    impl EventSink for Recorder {
        fn send(&mut self, event: Value, text: Option<String>) -> bool {
            if self.room == 0 {
                return false;
            }
            self.room -= 1;
            self.events.send((event, text)).is_ok()
        }

        fn fail(&mut self, message: String) {
            panic!("the stream failed: {message}");
        }
    }

    /// A stream tool is marked as one, refuses a call, checks its args when
    /// subscribed to, and its job emits through the subscriber's sink until
    /// the sink stops taking events. A tool that answers once refuses a
    /// subscribe.
    #[tokio::test]
    async fn a_stream_tool_is_subscribed_to_not_called() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::stream(Count));
        reg.register(RegisteredTool::new(Echo));
        let spec = reg.specs().find(|s| s.name() == "count").unwrap();
        assert_eq!(spec.stream_mode(), Some(StreamMode::Append));

        let ToolOutcome::Error(err) = reg.call(&mut cx, "count", &json!({ "to": 2 })) else {
            panic!("a stream can't be called");
        };
        assert!(err.contains("nd test count"), "{err}");
        let Err(err) = reg.subscribe(&mut cx, "count", &json!({ "to": 11 })) else {
            panic!("a bad arg fails before anything streams");
        };
        assert_eq!(err, "too far");
        let Err(err) = reg.subscribe(&mut cx, "echo", &json!({ "message": "hi" })) else {
            panic!("a call isn't a stream");
        };
        assert!(err.contains("doesn't stream"), "{err}");

        let (events, got) = std::sync::mpsc::channel();
        let (_gone_tx, gone) = tokio::sync::watch::channel(false);
        let job = reg
            .subscribe(&mut cx, "count", &json!({ "to": 5 }))
            .unwrap();
        job(Emitter::new(Recorder { events, room: 3 }, gone)).await;
        let got: Vec<_> = got.try_iter().collect();
        assert_eq!(
            got,
            vec![
                (json!(0), Some("0\n".to_owned())),
                (json!(1), Some("1\n".to_owned())),
                (json!(2), Some("2\n".to_owned())),
            ],
            "it stopped when the sink did"
        );
    }

    #[test]
    fn registry_dispatches_typed_app_tool() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };

        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));

        // A valid, typed call round-trips through serde to Data.
        let out = reg.call(&mut cx, "echo", &json!({ "message": "hi" }));
        let ToolOutcome::Data(value) = out else {
            panic!("expected data");
        };
        assert_eq!(value["echoed"], "hi");
    }

    /// A slow tool: doubles its number off the caller's thread, refusing a
    /// negative one before any work.
    struct Double;

    impl SlowTool for Double {
        type Args = i64;
        type Output = i64;

        fn spec(&self) -> ToolSpec {
            ToolSpec::new("double", "Double a number, slowly.", Vec::new())
        }

        fn prepare(&self, _cx: &mut ToolContext, n: i64) -> Result<ToolJob<i64>, String> {
            if n < 0 {
                return Err("negative".to_owned());
            }
            Ok(Box::new(move || Ok(n * 2)))
        }

        fn render(&self, out: &i64) -> Option<String> {
            Some(format!("{out}\n"))
        }

        fn notice(&self, out: &i64) -> Option<String> {
            Some(format!("doubled into {out}"))
        }
    }

    /// A slow tool's work is left pending by `start_rendered`, for a caller
    /// to run off its loop, and run inline by `call` and `call_rendered`;
    /// either way the value, text and notice are the same. A refusal in
    /// `prepare` is an answer straight away.
    #[test]
    fn a_slow_tool_runs_pending_or_inline() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::slow(Double));

        let Started::Pending(pending) = reg.start_rendered(&mut cx, "double", &json!(21)) else {
            panic!("the work is left pending");
        };
        let done = pending.run();
        assert_eq!(done.result, Ok(json!(42)));
        assert_eq!(done.text.as_deref(), Some("42\n"));
        assert_eq!(done.notice.as_deref(), Some("doubled into 42"));

        let ToolOutcome::Data(value) = reg.call(&mut cx, "double", &json!(4)) else {
            panic!("call runs the work inline");
        };
        assert_eq!(value, json!(8));
        let rendered = reg.call_rendered(&mut cx, "double", &json!(4));
        assert_eq!(rendered.notice.as_deref(), Some("doubled into 8"));

        let Started::Done(refused) = reg.start_rendered(&mut cx, "double", &json!(-1)) else {
            panic!("a refusal needs no worker");
        };
        assert!(matches!(refused.outcome, ToolOutcome::Error(e) if e == "negative"));
    }

    /// Asked for text, a data tool renders the value it answers with; an
    /// error or unknown tool carries none.
    #[test]
    fn call_rendered_carries_the_tool_text() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));

        let Rendered { outcome, text, .. } =
            reg.call_rendered(&mut cx, "echo", &json!({ "message": "hi" }));
        let ToolOutcome::Data(value) = outcome else {
            panic!("expected data");
        };
        assert_eq!(value["echoed"], "hi");
        assert_eq!(text.as_deref(), Some("echo: hi\n"));

        let bad = reg.call_rendered(&mut cx, "echo", &json!({ "wrong": 1 }));
        assert!(matches!(bad.outcome, ToolOutcome::Error(_)));
        assert_eq!(bad.text, None);
        let unknown = reg.call_rendered(&mut cx, "nope", &json!({}));
        assert!(matches!(unknown.outcome, ToolOutcome::Error(_)));
    }

    /// Answers with the pubkey of the comment key its context carries, if
    /// any; opts in to one only when `accepts` is set.
    struct WhoSigns {
        name: &'static str,
        accepts: bool,
    }

    impl AppTool for WhoSigns {
        type Args = Value;
        type Output = Option<String>;

        fn spec(&self) -> ToolSpec {
            let spec = ToolSpec::new(self.name, "Say who signs.", Vec::new());
            if self.accepts {
                spec.accepts_comment_signer()
            } else {
                spec
            }
        }

        fn call(&self, cx: &mut ToolContext, _args: Value) -> Result<Option<String>, String> {
            Ok(cx.comment_signer.map(|key| key.pubkey.hex()))
        }
    }

    /// A caller's comment key reaches a tool that accepts one, and is taken
    /// away from one that doesn't, on both dispatch paths.
    #[test]
    fn only_an_opted_in_tool_sees_the_comment_signer() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let key = nostrdb_net::FullKeypair::generate();
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(WhoSigns {
            name: "signs",
            accepts: true,
        }));
        reg.register(RegisteredTool::new(WhoSigns {
            name: "plain",
            accepts: false,
        }));
        let mut call = |name: &str, rendered: bool| {
            let mut cx = ToolContext {
                ndb: &db.ndb,
                note_cache: &mut note_cache,
                accounts: &accounts,
                publish: None,
                comment_signer: Some(&key),
                path: None,
                written: Vec::new(),
            };
            let outcome = if rendered {
                reg.call_rendered(&mut cx, name, &json!({})).outcome
            } else {
                reg.call(&mut cx, name, &json!({}))
            };
            let ToolOutcome::Data(value) = outcome else {
                panic!("expected data");
            };
            value
        };

        for rendered in [false, true] {
            assert_eq!(call("signs", rendered), json!(key.pubkey.hex()));
            assert_eq!(call("plain", rendered), Value::Null);
        }
    }

    #[test]
    fn registry_rejects_bad_args_and_unknown_tools() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };

        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));

        // Args that don't match the typed struct never reach `call`.
        let bad = reg.call(&mut cx, "echo", &json!({ "wrong": 1 }));
        assert!(matches!(bad, ToolOutcome::Error(e) if e.contains("invalid arguments")));

        // An unknown name is a clean error, not a panic.
        let unknown = reg.call(&mut cx, "nope", &json!({}));
        assert!(matches!(unknown, ToolOutcome::Error(e) if e.contains("unknown tool")));
    }

    #[test]
    fn registry_skips_duplicate_names() {
        let mut reg = ToolRegistry::default();
        reg.register(RegisteredTool::new(Echo));
        reg.register(RegisteredTool::new(Echo));

        // The duplicate is dropped; only one spec is advertised.
        assert_eq!(reg.specs().filter(|s| s.name() == "echo").count(), 1);
        assert!(reg.contains("echo"));
    }

    /// A UI tool spec owned by app slot `owner`.
    fn ui_entry(name: &'static str, owner: usize) -> UiToolEntry {
        UiToolEntry::new(ToolSpec::new(name, "a UI tool", Vec::new()), AppId(owner))
    }

    #[test]
    fn registry_lists_ui_tools_tagged_ui() {
        let mut reg = ToolRegistry::default();
        reg.reset(ToolUpdate {
            tools: vec![RegisteredTool::new(Echo)],
            ui_tools: vec![ui_entry("test_cursor_down", 3)],
        });

        let kinds: Vec<_> = reg.specs().map(|s| (s.name(), s.kind())).collect();
        assert_eq!(
            kinds,
            vec![("echo", ToolKind::Data), ("test_cursor_down", ToolKind::Ui)]
        );
        assert_eq!(
            reg.ui_owner("test_cursor_down"),
            Some(ToolOwner::App(AppId(3)))
        );
        // A data tool has no owner app to route to.
        assert_eq!(reg.ui_owner("echo"), None);
        assert_eq!(reg.ui_owner("nope"), None);
    }

    #[test]
    fn calling_a_ui_tool_yields_a_call_tool_intent() {
        let ToolEnv {
            db,
            mut note_cache,
            accounts,
        } = test_tool_env();
        let mut cx = ToolContext {
            ndb: &db.ndb,
            note_cache: &mut note_cache,
            accounts: &accounts,
            publish: None,
            comment_signer: None,
            path: None,
            written: Vec::new(),
        };

        let mut reg = ToolRegistry::default();
        reg.register_ui(ui_entry("test_cursor_down", 0));

        let args = json!({ "count": 2 });
        let out = reg.call(&mut cx, "test_cursor_down", &args);
        let ToolOutcome::Intent(AppAction::CallTool(invocation)) = out else {
            panic!("expected a CallTool intent");
        };
        assert_eq!(invocation, ToolInvocation::new("test_cursor_down", args));
    }

    #[test]
    fn duplicate_names_across_kinds_keep_the_first() {
        // A UI tool registered first keeps its name against a data tool...
        let mut reg = ToolRegistry::default();
        reg.register_ui(ui_entry("echo", 1));
        reg.register(RegisteredTool::new(Echo));
        let kinds: Vec<_> = reg.specs().map(|s| s.kind()).collect();
        assert_eq!(kinds, vec![ToolKind::Ui]);
        assert_eq!(reg.ui_owner("echo"), Some(ToolOwner::App(AppId(1))));

        // ...and a data tool registered first keeps it against a UI tool.
        let mut reg = ToolRegistry::default();
        reg.reset(ToolUpdate {
            tools: vec![RegisteredTool::new(Echo)],
            ui_tools: vec![ui_entry("echo", 1)],
        });
        let kinds: Vec<_> = reg.specs().map(|s| s.kind()).collect();
        assert_eq!(kinds, vec![ToolKind::Data]);
        assert_eq!(reg.ui_owner("echo"), None);
    }

    #[test]
    fn query_spec_json_schema_shape() {
        let schema = QueryCall::spec().json_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
        // Only `limit` is required.
        assert_eq!(schema["required"], Value::Array(vec![Value::from("limit")]));

        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props["search"]["type"], "string");
        assert_eq!(props["limit"]["type"], "number");
        // The default is folded into the description the model sees, with no
        // double-paren regression.
        let limit_desc = props["limit"]["description"].as_str().unwrap();
        assert!(limit_desc.contains("Default: 50"));
        assert!(!limit_desc.contains("))"));
    }

    #[test]
    fn parse_query_call() {
        let args = json!({ "search": "nostr", "limit": 5 });
        let ToolCall::Query(q) = ToolCall::parse("query", &args).unwrap();
        assert_eq!(q.search.as_deref(), Some("nostr"));
        assert_eq!(q.limit, Some(5));
    }

    #[test]
    fn parse_unknown_tool_errors() {
        let err = ToolCall::parse("nope", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn query_defaults() {
        // An empty query is valid and falls back to sensible defaults.
        let ToolCall::Query(q) = ToolCall::parse("query", &json!({})).unwrap();
        assert_eq!(q.limit(), 10);
        assert_eq!(q.kind, None);
    }
}
