use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessage,
        ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestSystemMessageContent,
        ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
        ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionTool, ChatCompletionToolType, CreateChatCompletionRequest, FunctionCall,
        FunctionObject,
    },
    Client,
};
use chrono::{DateTime, Duration, Local};
use egui_wgpu::RenderState;
use futures::StreamExt;
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::AppContext;
use notedeck_ui::icons::search_icon;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

pub use avatar::DaveAvatar;
pub use config::ModelConfig;
pub use quaternion::Quaternion;
pub use vec3::Vec3;

mod avatar;
mod config;
mod quaternion;
mod vec3;

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant(String),
    System(String),
    ToolCalls(Vec<ToolCall>),
    ToolResponse(ToolResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    context: QueryContext,
    notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Query(QueryResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    id: String,
    typ: ToolResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    typ: ToolCalls,
}

#[derive(Default, Debug, Clone)]
pub struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UnknownToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl UnknownToolCall {
    pub fn parse(&self, tools: &HashMap<String, Tool>) -> Result<ToolCall, ToolCallError> {
        let Some(tool) = tools.get(&self.name) else {
            return Err(ToolCallError::NotFound(self.name.to_owned()));
        };

        let parsed_args = (tool.parse_call)(&self.arguments)?;
        Ok(ToolCall {
            id: self.id.clone(),
            typ: parsed_args,
        })
    }
}

impl PartialToolCall {
    pub fn complete(&self) -> Option<UnknownToolCall> {
        Some(UnknownToolCall {
            id: self.id.clone()?,
            name: self.name.clone()?,
            arguments: self.arguments.clone()?,
        })
    }
}

impl ToolCall {
    pub fn to_api(&self) -> ChatCompletionMessageToolCall {
        ChatCompletionMessageToolCall {
            id: self.id.clone(),
            r#type: ChatCompletionToolType::Function,
            function: self.typ.to_api(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolCalls {
    Query(QueryCall),
}

impl ToolCalls {
    pub fn to_api(&self) -> FunctionCall {
        FunctionCall {
            name: self.name().to_owned(),
            arguments: self.arguments(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Query(_) => "search",
        }
    }

    fn arguments(&self) -> String {
        match self {
            Self::Query(search) => serde_json::to_string(search).unwrap(),
        }
    }
}

pub enum DaveResponse {
    ToolCalls(Vec<ToolCall>),
    Token(String),
}

impl Message {
    pub fn to_api_msg(&self, txn: &Transaction, ndb: &Ndb) -> ChatCompletionRequestMessage {
        match self {
            Message::User(msg) => {
                ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                    name: None,
                    content: ChatCompletionRequestUserMessageContent::Text(msg.clone()),
                })
            }

            Message::Assistant(msg) => {
                ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                    content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                        msg.clone(),
                    )),
                    ..Default::default()
                })
            }

            Message::System(msg) => {
                ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(msg.clone()),
                    ..Default::default()
                })
            }

            Message::ToolCalls(calls) => {
                ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                    tool_calls: Some(calls.iter().map(|c| c.to_api()).collect()),
                    ..Default::default()
                })
            }

            Message::ToolResponse(resp) => {
                let tool_response = format_tool_response_for_ai(txn, ndb, &resp.typ);

                ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
                    tool_call_id: resp.id.clone(),
                    content: ChatCompletionRequestToolMessageContent::Text(tool_response),
                })
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct SimpleNote {
    pubkey: String,
    name: String,
    content: String,
    created_at: String,
    note_kind: String, // todo: add replying to
}

fn note_kind_desc(kind: u64) -> String {
    match kind {
        1 => "microblog".to_string(),
        0 => "profile".to_string(),
        _ => kind.to_string(),
    }
}

