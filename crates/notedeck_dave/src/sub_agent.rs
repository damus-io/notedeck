//! A research sub-agent: a spec's `SubAgent` toolset, run as a child loop.
//!
//! An agent that reads private boards must not also fetch arbitrary pages:
//! an injected page tells it to put board contents into the next URL. So
//! the parent never fetches. It asks its sub-agent a question
//! ([`ASK_RESEARCHER`]), and the sub-agent answers it in a context of its
//! own: its own spec (no private data, checked by
//! [`AgentSpec::check_sub_agent`]), its own model and provider, its own
//! budget, and only the web tools. The parent gets back the summary and
//! nothing else: no transcript, no pages.
//!
//! ```text
//! parent (butler)                     child (researcher)
//!  ask_researcher{question} ─ gated ─▶ [system: its prompt, user: question]
//!                                       model ⇄ web_search / web_fetch
//!                                       … up to budget.max_steps rounds,
//!                                         budget.timeout_secs, max_cost_cents
//!  ◀──── summary + spent_cents ─────── final answer
//! ```
//!
//! What the child spent rides back beside its summary (or its error, once
//! it has spent anything), so the parent's run counts it toward its own
//! `max_cost_cents`.
//!
//! The child runs as one [`ToolJob`] of the parent's round, on its own
//! thread with its own runtime: it never touches the frame, and the
//! parent's stop reaches it through the round's [`ToolCancel`].
//!
//! While it runs, the user watches it in a sub-agent row under the
//! `ask_researcher` call ([`ChildRow`]): each search and fetch as it lands,
//! then the summary. The row is built from the responses a backend's own
//! subagent sends (spawned, a tool result per step, completed or failed),
//! [reported](JobCtx::report) over the round's channel, so the frame shows
//! and publishes it the way it does Claude's Task subagents. The parent's
//! model still gets the summary and nothing else.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use agentium_core::agent_spec::pricing::{self, Cost, Meter, Rate, Usage};
use agentium_core::agent_spec::provider::{self, Endpoint};
use agentium_core::agent_spec::web::{ASK_RESEARCHER, WEB_FETCH, WEB_SEARCH};
use agentium_core::agent_spec::{AgentSpec, Approval, Catalog, ToolSource};
use agentium_core::config::BackendProfile;
use async_openai::types::{ChatCompletionRequestMessage, CreateChatCompletionRequest};
use async_openai::Client;
use nostrdb::{Ndb, Transaction};
use serde_json::{json, Value};

use crate::backend::{finish_tool_call, message_to_api, openai_config, tool_to_api, SPENT_CENTS};
use crate::messages::{DaveApiResponse, ExecutedTool, SubagentInfo, SubagentStatus};
use crate::tool_round::{block_on_own_runtime, JobCtx, ToolCancel, ToolJob};
use crate::tools::{AppToolCall, AppToolResponse, PartialToolCall, Tool, ToolCall, ToolCalls};
use crate::web_tools::WebClient;
use crate::{Message, ToolResponse, ToolResponses};

/// What the child is told when its step budget is spent: answer from what
/// it has. It gets one more request, with no tools.
const OUT_OF_STEPS: &str = "You have used your step budget. Answer the question now with \
                            what you found, and say what you could not establish.";

/// A spec's sub-agent, resolved: everything a question needs to be
/// answered, checked once when the parent's tools are built.
pub(crate) struct SubAgent {
    /// The child spec: its prompt and budget.
    spec: AgentSpec,
    /// Where its model requests go.
    endpoint: Endpoint,
    /// Its tools and their approvals: the web tools.
    catalog: Catalog,
    /// The catalog as the model sees it.
    model_tools: HashMap<String, Tool>,
    /// The SearXNG instance its searches go to.
    search_url: Option<String>,
}

