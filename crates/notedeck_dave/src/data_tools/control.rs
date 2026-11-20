//! Data tools that drive agentium sessions — send a message, spawn, resume,
//! interrupt, answer a permission request, change the mode, link an issue,
//! and edit run configs — one per `agentium` CLI command, so
//! `nd agentium send|spawn|…` is that CLI.
//!
//! Each builds its event with an [embedded](agentium_core::Engine::embedded)
//! engine over the host's nostrdb, as Dave's own controls do: the engine
//! PNS-wraps the event and ingests it, and the host's private sync fans the
//! envelope out to the account's private relays. So a tool publishes the
//! same way in a headless notedeck that hosts no agents as in a window with
//! Dave open, and the session's host — this machine's Dave or another's —
//! picks the command up off its own subscription.
//!
//! `spawn --wait` and `link` wait for the host to answer. They are
//! [`SlowTool`]s: the wait runs on the socket's worker thread, never on a
//! frame.

use std::time::Duration;

use agentium_core::agent_spec::SpecOverrides;
use agentium_core::cli::config::{
    describe, pick_place, render_list, render_published, render_show, replacing, required_field,
    resolve_config, ConfigFilters, ConfigJson,
};
use agentium_core::cli::control::{
    parse_issue_url, parse_mode, parse_model, pick_request, question_approve_error, resolve_live,
    short_event, turn_running, Decision, DeletedMiss,
};
use agentium_core::cli::spawn::{
    duplicate_spawn_error, find_duplicate_spawn, prompt_receipt, resolve_spawn_wait,
    CurrentSession, Here, SpawnTarget, Spawned, TargetFlags, SPAWN_WAIT_ENV,
};
use agentium_core::cli::term::now_secs;
use agentium_core::config::RunConfig;
use agentium_core::session_events::{
    build_run_config_delete_event_at, build_run_config_event_at, spawn_idempotency_key, BuiltEvent,
    SessionRelation, SpawnOptions,
};
use agentium_core::session_loader::{
    latest_valid_session_for_author, load_all_run_configs_from_ndb,
    load_session_messages_for_author, load_session_states_for_author, pending_permission_requests,
    resolve_session_including_deleted, HostedRunConfig,
};
use agentium_core::{Engine, SendOptions, SessionWatch};
use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;
use notedeck::{
    AppTool, RegisteredTool, SlowTool, ToolArg, ToolArgType, ToolContext, ToolJob, ToolSpec,
};
use serde::{Deserialize, Serialize, Serializer};

use super::{open_txn, SESSION_ENV, TOOL_CATEGORY};
use crate::backend::BackendType;

/// How long `link`/`unlink` waits for the host to republish the session with
/// the new link before reporting that it hasn't. The host applies the command
/// on its next frame and publishes the state on the same pass, so a live host
/// answers well inside a second; the margin is for the relay round trip.
const LINK_CONFIRM: Duration = Duration::from_secs(10);

/// Every control tool, ready to register.
pub(super) fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(SendMessage),
        RegisteredTool::slow(Spawn),
        RegisteredTool::new(Resume),
        RegisteredTool::new(Interrupt),
        RegisteredTool::new(Respond(Decision::Approve)),
        RegisteredTool::new(Respond(Decision::Deny)),
        RegisteredTool::new(Mode),
        RegisteredTool::slow(Link { unlink: false }),
        RegisteredTool::slow(Link { unlink: true }),
        RegisteredTool::new(ConfigList),
        RegisteredTool::new(ConfigShow),
        RegisteredTool::new(ConfigAdd),
        RegisteredTool::new(ConfigEdit),
        RegisteredTool::new(ConfigRm),
    ]
}

// ---------------------------------------------------------------------------
// the writer
// ---------------------------------------------------------------------------

/// The selected account, ready to publish: whose sessions a selector
/// resolves among, and an engine signing as it.
pub(super) struct Writer {
    pub(super) author: Pubkey,
    pub(super) secret: [u8; 32],
    pub(super) engine: Engine,
}

impl Writer {
    /// The selected account's writer, or an error a person can act on. A
    /// read-only context (no `remote`) refuses too, so a command is never
    /// published where the caller asked for none.
    pub(super) fn new(cx: &ToolContext) -> Result<Self, String> {
        if cx.remote.is_none() {
            return Err("publishing is unavailable in this context".to_owned());
        }
        let secret = cx
            .accounts
            .selected_filled()
            .map(|f| f.secret_key.secret_bytes())
            .ok_or("the selected account is watch-only and cannot sign commands")?;
        let engine = Engine::embedded(cx.ndb.clone(), secret)
            .map_err(|e| format!("opening the agentium engine: {e:?}"))?;
        Ok(Self {
            author: *cx.accounts.selected_account_pubkey(),
            secret,
            engine,
        })
    }

    /// A read transaction on the engine's db.
    pub(super) fn txn(&self) -> Result<Transaction, String> {
        Transaction::new(self.engine.ndb()).map_err(|e| format!("opening a db transaction: {e}"))
    }

    /// The live session `selector` names, worded as `miss` when it names
    /// only a deleted one.
    fn live(&self, selector: &str, miss: DeletedMiss) -> Result<Target, String> {
        let txn = self.txn()?;
        resolve_live(self.engine.ndb(), &txn, &self.author, selector, miss).map(Target::new)
    }

    /// The session the caller runs inside (`from_session`, which nd fills
    /// from `$AGENTIUM_SESSION`), for the commands whose target defaults to
    /// "here". `caller_cwd` stands in for the session's cwd when its state
    /// isn't in this db.
    fn current(&self, from_session: Option<&str>, caller_cwd: Option<&str>) -> CurrentSession {
        let Ok(txn) = self.txn() else {
            return CurrentSession::default();
        };
        CurrentSession::resolve(self.engine.ndb(), &txn, &self.author, from_session, || {
            here(caller_cwd)
        })
    }
}

/// This machine, as a session running on it sees itself: the notedeck runs
/// on the caller's machine (the socket is local), so its hostname is the
/// caller's.
fn here(caller_cwd: Option<&str>) -> Here {
    Here {
        host: Some(gethostname::gethostname().to_string_lossy().into_owned()),
        cwd: caller_cwd.map(str::to_string),
    }
}

/// A session a command resolved to, read out of its state before anything is
/// published.
struct Target {
    session_id: String,
    uri: String,
    /// The state's `status` (`working`, `idle`, …).
    status: String,
    /// The state's linked issue.
    issue_url: Option<String>,
}