/// Take the result of a tool response and present it to the ai so that
/// it can interepret it and take further action
fn format_tool_response_for_ai(txn: &Transaction, ndb: &Ndb, resp: &ToolResponses) -> String {
    match resp {
        ToolResponses::Query(search_r) => {
            let simple_notes: Vec<SimpleNote> = search_r
                .notes
                .iter()
                .filter_map(|nkey| {
                    let Ok(note) = ndb.get_note_by_key(txn, NoteKey::new(*nkey)) else {
                        return None;
                    };

                    let name = ndb
                        .get_profile_by_pubkey(txn, note.pubkey())
                        .ok()
                        .and_then(|p| p.record().profile())
                        .and_then(|p| p.name().or_else(|| p.display_name()))
                        .unwrap_or("Anonymous")
                        .to_string();

                    let content = note.content().to_owned();
                    let pubkey = hex::encode(note.pubkey());
                    let note_kind = note_kind_desc(note.kind() as u64);

                    let created_at = {
                        let datetime =
                            DateTime::from_timestamp(note.created_at() as i64, 0).unwrap();
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    };

                    Some(SimpleNote {
                        pubkey,
                        name,
                        content,
                        created_at,
                        note_kind,
                    })
                })
                .collect();

            serde_json::to_string(&json!({"search_results": simple_notes})).unwrap()
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum QueryContext {
    Home,
    Profile,
    Any,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct QueryCall {
    context: Option<QueryContext>,
    limit: Option<u64>,
    since: Option<u64>,
    kind: Option<u64>,
    until: Option<u64>,
    author: Option<String>,
    search: Option<String>,
}

impl QueryCall {
    pub fn to_filter(&self) -> nostrdb::Filter {
        let mut filter = nostrdb::Filter::new()
            .limit(self.limit())
            .kinds([self.kind.unwrap_or(1)]);

        if let Some(search) = &self.search {
            filter = filter.search(search);
        }

        if let Some(until) = self.until {
            filter = filter.until(until);
        }

        if let Some(since) = self.since {
            filter = filter.since(since);
        }

        filter.build()
    }

    fn limit(&self) -> u64 {
        self.limit.unwrap_or(10)
    }

    fn context(&self) -> QueryContext {
        self.context.clone().unwrap_or(QueryContext::Any)
    }

    pub fn execute(&self, txn: &Transaction, ndb: &Ndb) -> QueryResponse {
        let notes = {
            if let Ok(results) = ndb.query(txn, &[self.to_filter()], self.limit() as i32) {
                results.into_iter().map(|r| r.note_key.as_u64()).collect()
            } else {
                vec![]
            }
        };
        QueryResponse {
            context: self.context.clone().unwrap_or(QueryContext::Any),
            notes,
        }
    }

    pub fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        match serde_json::from_str::<QueryCall>(args) {
            Ok(call) => Ok(ToolCalls::Query(call)),
            Err(e) => Err(ToolCallError::ArgParseFailure(format!(
                "Failed to parse args: '{}', error: {}",
                args, e
            ))),
        }
    }
}

pub struct Dave {
    chat: Vec<Message>,
    /// A 3d representation of dave.
    avatar: Option<DaveAvatar>,
    input: String,
    pubkey: String,
    tools: Arc<HashMap<String, Tool>>,
    client: async_openai::Client<OpenAIConfig>,
    incoming_tokens: Option<Receiver<DaveResponse>>,
    model_config: ModelConfig,
}

impl Dave {
    pub fn avatar_mut(&mut self) -> Option<&mut DaveAvatar> {
        self.avatar.as_mut()
    }

    pub fn new(render_state: Option<&RenderState>) -> Self {
        let model_config = ModelConfig::default();
        //let model_config = ModelConfig::ollama();
        let client = Client::with_config(model_config.to_api());

        let input = "".to_string();
        let pubkey = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".to_string();
        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in dave_tools() {
            tools.insert(tool.name.to_string(), tool);
        }

        let now = Local::now();
        let yesterday = now - Duration::hours(24);
        let date = now.format("%Y-%m-%d %H:%M:%S");
        let timestamp = now.timestamp();
        let yesterday_timestamp = yesterday.timestamp();

        let system_prompt = Message::System(format!(
            r#"
You are an AI agent for the nostr protocol called Dave, created by Damus. nostr is a decentralized social media and internet communications protocol. You are embedded in a nostr browser called 'Damus Notedeck'. The returned note results are formatted into clickable note widgets. This happens when a nostr-uri is detected (ie: nostr:neventnevent1y4mvl8046gjsvdvztnp7jvs7w29pxcmkyj5p58m7t0dmjc8qddzsje0zmj). When referencing notes, ensure that this uri is included in the response so notes can be rendered inline.

- The current date is {date} ({timestamp} unix timestamp if needed for queries).

- Yesterday (-24hrs) was {yesterday_timestamp}. You can use this in combination with `since` queries for pulling notes for summarizing notes the user might have missed while they were away.

- The current users pubkey is {pubkey}

# Response Guidelines

- Use plaintext formatting for all responses.
- Don't use markdown links
- Include nostr:nevent references when referring to notes
- When a user asks for a digest instead of specific query terms, make sure to include both `since` and `until` to pull notes for the correct range.
- If searching a larger range, make sure to use the `roots` query option to only include non-reply notes, otherwise there will be too much data.
"#
        ));

        Dave {
            client,
            pubkey,
            avatar,
            incoming_tokens: None,
            tools: Arc::new(tools),
            input,
            model_config,
            chat: vec![system_prompt],
        }
    }

    fn render(&mut self, app_ctx: &AppContext, ui: &mut egui::Ui) {
        let mut should_send = false;
        if let Some(recvr) = &self.incoming_tokens {
            while let Ok(res) = recvr.try_recv() {
                if let Some(avatar) = &mut self.avatar {
                    avatar.random_nudge();
                }
                match res {
                    DaveResponse::Token(token) => match self.chat.last_mut() {
                        Some(Message::Assistant(msg)) => *msg = msg.clone() + &token,
                        Some(_) => self.chat.push(Message::Assistant(token)),
                        None => {}
                    },

                    DaveResponse::ToolCalls(toolcalls) => {
                        tracing::info!("got tool calls: {:?}", toolcalls);
                        self.chat.push(Message::ToolCalls(toolcalls.clone()));

                        let txn = Transaction::new(app_ctx.ndb).unwrap();
                        for call in &toolcalls {
                            // execute toolcall
                            match &call.typ {
                                ToolCalls::Query(search_call) => {
                                    let resp = search_call.execute(&txn, app_ctx.ndb);
                                    self.chat.push(Message::ToolResponse(ToolResponse {
                                        id: call.id.clone(),
                                        typ: ToolResponses::Query(resp),
                                    }))
                                }
                            }
                        }

                        should_send = true;
                    }
                }
            }
        }

        // Scroll area for chat messages
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 50,
                right: 50,
                top: 50,
                bottom: 50,
            })
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            self.render_chat(ui);

                            self.inputbox(app_ctx, ui);
                        })
                    });
            });

        /*
        // he lives in the sidebar now
        if let Some(avatar) = &mut self.avatar {
            let avatar_size = Vec2::splat(300.0);
            let pos = Vec2::splat(100.0).to_pos2();
            let pos = Rect::from_min_max(pos, pos + avatar_size);
            avatar.render(pos, ui);
        }
        */

        // send again
        if should_send {
            self.send_user_message(app_ctx, ui.ctx());
        }
    }

    fn render_chat(&self, ui: &mut egui::Ui) {
        for message in &self.chat {
            match message {
                Message::User(msg) => self.user_chat(msg, ui),
                Message::Assistant(msg) => self.assistant_chat(msg, ui),
                Message::ToolResponse(msg) => Self::tool_response_ui(msg, ui),
                Message::System(_msg) => {
                    // system prompt is not rendered. Maybe we could
                    // have a debug option to show this
                }
                Message::ToolCalls(toolcalls) => {
                    Self::tool_call_ui(toolcalls, ui);
                }
            }
        }
    }

    fn tool_response_ui(_tool_response: &ToolResponse, _ui: &mut egui::Ui) {
        //ui.label(format!("tool_response: {:?}", tool_response));
    }

    fn search_call_ui(query_call: &QueryCall, ui: &mut egui::Ui) {
        ui.add(search_icon(16.0, 16.0));
        ui.add_space(8.0);
        let context = match query_call.context() {
            QueryContext::Profile => "profile ",
            QueryContext::Any => "",
            QueryContext::Home => "home ",
        };

        //TODO: fix this to support any query
        if let Some(search) = &query_call.search {
            ui.label(format!("Querying {context}for '{search}'"));
        } else {
            ui.label(format!("Querying {:?}", &query_call));
        }
    }

    fn tool_call_ui(toolcalls: &[ToolCall], ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            for call in toolcalls {
                match &call.typ {
                    ToolCalls::Query(search_call) => {
                        ui.horizontal(|ui| {
                            egui::Frame::new()
                                .inner_margin(10.0)
                                .corner_radius(10.0)
                                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                                .show(ui, |ui| {
                                    Self::search_call_ui(search_call, ui);
                                })
                        });
                    }
                }
            }
        });
    }

    fn inputbox(&mut self, app_ctx: &AppContext, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::multiline(&mut self.input));
            ui.add_space(8.0);
            if ui.button("Send").clicked() {
                self.chat.push(Message::User(self.input.clone()));
                self.send_user_message(app_ctx, ui.ctx());
                self.input.clear();
            }
        });
    }

    fn user_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            egui::Frame::new()
                .inner_margin(10.0)
                .corner_radius(10.0)
                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .show(ui, |ui| {
                    ui.label(msg);
                })
        });
    }

    fn assistant_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(msg).wrap_mode(egui::TextWrapMode::Wrap));
        });
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, ctx: &egui::Context) {
        let messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            self.chat
                .iter()
                .map(|c| c.to_api_msg(&txn, app_ctx.ndb))
                .collect()
        };
        tracing::debug!("sending messages, latest: {:?}", messages.last().unwrap());
        let pubkey = self.pubkey.clone();
        let ctx = ctx.clone();
        let client = self.client.clone();
        let tools = self.tools.clone();
        let model_name = self.model_config.model().to_owned();

        let (tx, rx) = mpsc::channel();
        self.incoming_tokens = Some(rx);

        tokio::spawn(async move {
            let mut token_stream = match client
                .chat()
                .create_stream(CreateChatCompletionRequest {
                    model: model_name,
                    stream: Some(true),
                    messages,
                    tools: Some(dave_tools().iter().map(|t| t.to_api()).collect()),
                    user: Some(pubkey),
                    ..Default::default()
                })
                .await
            {
                Err(err) => {
                    tracing::error!("openai chat error: {err}");
                    return;
                }

                Ok(stream) => stream,
            };

            let mut all_tool_calls: HashMap<u32, PartialToolCall> = HashMap::new();

            while let Some(token) = token_stream.next().await {
                let token = match token {
                    Ok(token) => token,
                    Err(err) => {
                        tracing::error!("failed to get token: {err}");
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
                                entry.id.get_or_insert(id.to_string());
                            }

                            if let Some(name) = tool.function.as_ref().and_then(|f| f.name.as_ref())
                            {
                                entry.name.get_or_insert(name.to_string());
                            }

                            if let Some(argchunk) =
                                tool.function.as_ref().and_then(|f| f.arguments.as_ref())
                            {
                                entry
                                    .arguments
                                    .get_or_insert_with(String::new)
                                    .push_str(argchunk);
                            }
                        }
                    }

                    if let Some(content) = &resp.content {
                        if let Err(err) = tx.send(DaveResponse::Token(content.to_owned())) {
                            tracing::error!("failed to send dave response token to ui: {err}");
                        }
                        ctx.request_repaint();
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
                        tracing::error!(
                            "failed to parse tool call {:?}: {:?}",
                            unknown_tool_call,
                            err,
                        );
                        // TODO: return error to user
                    }
                };
            }

            if !parsed_tool_calls.is_empty() {
                tx.send(DaveResponse::ToolCalls(parsed_tool_calls)).unwrap();
                ctx.request_repaint();
            }

            tracing::debug!("stream closed");
        });
    }
}

