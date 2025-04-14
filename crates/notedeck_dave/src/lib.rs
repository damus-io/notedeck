use async_openai::{
    config::OpenAIConfig,
    types::{ChatCompletionRequestMessage, CreateChatCompletionRequest},
    Client,
};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use futures::StreamExt;
use nostrdb::Transaction;
use notedeck::AppContext;
use notedeck_ui::icons::search_icon;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

pub use avatar::DaveAvatar;
pub use config::ModelConfig;
pub use messages::{DaveResponse, Message};
pub use quaternion::Quaternion;
pub use tools::{
    PartialToolCall, QueryCall, QueryContext, QueryResponse, Tool, ToolCall, ToolCalls,
    ToolResponse, ToolResponses,
};
pub use vec3::Vec3;

mod avatar;
mod config;
mod messages;
mod quaternion;
mod tools;
mod vec3;

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
        for tool in tools::dave_tools() {
            tools.insert(tool.name().to_string(), tool);
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
                            match call.calls() {
                                ToolCalls::Query(search_call) => {
                                    let resp = search_call.execute(&txn, app_ctx.ndb);
                                    self.chat.push(Message::ToolResponse(ToolResponse::new(
                                        call.id().to_owned(),
                                        ToolResponses::Query(resp),
                                    )))
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
        if let Some(search) = query_call.search() {
            ui.label(format!("Querying {context}for '{search}'"));
        } else {
            ui.label(format!("Querying {:?}", &query_call));
        }
    }

    fn tool_call_ui(toolcalls: &[ToolCall], ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            for call in toolcalls {
                match call.calls() {
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
                    tools: Some(tools::dave_tools().iter().map(|t| t.to_api()).collect()),
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
                                entry.id().get_or_insert(id);
                            }

                            if let Some(name) = tool.function.as_ref().and_then(|f| f.name.as_ref())
                            {
                                entry.name().get_or_insert(name);
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
