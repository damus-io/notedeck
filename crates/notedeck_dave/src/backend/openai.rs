use crate::backend::traits::{AiBackend, ResponseStream};
use crate::messages::{DaveApiResponse, UsageInfo};
use crate::tools::{PartialToolCall, Tool, ToolCall, ToolCalls, ToolResponses};
use crate::Message;
use agentium_core::agent_spec::pricing::{self, Cost, Meter, Rate, Usage};
use agentium_core::agent_spec::provider::Endpoint;
use agentium_core::agent_spec::web::ASK_RESEARCHER;
use agentium_core::agent_spec::Budget;
use async_openai::{config::OpenAIConfig, types::*, Client};
use claude_agent_sdk_rs::PermissionMode;
use dashmap::DashMap;
use futures::StreamExt;
use nostrdb::{Ndb, Transaction};
use notedeck::Waker;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The native agentic loop over an OpenAI-compatible endpoint.
///
/// One user message starts a **run**: the model's reply, every round of tool
/// calls it makes, and the request after each round that sends the results
/// back. Dave drives the rounds (it runs the tools and dispatches again); this
/// backend bounds them. A run stops after [`max_steps`](Self::max_steps)
/// rounds of tool calls, after [`timeout`](Self::timeout), or once it has
/// spent [`max_cost_cents`](Self::max_cost_cents); and
/// [`interrupt_session`](AiBackend::interrupt_session) stops it at once,
/// mid-stream or between rounds.
///
/// ```text
/// run ─ round 1 ─ tools ─ round 2 ─ tools ─ … ─ answer
///       │  each request: steps < max_steps? started + timeout not passed?
///       │                own spend + sub-agents' spend < max_cost_cents?
///       └─ each response: usage × the model's rate ─▶ the run's own Meter
/// ```
pub struct OpenAiBackend {
    client: Client<OpenAIConfig>,
    ndb: Ndb,
    /// Most rounds of tool calls in one run.
    max_steps: u32,
    /// Longest a run may take, from its first request.
    timeout: Option<Duration>,
    /// Most a run may spend, in cents, priced from each response's usage.
    max_cost_cents: Option<u32>,
    /// The model a request goes out on when its session names none.
    default_model: String,
    /// The agent's role (an agent spec's prompt), sent ahead of every
    /// request's messages; `None` sends the chat as it is.
    system_prompt: Option<String>,
    /// Each session's current run's stop flag, keyed by the backend session
    /// id. A new run replaces its session's entry; the stop it holds reaches
    /// every request of the run, including one already in flight.
    runs: DashMap<String, Run>,
}

/// A session's current run.
struct Run {
    /// Its stop flag.
    stop: watch::Sender<bool>,
    /// When its first request went out.
    started: Instant,
    /// What it has spent.
    spend: Arc<RunSpend>,
}

/// What a run has spent, and what its session had before it.
#[derive(Debug, Default)]
struct RunSpend {
    /// The session's spend before this run.
    before: Cost,
    /// This run's own requests.
    own: Meter,
    /// Its sub-agents', as their answers in the run report it.
    children: Meter,
}

impl RunSpend {
    /// The run's spend: its own requests and its sub-agents'.
    fn run(&self) -> Cost {
        self.own.spent() + self.children.spent()
    }

    /// The session's spend, this run's included.
    fn session(&self) -> Cost {
        self.before + self.run()
    }
}

/// The run a request belongs to, as the request sees it.
struct RunView {
    stop: watch::Receiver<bool>,
    started: Instant,
    spend: Arc<RunSpend>,
}

impl OpenAiBackend {
    /// A backend that sends requests through `client`, running each loop
    /// under [`Budget::DEFAULT_MAX_STEPS`].
    pub fn new(client: Client<OpenAIConfig>, ndb: Ndb) -> Self {
        Self {
            client,
            ndb,
            max_steps: Budget::DEFAULT_MAX_STEPS,
            timeout: None,
            max_cost_cents: None,
            default_model: DEFAULT_MODEL.to_owned(),
            system_prompt: None,
            runs: DashMap::new(),
        }
    }

    /// A backend that sends every request to `endpoint` (an agent spec's
    /// provider, resolved by
    /// [`provider::resolve`](agentium_core::agent_spec::provider::resolve)),
    /// on its model unless a session names another.
    pub fn for_endpoint(endpoint: &Endpoint, ndb: Ndb) -> Self {
        Self {
            default_model: endpoint.model.clone(),
            ..Self::new(Client::with_config(openai_config(endpoint)), ndb)
        }
    }

    /// Send `prompt` (an agent spec's) as the system message ahead of every
    /// request, with today's date after it, so the role never sits in the
    /// session's chat: the chat stays the user's conversation, and a restored
    /// session gets the role from its spec again rather than from history.
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    /// Cap each run at `max_steps` rounds of tool calls (an agent spec's
    /// `budget.max_steps`).
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Bound each run by an agent spec's `budget`: its step cap, its
    /// wall-clock limit and its spend.
    pub fn with_budget(mut self, budget: &Budget) -> Self {
        self.max_steps = budget.max_steps;
        self.timeout = budget.timeout_secs.map(Duration::from_secs);
        self.max_cost_cents = budget.max_cost_cents;
        self
    }