impl notedeck::App for Dave {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        /*
        self.app
            .frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        */

        //update_dave(self, ctx, ui.ctx());
        self.render(ctx, ui);
    }
}

#[derive(Debug, Clone)]
enum ArgType {
    String,
    Number,
    Enum(Vec<&'static str>),
}

impl ArgType {
    pub fn type_string(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Enum(_) => "string",
        }
    }
}

#[derive(Debug, Clone)]
struct ToolArg {
    typ: ArgType,
    name: &'static str,
    required: bool,
    description: &'static str,
    default: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct Tool {
    parse_call: fn(&str) -> Result<ToolCalls, ToolCallError>,
    name: &'static str,
    description: &'static str,
    arguments: Vec<ToolArg>,
}

impl Tool {
    pub fn to_function_object(&self) -> FunctionObject {
        let required_args = self
            .arguments
            .iter()
            .filter_map(|arg| {
                if arg.required {
                    Some(Value::String(arg.name.to_owned()))
                } else {
                    None
                }
            })
            .collect();

        let mut parameters: serde_json::Map<String, Value> = serde_json::Map::new();
        parameters.insert("type".to_string(), Value::String("object".to_string()));
        parameters.insert("required".to_string(), Value::Array(required_args));
        parameters.insert("additionalProperties".to_string(), Value::Bool(false));

        let mut properties: serde_json::Map<String, Value> = serde_json::Map::new();

        for arg in &self.arguments {
            let mut props: serde_json::Map<String, Value> = serde_json::Map::new();
            props.insert(
                "type".to_string(),
                Value::String(arg.typ.type_string().to_string()),
            );

            let description = if let Some(default) = &arg.default {
                format!("{} (Default: {default}))", arg.description)
            } else {
                arg.description.to_owned()
            };

            props.insert("description".to_string(), Value::String(description));
            if let ArgType::Enum(enums) = &arg.typ {
                props.insert(
                    "enum".to_string(),
                    Value::Array(
                        enums
                            .iter()
                            .map(|s| Value::String((*s).to_owned()))
                            .collect(),
                    ),
                );
            }

            properties.insert(arg.name.to_owned(), Value::Object(props));
        }

        parameters.insert("properties".to_string(), Value::Object(properties));

        FunctionObject {
            name: self.name.to_owned(),
            description: Some(self.description.to_owned()),
            strict: Some(false),
            parameters: Some(Value::Object(parameters)),
        }
    }