impl Target {
    fn new(state: agentium_core::session_loader::SessionState) -> Self {
        Self {
            uri: state.agentium_uri(),
            session_id: state.claude_session_id,
            status: state.status,
            issue_url: state.issue_url,
        }
    }
}

/// An event's id as hex.
pub(super) fn event_hex(built: &BuiltEvent) -> String {
    hex::encode(built.note_id)
}

/// The required session argument of a command acting on one session.
fn session_arg() -> ToolArg {
    ToolArg::new(
        "session",
        ToolArgType::String,
        "The session: an `agentium:<word-id>` ref, a word-id, a session id or a unique prefix.",
    )
    .required(true)
    .positional()
}

/// The argument naming the session the caller runs inside.
fn from_session_arg(what: &'static str) -> ToolArg {
    ToolArg::new("from_session", ToolArgType::String, what).env(SESSION_ENV)
}

/// Run `fut` to completion on a runtime of its own, on a thread of its own:
/// a [`SlowTool`]'s job may run inline on a thread already inside a runtime
/// (a key binding, Dave), where blocking on another would panic.
fn block_on<F: std::future::Future + Send>(fut: F) -> F::Output
where
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("current-thread runtime")
                    .block_on(fut)
            })
            .join()
            .expect("the wait does not panic")
    })
}