    /// The run the request about to go out on `session_id` belongs to. A
    /// request that starts a run gets a fresh one (its clock started, its
    /// spend at zero); a later round of the same run shares the run's, so a
    /// stop sent during one round still holds at the next, and its clock
    /// and spend carry on.
    fn run(&self, session_id: &str, starts_run: bool) -> RunView {
        let last = self.runs.get(session_id).map(|run| RunView {
            stop: run.stop.subscribe(),
            started: run.started,
            spend: Arc::clone(&run.spend),
        });
        let last = match last {
            Some(run) if !starts_run => return run,
            last => last,
        };
        let before = last.map_or(Cost::ZERO, |run| run.spend.session());
        let (stop, rx) = watch::channel(false);
        let run = Run {
            stop,
            started: Instant::now(),
            spend: Arc::new(RunSpend {
                before,
                ..Default::default()
            }),
        };
        let view = RunView {
            stop: rx,
            started: run.started,
            spend: Arc::clone(&run.spend),
        };
        self.runs.insert(session_id.to_owned(), run);
        view
    }
}

/// `prompt` as a system message, with today's date after it (an agent's
/// prompt can say "a week from today" and have the model know what today
/// is), and the session's own `agentium:` ref when `session_env` has one.
///
/// The ref is the `AGENTIUM_SESSION` a Claude session's CLI gets in its
/// environment ([`session_env`](super::shared::session_env)). A native
/// loop's agent runs inside notedeck, with no environment of its own, so
/// this is how it knows which session it is: to quote itself in a headway
/// comment, or pass itself as a spawn's or a message's `from_session`.
fn dated_system_prompt(prompt: &str, session_env: &BTreeMap<String, String>) -> Message {
    let today = chrono::Local::now().format("%Y-%m-%d");
    let mut text = format!("{prompt}\n\nToday is {today}.");
    if let Some(me) = session_env.get(SESSION_ENV) {
        text.push_str(&format!(" Your agentium session is `{me}`."));
    }
    Message::System(text.into())
}

/// The variable holding a session's own `agentium:` ref in its env.
const SESSION_ENV: &str = "AGENTIUM_SESSION";

/// The model a request goes out on when neither its session nor the
/// backend's endpoint names one.
const DEFAULT_MODEL: &str = "gpt-4.1-mini";

/// The client config that sends requests to `base_url` with `api_key` as the
/// bearer token, and nothing from the process environment.
///
/// This is the only place `OpenAIConfig::new` may be called. `new` (and
/// `Default`) fills the key from the process's `OPENAI_API_KEY`, so a config
/// built any other way and left keyless sends the user's own OpenAI key to
/// whatever endpoint it names: a local model, a work proxy, anyone's URL.
/// Here both fields are always overwritten, an absent key as `""`.
/// async-openai inserts the `Authorization` header whatever the key, so a
/// keyless endpoint is sent `Bearer ` rather than no header.
///
/// A caller that wants the env's key reads it itself, only for an endpoint
/// that should get it (see `ModelConfig::from_settings`).
pub fn client_config(base_url: &str, api_key: Option<&str>) -> OpenAIConfig {
    OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key.unwrap_or(""))
}

/// The client config that sends requests to `endpoint`: its base URL, and its
/// key as the bearer token (see [`client_config`]).
pub fn openai_config(endpoint: &Endpoint) -> OpenAIConfig {
    client_config(&endpoint.base_url, endpoint.api_key.as_deref())
}

impl AiBackend for OpenAiBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: Option<String>,
        user_id: String,
        session_id: String,
        session_env: BTreeMap<String, String>,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        _permission_mode: PermissionMode,
        waker: Waker,
    ) -> ResponseStream {
        let RunView {
            stop,
            started,
            spend,
        } = self.run(&session_id, starts_run(&messages));

        // The run was stopped after its last round's tool calls had already
        // arrived: Dave ran them and is sending the results back. Don't.
        if *stop.borrow() {
            return answered(DaveApiResponse::CommandReply(STOPPED.to_owned()), &waker);
        }
        if tool_steps(&messages) >= self.max_steps as usize {
            return answered(
                DaveApiResponse::Failed(step_limit_message(self.max_steps)),
                &waker,
            );
        }
        if let Some(limit) = self.timeout.filter(|limit| started.elapsed() >= *limit) {
            return answered(DaveApiResponse::Failed(time_limit_message(limit)), &waker);
        }
        spend
            .children
            .raise_to(sub_agent_spend(current_run(&messages)));
        if let Some(cap) = self.max_cost_cents.filter(|cap| spend.run().reached(*cap)) {
            return answered(
                DaveApiResponse::Failed(cost_limit_message(spend.run(), cap)),
                &waker,
            );
        }

        let (tx, rx) = mpsc::channel();

        let api_messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(&self.ndb).expect("txn");
            let role = self
                .system_prompt
                .as_deref()
                .map(|prompt| dated_system_prompt(prompt, &session_env));
            role.iter()
                .chain(messages.iter())
                .filter_map(|c| message_to_api(c, &txn, &self.ndb))
                .collect()
        };

        let model = model.unwrap_or_else(|| self.default_model.clone());
        let metering = Metering {
            rate: pricing::rate(&model),
            spend,
            max_cost_cents: self.max_cost_cents,
        };
        let request = CreateChatCompletionRequest {
            model,
            stream: Some(true),
            // Usage comes on a last chunk, and only when asked for. Only a
            // model the price table knows is asked, so a provider that
            // rejects the option is never sent it.
            stream_options: metering.rate.map(|_| ChatCompletionStreamOptions {
                include_usage: true,
            }),
            messages: api_messages,
            tools: Some(tools.values().map(tool_to_api).collect()),
            user: Some(user_id),
            ..Default::default()
        };
        let client = self.client.clone();
        let limit = self.timeout;

        let handle = tokio::spawn(async move {
            let turn = stream_turn(client, request, tools, metering, tx.clone(), waker.clone());
            tokio::select! {
                // A stop wins a tie: the round's tool calls are dropped
                // unsent, so nothing more runs.
                biased;
                () = stopped(stop) => {
                    let _ = tx.send(DaveApiResponse::CommandReply(STOPPED.to_owned()));
                    waker.wake();
                }
                () = expired(started, limit) => {
                    let limit = limit.unwrap_or_default();
                    let _ = tx.send(DaveApiResponse::Failed(time_limit_message(limit)));
                    waker.wake();
                }
                () = turn => {}
            }
        });

        ResponseStream {
            rx: Some(rx),
            task: Some(handle),
        }
    }

    fn cleanup_session(&self, session_id: String) {
        self.runs.remove(&session_id);
    }

    /// Stop the session's run: an in-flight request is dropped (its partial
    /// tool calls with it) and no later round is sent. The turn ends with a
    /// "Stopped." row; the next user message starts a new run.
    fn interrupt_session(&self, session_id: String, _waker: Waker) {
        if let Some(run) = self.runs.get(&session_id) {
            run.stop.send_replace(true);
        }
    }

    fn set_permission_mode(&self, _session_id: String, _mode: PermissionMode, _waker: Waker) {
        // OpenAI backend doesn't support permission modes / plan mode
        tracing::warn!("Plan mode is not supported with the OpenAI backend");
    }
}

