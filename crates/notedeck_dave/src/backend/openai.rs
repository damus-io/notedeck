use crate::backend::traits::{AiBackend, ResponseStream};
use crate::messages::DaveApiResponse;
use crate::tools::{PartialToolCall, Tool, ToolCall, ToolCalls, ToolResponses};
use crate::Message;
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
use tokio::sync::watch;

/// The native agentic loop over an OpenAI-compatible endpoint.
///
/// One user message starts a **run**: the model's reply, every round of tool
/// calls it makes, and the request after each round that sends the results
/// back. Dave drives the rounds (it runs the tools and dispatches again); this
/// backend bounds them. A run stops after [`max_steps`](Self::max_steps)
/// rounds of tool calls, and [`interrupt_session`](AiBackend::interrupt_session)
/// stops it at once, mid-stream or between rounds.
pub struct OpenAiBackend {
    client: Client<OpenAIConfig>,
    ndb: Ndb,
    /// Most rounds of tool calls in one run.
    max_steps: u32,
    /// Each session's current run's stop flag, keyed by the backend session
    /// id. A new run replaces its session's entry; the stop it holds reaches
    /// every request of the run, including one already in flight.
    runs: DashMap<String, watch::Sender<bool>>,
}

impl OpenAiBackend {
    /// A backend that sends requests through `client`, running each loop
    /// under [`Budget::DEFAULT_MAX_STEPS`].
    pub fn new(client: Client<OpenAIConfig>, ndb: Ndb) -> Self {
        Self {
            client,
            ndb,
            max_steps: Budget::DEFAULT_MAX_STEPS,
            runs: DashMap::new(),
        }
    }

    /// Cap each run at `max_steps` rounds of tool calls (an agent spec's
    /// `budget.max_steps`).
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// The stop flag for the request about to go out on `session_id`. A
    /// request that starts a run gets a fresh one; a later round of the same
    /// run shares the run's, so a stop sent during one round still holds at
    /// the next.
    fn run_stop(&self, session_id: &str, starts_run: bool) -> watch::Receiver<bool> {
        if !starts_run {
            if let Some(stop) = self.runs.get(session_id) {
                return stop.subscribe();
            }
        }
        let (stop, rx) = watch::channel(false);
        self.runs.insert(session_id.to_owned(), stop);
        rx
    }
}

impl AiBackend for OpenAiBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: Option<String>,
        user_id: String,
        session_id: String,
        _session_env: BTreeMap<String, String>,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        _permission_mode: PermissionMode,
        waker: Waker,
    ) -> ResponseStream {
        let stop = self.run_stop(&session_id, starts_run(&messages));

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

        let (tx, rx) = mpsc::channel();

        let api_messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(&self.ndb).expect("txn");
            messages
                .iter()
                .filter_map(|c| message_to_api(c, &txn, &self.ndb))
                .collect()
        };

        let request = CreateChatCompletionRequest {
            model: model.unwrap_or_else(|| "gpt-4.1-mini".to_string()),
            stream: Some(true),
            messages: api_messages,
            tools: Some(tools.values().map(tool_to_api).collect()),
            user: Some(user_id),
            ..Default::default()
        };
        let client = self.client.clone();

        let handle = tokio::spawn(async move {
            let turn = stream_turn(client, request, tools, tx.clone(), waker.clone());
            tokio::select! {
                // A stop wins a tie: the round's tool calls are dropped
                // unsent, so nothing more runs.
                biased;
                () = stopped(stop) => {
                    let _ = tx.send(DaveApiResponse::CommandReply(STOPPED.to_owned()));
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
        if let Some(stop) = self.runs.get(&session_id) {
            stop.send_replace(true);
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

/// Whether `messages` (a request's whole history) starts a new run: the
/// user's message is the last thing in it. A later round of a run ends on the
/// tool results it sends back.
fn starts_run(messages: &[Message]) -> bool {
    messages.last().is_some_and(Message::is_user_turn)
}

/// How many rounds of tool calls the current run has made: the tool-call rows
/// since the user's last message.
fn tool_steps(messages: &[Message]) -> usize {
    let run_start = messages
        .iter()
        .rposition(Message::is_user_turn)
        .map_or(0, |i| i + 1);
    messages[run_start..]
        .iter()
        .filter(|m| matches!(m, Message::ToolCalls(_)))
        .count()
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

    let mut parsed_tool_calls = vec![];
    for (_index, partial) in all_tool_calls {
        let Some(unknown_tool_call) = partial.complete() else {
            tracing::error!("could not complete partial tool call: {:?}", partial);
            continue;
        };

        match unknown_tool_call.parse(&tools) {
            Ok(tool_call) => {
                parsed_tool_calls.push(tool_call);
            }
            Err(err) => {
                tracing::error!("failed to parse tool call {:?}: {}", unknown_tool_call, err,);

                if let Some(id) = partial.id() {
                    parsed_tool_calls.push(ToolCall::invalid(
                        id.to_string(),
                        partial.name,
                        partial.arguments,
                        err.to_string(),
                    ));
                }
            }
        };
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

// --- async_openai request mapping -------------------------------------------
//
// `Message`/`ToolCall`/`Tool` live in the egui- and async_openai-free
// `agentium-core` engine crate. The mapping to async_openai's request types is
// an OpenAI-backend concern, so it lives here as free functions rather than as
// inherent methods on the engine types.

/// Map a dave `Message` to an async_openai chat message. UI-only messages
/// (errors, permission requests, executed-tool results, compaction, subagents,
/// todo updates) are not sent to the API and map to `None`.
fn message_to_api(
    msg: &Message,
    txn: &Transaction,
    ndb: &Ndb,
) -> Option<ChatCompletionRequestMessage> {
    match msg {
        Message::Error(_err) => None,

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
fn tool_to_api(tool: &Tool) -> ChatCompletionTool {
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
    use crate::tools::{dave_tools, ToolCall};
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

    /// A backend whose endpoint accepts and never answers, so a request
    /// stays in flight until it is stopped.
    fn hanging_backend(ndb: &Ndb) -> (OpenAiBackend, std::net::TcpListener) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let client = Client::with_config(OpenAIConfig::new().with_api_base(base));
        (OpenAiBackend::new(client, ndb.clone()), listener)
    }

    /// Send `messages` on session "s" and collect the turn's responses
    /// until the backend ends it.
    async fn turn(backend: &OpenAiBackend, messages: Vec<Message>) -> Vec<DaveApiResponse> {
        let stream = request(backend, messages);
        drain(stream).await
    }

    fn request(backend: &OpenAiBackend, messages: Vec<Message>) -> ResponseStream {
        backend.stream_request(
            messages,
            Arc::new(HashMap::new()),
            None,
            "user".to_string(),
            "s".to_string(),
            BTreeMap::new(),
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
    async fn a_run_stops_at_its_step_cap_without_asking_the_model() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let (backend, _listener) = hanging_backend(&ndb);
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
        let (backend, _listener) = hanging_backend(&ndb);
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