/// Wait up to `bound` for `found` to answer, checking once now and again on
/// each change `watch` sees. `None` when the bound passes first, or the
/// watch ends (the db is closing).
fn wait_for<T: Send>(
    mut watch: SessionWatch,
    bound: Duration,
    mut found: impl FnMut() -> Option<T> + Send,
) -> Option<T> {
    block_on(async move {
        tokio::time::timeout(bound, async {
            loop {
                if let Some(answer) = found() {
                    return Some(answer);
                }
                if !watch.changed().await {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten()
    })
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

/// `agentium_send`: a `user` message into a live session.
struct SendMessage;

/// Arguments for [`SendMessage`].
#[derive(Deserialize)]
struct SendArgs {
    session: String,
    text: String,
    #[serde(default)]
    immediate: bool,
    #[serde(default)]
    from_session: Option<String>,
}

/// [`SendMessage`]'s answer: `{session, event_id, interrupted?}`.
#[derive(Serialize)]
struct Sent {
    session: String,
    event_id: String,
    /// Set only for an `immediate` send.
    #[serde(skip_serializing_if = "Option::is_none")]
    interrupted: Option<bool>,
    /// The session's status when it was sent, for the text.
    #[serde(skip)]
    status: String,
}

impl AppTool for SendMessage {
    type Args = SendArgs;
    type Output = Sent;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_send",
            "Send a `user` message to a live agentium session, as `nd agentium send` does: its running agent, on this machine or another, reads it as its next message. Answers `{session, event_id}`. `immediate` also interrupts a running turn so the agent reads the message now (`interrupted` says whether it did). A deleted session is refused: resume it first.",
            vec![
                session_arg(),
                ToolArg::new("text", ToolArgType::String, "The message.")
                    .required(true)
                    .rest(),
                ToolArg::new(
                    "immediate",
                    ToolArgType::Boolean,
                    "Also interrupt the session's running turn, so the message is read now rather than when the turn ends.",
                ),
                from_session_arg("The session sending it, stamped on the message so the receiving chat shows who it came from. Left out when a person sends it."),
            ],
        )
        .titled("Send to agent session")
        .command(&["agentium", "send"])
        .filed_under(TOOL_CATEGORY)
    }

    /// The message goes first and the interrupt second: a host that sees a
    /// user message mid-turn queues it, an interrupt ends the turn's stream,
    /// and the stream's end dispatches the queued message as the next turn.
    /// Only a session whose state says a turn is running is interrupted, since
    /// the host dispatches a message before it applies an interrupt in the
    /// same frame and an idle session would abort the message's own turn.
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        if args.text.trim().is_empty() {
            return Err("`send` needs a non-empty message".to_owned());
        }
        let writer = Writer::new(cx)?;
        let target = writer.live(&args.session, DeletedMiss::Send)?;
        let opts = SendOptions {
            from_session: args.from_session.as_deref().filter(|s| !s.is_empty()),
        };
        let built = writer
            .engine
            .send_message(&target.session_id, &args.text, opts)
            .map_err(|e| format!("sending to {}: {e:?}", target.uri))?;
        let interrupted = args.immediate && turn_running(&target.status);
        if interrupted {
            writer
                .engine
                .interrupt_session(&target.session_id)
                .map_err(|e| format!("interrupting {}: {e:?}", target.uri))?;
        }
        Ok(Sent {
            session: target.uri,
            event_id: event_hex(&built),
            interrupted: args.immediate.then_some(interrupted),
            status: target.status,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let uri = &out.session;
        let mut text = format!("sent to {uri} (event {}…)\n", short_event(&out.event_id));
        match out.interrupted {
            Some(true) => text.push_str(&format!("interrupted {uri}\n")),
            Some(false) => text.push_str(&format!(
                "not interrupted: {uri} is {}, so the message starts its turn\n",
                out.status
            )),
            None => {}
        }
        Some(text)
    }
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

/// `agentium_spawn`: a new session on a Dave host, anywhere.
struct Spawn;

/// Arguments for [`Spawn`].
#[derive(Deserialize)]
struct SpawnArgs {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// The caller's working directory, which nd fills in.
    #[serde(default)]
    caller_cwd: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    issue_url: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    spec: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    max_steps: Option<u32>,
    #[serde(default)]
    max_cost_cents: Option<u32>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    child: bool,
    #[serde(default)]
    successor: bool,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    allow_duplicate: bool,
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    wait_timeout: Option<u64>,
    #[serde(default)]
    print_ref: bool,
    #[serde(default)]
    from_session: Option<String>,
}

impl SpawnArgs {
    /// How the new session relates to the caller's.
    fn relation(&self) -> Result<SessionRelation, String> {
        match (self.child, self.successor) {
            (true, true) => Err("`child` and `successor` are exclusive: a spawned session \
                                 either reports back to you or carries your work on"
                .to_owned()),
            (true, false) => Ok(SessionRelation::Child),
            (false, true) => Ok(SessionRelation::Successor),
            (false, false) => Ok(SessionRelation::None),
        }
    }

    /// The agent spec the session runs under, when one is named. A spec
    /// names its own provider and runs in the native loop, so it takes no
    /// profile and no backend but `openai`. Whether the host has a spec of
    /// that slug is the host's to say: specs are its local config.
    fn agent_spec(&self) -> Result<Option<&str>, String> {
        let Some(spec) = self.spec.as_deref().filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        agentium_core::config::validate_slug(spec)
            .map_err(|_| format!("`spec` {spec:?} is not a spec slug"))?;
        if self.profile.is_some() {
            return Err(
                "`spec` and `profile` are exclusive: a spec names its own provider".to_owned(),
            );
        }
        match self.backend.as_deref() {
            None | Some("openai") => Ok(Some(spec)),
            Some(other) => Err(format!(
                "a spec runs in the native loop (backend `openai`), not `{other}`"
            )),
        }
    }

    /// What the session changes about its spec's prompt and budget, for it
    /// alone; its model override is `model`. Every override needs a `spec`
    /// to change, and a step cap is at least 1. Whatever else the resolved
    /// spec must satisfy is checked by its host, which has the spec.
    fn spec_overrides(&self, spec: Option<&str>) -> Result<SpecOverrides, String> {
        let overrides = SpecOverrides {
            prompt: self.system_prompt.clone().filter(|p| !p.is_empty()),
            max_steps: self.max_steps,
            max_cost_cents: self.max_cost_cents,
            timeout_secs: self.timeout_secs,
        };
        if spec.is_none() && !overrides.is_empty() {
            return Err(
                "`system_prompt`, `max_steps`, `max_cost_cents` and `timeout_secs` \
                        override a spec's: name one with `spec`"
                    .to_owned(),
            );
        }
        if overrides.max_steps == Some(0) {
            return Err("`max_steps` is at least 1".to_owned());
        }
        Ok(overrides)
    }

    /// Whether to wait for the host's answer: `wait` or `print_ref`
    /// explicitly, or whenever a prompt rides the spawn, so the common
    /// prompted spawn reports its ref when the host answers in time.
    fn effective_wait(&self) -> bool {
        self.wait || self.print_ref || self.prompt.is_some()
    }

    /// The target directory: `cwd` as given, made absolute against the
    /// caller's directory when it is relative (an absolute one may name a
    /// directory on another host, so it is never checked here).
    fn cwd(&self) -> Option<String> {
        let cwd = self.cwd.as_deref().filter(|c| !c.is_empty())?;
        match self.caller_cwd.as_deref() {
            Some(base) if std::path::Path::new(cwd).is_relative() => Some(
                std::path::Path::new(base)
                    .join(cwd)
                    .to_string_lossy()
                    .into_owned(),
            ),
            _ => Some(cwd.to_owned()),
        }
    }
}

/// [`Spawn`]'s answer: [`Spawned`] (`{spawn_id, host, session}`).
struct SpawnAnswer {
    spawned: Spawned,
    /// The receipt for a prompt that rode the spawn.
    receipt: Option<String>,
    /// Print only the ref.
    print_ref: bool,
}

impl Serialize for SpawnAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.spawned.serialize(serializer)
    }
}

impl SlowTool for Spawn {
    type Args = SpawnArgs;
    type Output = SpawnAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_spawn",
            "Spawn a new agentium session on a Dave host — this machine's or another's — as `nd agentium spawn` does. Answers `{spawn_id, host, session}`; `session` is the new session's `agentium:` ref once the host answers, which `wait` (implied by `prompt` and `print_ref`) waits for, else null. `host`, `cwd` and `backend` default to the session the caller runs inside (`from_session`), so a bare spawn starts a sibling in the same worktree. A titled spawn matching a session started on the same host and cwd in the last ten minutes is refused unless `allow_duplicate`.",
            vec![
                ToolArg::new("host", ToolArgType::String, "The host to run it on. Defaults to the current session's."),
                ToolArg::new(
                    "cwd",
                    ToolArgType::String,
                    "The directory to run it in, on that host; a relative one is taken from the caller's directory. Defaults to the current session's, else the caller's.",
                ),
                ToolArg::new(
                    "caller_cwd",
                    ToolArgType::Path,
                    "The caller's working directory, which a command line fills in: a relative `cwd` is taken from it, and it is the cwd when nothing else gives one.",
                )
                .default_cwd(),
                ToolArg::new("backend", ToolArgType::String, "The agent backend, e.g. `claude` or `codex`. Defaults to the current session's, else `claude`."),
                ToolArg::new("title", ToolArgType::String, "An explicit, sticky title; without one the host derives it from the first message."),
                ToolArg::new(
                    "prompt",
                    ToolArgType::String,
                    "The first `user` message. It rides the spawn command, so the host delivers it whenever it starts the session.",
                )
                .from_file("prompt-file"),
                ToolArg::new("permission_mode", ToolArgType::String, "The permission mode it starts in: default, plan, accept_edits, auto, … (aliases are normalized)."),
                ToolArg::new("model", ToolArgType::String, "The model it starts on, as the backend names it (`opus`, a full id). Under a `spec`, it replaces the spec's model for this session only: `provider/model`, or a model on the spec's provider."),
                ToolArg::new("issue_url", ToolArgType::String, "The issue it works, as a URI, e.g. `headway:<board>/<word-id>`."),
                ToolArg::new("profile", ToolArgType::String, "The backend profile it runs under. Defaults to the current session's unless `backend` is given."),
                ToolArg::new("spec", ToolArgType::String, "The agent spec it runs under, e.g. `butler`: the native loop on the spec's model, prompt, tools and budget. Implies backend `openai`; excludes `profile`. `model`, `system_prompt`, `max_steps`, `max_cost_cents` and `timeout_secs` override it for this session only; the account's spec is unchanged."),
                ToolArg::new(
                    "system_prompt",
                    ToolArgType::String,
                    "Replaces the spec's system prompt for this session (not the first message, which is `prompt`). Needs `spec`.",
                )
                .from_file("system-prompt-file"),
                ToolArg::new("max_steps", ToolArgType::Integer, "Replaces the spec's cap on tool-call steps per run, at least 1. Needs `spec`."),
                ToolArg::new("max_cost_cents", ToolArgType::Integer, "Replaces the spec's cap on one run's spend, in US cents. Needs `spec`."),
                ToolArg::new("timeout_secs", ToolArgType::Integer, "Replaces the spec's wall-clock limit on one run, in seconds. Needs `spec`."),
                ToolArg::new("child", ToolArgType::Boolean, "Record it as a child of the current session, which it reports back to."),
                ToolArg::new("successor", ToolArgType::Boolean, "Record it as the current session's successor, which carries its work on."),
                ToolArg::new("idempotency_key", ToolArgType::String, "Your own name for this request, so a retry of it is recognized as one."),
                ToolArg::new("allow_duplicate", ToolArgType::Boolean, "Spawn even if it looks like a retry of a recent session."),
                ToolArg::new("wait", ToolArgType::Boolean, "Wait for the host to answer with the new session's ref."),
                ToolArg::new("wait_timeout", ToolArgType::Integer, "Seconds to wait for the host. Defaults to 30.")
                    .env(SPAWN_WAIT_ENV),
                ToolArg::new("print_ref", ToolArgType::Boolean, "Answer only the new session's `agentium:` ref (implies `wait`), and fail without one.")
                    .long("ref"),
                from_session_arg("The session the caller runs inside, which the target defaults to and which is recorded as the new session's parent."),
            ],
        )
        .titled("Spawn agent session")
        .command(&["agentium", "spawn"])
        .filed_under(TOOL_CATEGORY)
    }

    /// Resolves the target, refuses a likely retry, and publishes the
    /// kind-31989 command here, quickly; the job waits for the host's
    /// kind-31988 answer. The session-list watch is installed *before* the
    /// publish, so a fast host's answer can't slip between the two.
    fn prepare(
        &self,
        cx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<ToolJob<SpawnAnswer>, String> {
        let relation = args.relation()?;
        let spec = args.agent_spec()?;
        let spec_overrides = args.spec_overrides(spec)?;
        let permission_mode = args
            .permission_mode
            .as_deref()
            .map(parse_mode)
            .transpose()?;
        let model = args.model.as_deref().map(parse_model).transpose()?;
        let issue_url = args
            .issue_url
            .as_deref()
            .map(|u| parse_issue_url(u, "--issue-url"))
            .transpose()?;
        let writer = Writer::new(cx)?;
        let caller_cwd = args.caller_cwd.as_deref();
        let mut current = writer.current(args.from_session.as_deref(), caller_cwd);
        // Outside a session, the caller's directory is the place nothing
        // else names.
        if current.cwd.is_none() {
            current.cwd = caller_cwd.map(str::to_string);
        }
        let cwd = args.cwd();
        let flags = TargetFlags {
            host: args.host.as_deref(),
            cwd: cwd.as_deref(),
            // A spec's session runs in the native loop, and naming the backend
            // keeps the current session's profile from being inherited.
            backend: args
                .backend
                .as_deref()
                .or(spec.map(|_| BackendType::OpenAI.as_str())),
            profile: args.profile.as_deref(),
        };
        let target = SpawnTarget::resolve(&flags, current)?;

        // Refuse an obvious retry before publishing anything: the caller gets
        // the existing ref instead of a second session to reconcile.
        if !args.allow_duplicate {
            let now = now_secs();
            let txn = writer.txn()?;
            let live = load_session_states_for_author(writer.engine.ndb(), &txn, &writer.author);
            if let Some(existing) = find_duplicate_spawn(&live, &target, args.title.as_deref(), now)
            {
                let title = args.title.as_deref().unwrap_or_default();
                return Err(duplicate_spawn_error(existing, title, now));
            }
        }

        let request = SpawnOptions {
            title: args.title.as_deref(),
            prompt: args.prompt.as_deref(),
            permission_mode: permission_mode.as_deref(),
            model: model.as_deref(),
            issue_url: issue_url.as_deref(),
            idempotency_key: None,
            parent_session: target.parent_session.as_deref(),
            relation,
            profile: target.profile.as_deref(),
            spec,
            spec_overrides: Some(&spec_overrides),
        };
        // `allow_duplicate` omits the key: opting out of the guard has to opt
        // out of the host's dedupe too, or the host would re-impose it.
        let idempotency_key = (!args.allow_duplicate).then(|| {
            args.idempotency_key.clone().unwrap_or_else(|| {
                spawn_idempotency_key(&target.host, &target.cwd, &target.backend, &request)
            })
        });

        let wait = args.effective_wait();
        let watch = wait
            .then(|| writer.engine.watch_sessions())
            .transpose()
            .map_err(|e| format!("watching the session list: {e:?}"))?;
        let spawn_id = writer
            .engine
            .spawn_session(
                &target.host,
                &target.cwd,
                &target.backend,
                &SpawnOptions {
                    idempotency_key: idempotency_key.as_deref(),
                    ..request
                },
            )
            .map_err(|e| format!("spawning on {}: {e:?}", target.host))?;

        let mut answer = SpawnAnswer {
            spawned: Spawned {
                spawn_id,
                host: target.host,
                session: None,
            },
            receipt: args.prompt.as_deref().map(prompt_receipt),
            print_ref: args.print_ref,
        };
        let Some(watch) = watch else {
            return Ok(Box::new(move || Ok(answer)));
        };

        let bound = resolve_spawn_wait(args.wait_timeout, None);
        let has_prompt = args.prompt.is_some();
        let ndb = writer.engine.ndb().clone();
        let author = writer.author;
        Ok(Box::new(move || {
            let spawn_id = answer.spawned.spawn_id.clone();
            answer.spawned.session =
                wait_for(watch, bound, || answering_session(&ndb, &author, &spawn_id));
            // A prompt rides the command, so a host slower than the wait still
            // delivers it: not a failure. A bare wait had no other purpose
            // than the ref.
            if answer.spawned.session.is_none() && !has_prompt && !answer.print_ref {
                return Err(answer.spawned.unanswered(bound));
            }
            if answer.print_ref {
                answer.spawned.session_ref()?;
            }
            Ok(answer)
        }))
    }

    fn render(&self, out: &SpawnAnswer) -> Option<String> {
        if out.print_ref {
            return out.spawned.session.as_ref().map(|uri| format!("{uri}\n"));
        }
        Some(out.spawned.render())
    }

    /// The prompt's receipt, on stderr: a prompt the caller's shell cut short
    /// shows up as the wrong last line.
    fn notice(&self, out: &SpawnAnswer) -> Option<String> {
        out.receipt.clone()
    }
}

/// The `agentium:` ref of the session a host materialized for `spawn_id`, if
/// its state has landed.
fn answering_session(ndb: &Ndb, author: &Pubkey, spawn_id: &str) -> Option<String> {
    let txn = Transaction::new(ndb).ok()?;
    load_session_states_for_author(ndb, &txn, author)
        .iter()
        .find(|s| s.spawn_id.as_deref() == Some(spawn_id))
        .map(|s| s.agentium_uri())
}

// ---------------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------------

/// `agentium_resume`: reopen a closed session's backend on its host.
struct Resume;

/// Arguments naming one session.
#[derive(Deserialize)]
struct SessionArgs {
    session: String,
}

/// [`Resume`]'s answer: `{session, host}`.
#[derive(Serialize)]
struct Resumed {
    session: String,
    host: String,
}

impl AppTool for Resume {
    type Args = SessionArgs;
    type Output = Resumed;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_resume",
            "Reopen a closed agentium session, deleted ones included, on the host it ran on, as `nd agentium resume` does: the host revives its ref, rehydrates its history and resumes its backend. Answers `{session, host}`.",
            vec![session_arg()],
        )
        .titled("Resume agent session")
        .command(&["agentium", "resume"])
        .filed_under(TOOL_CATEGORY)
    }

    /// The `claude --resume` id is resolved as the host's own hydrator does:
    /// a non-empty `cli_session` is the CLI id, an empty one means the backend
    /// never started (the host opens a fresh one), and none at all is a legacy
    /// event whose d-tag is the CLI id. The host re-derives it from the
    /// session's own state anyway, so it is advisory.
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let writer = Writer::new(cx)?;
        let state = {
            let txn = writer.txn()?;
            let live = load_session_states_for_author(writer.engine.ndb(), &txn, &writer.author);
            let deleted = agentium_core::session_loader::load_deleted_session_states_for_author(
                writer.engine.ndb(),
                &txn,
                &writer.author,
            );
            resolve_session_including_deleted(&live, &deleted, &args.session)?.clone()
        };
        let uri = state.agentium_uri();
        if state.hostname.is_empty() {
            return Err(format!(
                "{uri} has no recorded host; cannot target a resume"
            ));
        }
        let cli = match state.cli_session_id.as_deref() {
            Some(cli) if !cli.is_empty() => cli.to_string(),
            Some(_) => String::new(),
            None => state.claude_session_id.clone(),
        };
        let backend = state.backend.as_deref().unwrap_or("claude");
        writer
            .engine
            .resume_session(
                &state.hostname,
                &state.cwd,
                backend,
                &state.claude_session_id,
                &cli,
            )
            .map_err(|e| format!("resuming {uri}: {e:?}"))?;
        Ok(Resumed {
            session: uri,
            host: state.hostname,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!(
            "resume command sent to {} for {}\n",
            out.host, out.session
        ))
    }
}