/// The row a stopped run ends on.
const STOPPED: &str = "Stopped.";

/// Why a run ended at its step cap, and how to let it go on.
fn step_limit_message(max_steps: u32) -> String {
    format!(
        "Stopped after {max_steps} rounds of tool calls, this turn's limit. \
         Send a message to let it continue."
    )
}

/// Why a run ended at its time limit, and how to let it go on.
fn time_limit_message(limit: Duration) -> String {
    format!(
        "Stopped after {}s, this turn's time limit. Send a message to let it continue.",
        limit.as_secs()
    )
}

/// Why a run ended at its spending limit, and how to let it go on.
fn cost_limit_message(spent: Cost, cap_cents: u32) -> String {
    let cap = Cost::from_cents(f64::from(cap_cents));
    format!(
        "Stopped after spending {spent}, this turn's limit of {cap}. \
         Send a message to let it continue."
    )
}

/// Resolve once `limit` has passed since `started`; never, with no limit.
async fn expired(started: Instant, limit: Option<Duration>) {
    match limit {
        Some(limit) => tokio::time::sleep_until((started + limit).into()).await,
        None => std::future::pending().await,
    }
}

/// Whether `messages` (a request's whole history) starts a new run: the
/// user's message is the last thing in it. A later round of a run ends on the
/// tool results it sends back.
fn starts_run(messages: &[Message]) -> bool {
    messages.last().is_some_and(Message::is_user_turn)
}

/// The current run's rows: everything since the user's last message.
fn current_run(messages: &[Message]) -> &[Message] {
    let run_start = messages
        .iter()
        .rposition(Message::is_user_turn)
        .map_or(0, |i| i + 1);
    &messages[run_start..]
}

/// How many rounds of tool calls the current run has made: the tool-call rows
/// since the user's last message.
fn tool_steps(messages: &[Message]) -> usize {
    current_run(messages)
        .iter()
        .filter(|m| matches!(m, Message::ToolCalls(_)))
        .count()
}

/// What the sub-agents the `run` asked spent, as each answer reports it
/// (`spent_cents` beside the researcher's summary or error).
fn sub_agent_spend(run: &[Message]) -> Cost {
    run.iter()
        .filter_map(|m| match m {
            Message::ToolResponse(response) => match response.responses() {
                ToolResponses::App(app) if app.tool == ASK_RESEARCHER => {
                    app.value.get(SPENT_CENTS)?.as_f64()
                }
                _ => None,
            },
            _ => None,
        })
        .map(Cost::from_cents)
        .fold(Cost::ZERO, |total, cost| total + cost)
}

/// The key a sub-agent's answer reports its spend under, in cents.
pub(crate) const SPENT_CENTS: &str = "spent_cents";

/// How a request's spend is counted and capped.
struct Metering {
    /// Its model's price; `None` for one the table doesn't list, whose
    /// spend isn't known.
    rate: Option<Rate>,
    /// The run it adds to.
    spend: Arc<RunSpend>,
    /// The run's cap, in cents.
    max_cost_cents: Option<u32>,
}

impl Metering {
    /// Count a response's `usage`: its cost to the run, and its tokens and
    /// the session's spend so far to the session's usage meter.
    fn count(&self, usage: &CompletionUsage, tx: &mpsc::Sender<DaveApiResponse>) {
        let tokens = UsageInfo {
            input_tokens: u64::from(usage.prompt_tokens),
            output_tokens: u64::from(usage.completion_tokens),
            num_turns: 1,
            ..Default::default()
        };
        let cost_usd = self.rate.map(|rate| {
            self.spend.own.add(rate.cost(Usage {
                input_tokens: tokens.input_tokens,
                output_tokens: tokens.output_tokens,
            }));
            // The usage card takes each report as the session's running
            // total, and each round as a turn.
            self.spend.session().usd()
        });
        let _ = tx.send(DaveApiResponse::UsageUpdate(tokens.clone()));
        let _ = tx.send(DaveApiResponse::QueryComplete(UsageInfo {
            cost_usd,
            ..tokens
        }));
    }

