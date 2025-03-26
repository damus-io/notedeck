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
use futures::StreamExt;
use nostrdb::{Ndb, NoteKey, Transaction};
use notedeck::AppContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use avatar::DaveAvatar;
use egui::{Rect, Vec2};
use egui_wgpu::RenderState;

mod avatar;

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant(String),
    System(String),
    ToolCalls(Vec<ToolCall>),
    ToolResponse(ToolResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    context: SearchContext,
    notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Search(SearchResponse),
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
    Search(SearchCall),
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
            Self::Search(_) => "search",
        }
    }

    fn arguments(&self) -> String {
        match self {
            Self::Search(search) => serde_json::to_string(search).unwrap(),
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
        ToolResponses::Search(search_r) => {
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
                    let created_at = OffsetDateTime::from_unix_timestamp(note.created_at() as i64)
                        .unwrap()
                        .format(&Rfc3339)
                        .unwrap();

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
pub enum SearchContext {
    Home,
    Profile,
    Any,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SearchCall {
    context: SearchContext,
    query: String,
}

impl SearchCall {
    pub fn execute(&self, txn: &Transaction, ndb: &Ndb) -> SearchResponse {
        let limit = 10i32;
        let filter = nostrdb::Filter::new()
            .search(&self.query)
            .limit(limit as u64)
            .build();
        let notes = {
            if let Ok(results) = ndb.query(txn, &[filter], limit) {
                results.into_iter().map(|r| r.note_key.as_u64()).collect()
            } else {
                vec![]
            }
        };
        SearchResponse {
            context: self.context.clone(),
            notes,
        }
    }

    pub fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        match serde_json::from_str::<SearchCall>(args) {
            Ok(call) => Ok(ToolCalls::Search(call)),
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
}

impl Dave {
    pub fn new(render_state: Option<&RenderState>) -> Self {
        let mut config = OpenAIConfig::new(); //.with_api_base("http://ollama.jb55.com/v1");
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            config = config.with_api_key(api_key);
        }

        let client = Client::with_config(config);

        let input = "".to_string();
        let pubkey = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".to_string();
        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in dave_tools() {
            tools.insert(tool.name.to_string(), tool);
        }

        let system_prompt = Message::System(format!("You are an ai agent for the nostr protocol. You have access to tools that can query the network, so you can help find and summarize content for users. The current user's pubkey is {}.", &pubkey).to_string());

        Dave {
            client,
            pubkey,
            avatar,
            incoming_tokens: None,
            tools: Arc::new(tools),
            input,
            chat: vec![system_prompt],
        }
    }

    fn render(&mut self, app_ctx: &AppContext, ui: &mut egui::Ui) {
        if let Some(recvr) = &self.incoming_tokens {
            while let Ok(res) = recvr.try_recv() {
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
                                ToolCalls::Search(search_call) => {
                                    let resp = search_call.execute(&txn, app_ctx.ndb);
                                    self.chat.push(Message::ToolResponse(ToolResponse {
                                        id: call.id.clone(),
                                        typ: ToolResponses::Search(resp),
                                    }))
                                }
                            }
                        }
                    }
                }
            }
        }

        // Scroll area for chat messages
        egui::Frame::new().inner_margin(10.0).show(ui, |ui| {
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

        if let Some(avatar) = &mut self.avatar {
            let avatar_size = Vec2::splat(200.0);
            let pos = Vec2::splat(100.0).to_pos2();
            let pos = Rect::from_min_max(pos, pos + avatar_size);
            avatar.render(pos, ui);
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

    fn tool_response_ui(tool_response: &ToolResponse, ui: &mut egui::Ui) {
        ui.label(format!("tool_response: {:?}", tool_response));
    }

    fn tool_call_ui(toolcalls: &[ToolCall], ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            for call in toolcalls {
                match &call.typ {
                    ToolCalls::Search(search_call) => {
                        ui.horizontal(|ui| {
                            let context = match search_call.context {
                                SearchContext::Profile => "profile ",
                                SearchContext::Any => " ",
                                SearchContext::Home => "home ",
                            };

                            ui.label(format!("Searching {}for '{}'", context, search_call.query));
                        });
                    }
                }
            }
        });
    }

    fn inputbox(&mut self, app_ctx: &AppContext, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::multiline(&mut self.input));
            if ui.button("Sned").clicked() {
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
                .fill(ui.visuals().extreme_bg_color)
                .show(ui, |ui| {
                    ui.label(msg);
                })
        });
    }

    fn assistant_chat(&self, msg: &str, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(msg);
        });
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, ctx: &egui::Context) {
        let messages = {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            self.chat
                .iter()
                .map(|c| c.to_api_msg(&txn, app_ctx.ndb))
                .collect()
        };
        let pubkey = self.pubkey.clone();
        let ctx = ctx.clone();
        let client = self.client.clone();
        let tools = self.tools.clone();

        let (tx, rx) = mpsc::channel();
        self.incoming_tokens = Some(rx);

        tokio::spawn(async move {
            let mut token_stream = match client
                .chat()
                .create_stream(CreateChatCompletionRequest {
                    model: "gpt-4o".to_string(),
                    //model: "gpt-4o".to_string(),
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
                        tx.send(DaveResponse::Token(content.to_owned())).unwrap();
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
            props.insert(
                "description".to_string(),
                Value::String(arg.description.to_owned()),
            );
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
            strict: Some(true),
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

fn search_tool() -> Tool {
    Tool {
        name: "search",
        parse_call: SearchCall::parse,
        description: "Full-text search functionality. Used for finding individual notes with specific terms. Queries with multiple words will only return results with notes that have all of those words.",
        arguments: vec![
            ToolArg {
                name: "query",
                typ: ArgType::String,
                required: true,
                description: "The search query",
            },

            ToolArg {
                name: "context",
                typ: ArgType::Enum(vec!["home", "profile", "any"]),
                required: true,
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
    vec![search_tool()]
}