// ---------------------------------------------------------------------------
// interrupt
// ---------------------------------------------------------------------------

/// `agentium_interrupt`: abort a live session's running turn.
struct Interrupt;

/// [`Interrupt`]'s answer: `{session}`.
#[derive(Serialize)]
struct Interrupted {
    session: String,
}

impl AppTool for Interrupt {
    type Args = SessionArgs;
    type Output = Interrupted;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_interrupt",
            "Interrupt a live agentium session's running turn on its host, as `nd agentium interrupt` does — the Esc of Dave. Answers `{session}`. A no-op on a session with no turn running; a deleted one is refused.",
            vec![session_arg()],
        )
        .titled("Interrupt agent session")
        .command(&["agentium", "interrupt"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let writer = Writer::new(cx)?;
        let target = writer.live(&args.session, DeletedMiss::Interrupt)?;
        writer
            .engine
            .interrupt_session(&target.session_id)
            .map_err(|e| format!("interrupting {}: {e:?}", target.uri))?;
        Ok(Interrupted {
            session: target.uri,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!("interrupt sent to {}\n", out.session))
    }
}

// ---------------------------------------------------------------------------
// approve / deny
// ---------------------------------------------------------------------------

/// `agentium_approve` / `agentium_deny`: answer a pending permission request.
struct Respond(Decision);

/// Arguments for [`Respond`].
#[derive(Deserialize)]
struct RespondArgs {
    session: String,
    #[serde(default)]
    request: Option<String>,
    #[serde(default)]
    message: Option<String>,
    /// Deny's only.
    #[serde(default)]
    interrupt: bool,
}

/// [`Respond`]'s answer: `{session, event_id, perm_id, tool_name, decision,
/// interrupt}`.
#[derive(Serialize)]
struct Responded {
    session: String,
    event_id: String,
    perm_id: String,
    tool_name: String,
    decision: &'static str,
    interrupt: bool,
}

impl AppTool for Respond {
    type Args = RespondArgs;
    type Output = Responded;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![
            session_arg(),
            ToolArg::new(
                "request",
                ToolArgType::String,
                "The pending request to answer, by perm-id prefix. Defaults to the newest, the one blocking the agent now.",
            ),
            ToolArg::new(
                "message",
                ToolArgType::String,
                "Text the agent sees with the decision: a reason, or a note.",
            ),
        ];
        let spec = match self.0 {
            Decision::Approve => ToolSpec::new(
                "agentium_approve",
                "Approve a live agentium session's pending permission request, as `nd agentium approve` does: the newest, or the one `request` names. Answers `{session, event_id, perm_id, tool_name, decision, interrupt}`. An AskUserQuestion request is refused: it needs answers, which an approve can't send.",
                arguments,
            )
            .titled("Approve agent permission")
            .command(&["agentium", "approve"]),
            Decision::Deny => {
                arguments.push(ToolArg::new(
                    "interrupt",
                    ToolArgType::Boolean,
                    "Also stop the turn, rather than letting the agent carry on without the tool.",
                ));
                ToolSpec::new(
                    "agentium_deny",
                    "Deny a live agentium session's pending permission request, as `nd agentium deny` does: the newest, or the one `request` names. Answers `{session, event_id, perm_id, tool_name, decision, interrupt}`.",
                    arguments,
                )
                .titled("Deny agent permission")
                .command(&["agentium", "deny"])
            }
        };
        spec.filed_under(TOOL_CATEGORY)
    }

    /// A response is matched on the host by `perm-id` against the requests it
    /// holds in memory: a host that restarted since the request drops it, and
    /// no ack comes back to say so.
    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let decision = self.0;
        let interrupt = decision == Decision::Deny && args.interrupt;
        let writer = Writer::new(cx)?;
        let target = writer.live(&args.session, DeletedMiss::Running)?;
        let pending = {
            let txn = writer.txn()?;
            let loaded = load_session_messages_for_author(
                writer.engine.ndb(),
                &txn,
                &writer.author,
                &target.session_id,
            );
            pending_permission_requests(&loaded)
        };
        let request = pick_request(&pending, args.request.as_deref(), &target.uri)?;
        if request.is_question && decision == Decision::Approve {
            return Err(question_approve_error(request, &target.uri));
        }
        let perm_id = request.perm_id.to_string();
        let built = writer
            .engine
            .respond_permission(
                &target.session_id,
                &perm_id,
                decision == Decision::Approve,
                args.message,
                interrupt,
            )
            .map_err(|e| format!("answering {perm_id} on {}: {e:?}", target.uri))?;
        Ok(Responded {
            session: target.uri,
            event_id: event_hex(&built),
            perm_id,
            tool_name: request.tool_name.clone(),
            decision: decision.verb(),
            interrupt,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let stop = if out.interrupt {
            " and stopped the turn"
        } else {
            ""
        };
        Some(format!(
            "{} {} request {} on {}{stop} (event {}…)\n",
            out.decision,
            out.tool_name,
            &out.perm_id[..out.perm_id.len().min(8)],
            out.session,
            short_event(&out.event_id),
        ))
    }
}

// ---------------------------------------------------------------------------
// mode
// ---------------------------------------------------------------------------

/// `agentium_set_mode`: change a live session's permission mode.
struct Mode;

/// Arguments for [`Mode`].
#[derive(Deserialize)]
struct ModeArgs {
    session: String,
    mode: String,
}

/// [`Mode`]'s answer: `{session, event_id, mode}`.
#[derive(Serialize)]
struct ModeSet {
    session: String,
    event_id: String,
    mode: String,
}

impl AppTool for Mode {
    type Args = ModeArgs;
    type Output = ModeSet;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_set_mode",
            "Change a live agentium session's permission mode on its host, as `nd agentium mode` does — the Ctrl+M of Dave. Answers `{session, event_id, mode}` with the mode's canonical spelling.",
            vec![
                session_arg(),
                ToolArg::new(
                    "mode",
                    ToolArgType::String,
                    "The mode: default, plan, accept_edits, auto, … (aliases such as `manual` and `acceptEdits` are normalized).",
                )
                .required(true)
                .positional(),
            ],
        )
        .titled("Set agent permission mode")
        .command(&["agentium", "mode"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        // Normalized before anything is published: the host reads an unknown
        // mode as `default`.
        let mode = parse_mode(&args.mode)?;
        let writer = Writer::new(cx)?;
        let target = writer.live(&args.session, DeletedMiss::Running)?;
        let built = writer
            .engine
            .set_permission_mode(&target.session_id, &mode)
            .map_err(|e| format!("setting {}'s mode: {e:?}", target.uri))?;
        Ok(ModeSet {
            session: target.uri,
            event_id: event_hex(&built),
            mode,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(format!(
            "set {} to {} mode (event {}…)\n",
            out.session,
            out.mode,
            short_event(&out.event_id)
        ))
    }
}

// ---------------------------------------------------------------------------
// link / unlink
// ---------------------------------------------------------------------------

/// `agentium_link` / `agentium_unlink`: set or clear the issue a live session
/// works, after it was spawned.
struct Link {
    unlink: bool,
}

/// Arguments for [`Link`].
#[derive(Deserialize)]
struct LinkArgs {
    session: String,
    /// `link`'s only.
    #[serde(default)]
    issue_url: Option<String>,
}

/// [`Link`]'s answer: `{session, issue_url, event_id, unchanged}`;
/// `event_id` is null when nothing was published.
#[derive(Serialize)]
struct Linked {
    session: String,
    issue_url: Option<String>,
    event_id: Option<String>,
    unchanged: bool,
}

impl SlowTool for Link {
    type Args = LinkArgs;
    type Output = Linked;

    fn spec(&self) -> ToolSpec {
        if self.unlink {
            return ToolSpec::new(
                "agentium_unlink",
                "Clear the issue a live agentium session is linked to, as `nd agentium unlink` does, then wait for its host to republish the session without it. Answers `{session, issue_url, event_id, unchanged}`.",
                vec![session_arg()],
            )
            .titled("Unlink agent session")
            .command(&["agentium", "unlink"])
            .filed_under(TOOL_CATEGORY);
        }
        ToolSpec::new(
            "agentium_link",
            "Link a live agentium session to the issue it works, as `nd agentium link` does — `spawn`'s `issue_url` for a session spawned without one — then wait for its host to republish the session with it. Answers `{session, issue_url, event_id, unchanged}`.",
            vec![
                session_arg(),
                ToolArg::new(
                    "issue_url",
                    ToolArgType::String,
                    "The issue, as a URI, e.g. `headway:<board>/<word-id>`.",
                )
                .required(true)
                .positional(),
            ],
        )
        .titled("Link agent session")
        .command(&["agentium", "link"])
        .filed_under(TOOL_CATEGORY)
    }

    /// The issue lives in the session's kind-31988 state, which only its host
    /// publishes, so this publishes a set-issue command and the job waits for
    /// the republished state to carry the change. Only that answer is success:
    /// a host from before `link` ignores the command. A session already in the
    /// asked-for state publishes nothing.
    fn prepare(&self, cx: &mut ToolContext, args: Self::Args) -> Result<ToolJob<Linked>, String> {
        let issue_url = match (self.unlink, args.issue_url.as_deref()) {
            (true, _) => None,
            (false, Some(url)) => Some(parse_issue_url(url, "issue")?),
            (false, None) => return Err("`link` needs an issue URL".to_owned()),
        };
        let writer = Writer::new(cx)?;
        let target = writer.live(&args.session, DeletedMiss::Running)?;
        if target.issue_url == issue_url {
            let linked = Linked {
                session: target.uri,
                issue_url,
                event_id: None,
                unchanged: true,
            };
            return Ok(Box::new(move || Ok(linked)));
        }

        let watch = writer
            .engine
            .watch_sessions()
            .map_err(|e| format!("watching the session list: {e:?}"))?;
        let built = writer
            .engine
            .set_issue_url(&target.session_id, issue_url.as_deref())
            .map_err(|e| format!("linking {}: {e:?}", target.uri))?;
        let event_id = event_hex(&built);
        let ndb = writer.engine.ndb().clone();
        let author = writer.author;
        Ok(Box::new(move || {
            let landed = wait_for(watch, LINK_CONFIRM, || {
                let txn = Transaction::new(&ndb).ok()?;
                latest_valid_session_for_author(&ndb, &txn, &author, &target.session_id)
                    .filter(|s| s.issue_url == issue_url)
                    .map(|_| ())
            });
            if landed.is_none() {
                return Err(format!(
                    "sent the command to {} (event {}…), but its host hasn't republished the \
                     session within {}s. An offline host applies it when it next syncs; a host \
                     running a Dave build from before `nd agentium link` ignores it and needs \
                     rebuilding.",
                    target.uri,
                    short_event(&event_id),
                    LINK_CONFIRM.as_secs(),
                ));
            }
            Ok(Linked {
                session: target.uri,
                issue_url,
                event_id: Some(event_id),
                unchanged: false,
            })
        }))
    }

    fn render(&self, out: &Linked) -> Option<String> {
        let already = if out.unchanged { " (already)" } else { "" };
        Some(match &out.issue_url {
            Some(issue) => format!("linked {} to {issue}{already}\n", out.session),
            None => format!("unlinked {}{already}\n", out.session),
        })
    }
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

/// The run configs the `host`/`cwd` filters keep, as arguments.
#[derive(Deserialize)]
struct FilterArgs {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

impl FilterArgs {
    /// Every run config of the selected account the filters keep, in host,
    /// cwd, name order.
    fn configs(&self, cx: &ToolContext) -> Result<Vec<HostedRunConfig>, String> {
        let author = *cx.accounts.selected_account_pubkey();
        let txn = open_txn(cx)?;
        let filters = ConfigFilters {
            host: self.host.clone(),
            cwd: self.cwd.clone(),
        };
        let mut all = load_all_run_configs_from_ndb(cx.ndb, &txn, &author);
        all.retain(|c| filters.matches(c));
        Ok(all)
    }
}

/// The `host`/`cwd` filter arguments.
fn filter_args() -> [ToolArg; 2] {
    [
        ToolArg::new(
            "host",
            ToolArgType::String,
            "Keep configs whose host contains this (case-insensitive).",
        ),
        ToolArg::new(
            "cwd",
            ToolArgType::String,
            "Keep configs whose directory contains this (case-insensitive).",
        ),
    ]
}

/// The run config selector argument.
fn config_arg() -> ToolArg {
    ToolArg::new(
        "config",
        ToolArgType::String,
        "The run config: its id, a unique id prefix, or its exact name. `host`/`cwd` narrow an ambiguous one.",
    )
    .required(true)
    .positional()
}

/// What a run config looks like as JSON (see [`ConfigJson`]).
fn serialize_config<S: Serializer>(c: &HostedRunConfig, serializer: S) -> Result<S::Ok, S::Error> {
    ConfigJson::new(c).serialize(serializer)
}

/// `agentium_config_list`: every run config, on every host.
struct ConfigList;

/// [`ConfigList`]'s answer: the configs, flat.
struct Configs(Vec<HostedRunConfig>);

impl Serialize for Configs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(ConfigJson::new))
    }
}

impl AppTool for ConfigList {
    type Args = FilterArgs;
    type Output = Configs;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_config_list",
            "List the run configs Dave's run bar launches — named shell commands, each registered for one host and directory — on every host, as `nd agentium config list` does: `[{id, name, command, host, cwd, updated_at}]` in host, cwd, name order.",
            filter_args().into(),
        )
        .titled("List run configs")
        .read_only()
        .command(&["agentium", "config", "list"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        args.configs(cx).map(Configs)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_list(&out.0, true))
    }
}

/// `agentium_config_show`: one run config.
struct ConfigShow;

/// Arguments for the tools acting on one config.
#[derive(Deserialize)]
struct ConfigArgs {
    config: String,
    #[serde(flatten)]
    filters: FilterArgs,
}

/// One run config, as [`ConfigJson`].
struct OneConfig(HostedRunConfig);

impl Serialize for OneConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_config(&self.0, serializer)
    }
}

