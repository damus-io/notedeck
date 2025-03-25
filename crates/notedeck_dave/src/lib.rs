use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, CreateChatCompletionRequest,
    },
    Client,
};
use futures::StreamExt;
use notedeck::AppContext;
use std::sync::mpsc::{self, Receiver};

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

pub struct Dave {
    chat: Vec<Message>,
    /// A 3d representation of dave.
    avatar: Option<DaveAvatar>,
    input: String,
    pubkey: String,
    client: async_openai::Client<OpenAIConfig>,
    incoming_tokens: Option<Receiver<String>>,
}

impl Dave {
    pub fn new(render_state: Option<&RenderState>) -> Self {
        let mut config = OpenAIConfig::new().with_api_base("http://ollama.jb55.com/v1");
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            config = config.with_api_key(api_key);
        }

        let client = Client::with_config(config);

        let input = "".to_string();
        let pubkey = "test_pubkey".to_string();
        let avatar = render_state.map(DaveAvatar::new);

        Dave {
            client,
            pubkey,
            avatar,
            incoming_tokens: None,
            input,
            chat: vec![
                Message::System("You are an ai agent for the nostr protocol. You have access to tools that can query the network, so you can help find content for users".to_string()),
            ],
        }
    }

    fn render(&mut self, ui: &mut egui::Ui) {
        if let Some(recvr) = &self.incoming_tokens {
            if let Ok(token) = recvr.try_recv() {
                match self.chat.last_mut() {
                    Some(Message::Assistant(msg)) => *msg = msg.clone() + &token,
                    Some(_) => self.chat.push(Message::Assistant(token)),
                    None => {}
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
        tokio::spawn(async move {
            let mut token_stream = match client
                .chat()
                .create_stream(CreateChatCompletionRequest {
                    //model: "gpt-4o".to_string(),
                    model: "llama3.1:latest".to_string(),
                    stream: Some(true),
                    messages,
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

            tracing::info!("got stream!");

            while let Some(token) = token_stream.next().await {
                let token = match token {
                    Ok(token) => token,
                    Err(err) => {
                        tracing::error!("failed to get token: {err}");
                        return;
                    }
                };
                let Some(choice) = token.choices.first() else {
                    return;
                };
                let Some(content) = &choice.delta.content else {
                    return;
                };

                tx.send(content.to_owned()).unwrap();
                ctx.request_repaint();
            }
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
