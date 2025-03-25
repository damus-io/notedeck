use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionToolType,
        CreateChatCompletionRequest, FunctionObject,
    },
    Client,
};
use futures::StreamExt;
use notedeck::AppContext;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use avatar::DaveAvatar;
use egui::{Rect, Vec2};
use egui_wgpu::RenderState;

mod avatar;

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant(String),
    System(String),
}

impl Message {
    pub fn to_api_msg(&self) -> ChatCompletionRequestMessage {
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
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchContext {
    Home,
    Profile,
    Any,
}

#[derive(Debug, Deserialize)]
pub struct SearchCall {
    context: SearchContext,
    query: String,
}

impl SearchCall {
    pub fn parse(args: &str) -> Result<ToolCall, ToolCallError> {
        match serde_json::from_str::<SearchCall>(args) {
            Ok(call) => Ok(ToolCall::Search(call)),
            Err(e) => Err(ToolCallError::ArgParseFailure(format!(
                "Failed to parse args: '{}', error: {}",
                args, e
            ))),
        }
    }
}

#[derive(Debug)]
pub enum ToolCall {
    Search(SearchCall),
}

pub enum DaveResponse {
    ToolCall(ToolCall),
    Token(String),
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
        let pubkey = "test_pubkey".to_string();
        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in dave_tools() {
            tools.insert(tool.name.to_string(), tool);
        }

        Dave {
            client,
            pubkey,
            avatar,
            incoming_tokens: None,
            tools: Arc::new(tools),
            input,
            chat: vec![
                Message::System("You are an ai agent for the nostr protocol. You have access to tools that can query the network, so you can help find content for users (TODO: actually implement this)".to_string()),
            ],
        }
    }

    fn render(&mut self, ui: &mut egui::Ui) {
        if let Some(recvr) = &self.incoming_tokens {
            while let Ok(res) = recvr.try_recv() {
                match res {
                    DaveResponse::Token(token) => match self.chat.last_mut() {
                        Some(Message::Assistant(msg)) => *msg = msg.clone() + &token,
                        Some(_) => self.chat.push(Message::Assistant(token)),
                        None => {}
                    },

                    DaveResponse::ToolCall(tool) => {
                        tracing::info!("got tool call: {:?}", tool);
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

                        self.inputbox(ui);
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
                Message::System(_msg) => {
                    // system prompt is not rendered. Maybe we could
                    // have a debug option to show this
                }
            }
        }
    }

    fn inputbox(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::multiline(&mut self.input));
            if ui.button("Sned").clicked() {
                self.chat.push(Message::User(self.input.clone()));
                self.send_user_message(ui.ctx());
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

    fn send_user_message(&mut self, ctx: &egui::Context) {
        let messages = self.chat.iter().map(|c| c.to_api_msg()).collect();
        let pubkey = self.pubkey.clone();
        let (tx, rx) = mpsc::channel();
        self.incoming_tokens = Some(rx);
        let ctx = ctx.clone();
        let client = self.client.clone();
        let tools = self.tools.clone();
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

            let mut tool_call_name: Option<String> = None;
            let mut tool_call_chunks: Vec<String> = vec![];

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
                            let Some(fcall) = &tool.function else {
                                continue;
                            };

                            if let Some(name) = &fcall.name {
                                tool_call_name = Some(name.clone());
                            }

                            let Some(argchunk) = &fcall.arguments else {
                                continue;
                            };

                            tool_call_chunks.push(argchunk.clone());
                        }
                    }

                    if let Some(content) = &resp.content {
                        tx.send(DaveResponse::Token(content.to_owned())).unwrap();
                        ctx.request_repaint();
                    }
                }
            }

            if let Some(tool_name) = tool_call_name {
                if !tool_call_chunks.is_empty() {
                    let args = tool_call_chunks.join("");
                    match parse_tool_call(&tools, &tool_name, &args) {
                        Ok(tool_call) => {
                            tx.send(DaveResponse::ToolCall(tool_call)).unwrap();
                            ctx.request_repaint();
                        }
                        Err(err) => {
                            tracing::error!(
                                "failed to parse tool call err({:?}): name({:?}) args({:?})",
                                err,
                                tool_name,
                                args,
                            );
                            // TODO: return error to user
                        }
                    };
                } else {
                    // TODO: return error to user
                    tracing::error!("got tool call '{}' with no arguments?", tool_name);
                }
            }

            tracing::debug!("stream closed");
        });
    }
}

impl notedeck::App for Dave {
    fn update(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        /*
        self.app
            .frame_history
            .on_new_frame(ctx.input(|i| i.time), frame.info().cpu_usage);
        */

        //update_dave(self, ctx, ui.ctx());
        self.render(ui);
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
    parse_call: fn(&str) -> Result<ToolCall, ToolCallError>,
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
                            .into_iter()
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

fn parse_tool_call(
    tools: &HashMap<String, Tool>,
    name: &str,
    args: &str,
) -> Result<ToolCall, ToolCallError> {
    let Some(tool) = tools.get(name) else {
        return Err(ToolCallError::NotFound(name.to_owned()));
    };

    (tool.parse_call)(&args)
}

fn dave_tools() -> Vec<Tool> {
    vec![search_tool()]
}