    /// Whether the run has spent its budget.
    fn over_budget(&self) -> Option<String> {
        let cap = self.max_cost_cents?;
        let spent = self.spend.run();
        spent.reached(cap).then(|| cost_limit_message(spent, cap))
    }
}

/// A turn answered without asking the model: `response`, then the end of the
/// stream.
fn answered(response: DaveApiResponse, waker: &Waker) -> ResponseStream {
    let (tx, rx) = mpsc::channel();
    let _ = tx.send(response);
    waker.wake();
    ResponseStream {
        rx: Some(rx),
        task: None,
    }
}

/// Resolve once `stop` is set. A stop whose run was replaced (its sender
/// dropped) can no longer be set, so it never resolves.
async fn stopped(mut stop: watch::Receiver<bool>) {
    if stop.wait_for(|stopped| *stopped).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Stream one request's reply into `tx`: its text as tokens, then its tool
/// calls, parsed against `tools`, once the stream ends.
async fn stream_turn(
    client: Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
    tools: Arc<HashMap<String, Tool>>,
    metering: Metering,
    tx: mpsc::Sender<DaveApiResponse>,
    waker: Waker,
) {
    // Timeout for the initial API connection (creating the stream).
    const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    let mut token_stream =
        match tokio::time::timeout(CONNECT_TIMEOUT, client.chat().create_stream(request)).await {
            Ok(Err(err)) => {
                tracing::error!("openai chat error: {err}");
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                return;
            }
            Err(_) => {
                tracing::error!(
                    "openai stream creation timed out after {}s",
                    CONNECT_TIMEOUT.as_secs()
                );
                let _ = tx.send(DaveApiResponse::Failed(
                    "OpenAI API connection timed out".to_string(),
                ));
                return;
            }
            Ok(Ok(stream)) => stream,
        };

    let mut all_tool_calls: HashMap<u32, PartialToolCall> = HashMap::new();
    let mut usage = None;

    // Timeout for receiving each stream chunk — if no data arrives
    // for this long, the API connection is considered stalled.
    const CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    loop {
        let token = match tokio::time::timeout(CHUNK_TIMEOUT, token_stream.next()).await {
            Ok(Some(Ok(token))) => token,
            Ok(Some(Err(err))) => {
                tracing::error!("failed to get token: {err}");
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                return;
            }
            Ok(None) => break, // stream ended normally
            Err(_) => {
                tracing::error!(
                    "openai stream stalled (no data for {}s)",
                    CHUNK_TIMEOUT.as_secs()
                );
                let _ = tx.send(DaveApiResponse::Failed(
                    "OpenAI stream timed out (no data received)".to_string(),
                ));
                return;
            }
        };

        // Asked for, the request's usage comes on the last chunk.
        if token.usage.is_some() {
            usage = token.usage;
        }

        for choice in &token.choices {
            let resp = &choice.delta;

            // if we have tool call arg chunks, collect them here
            if let Some(tool_calls) = &resp.tool_calls {
                for tool in tool_calls {
                    let entry = all_tool_calls.entry(tool.index).or_default();

                    if let Some(id) = &tool.id {
                        entry.id_mut().get_or_insert(id.clone());
                    }

                    if let Some(name) = tool.function.as_ref().and_then(|f| f.name.as_ref()) {
                        entry.name_mut().get_or_insert(name.to_string());
                    }

                    if let Some(argchunk) =
                        tool.function.as_ref().and_then(|f| f.arguments.as_ref())
                    {
                        entry
                            .arguments_mut()
                            .get_or_insert_with(String::new)
                            .push_str(argchunk);
                    }
                }
            }

            if let Some(content) = &resp.content {
                if let Err(err) = tx.send(DaveApiResponse::Token(content.to_owned())) {
                    tracing::error!("failed to send dave response token to ui: {err}");
                }
                waker.wake();
            }
        }
    }

    if let Some(usage) = &usage {
        metering.count(usage, &tx);
        waker.wake();
    }

    let parsed_tool_calls: Vec<ToolCall> = all_tool_calls
        .into_values()
        .filter_map(|partial| finish_tool_call(partial, &tools))
        .collect();

    // The reply that spent the budget asked for more tools: don't run
    // them, which could spend more (a sub-agent), and end the run here.
    if !parsed_tool_calls.is_empty() {
        if let Some(why) = metering.over_budget() {
            let _ = tx.send(DaveApiResponse::Failed(why));
            waker.wake();
            return;
        }
    }

    if !parsed_tool_calls.is_empty()
        && tx
            .send(DaveApiResponse::ToolCalls(parsed_tool_calls))
            .is_ok()
    {
        waker.wake();
    }

    tracing::debug!("stream closed");
}

/// The tool call `partial` collected, parsed against `tools`: a call the
/// model got wrong (an unknown tool, bad arguments) is an
/// [invalid](ToolCall::invalid) one, so its error goes back to the model.
/// `None` for one with no id or name, which nothing can answer.
pub(crate) fn finish_tool_call(
    partial: PartialToolCall,
    tools: &HashMap<String, Tool>,
) -> Option<ToolCall> {
    let Some(unknown_tool_call) = partial.complete() else {
        tracing::error!("could not complete partial tool call: {:?}", partial);
        return None;
    };
    match unknown_tool_call.parse(tools) {
        Ok(tool_call) => Some(tool_call),
        Err(err) => {
            tracing::error!("failed to parse tool call {:?}: {}", unknown_tool_call, err);
            let id = partial.id()?.to_string();
            Some(ToolCall::invalid(
                id,
                partial.name,
                partial.arguments,
                err.to_string(),
            ))
        }
    }
}

// --- async_openai request mapping -------------------------------------------
//
// `Message`/`ToolCall`/`Tool` live in the egui- and async_openai-free
// `agentium-core` engine crate. The mapping to async_openai's request types is
// an OpenAI-backend concern, so it lives here as free functions rather than as
// inherent methods on the engine types.

/// Map a dave `Message` to an async_openai chat message. UI-only messages
/// (errors, permission requests, executed-tool results, compaction, subagents,
/// todo updates) are not sent to the API and map to `None`.
pub(crate) fn message_to_api(
    msg: &Message,
    txn: &Transaction,
    ndb: &Ndb,
) -> Option<ChatCompletionRequestMessage> {
    match msg {
        Message::Error(_err) => None,

        // The reply text of a permission answer (a deny reason) is shown in
        // the chat, but the model gets it in the denied call's answer: a
        // user message between a round's tool calls and their answers is
        // one the API refuses.
        Message::User(m) if m.permission_reply => None,

        Message::User(m) => Some(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                name: None,
                content: ChatCompletionRequestUserMessageContent::Text(m.text.clone()),
            },
        )),

        Message::Assistant(m) => Some(ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                    m.text().to_string(),
                )),
                ..Default::default()
            },
        )),

        Message::System(m) => Some(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(m.text().to_owned()),
                ..Default::default()
            },
        )),

        Message::ToolCalls(calls) => Some(ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                tool_calls: Some(calls.iter().map(toolcall_to_api).collect()),
                ..Default::default()
            },
        )),

        Message::ToolResponse(resp) => {
            // ExecutedTool results are UI-only, not sent to the API
            if matches!(resp.responses(), ToolResponses::ExecutedTool(_)) {
                return None;
            }

            let tool_response = resp.responses().format_for_dave(txn, ndb);

            Some(ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    tool_call_id: resp.id().to_owned(),
                    content: ChatCompletionRequestToolMessageContent::Text(tool_response),
                },
            ))
        }

        // The remaining variants are UI-only, not sent to the API.
        Message::ToolRunning(_)
        | Message::PermissionRequest(_)
        | Message::CompactionComplete(_)
        | Message::Subagent(_)
        | Message::TodoUpdate(_) => None,
    }
}

