use async_openai::{
    config::OpenAIConfig,
    types::{ChatCompletionRequestMessage, CreateChatCompletionRequest},
    Client,
};
use chrono::{Duration, Local};
use egui_wgpu::RenderState;
use enostr::KeypairUnowned;
use futures::StreamExt;
use nostrdb::Transaction;
use notedeck::{AppAction, AppContext, AppResponse, JobsCache};
use std::collections::HashMap;
use std::string::ToString;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

pub use avatar::DaveAvatar;
pub use config::ModelConfig;
pub use messages::{DaveApiResponse, Message};
pub use quaternion::Quaternion;
pub use tools::{
    PartialToolCall, QueryCall, QueryResponse, Tool, ToolCall, ToolCalls, ToolResponse,
    ToolResponses,
};
pub use ui::{DaveAction, DaveResponse, DaveUi};
pub use vec3::Vec3;

mod avatar;
mod config;
pub(crate) mod mesh;
mod messages;
mod quaternion;
mod tools;
mod ui;
mod vec3;

pub struct Dave {
    chat: Vec<Message>,
    /// A 3d representation of dave.
    avatar: Option<DaveAvatar>,
    input: String,
    tools: Arc<HashMap<String, Tool>>,
    client: async_openai::Client<OpenAIConfig>,
    incoming_tokens: Option<Receiver<DaveApiResponse>>,
    model_config: ModelConfig,
    jobs: JobsCache,
}

/// Calculate an anonymous user_id from a keypair
fn calculate_user_id(keypair: KeypairUnowned) -> String {
    use sha2::{Digest, Sha256};
    // pubkeys have degraded privacy, don't do that
    let key_input = keypair
        .secret_key
        .map(|sk| sk.as_secret_bytes())
        .unwrap_or(keypair.pubkey.bytes());
    let hex_key = hex::encode(key_input);
    let input = format!("{hex_key}notedeck_dave_user_id");
    hex::encode(Sha256::digest(input))
}

impl Dave {
    pub fn avatar_mut(&mut self) -> Option<&mut DaveAvatar> {
        self.avatar.as_mut()
    }

    fn system_prompt() -> Message {
        let now = Local::now();
        let yesterday = now - Duration::hours(24);
        let date = now.format("%Y-%m-%d %H:%M:%S");
        let timestamp = now.timestamp();
        let yesterday_timestamp = yesterday.timestamp();

        Message::System(format!(
            r#"
You are an AI agent for the nostr protocol called Dave, created by Damus. nostr is a decentralized social media and internet communications protocol. You are embedded in a nostr browser called 'Damus Notedeck'.

- The current date is {date} ({timestamp} unix timestamp if needed for queries).

- Yesterday (-24hrs) was {yesterday_timestamp}. You can use this in combination with `since` queries for pulling notes for summarizing notes the user might have missed while they were away.

# Response Guidelines

- You *MUST* call the present_notes tool with a list of comma-separated note id references when referring to notes so that the UI can display them. Do *NOT* include note id references in the text response, but you *SHOULD* use ^1, ^2, etc to reference note indices passed to present_notes.
- When a user asks for a digest instead of specific query terms, make sure to include both since and until to pull notes for the correct range.
- When tasked with open-ended queries such as looking for interesting notes or summarizing the day, make sure to add enough notes to the context (limit: 100-200) so that it returns enough data for summarization.
"#
        ))
    }

    pub fn new(render_state: Option<&RenderState>) -> Self {
        let model_config = ModelConfig::default();
        //let model_config = ModelConfig::ollama();
        let client = Client::with_config(model_config.to_api());

        let input = "".to_string();
        let avatar = render_state.map(DaveAvatar::new);
        let mut tools: HashMap<String, Tool> = HashMap::new();
        for tool in tools::dave_tools() {
            tools.insert(tool.name().to_string(), tool);
        }

        Dave {
            client,
            avatar,
            incoming_tokens: None,
            tools: Arc::new(tools),
            input,
            model_config,
            chat: vec![],
            jobs: JobsCache::default(),
        }
    }