/// Why a spec's sub-agent can't be set up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SubAgentError {
    /// No spec has the slug.
    #[error("no agent spec {0:?} to call as a sub-agent")]
    UnknownSpec(String),
    /// The child spec isn't fit to be one.
    #[error(transparent)]
    Spec(#[from] agentium_core::agent_spec::SpecError),
    /// The child's tools can't be built.
    #[error(transparent)]
    Catalog(#[from] agentium_core::agent_spec::CatalogError),
    /// The child's model has no endpoint.
    #[error(transparent)]
    Provider(#[from] agentium_core::agent_spec::provider::ProviderError),
}

impl SubAgent {
    /// The sub-agent a parent's `SubAgent { spec: slug }` toolset names:
    /// the spec `find` has for `slug`, its model resolved against the
    /// account's `profiles` and the process `env` as any spec's is
    /// ([`provider::resolve`](agentium_core::agent_spec::provider::resolve)).
    pub(crate) fn resolve(
        slug: &str,
        find: impl FnOnce(&str) -> Option<AgentSpec>,
        profiles: &BTreeMap<String, BackendProfile>,
        env: impl Fn(&str) -> Option<String>,
        search_url: Option<String>,
    ) -> Result<Self, SubAgentError> {
        let spec = find(slug).ok_or_else(|| SubAgentError::UnknownSpec(slug.to_owned()))?;
        let endpoint = provider::resolve(&spec.model, profiles, env)?;
        Self::new(spec, endpoint, search_url)
    }

    /// The sub-agent `spec`, answering on `endpoint` (its model, resolved),
    /// searching `search_url`. Fails if the spec holds private data or calls
    /// a sub-agent of its own.
    pub(crate) fn new(
        spec: AgentSpec,
        endpoint: Endpoint,
        search_url: Option<String>,
    ) -> Result<Self, SubAgentError> {
        spec.check_sub_agent()?;
        if spec.budget.max_cost_cents.is_some() && pricing::rate(&endpoint.model).is_none() {
            tracing::warn!(
                "agent spec {}: no price for model {:?}, so its max_cost_cents isn't enforced",
                spec.slug,
                endpoint.model
            );
        }
        // A sub-agent's tools never come from the registry: it holds no app
        // toolset, so it is offered nothing of the host's.
        let catalog = Catalog::build(&spec, &[])?;
        let model_tools = catalog.model_tools();
        Ok(Self {
            spec,
            endpoint,
            catalog,
            model_tools,
            search_url,
        })
    }

    /// The child spec's slug.
    pub(crate) fn slug(&self) -> &str {
        &self.spec.slug
    }

    /// The job that answers `question`: the child loop, on the round's
    /// thread. `ndb` renders the child's tool answers for its model.
    ///
    /// The run shows as a sub-agent row under the call ([`ChildRow`]).
    pub(crate) fn job(self: &Arc<Self>, question: String, ndb: Ndb) -> ToolJob {
        let agent = Arc::clone(self);
        Box::new(move |ctx| {
            let row = ChildRow::start(ctx, agent.slug(), &question);
            let Answered { result, spent } = agent.answer(&question, &ndb, ctx.cancel(), &row);
            row.finish(&result);
            let mut value = match result {
                Ok(summary) => json!({ "researcher": agent.slug(), "summary": summary }),
                // A child that spent nothing failed for free: a plain error.
                Err(err) if spent.is_zero() => return Err(err),
                // One that spent something still reports it, for the
                // parent's budget.
                Err(err) => json!({ "researcher": agent.slug(), "error": err }),
            };
            if !spent.is_zero() {
                value[SPENT_CENTS] = json!(spent.cents());
            }
            Ok(ToolResponses::App(AppToolResponse {
                tool: ASK_RESEARCHER.to_owned(),
                value,
            }))
        })
    }

    /// Run the child loop on a runtime of its own until it answers, showing
    /// each step on `row`.
    fn answer(&self, question: &str, ndb: &Ndb, cancel: &ToolCancel, row: &ChildRow) -> Answered {
        let meter = Meter::default();
        let result = block_on_own_runtime(async {
            let mut model = OpenAiChild {
                client: Client::with_config(openai_config(&self.endpoint)),
                model: self.endpoint.model.clone(),
                rate: pricing::rate(&self.endpoint.model),
                meter: &meter,
                api_tools: self.model_tools.values().map(tool_to_api).collect(),
                tools: &self.model_tools,
                ndb,
            };
            let web = WebClient::new(self.search_url.clone());
            let mut tools = |call: AppToolCall| {
                let web = &web;
                async move { web.call(&call).await }
            };
            let mut steps = |step: ChildStep<'_>| row.step(step);
            let run = run_child(
                &self.spec,
                &self.catalog,
                question,
                &mut model,
                &mut tools,
                &meter,
                &mut steps,
            );
            bounded(run, self.spec.budget.timeout_secs, cancel).await
        });
        Answered {
            result,
            spent: meter.spent(),
        }
    }
}

/// How the child loop ended, and what it spent getting there.
struct Answered {
    /// Its summary, or why there is none.
    result: Result<String, String>,
    /// What its model requests cost; nothing on a model the price table
    /// doesn't list.
    spent: Cost,
}

/// `run`, stopped by `cancel` or after `timeout_secs`.
async fn bounded(
    run: impl Future<Output = Result<String, String>>,
    timeout_secs: Option<u64>,
    cancel: &ToolCancel,
) -> Result<String, String> {
    let timeout = async {
        match timeout_secs {
            Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        result = run => result,
        () = cancel.cancelled() => Err("the research was stopped".to_owned()),
        () = timeout => Err(format!(
            "the researcher ran past its {}s budget without answering",
            timeout_secs.unwrap_or_default()
        )),
    }
}

/// The sub-agent row a child's run builds under the parent's
/// `ask_researcher` call, keyed by that call's id. It reports the responses
/// a backend's own subagent would send (see the module docs): spawned when
/// the run starts, a tool result per step, then completed with the summary
/// or failed with why. The frame applies them, which shows the row and
/// publishes it for remote observers.
///
/// A run that never finishes (its round was stopped first, or it panicked)
/// leaves the row running; the frame fails it when the round ends.
pub(crate) struct ChildRow<'a> {
    ctx: &'a JobCtx,
}

/// The longest a question shows as its row's description: the row is one
/// line, and the whole question is in the parent's call above it.
const ROW_QUESTION_CHARS: usize = 80;

impl<'a> ChildRow<'a> {
    /// Show the row for sub-agent `slug` asked `question`, running.
    pub(crate) fn start(ctx: &'a JobCtx, slug: &str, question: &str) -> Self {
        ctx.report(DaveApiResponse::SubagentSpawned(SubagentInfo {
            task_id: ctx.call_id().to_owned(),
            description: headline(question, ROW_QUESTION_CHARS),
            subagent_type: slug.to_owned(),
            status: SubagentStatus::Running,
            output: String::new(),
            max_output_size: 4000,
            tool_results: Vec::new(),
            background: false,
            agent_id: None,
            transcript: Default::default(),
        }));
        Self { ctx }
    }

    /// Add a finished step to the row: what the tool was asked, and whether
    /// it failed.
    pub(crate) fn step(&self, step: ChildStep<'_>) {
        self.ctx.report(DaveApiResponse::ToolResult(ExecutedTool {
            parent_task_id: Some(self.ctx.call_id().to_owned()),
            is_error: matches!(step.response, ToolResponses::Error(_)),
            ..ExecutedTool::new(step.call.tool_name().to_owned(), step_subject(step.call))
        }));
    }

    /// End the row: completed with the summary, or failed with why.
    pub(crate) fn finish(&self, result: &Result<String, String>) {
        let task_id = self.ctx.call_id().to_owned();
        self.ctx.report(match result {
            Ok(summary) => DaveApiResponse::SubagentCompleted {
                task_id,
                result: summary.clone(),
            },
            Err(error) => DaveApiResponse::SubagentFailed {
                task_id,
                error: error.clone(),
            },
        });
    }
}

/// `text`'s first line, cut to `max` chars with an ellipsis.
fn headline(text: &str, max: usize) -> String {
    let line = text.trim().lines().next().unwrap_or_default();
    match line.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// What a step was asked, as its row's summary: a search's query, a fetch's
/// URL, or the arguments of anything else.
fn step_subject(call: &ToolCalls) -> String {
    let ToolCalls::App(app) = call else {
        return call.arguments();
    };
    let key = match app.name.as_str() {
        WEB_SEARCH => "query",
        WEB_FETCH => "url",
        _ => return app.args.to_string(),
    };
    match app.args.get(key).and_then(Value::as_str) {
        Some(subject) => subject.to_owned(),
        None => app.args.to_string(),
    }
}

/// One finished step of the child's: a tool call and its answer.
pub(crate) struct ChildStep<'a> {
    pub(crate) call: &'a ToolCalls,
    pub(crate) response: &'a ToolResponses,
}

/// One reply of the child's model.
#[derive(Debug)]
pub(crate) enum ChildReply {
    /// It answered: the summary.
    Answer(String),
    /// It called tools.
    Calls(Vec<ToolCall>),
}

/// The child's model: its chat so far in, one reply out.
pub(crate) trait ChildModel {
    /// The reply to `chat`, offered the child's tools when `with_tools`.
    fn reply(
        &mut self,
        chat: &[Message],
        with_tools: bool,
    ) -> impl Future<Output = Result<ChildReply, String>>;
}

/// Runs a web tool call: its answer, or an error for the model.
pub(crate) trait ChildTools {
    fn run(&mut self, call: AppToolCall) -> impl Future<Output = Result<Value, String>>;
}

impl<F, Fut> ChildTools for F
where
    F: FnMut(AppToolCall) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    fn run(&mut self, call: AppToolCall) -> impl Future<Output = Result<Value, String>> {
        self(call)
    }
}

/// The child loop: ask `model` the `question` under `spec`'s prompt, run the
/// tools it calls as `catalog` allows, and answer what it finally says.
///
/// After `spec.budget.max_steps` rounds of tool calls the model is told to
/// answer from what it has and asked once more with no tools. Once `meter`
/// (what the model's replies cost) reaches `spec.budget.max_cost_cents`, a
/// reply's tool calls aren't run and the child fails. A call of a gated
/// tool answers an error: there is nobody in a sub-agent to ask.
///
/// Each call, once answered, goes to `steps`, for the parent's row.
pub(crate) async fn run_child(
    spec: &AgentSpec,
    catalog: &Catalog,
    question: &str,
    model: &mut impl ChildModel,
    tools: &mut impl ChildTools,
    meter: &Meter,
    steps: &mut impl FnMut(ChildStep<'_>),
) -> Result<String, String> {
    let mut chat = vec![
        Message::System(spec.prompt.as_str().into()),
        Message::User(question.into()),
    ];
    for _ in 0..spec.budget.max_steps {
        let calls = match model.reply(&chat, true).await? {
            ChildReply::Answer(answer) => return answered(answer),
            ChildReply::Calls(calls) => calls,
        };
        if let Some(cap) = spec.budget.max_cost_cents {
            let spent = meter.spent();
            if spent.reached(cap) {
                let cap = Cost::from_cents(f64::from(cap));
                return Err(format!(
                    "the researcher spent {spent}, its budget of {cap}, without answering"
                ));
            }
        }
        chat.push(Message::ToolCalls(calls.clone()));
        for call in calls {
            let response = run_call(catalog, call.calls(), tools).await;
            steps(ChildStep {
                call: call.calls(),
                response: &response,
            });
            chat.push(Message::ToolResponse(ToolResponse::new(
                call.id().to_owned(),
                response,
            )));
        }
    }
    chat.push(Message::User(OUT_OF_STEPS.into()));
    match model.reply(&chat, false).await? {
        ChildReply::Answer(answer) => answered(answer),
        ChildReply::Calls(_) => Err(format!(
            "the researcher used its {} steps without answering",
            spec.budget.max_steps
        )),
    }
}

/// The child's final text, which must say something.
fn answered(answer: String) -> Result<String, String> {
    if answer.trim().is_empty() {
        return Err("the researcher answered nothing".to_owned());
    }
    Ok(answer)
}

/// Answer one of the child's calls: a web tool its catalog runs unasked
/// goes to `tools`; anything else answers an error.
async fn run_call(
    catalog: &Catalog,
    call: &ToolCalls,
    tools: &mut impl ChildTools,
) -> ToolResponses {
    let app = match call {
        ToolCalls::App(app) => app,
        ToolCalls::Invalid(invalid) => return ToolResponses::Error(invalid.error.clone()),
        other => {
            return ToolResponses::Error(format!("'{}' isn't one of your tools", other.tool_name()))
        }
    };
    match catalog.get(&app.name) {
        Some(tool) if tool.source == ToolSource::Web && tool.approval == Approval::Auto => {
            match tools.run(app.clone()).await {
                Ok(value) => ToolResponses::App(AppToolResponse {
                    tool: app.name.clone(),
                    value,
                }),
                Err(err) => ToolResponses::Error(err),
            }
        }
        Some(_) => ToolResponses::Error(format!(
            "'{}' needs the user's approval, which a sub-agent can't ask for",
            app.name
        )),
        None => ToolResponses::Error(format!("'{}' isn't one of your tools", app.name)),
    }
}

/// The child's model over an OpenAI-compatible endpoint, one non-streaming
/// request per reply: nobody watches the child's tokens.
struct OpenAiChild<'a> {
    client: Client<async_openai::config::OpenAIConfig>,
    model: String,
    /// Its model's price, when the table lists it.
    rate: Option<Rate>,
    /// What each reply cost is counted on.
    meter: &'a Meter,
    /// The tools as the request carries them.
    api_tools: Vec<async_openai::types::ChatCompletionTool>,
    /// The tools a reply's calls are parsed against.
    tools: &'a HashMap<String, Tool>,
    ndb: &'a Ndb,
}

impl ChildModel for OpenAiChild<'_> {
    async fn reply(&mut self, chat: &[Message], with_tools: bool) -> Result<ChildReply, String> {
        let messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(self.ndb).map_err(|err| format!("db: {err}"))?;
            chat.iter()
                .filter_map(|m| message_to_api(m, &txn, self.ndb))
                .collect()
        };
        let request = CreateChatCompletionRequest {
            model: self.model.clone(),
            messages,
            tools: with_tools.then(|| self.api_tools.clone()),
            ..Default::default()
        };
        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|err| format!("the researcher's model failed: {err}"))?;
        if let (Some(rate), Some(usage)) = (self.rate, &response.usage) {
            self.meter.add(rate.cost(Usage {
                input_tokens: u64::from(usage.prompt_tokens),
                output_tokens: u64::from(usage.completion_tokens),
            }));
        }
        let message = response
            .choices
            .into_iter()
            .next()
            .ok_or("the researcher's model sent no reply")?
            .message;
        let calls: Vec<ToolCall> = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|call| {
                let partial = PartialToolCall {
                    id: Some(call.id),
                    name: Some(call.function.name),
                    arguments: Some(call.function.arguments),
                };
                finish_tool_call(partial, self.tools)
            })
            .collect();
        if calls.is_empty() {
            return Ok(ChildReply::Answer(message.content.unwrap_or_default()));
        }
        Ok(ChildReply::Calls(calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fake_openai::FakeOpenAi;
    use agentium_core::agent_spec::web::{WEB_FETCH, WEB_SEARCH};
    use std::collections::VecDeque;

    const RESEARCHER: &str =
        include_str!("../../agentium-core/src/agent_spec/builtin/researcher.json");

    fn researcher() -> AgentSpec {
        AgentSpec::from_json(RESEARCHER).unwrap()
    }

    /// A model that replies from a script and records what it was sent.
    struct Scripted {
        replies: VecDeque<ChildReply>,
        tools: HashMap<String, Tool>,
        /// Each request's chat length and whether tools were offered.
        requests: Vec<(usize, bool)>,
        /// The last chat it saw, as text rows.
        last_chat: Vec<String>,
        /// What each reply costs, counted on `meter`.
        cost_per_reply: Cost,
        meter: Arc<Meter>,
    }

    impl Scripted {
        fn new(catalog: &Catalog, replies: Vec<ChildReply>) -> Self {
            Self {
                replies: replies.into(),
                tools: catalog.model_tools(),
                requests: Vec::new(),
                last_chat: Vec::new(),
                cost_per_reply: Cost::ZERO,
                meter: Arc::default(),
            }
        }

        /// A call of `name` with `args`, parsed as the real model's would be.
        fn call(&self, id: &str, name: &str, args: Value) -> ToolCall {
            finish_tool_call(
                PartialToolCall {
                    id: Some(id.into()),
                    name: Some(name.into()),
                    arguments: Some(args.to_string()),
                },
                &self.tools,
            )
            .unwrap()
        }
    }

    impl ChildModel for Scripted {
        async fn reply(
            &mut self,
            chat: &[Message],
            with_tools: bool,
        ) -> Result<ChildReply, String> {
            self.requests.push((chat.len(), with_tools));
            self.last_chat = chat.iter().map(row).collect();
            self.meter.add(self.cost_per_reply);
            self.replies.pop_front().ok_or("script ran out".to_owned())
        }
    }

    fn row(message: &Message) -> String {
        match message {
            Message::System(m) => format!("system: {}", m.text()),
            Message::User(m) => format!("user: {}", m.text),
            Message::ToolCalls(calls) => format!("calls: {}", calls.len()),
            Message::ToolResponse(r) => match r.responses() {
                ToolResponses::App(app) => format!("{}: {}", app.tool, app.value),
                ToolResponses::Error(err) => format!("error: {err}"),
                other => format!("{other:?}"),
            },
            other => format!("{other:?}"),
        }
    }

    /// Web tools that answer from a fixed table and record each call.
    fn web(
        calls: &mut Vec<String>,
    ) -> impl FnMut(AppToolCall) -> std::future::Ready<Result<Value, String>> + '_ {
        move |call| {
            calls.push(format!("{} {}", call.name, call.args));
            std::future::ready(match call.name.as_str() {
                WEB_SEARCH => Ok(json!({ "results": [{ "url": "https://acme.example" }] })),
                WEB_FETCH => Ok(json!({ "text": "Acme: $9/mo" })),
                _ => Err("no".to_owned()),
            })
        }
    }

    /// The child is asked the question under its own prompt, runs the web
    /// tools it calls, and its last message is the summary.
    #[tokio::test]
    async fn the_child_searches_fetches_and_answers() {
        let spec = researcher();
        let catalog = Catalog::build(&spec, &[]).unwrap();
        let mut model = Scripted::new(&catalog, vec![]);
        let search = model.call("1", WEB_SEARCH, json!({ "query": "idea journal apps" }));
        let fetch = model.call("2", WEB_FETCH, json!({ "url": "https://acme.example" }));
        model.replies = vec![
            ChildReply::Calls(vec![search]),
            ChildReply::Calls(vec![fetch]),
            ChildReply::Answer("Acme — pricing: $9/mo".into()),
        ]
        .into();
        let mut calls = Vec::new();
        let mut steps = Vec::new();
        let summary = run_child(
            &spec,
            &catalog,
            "Who sells idea journals?",
            &mut model,
            &mut web(&mut calls),
            &Meter::default(),
            &mut |step: ChildStep<'_>| {
                let failed = matches!(step.response, ToolResponses::Error(_));
                steps.push(format!(
                    "{} {} failed={failed}",
                    step.call.tool_name(),
                    step_subject(step.call)
                ));
            },
        )
        .await
        .unwrap();

        assert_eq!(summary, "Acme — pricing: $9/mo");
        assert_eq!(
            steps,
            [
                "web_search idea journal apps failed=false",
                "web_fetch https://acme.example failed=false"
            ],
            "each step is reported, in order, with what it was asked"
        );
        assert_eq!(
            calls,
            [
                r#"web_search {"query":"idea journal apps"}"#,
                r#"web_fetch {"url":"https://acme.example"}"#
            ]
        );
        assert_eq!(model.last_chat[0], format!("system: {}", spec.prompt));
        assert_eq!(model.last_chat[1], "user: Who sells idea journals?");
        assert_eq!(model.last_chat[5], r#"web_fetch: {"text":"Acme: $9/mo"}"#);
        assert!(model.requests.iter().all(|(_, tools)| *tools));
    }

    /// Out of steps, the child is told to answer, offered no tools, and
    /// what it says then is the summary. Still calling tools is an error.
    #[tokio::test]
    async fn out_of_steps_the_child_answers_with_what_it_has() {
        let mut spec = researcher();
        spec.budget.max_steps = 2;
        let catalog = Catalog::build(&spec, &[]).unwrap();
        let mut model = Scripted::new(&catalog, vec![]);
        let search = |id| model.call(id, WEB_SEARCH, json!({ "query": "q" }));
        let replies = vec![
            ChildReply::Calls(vec![search("1")]),
            ChildReply::Calls(vec![search("2")]),
            ChildReply::Answer("partial: Acme".into()),
        ];
        model.replies = replies.into();
        let mut calls = Vec::new();
        let summary = run_child(
            &spec,
            &catalog,
            "q",
            &mut model,
            &mut web(&mut calls),
            &Meter::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(summary, "partial: Acme");
        assert_eq!(calls.len(), 2);
        assert_eq!(model.requests.last(), Some(&(7, false)));
        assert_eq!(model.last_chat[6], format!("user: {OUT_OF_STEPS}"));

        let still_calling = model.call("3", WEB_SEARCH, json!({ "query": "q" }));
        model.replies = vec![
            ChildReply::Calls(vec![still_calling.clone()]),
            ChildReply::Calls(vec![still_calling.clone()]),
            ChildReply::Calls(vec![still_calling]),
        ]
        .into();
        let err = run_child(
            &spec,
            &catalog,
            "q",
            &mut model,
            &mut web(&mut calls),
            &Meter::default(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.contains("2 steps"), "{err}");
    }

    /// Once its replies have cost its budget, the child runs no more of
    /// the tools they call and fails, saying what it spent.
    #[tokio::test]
    async fn past_its_cost_budget_the_child_stops() {
        let mut spec = researcher();
        spec.budget.max_cost_cents = Some(2);
        let catalog = Catalog::build(&spec, &[]).unwrap();
        let mut model = Scripted::new(&catalog, vec![]);
        model.cost_per_reply = Cost::from_cents(1.0);
        let search = |id| model.call(id, WEB_SEARCH, json!({ "query": "q" }));
        model.replies = vec![
            ChildReply::Calls(vec![search("1")]),
            ChildReply::Calls(vec![search("2")]),
            ChildReply::Answer("never asked".into()),
        ]
        .into();
        let meter = Arc::clone(&model.meter);
        let mut calls = Vec::new();
        let err = run_child(
            &spec,
            &catalog,
            "q",
            &mut model,
            &mut web(&mut calls),
            &meter,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(calls.len(), 1, "the second reply's call isn't run");
        assert!(err.contains("$0.02"), "{err}");
    }

    /// A child whose spec doesn't widen its web tools can't run them: there
    /// is nobody to ask. A tool it wasn't offered doesn't run either.
    #[tokio::test]
    async fn a_gated_or_unknown_tool_answers_an_error_and_runs_nothing() {
        let mut spec = researcher();
        spec.policy = Default::default();
        let catalog = Catalog::build(&spec, &[]).unwrap();
        let mut model = Scripted::new(&catalog, vec![]);
        let fetch = model.call("1", WEB_FETCH, json!({ "url": "https://a.example" }));
        let board = model.call("2", "headway_show_board", json!({}));
        model.replies = vec![
            ChildReply::Calls(vec![fetch, board]),
            ChildReply::Answer("nothing found".into()),
        ]
        .into();
        let mut calls = Vec::new();
        run_child(
            &spec,
            &catalog,
            "q",
            &mut model,
            &mut web(&mut calls),
            &Meter::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(calls.is_empty(), "nothing ran: {calls:?}");
        assert!(
            model.last_chat[3].contains("can't ask"),
            "{}",
            model.last_chat[3]
        );
        assert!(
            model.last_chat[4].starts_with("error:"),
            "{}",
            model.last_chat[4]
        );
    }

    /// A row's description is the question's first line, cut short; the
    /// whole question is in the call above the row.
    #[test]
    fn a_rows_description_is_the_questions_headline() {
        assert_eq!(
            headline("  Who sells idea journals?\nAnd for how much?", 80),
            "Who sells idea journals?"
        );
        assert_eq!(headline("abcdef", 3), "abc…");
        assert_eq!(headline("abc", 3), "abc");
    }

    /// An empty answer is an error, not a blank summary for the parent.
    #[tokio::test]
    async fn an_empty_answer_is_an_error() {
        let spec = researcher();
        let catalog = Catalog::build(&spec, &[]).unwrap();
        let mut model = Scripted::new(&catalog, vec![ChildReply::Answer("  ".into())]);
        let mut calls = Vec::new();
        assert!(run_child(
            &spec,
            &catalog,
            "q",
            &mut model,
            &mut web(&mut calls),
            &Meter::default(),
            &mut |_| {}
        )
        .await
        .is_err());
    }

    /// A spec with private data can't be a sub-agent, whatever names it.
    #[test]
    fn a_spec_with_private_data_is_no_sub_agent() {
        let butler = AgentSpec::from_json(include_str!(
            "../../agentium-core/src/agent_spec/builtin/butler.json"
        ))
        .unwrap();
        let endpoint = Endpoint {
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api_key: None,
            model: "m".into(),
        };
        assert!(matches!(
            SubAgent::new(butler, endpoint, None),
            Err(SubAgentError::Spec(_))
        ));
    }

    /// A parent's sub-agent slug resolves to its spec and its model's
    /// endpoint; an unknown slug and a provider without its key are errors.
    #[test]
    fn a_sub_agent_resolves_by_slug() {
        let find = |slug: &str| (slug == "researcher").then(researcher);
        let key = |var: &str| (var == "ANTHROPIC_API_KEY").then(|| "sk-ant".to_owned());
        let agent = SubAgent::resolve("researcher", find, &BTreeMap::new(), key, None).unwrap();
        assert_eq!(agent.slug(), "researcher");
        assert_eq!(agent.endpoint.base_url, "https://api.anthropic.com/v1");

        assert!(matches!(
            SubAgent::resolve("scout", find, &BTreeMap::new(), key, None),
            Err(SubAgentError::UnknownSpec(_))
        ));
        assert!(matches!(
            SubAgent::resolve("researcher", find, &BTreeMap::new(), |_| None, None),
            Err(SubAgentError::Provider(_))
        ));
    }

    /// An OpenAI-compatible endpoint on a local port that answers one chat
    /// completion with `answer` and hands back the request it got.
    fn one_completion(answer: &str) -> FakeOpenAi {
        one_completion_reporting(answer, None)
    }

    /// [`one_completion`], reporting `usage` when given.
    fn one_completion_reporting(answer: &str, usage: Option<Value>) -> FakeOpenAi {
        let mut body = json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 0,
            "model": "tiny",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": answer }
            }]
        });
        if let Some(usage) = usage {
            body["usage"] = usage;
        }
        FakeOpenAi::completion(body)
    }

    /// The job a parent's round runs: one real request to the child's own
    /// endpoint, carrying its prompt, the question and its web tools and
    /// nothing of the parent's, and the summary comes back as the call's
    /// answer.
    #[test]
    fn the_job_asks_the_childs_endpoint_and_answers_its_summary() {
        let server = one_completion("Acme: $9/mo, 4 people, seed-funded.");
        let agent = Arc::new(SubAgent::new(researcher(), server.endpoint("tiny"), None).unwrap());
        let ndb_testing::TempNdb { dir: _dir, ndb } = ndb_testing::TempNdb::new();
        let job = agent.job("Who sells idea journals?".into(), ndb);
        let answer = job(&JobCtx::detached("c1", ToolCancel::default())).unwrap();

        let ToolResponses::App(app) = answer else {
            panic!("an app answer: {answer:?}");
        };
        assert_eq!(app.tool, ASK_RESEARCHER);
        assert_eq!(
            app.value,
            json!({ "researcher": "researcher", "summary": "Acme: $9/mo, 4 people, seed-funded." })
        );
        let request = server.request();
        assert!(request.contains("Who sells idea journals?"), "{request}");
        assert!(request.contains("fixed competitor") || request.contains("team size"));
        assert!(request.contains("\"web_search\"") && request.contains("\"web_fetch\""));
        assert!(
            !request.contains("headway"),
            "nothing of the parent's: {request}"
        );
    }

    /// What the child's replies cost rides back beside its summary, for
    /// the parent's budget.
    #[test]
    fn the_jobs_answer_reports_what_the_child_spent() {
        // Haiku 5.5: 10 cents per million in, 50 out.
        let usage = json!({ "prompt_tokens": 100_000, "completion_tokens": 10_000,
                            "total_tokens": 110_000 });
        let server = one_completion_reporting("Acme.", Some(usage));
        let endpoint = server.endpoint("claude-haiku-5-5");
        let agent = Arc::new(SubAgent::new(researcher(), endpoint, None).unwrap());
        let ndb_testing::TempNdb { dir: _dir, ndb } = ndb_testing::TempNdb::new();
        let ctx = JobCtx::detached("c1", ToolCancel::default());
        let answer = agent.job("q".into(), ndb)(&ctx).unwrap();
        server.request();

        let ToolResponses::App(app) = answer else {
            panic!("an app answer: {answer:?}");
        };
        assert_eq!(app.value["summary"], "Acme.");
        assert_eq!(app.value[SPENT_CENTS], 1.5);
    }

    /// A stop from the parent ends a child that is still running.
    #[tokio::test]
    async fn a_stop_ends_the_child() {
        let cancel = ToolCancel::default();
        cancel.cancel();
        let err = bounded(std::future::pending(), None, &cancel)
            .await
            .unwrap_err();
        assert!(err.contains("stopped"), "{err}");

        let err = bounded(std::future::pending(), Some(0), &ToolCancel::default())
            .await
            .unwrap_err();
        assert!(err.contains("budget"), "{err}");
    }
}