/// Map a dave `ToolCall` to an async_openai tool call.
fn toolcall_to_api(call: &ToolCall) -> ChatCompletionMessageToolCall {
    ChatCompletionMessageToolCall {
        id: call.id().to_owned(),
        r#type: ChatCompletionToolType::Function,
        function: toolcalls_to_function(call.calls()),
    }
}

/// Map the inner `ToolCalls` payload to an async_openai function call.
fn toolcalls_to_function(calls: &ToolCalls) -> FunctionCall {
    FunctionCall {
        name: calls.api_name().to_owned(),
        arguments: calls.arguments(),
    }
}

/// Map a dave `Tool` definition to an async_openai tool (function) definition.
/// The JSON-Schema `parameters` come from the engine's backend-agnostic
/// [`ToolSpec::json_schema`]; this wraps them in async_openai's envelope.
pub(crate) fn tool_to_api(tool: &Tool) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: tool.name().to_owned(),
            description: Some(tool.description().to_owned()),
            strict: Some(false),
            parameters: Some(tool.spec().json_schema()),
        },
    }
}

#[cfg(test)]
mod tests {
    //! Network-free coverage for the async_openai request mapping extracted
    //! from `agentium-core`. Guards the wire shape (roles, tool-call ids,
    //! function names, and the tool schema envelope) without a live API.
    use super::*;
    use crate::messages::AssistantMessage;
    use crate::test_support::fake_openai::FakeOpenAi;
    use crate::tools::{dave_tools, ToolCall, ToolResponse};
    use notedeck_testing::fixtures::TempNdb;

    #[test]
    fn message_to_api_maps_core_roles() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let txn = Transaction::new(&ndb).unwrap();

        assert!(matches!(
            message_to_api(&Message::User("hello".into()), &txn, &ndb),
            Some(ChatCompletionRequestMessage::User(_))
        ));
        assert!(matches!(
            message_to_api(&Message::System("sys".into()), &txn, &ndb),
            Some(ChatCompletionRequestMessage::System(_))
        ));
        assert!(matches!(
            message_to_api(
                &Message::Assistant(AssistantMessage::from_text("hi".to_string())),
                &txn,
                &ndb
            ),
            Some(ChatCompletionRequestMessage::Assistant(_))
        ));