impl AppTool for ConfigShow {
    type Args = ConfigArgs;
    type Output = OneConfig;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![config_arg()];
        arguments.extend(filter_args());
        ToolSpec::new(
            "agentium_config_show",
            "Show one run config, as `nd agentium config show` does: `{id, name, command, host, cwd, updated_at}`.",
            arguments,
        )
        .titled("Show run config")
        .read_only()
        .command(&["agentium", "config", "show"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let configs = args.filters.configs(cx)?;
        resolve_config(&configs, &args.config).map(|c| OneConfig(c.clone()))
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_show(&out.0, now_secs()))
    }
}

/// What `add`/`edit`/`rm` published: `{action, event_id, config}`.
struct Published {
    action: &'static str,
    event_id: String,
    config: HostedRunConfig,
}

impl Serialize for Published {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "action": self.action,
            "event_id": self.event_id,
            "config": ConfigJson::new(&self.config),
        })
        .serialize(serializer)
    }
}

/// Publish a built kind-31991 event as the selected account.
fn publish_config(
    writer: &Writer,
    action: &'static str,
    config: HostedRunConfig,
    built: &BuiltEvent,
) -> Result<Published, String> {
    writer
        .engine
        .publish_event(built)
        .map_err(|e| format!("publishing run config {}: {e:?}", describe(&config)))?;
    Ok(Published {
        action,
        event_id: event_hex(built),
        config,
    })
}