    pub fn to_api(&self) -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: self.to_function_object(),
        }
    }
}

fn query_tool() -> Tool {
    Tool {
        name: "query",
        parse_call: QueryCall::parse,
        description: "Note query functionality. Used for finding notes using full-text search terms, scoped by different contexts. You can use a combination of limit, since, and until to pull notes from any time range.",
        arguments: vec![
            ToolArg {
                name: "search",
                typ: ArgType::String,
                required: false,
                default: None,
                description: "A fulltext search query. Queries with multiple words will only return results with notes that have all of those words. Don't include filler words/symbols like 'and', punctuation, etc",
            },

            ToolArg {
                name: "limit",
                typ: ArgType::Number,
                required: true,
                default: Some(Value::Number(serde_json::Number::from_i128(10).unwrap())),
                description: "The number of results to return.",
            },

            ToolArg {
                name: "since",
                typ: ArgType::Number,
                required: false,
                default: None,
                description: "Only pull notes after this unix timestamp",
            },

            ToolArg {
                name: "until",
                typ: ArgType::Number,
                required: false,
                default: None,
                description: "Only pull notes up until this unix timestamp. Always include this when searching notes within some date range (yesterday, last week, etc).",
            },

            ToolArg {
                name: "kind",
                typ: ArgType::Number,
                required: false,
                default: Some(Value::Number(serde_json::Number::from_i128(1).unwrap())),
                description: r#"The kind of note. Kind list:
                - 0: profiles
                - 1: microblogs/\"tweets\"/posts
                - 6: reposts of kind 1 notes
                - 7: emoji reactions/likes
                - 9735: zaps (bitcoin micropayment receipts)
                - 30023: longform articles, blog posts, etc

                "#,
            },

            ToolArg {
                name: "author",
                typ: ArgType::String,
                required: false,
                default: None,
                description: "An author *pubkey* to constrain the query on. Can be used to search for notes from individual users. If unsure what pubkey to use, you can query for kind 0 profiles with the search argument.",
            },

            ToolArg {
                name: "context",
                typ: ArgType::Enum(vec!["home", "profile", "any"]),
                required: false,
                default: Some(Value::String("any".to_string())),
                description: "The context in which the search is occuring. valid options are 'home', 'profile', 'any'",
            }
        ]
    }
}

#[derive(Debug)]
pub enum ToolCallError {
    EmptyName,
    EmptyArgs,
    NotFound(String),
    ArgParseFailure(String),
}

fn dave_tools() -> Vec<Tool> {
    vec![query_tool()]
}