        // UI-only messages are never sent to the API.
        assert!(message_to_api(&Message::Error("e".to_string()), &txn, &ndb).is_none());
        // Nor is a permission answer's reply text, which the call's own
        // answer carries.
        let reply = Message::User(crate::messages::UserMessage::permission_reply("no"));
        assert!(message_to_api(&reply, &txn, &ndb).is_none());
    }

    /// An app tool's call goes out under the tool's own name, and its answer
    /// is the tool's JSON value.
    #[test]
    fn app_tool_calls_and_answers_map_to_the_tools_own_name_and_value() {
        use crate::tools::{AppToolCall, AppToolResponse, ToolCalls, ToolResponse};
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let txn = Transaction::new(&ndb).unwrap();

        let call = ToolCall::new(
            "call_1".to_string(),
            ToolCalls::App(AppToolCall {
                name: "headway_show_board".to_string(),
                args: serde_json::json!({ "board": "dave" }),
            }),
        );
        let api = message_to_api(&Message::ToolCalls(vec![call]), &txn, &ndb).unwrap();
        let ChatCompletionRequestMessage::Assistant(a) = api else {
            panic!("tool calls must map to an assistant message");
        };
        let calls = a.tool_calls.expect("tool_calls present");
        assert_eq!(calls[0].function.name, "headway_show_board");
        assert_eq!(calls[0].function.arguments, r#"{"board":"dave"}"#);

        let answer = Message::ToolResponse(ToolResponse::new(
            "call_1".to_string(),
            ToolResponses::App(AppToolResponse {
                tool: "headway_show_board".to_string(),
                value: serde_json::json!({ "cards": 3 }),
            }),
        ));
        let ChatCompletionRequestMessage::Tool(t) = message_to_api(&answer, &txn, &ndb).unwrap()
        else {
            panic!("an answer maps to a tool message");
        };
        assert_eq!(t.tool_call_id, "call_1");
        assert!(matches!(
            t.content,
            ChatCompletionRequestToolMessageContent::Text(ref text) if text == r#"{"cards":3}"#
        ));
    }

    #[test]
    fn message_to_api_maps_tool_calls() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let txn = Transaction::new(&ndb).unwrap();

        let call = ToolCall::invalid(
            "call_1".to_string(),
            Some("mytool".to_string()),
            Some("{}".to_string()),
            "bad".to_string(),
        );
        let api = message_to_api(&Message::ToolCalls(vec![call]), &txn, &ndb).unwrap();
        let ChatCompletionRequestMessage::Assistant(a) = api else {
            panic!("tool calls must map to an assistant message");
        };
        let calls = a.tool_calls.expect("tool_calls present");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_1");
        assert_eq!(calls[0].function.name.as_str(), "error");
    }

    // --- the loop's bounds -------------------------------------------------

    /// A round of tool calls, as Dave records it.
    fn round(id: &str) -> Message {
        Message::ToolCalls(vec![ToolCall::invalid(
            id.to_string(),
            Some("mytool".to_string()),
            Some("{}".to_string()),
            "bad".to_string(),
        )])
    }

    /// A round's result, sent back on the next request.
    fn result(id: &str) -> Message {
        Message::tool_error(id.to_string(), "bad".to_string())
    }

    /// A history that ends on `rounds` rounds of tool calls and their results
    /// after the user's message.
    fn run_of(rounds: usize) -> Vec<Message> {
        let mut messages = vec![Message::User("earlier".into()), round("old"), result("old")];
        messages.push(Message::User("go".into()));
        for i in 0..rounds {
            let id = format!("call_{i}");
            messages.push(round(&id));
            messages.push(result(&id));
        }
        messages
    }

    #[test]
    fn steps_count_rounds_since_the_users_last_message() {
        assert_eq!(tool_steps(&[]), 0);
        assert_eq!(tool_steps(&run_of(0)), 0);
        // The earlier run's round doesn't count against this one.
        assert_eq!(tool_steps(&run_of(3)), 3);
        // A permission reply is the turn's content, not a new message.
        let mut messages = run_of(2);
        let reply = crate::messages::UserMessage::permission_reply("ok".to_string());
        messages.push(Message::User(reply));
        assert_eq!(tool_steps(&messages), 2);
    }

    #[test]
    fn a_run_starts_on_the_users_message_not_on_tool_results() {
        assert!(starts_run(&run_of(0)));
        assert!(!starts_run(&run_of(1)));
        // A message queued during a run is sent behind its results and
        // starts a new run.
        let mut queued = run_of(1);
        queued.push(Message::User("and then".into()));
        assert!(starts_run(&queued));
        assert_eq!(tool_steps(&queued), 0);
    }

    /// A backend on `endpoint`, which a test keeps alive beside it.
    fn backend_on(endpoint: &FakeOpenAi, ndb: &Ndb) -> OpenAiBackend {
        let client = Client::with_config(client_config(endpoint.base_url(), None));
        OpenAiBackend::new(client, ndb.clone())
    }

    /// Send `messages` on session "s" and collect the turn's responses
    /// until the backend ends it.
    async fn turn(backend: &OpenAiBackend, messages: Vec<Message>) -> Vec<DaveApiResponse> {
        let stream = request(backend, messages);
        drain(stream).await
    }

    fn request(backend: &OpenAiBackend, messages: Vec<Message>) -> ResponseStream {
        request_of(backend, messages)
    }

    /// [`request`], for a test with a local named `request`.
    fn request_of(backend: &OpenAiBackend, messages: Vec<Message>) -> ResponseStream {
        request_in(backend, messages, BTreeMap::new())
    }

    /// [`request`] from a session whose env is `session_env`.
    fn request_in(
        backend: &OpenAiBackend,
        messages: Vec<Message>,
        session_env: BTreeMap<String, String>,
    ) -> ResponseStream {
        backend.stream_request(
            messages,
            Arc::new(HashMap::new()),
            None,
            "user".to_string(),
            "s".to_string(),
            session_env,
            None,
            None,
            PermissionMode::Default,
            Waker::noop(),
        )
    }

    async fn drain(stream: ResponseStream) -> Vec<DaveApiResponse> {
        if let Some(task) = stream.task {
            tokio::time::timeout(std::time::Duration::from_secs(10), task)
                .await
                .expect("the turn ends")
                .expect("the turn's task doesn't panic");
        }
        stream.rx.expect("a receiver").try_iter().collect()
    }

    /// What a turn's responses say, one line each, to assert on.
    fn said(responses: &[DaveApiResponse]) -> Vec<String> {
        responses
            .iter()
            .map(|response| match response {
                DaveApiResponse::Failed(text) => format!("failed: {text}"),
                DaveApiResponse::CommandReply(text) => format!("reply: {text}"),
                _ => "something else".to_string(),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_backend_for_an_endpoint_sends_its_url_key_and_model() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let seen = FakeOpenAi::silent();
        let endpoint = Endpoint {
            base_url: format!("http://{}/v9", seen.addr()),
            api_key: Some("sk-provider".into()),
            ..seen.endpoint("qwen3")
        };
        let backend = OpenAiBackend::for_endpoint(&endpoint, ndb.clone());

        let stream = request(&backend, run_of(0));
        let request = tokio::task::spawn_blocking(move || seen.request())
            .await
            .unwrap();
        stream.task.unwrap().abort();

        let lower = request.to_ascii_lowercase();
        assert!(
            request.starts_with("POST /v9/chat/completions "),
            "{request}"
        );
        assert!(
            lower.contains("authorization: bearer sk-provider\r\n"),
            "{request}"
        );
        assert!(request.contains(r#""model":"qwen3""#), "{request}");
    }

    /// A spec's prompt goes out as the request's first message, dated,
    /// ahead of the chat, which never holds it.
    #[tokio::test]
    async fn a_system_prompt_leads_every_request() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let seen = FakeOpenAi::silent();
        let endpoint = seen.endpoint("qwen3");
        let backend = OpenAiBackend::for_endpoint(&endpoint, ndb.clone())
            .with_system_prompt("You are the butler.".into());

        let stream = request(&backend, run_of(0));
        let request = tokio::task::spawn_blocking(move || seen.request())
            .await
            .unwrap();
        stream.task.unwrap().abort();

        let body = &request[request.find("\r\n\r\n").expect("a body") + 4..];
        let body: serde_json::Value = serde_json::from_str(body).expect("JSON body");
        let first = &body["messages"][0];
        assert_eq!(first["role"], "system", "{body}");
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(
            first["content"],
            format!("You are the butler.\n\nToday is {today}."),
        );
        assert_eq!(body["messages"][1]["role"], "user", "the chat follows");
    }

    /// A session with an agentium identity is told its own ref after the
    /// date, so it can name itself without an environment to read it from
    /// (headway:dave/talk-giraffe-south).
    #[tokio::test]
    async fn the_system_prompt_names_the_session() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let seen = FakeOpenAi::silent();
        let endpoint = seen.endpoint("qwen3");
        let backend = OpenAiBackend::for_endpoint(&endpoint, ndb.clone())
            .with_system_prompt("You are the butler.".into());
        let env = super::super::shared::session_env(Some("d-tag"), &BTreeMap::new(), None);
        let me = env["AGENTIUM_SESSION"].clone();

        let stream = request_in(&backend, run_of(0), env);
        let request = tokio::task::spawn_blocking(move || seen.request())
            .await
            .unwrap();
        stream.task.unwrap().abort();

        let body = &request[request.find("\r\n\r\n").expect("a body") + 4..];
        let body: serde_json::Value = serde_json::from_str(body).expect("JSON body");
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(
            body["messages"][0]["content"],
            format!("You are the butler.\n\nToday is {today}. Your agentium session is `{me}`."),
        );
    }

    /// A keyless endpoint is sent an empty bearer, not the process's
    /// `OPENAI_API_KEY`. The var is set for the test, so a config that read it
    /// would fail here on every box, not only on one that happens to have it.
    #[test]
    #[serial_test::serial]
    fn a_keyless_endpoint_sends_no_key_not_the_envs() {
        use async_openai::config::Config;
        let saved = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "sk-env");
        let endpoint = Endpoint {
            provider: "ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: None,
            model: "qwen3".into(),
        };
        let headers = openai_config(&endpoint).headers();
        match saved {
            Some(key) => std::env::set_var("OPENAI_API_KEY", key),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        assert_eq!(headers["authorization"], "Bearer ");
    }

    #[tokio::test]
    async fn a_run_stops_at_its_step_cap_without_asking_the_model() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let endpoint = FakeOpenAi::hanging();
        let backend = backend_on(&endpoint, &ndb);
        let backend = backend.with_max_steps(2);

        // One round under the cap still goes out.
        let stream = request(&backend, run_of(1));
        assert!(stream.task.is_some(), "a round under the cap is sent");
        stream.task.unwrap().abort();

        let responses = turn(&backend, run_of(2)).await;
        assert_eq!(
            said(&responses),
            [format!("failed: {}", step_limit_message(2))]
        );
    }

    #[tokio::test]
    async fn a_stop_ends_the_request_in_flight_and_every_round_after_it() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let endpoint = FakeOpenAi::hanging();
        let backend = backend_on(&endpoint, &ndb);
        let stopped = [format!("reply: {STOPPED}")];

        // The user's message goes out and hangs; the stop ends it.
        let stream = request(&backend, run_of(0));
        backend.interrupt_session("s".to_string(), Waker::noop());
        assert_eq!(said(&drain(stream).await), stopped);

        // A round whose tool calls had already arrived is not sent back.
        assert_eq!(said(&turn(&backend, run_of(1)).await), stopped);

        // The user's next message starts a new run, which goes out.
        let stream = request(&backend, run_of(0));
        assert!(stream.task.is_some(), "a new run is sent to the model");
        stream.task.unwrap().abort();
    }

    /// An endpoint on a local port that streams one reply: a tool call, then
    /// a last chunk reporting `prompt_tokens` of usage.
    fn streams_a_tool_call_costing(prompt_tokens: u32) -> FakeOpenAi {
        let call = serde_json::json!({
            "id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m",
            "choices": [{ "index": 0, "finish_reason": null, "delta": { "tool_calls": [{
                "index": 0, "id": "t1", "type": "function",
                "function": { "name": "mytool", "arguments": "{}" }
            }]}}]
        });
        let usage = serde_json::json!({
            "id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m",
            "choices": [],
            "usage": { "prompt_tokens": prompt_tokens, "completion_tokens": 0,
                       "total_tokens": prompt_tokens }
        });
        FakeOpenAi::streaming(vec![call, usage])
    }

    /// A reply whose usage spends the run's budget doesn't get its tool calls
    /// run, and the run's next round isn't sent: both end on the cost row.
    #[tokio::test]
    async fn a_run_stops_once_its_usage_spends_its_budget() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        // 10k prompt tokens on Sonnet 5.5 ($2/MTok) is 2 cents.
        let server = streams_a_tool_call_costing(10_000);
        let endpoint = Endpoint {
            provider: "anthropic".into(),
            ..server.endpoint("claude-sonnet-5-5")
        };
        let budget = Budget {
            max_steps: 10,
            max_cost_cents: Some(1),
            timeout_secs: None,
        };
        let backend = OpenAiBackend::for_endpoint(&endpoint, ndb.clone()).with_budget(&budget);
        let over = format!("failed: {}", cost_limit_message(Cost::from_cents(2.0), 1));

        let responses = turn(&backend, run_of(0)).await;
        let request = tokio::task::spawn_blocking(move || server.request())
            .await
            .unwrap();
        assert!(
            request.contains(r#""stream_options":{"include_usage":true}"#),
            "a priced model is asked for its usage: {request}"
        );
        assert!(
            !responses
                .iter()
                .any(|r| matches!(r, DaveApiResponse::ToolCalls(_))),
            "the over-budget reply's calls aren't run"
        );
        let cost = responses.iter().find_map(|r| match r {
            DaveApiResponse::QueryComplete(usage) => usage.cost_usd,
            _ => None,
        });
        assert_eq!(cost, Some(0.02), "the spend reaches the usage card");
        assert_eq!(said(&responses).last(), Some(&over));

        // Had the calls run, their results would not go out either.
        let stream = request_of(&backend, run_of(1));
        assert!(stream.task.is_none(), "the model isn't asked again");
        assert_eq!(said(&drain(stream).await), [over]);

        // The user's next message starts a run with nothing spent.
        let fresh = backend.run("s", true);
        assert_eq!(fresh.spend.run(), Cost::ZERO);
        assert_eq!(fresh.spend.session(), Cost::from_cents(2.0));
    }

    /// What a sub-agent spent, reported beside its answer, counts toward the
    /// parent's budget.
    #[tokio::test]
    async fn a_sub_agents_spend_counts_toward_the_run() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let endpoint = FakeOpenAi::hanging();
        let backend = backend_on(&endpoint, &ndb);
        let backend = backend.with_budget(&Budget {
            max_steps: 10,
            max_cost_cents: Some(4),
            timeout_secs: None,
        });
        let research = |id: &str, cents: f64| {
            Message::ToolResponse(ToolResponse::new(
                id.to_string(),
                ToolResponses::App(crate::tools::AppToolResponse {
                    tool: ASK_RESEARCHER.to_owned(),
                    value: serde_json::json!({ "summary": "…", SPENT_CENTS: cents }),
                }),
            ))
        };

        // Under the cap, the round goes out.
        let mut messages = run_of(0);
        messages.extend([round("r1"), research("r1", 3.0)]);
        let stream = request_of(&backend, messages.clone());
        assert!(stream.task.is_some(), "3 of 4 cents: the round is sent");
        stream.task.unwrap().abort();

        // A second research call takes it past.
        messages.extend([round("r2"), research("r2", 2.0)]);
        assert_eq!(
            said(&turn(&backend, messages).await),
            [format!(
                "failed: {}",
                cost_limit_message(Cost::from_cents(5.0), 4)
            )]
        );
    }

    /// A run past its time limit ends: the request in flight, and any round
    /// after it.
    #[tokio::test]
    async fn a_run_stops_at_its_time_limit() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let endpoint = FakeOpenAi::hanging();
        let mut backend = backend_on(&endpoint, &ndb);
        let limit = std::time::Duration::from_millis(200);
        backend.timeout = Some(limit);
        let timed_out = [format!("failed: {}", time_limit_message(limit))];

        // The user's message goes out and hangs; the clock ends it.
        assert_eq!(said(&turn(&backend, run_of(0)).await), timed_out);

        // A later round of the same run isn't sent.
        let stream = request_of(&backend, run_of(1));
        assert!(stream.task.is_none(), "the model isn't asked again");
        assert_eq!(said(&drain(stream).await), timed_out);
    }

    #[test]
    fn tool_to_api_preserves_schema_envelope() {
        for tool in dave_tools() {
            let api = tool_to_api(&tool);
            assert!(matches!(api.r#type, ChatCompletionToolType::Function));
            assert_eq!(api.function.name.as_str(), tool.name());
            assert_eq!(api.function.strict, Some(false));
            let params = api.function.parameters.expect("parameters present");
            assert_eq!(params.get("type").and_then(|v| v.as_str()), Some("object"));
            assert!(
                params.get("properties").is_some(),
                "schema for `{}` must carry properties",
                tool.name()
            );
        }
    }
}