/// The text every config write prints.
fn render_write(out: &Published) -> Option<String> {
    Some(render_published(out.action, &out.config, &out.event_id))
}

/// `agentium_config_add`: register a run config on a host and directory.
struct ConfigAdd;

/// Arguments for [`ConfigAdd`].
#[derive(Deserialize)]
struct AddArgs {
    name: String,
    command: String,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    from_session: Option<String>,
}

impl AppTool for ConfigAdd {
    type Args = AddArgs;
    type Output = Published;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_config_add",
            "Register a run config — a named shell command Dave's run bar launches — on a host and directory, as `nd agentium config add` does. `host` and `cwd` default to the session the caller runs inside. Answers `{action, event_id, config}`.",
            vec![
                ToolArg::new("name", ToolArgType::String, "What the run bar calls it.")
                    .required(true),
                ToolArg::new("command", ToolArgType::String, "The shell command, run with `sh -c`.")
                    .required(true),
                ToolArg::new("host", ToolArgType::String, "The host it runs on. Defaults to the current session's."),
                ToolArg::new("cwd", ToolArgType::String, "The directory it runs in. Defaults to the current session's."),
                from_session_arg("The session the caller runs inside, whose host and directory the config defaults to."),
            ],
        )
        .titled("Add run config")
        .command(&["agentium", "config", "add"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let name = required_field("--name", Some(&args.name))?;
        let command = required_field("--command", Some(&args.command))?;
        let writer = Writer::new(cx)?;
        let current = writer.current(args.from_session.as_deref(), None);
        let config = HostedRunConfig {
            hostname: pick_place(args.host.as_deref(), current.host, "--host")?,
            cwd: pick_place(args.cwd.as_deref(), current.cwd, "--cwd")?.into(),
            config: RunConfig::new(name, command),
        };
        let built = build_run_config_event_at(
            &config.config,
            &config.cwd.to_string_lossy(),
            &config.hostname,
            None,
            &writer.secret,
        )
        .map_err(|e| format!("building the run config: {e}"))?;
        publish_config(&writer, "added", config, &built)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        render_write(out)
    }
}