    /// Process incoming tokens from the ai backend
    fn process_events(&mut self, app_ctx: &AppContext) -> bool {
        // Should we continue sending requests? Set this to true if
        // we have tool responses to send back to the ai
        let mut should_send = false;

        let Some(recvr) = &self.incoming_tokens else {
            return should_send;
        };

        while let Ok(res) = recvr.try_recv() {
            if let Some(avatar) = &mut self.avatar {
                avatar.random_nudge();
            }
            match res {
                DaveApiResponse::Failed(err) => self.chat.push(Message::Error(err)),

                DaveApiResponse::Token(token) => match self.chat.last_mut() {
                    Some(Message::Assistant(msg)) => *msg = msg.clone() + &token,
                    Some(_) => self.chat.push(Message::Assistant(token)),
                    None => {}
                },

                DaveApiResponse::ToolCalls(toolcalls) => {
                    tracing::info!("got tool calls: {:?}", toolcalls);
                    self.chat.push(Message::ToolCalls(toolcalls.clone()));

                    let txn = Transaction::new(app_ctx.ndb).unwrap();
                    for call in &toolcalls {
                        // execute toolcall
                        match call.calls() {
                            ToolCalls::PresentNotes(present) => {
                                self.chat.push(Message::ToolResponse(ToolResponse::new(
                                    call.id().to_owned(),
                                    ToolResponses::PresentNotes(present.note_ids.len() as i32),
                                )));

                                should_send = true;
                            }

                            ToolCalls::Invalid(invalid) => {
                                should_send = true;

                                self.chat.push(Message::tool_error(
                                    call.id().to_string(),
                                    invalid.error.clone(),
                                ));
                            }

                            ToolCalls::Query(search_call) => {
                                should_send = true;

                                let resp = search_call.execute(&txn, app_ctx.ndb);
                                self.chat.push(Message::ToolResponse(ToolResponse::new(
                                    call.id().to_owned(),
                                    ToolResponses::Query(resp),
                                )))
                            }
                        }
                    }
                }
            }
        }

        should_send
    }

    fn ui(&mut self, app_ctx: &mut AppContext, ui: &mut egui::Ui) -> DaveResponse {
        /*
        let rect = ui.available_rect_before_wrap();
        if let Some(av) = self.avatar.as_mut() {
            av.render(rect, ui);
            ui.ctx().request_repaint();
        }
        DaveResponse::default()
            */

        DaveUi::new(self.model_config.trial, &self.chat, &mut self.input).ui(
            app_ctx,
            &mut self.jobs,
            ui,
        )
    }

    fn handle_new_chat(&mut self) {
        self.chat = vec![];
        self.input.clear();
    }

    /// Handle a user send action triggered by the ui
    fn handle_user_send(&mut self, app_ctx: &AppContext, ui: &egui::Ui) {
        self.chat.push(Message::User(self.input.clone()));
        self.send_user_message(app_ctx, ui.ctx());
        self.input.clear();
    }

    fn send_user_message(&mut self, app_ctx: &AppContext, ctx: &egui::Context) {
        let messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(app_ctx.ndb).expect("txn");
            self.chat
                .iter()
                .filter_map(|c| c.to_api_msg(&txn, app_ctx.ndb))
                .collect()
        };
        tracing::debug!("sending messages, latest: {:?}", messages.last().unwrap());

        let user_id = calculate_user_id(app_ctx.accounts.get_selected_account().keypair());

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
                    user: Some(user_id),
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
                        let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
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

                            if let Some(name) = tool.function.as_ref().and_then(|f| f.name.as_ref())
                            {
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
                        // TODO: we should be
                        tracing::error!(
                            "failed to parse tool call {:?}: {}",
                            unknown_tool_call,
                            err,
                        );

                        if let Some(id) = partial.id() {
                            // we have an id, so we can communicate the error
                            // back to the ai
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

            if !parsed_tool_calls.is_empty() {
                tx.send(DaveApiResponse::ToolCalls(parsed_tool_calls))
                    .unwrap();
                ctx.request_repaint();
            }

            tracing::debug!("stream closed");
        });
    }
}

impl notedeck::App for Dave {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let mut app_action: Option<AppAction> = None;

        // always insert system prompt if we have no context
        if self.chat.is_empty() {
            self.chat.push(Dave::system_prompt());
        }

        //update_dave(self, ctx, ui.ctx());
        let should_send = self.process_events(ctx);
        if let Some(action) = self.ui(ctx, ui).action {
            match action {
                DaveAction::ToggleChrome => {
                    app_action = Some(AppAction::ToggleChrome);
                }
                DaveAction::Note(n) => {
                    app_action = Some(AppAction::Note(n));
                }
                DaveAction::NewChat => {
                    self.handle_new_chat();
                }
                DaveAction::Send => {
                    self.handle_user_send(ctx, ui);
                }
            }
        }

        if should_send {
            self.send_user_message(ctx, ui.ctx());
        }

        AppResponse::action(app_action)
    }
}