/// `agentium_config_edit`: rename a run config or change its command.
struct ConfigEdit;

/// Arguments for [`ConfigEdit`].
#[derive(Deserialize)]
struct EditArgs {
    config: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(flatten)]
    filters: FilterArgs,
}

impl AppTool for ConfigEdit {
    type Args = EditArgs;
    type Output = Published;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![
            config_arg(),
            ToolArg::new("name", ToolArgType::String, "Its new name."),
            ToolArg::new("command", ToolArgType::String, "Its new command."),
        ];
        arguments.extend(filter_args());
        ToolSpec::new(
            "agentium_config_edit",
            "Rename a run config or change its command, keeping its id, as `nd agentium config edit` does. Answers `{action, event_id, config}`.",
            arguments,
        )
        .titled("Edit run config")
        .command(&["agentium", "config", "edit"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let name = args
            .name
            .as_deref()
            .map(|n| required_field("--name", Some(n)))
            .transpose()?;
        let command = args
            .command
            .as_deref()
            .map(|c| required_field("--command", Some(c)))
            .transpose()?;
        if name.is_none() && command.is_none() {
            return Err("`config edit` needs a name and/or a command".to_owned());
        }
        let writer = Writer::new(cx)?;
        let configs = args.filters.configs(cx)?;
        let mut config = resolve_config(&configs, &args.config)?.clone();
        if let Some(name) = name {
            config.config.name = name;
        }
        if let Some(command) = command {
            config.config.command = command;
        }
        let built = build_run_config_event_at(
            &config.config,
            &config.cwd.to_string_lossy(),
            &config.hostname,
            Some(replacing(config.config.updated_at)),
            &writer.secret,
        )
        .map_err(|e| format!("building the run config: {e}"))?;
        publish_config(&writer, "updated", config, &built)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        render_write(out)
    }
}

/// `agentium_config_rm`: delete a run config.
struct ConfigRm;

impl AppTool for ConfigRm {
    type Args = ConfigArgs;
    type Output = Published;

    fn spec(&self) -> ToolSpec {
        let mut arguments = vec![config_arg()];
        arguments.extend(filter_args());
        ToolSpec::new(
            "agentium_config_rm",
            "Delete a run config, as `nd agentium config rm` does. Answers `{action, event_id, config}`.",
            arguments,
        )
        .titled("Remove run config")
        .command(&["agentium", "config", "rm"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let writer = Writer::new(cx)?;
        let configs = args.filters.configs(cx)?;
        let config = resolve_config(&configs, &args.config)?.clone();
        let built = build_run_config_delete_event_at(
            &config.config.id,
            &config.cwd.to_string_lossy(),
            &config.hostname,
            Some(replacing(config.config.updated_at)),
            &writer.secret,
        )
        .map_err(|e| format!("building the run config delete: {e}"))?;
        publish_config(&writer, "removed", config, &built)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        render_write(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spawn_args(args: serde_json::Value) -> SpawnArgs {
        serde_json::from_value(args).expect("spawn args")
    }

    /// `spec` runs in the native loop under the spec's own provider: it
    /// takes the `openai` backend or none, never a profile, and only a slug.
    #[test]
    fn a_spec_takes_no_profile_and_no_other_backend() {
        let spec = |args| spawn_args(args).agent_spec().map(|s| s.map(str::to_owned));
        assert_eq!(spec(json!({})), Ok(None));
        assert_eq!(spec(json!({"spec": "butler"})), Ok(Some("butler".into())));
        assert_eq!(
            spec(json!({"spec": "butler", "backend": "openai"})),
            Ok(Some("butler".into()))
        );
        let err = spec(json!({"spec": "butler", "backend": "claude"})).unwrap_err();
        assert!(err.contains("`claude`"), "{err}");
        let err = spec(json!({"spec": "butler", "profile": "hydra"})).unwrap_err();
        assert!(err.contains("exclusive"), "{err}");
        assert!(spec(json!({"spec": "not a slug!"})).is_err());
    }

    /// The prompt and budget overrides change a spec, so each needs one;
    /// a step cap of zero is refused before anything is published
    /// (headway:dave/west-try-slab).
    #[test]
    fn spec_overrides_need_a_spec_and_a_step() {
        let overrides = |args| {
            let args = spawn_args(args);
            let spec = args.agent_spec().unwrap();
            args.spec_overrides(spec)
        };
        let got = overrides(json!({
            "spec": "butler",
            "system_prompt": "Review only.",
            "max_steps": 5,
            "max_cost_cents": 40,
            "timeout_secs": 300,
        }))
        .unwrap();
        assert_eq!(
            got,
            SpecOverrides {
                prompt: Some("Review only.".into()),
                max_steps: Some(5),
                max_cost_cents: Some(40),
                timeout_secs: Some(300),
            }
        );
        assert!(overrides(json!({"spec": "butler"})).unwrap().is_empty());
        assert!(overrides(json!({})).unwrap().is_empty());
        let err = overrides(json!({"max_steps": 5})).unwrap_err();
        assert!(err.contains("`spec`"), "{err}");
        let err = overrides(json!({"system_prompt": "x"})).unwrap_err();
        assert!(err.contains("`spec`"), "{err}");
        let err = overrides(json!({"spec": "butler", "max_steps": 0})).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }
}
