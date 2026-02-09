use crate::backend::traits::AiBackend;
use crate::messages::DaveApiResponse;
use crate::tools::{PartialToolCall, Tool, ToolCall};
use crate::Message;
use async_openai::{
    config::OpenAIConfig,
    types::{ChatCompletionRequestMessage, CreateChatCompletionRequest},
    Client,
};
use claude_agent_sdk_rs::PermissionMode;
use futures::StreamExt;
use nostrdb::{Ndb, Transaction};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

pub struct OpenAiBackend {
    client: Client<OpenAIConfig>,
    ndb: Ndb,
}

impl OpenAiBackend {
    pub fn new(client: Client<OpenAIConfig>, ndb: Ndb) -> Self {
        Self { client, ndb }
    }
}

impl AiBackend for OpenAiBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        tools: Arc<HashMap<String, Tool>>,
        model: String,
        user_id: String,
        _session_id: String,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        ctx: egui::Context,
    ) -> (
        mpsc::Receiver<DaveApiResponse>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (tx, rx) = mpsc::channel();

        let api_messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(&self.ndb).expect("txn");
            messages
                .iter()
                .filter_map(|c| c.to_api_msg(&txn, &self.ndb))
                .collect()
        };

        let client = self.client.clone();
        let tool_list: Vec<_> = tools.values().map(|t| t.to_api()).collect();

        let handle = tokio::spawn(async move {
            let mut token_stream = match client
                .chat()
                .create_stream(CreateChatCompletionRequest {
                    model,
                    stream: Some(true),
                    messages: api_messages,
                    tools: Some(tool_list),
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
                        tracing::error!(
                            "failed to parse tool call {:?}: {}",
                            unknown_tool_call,
                            err,
                        );

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

            if !parsed_tool_calls.is_empty() {
                if tx
                    .send(DaveApiResponse::ToolCalls(parsed_tool_calls))
                    .is_ok()
                {
                    ctx.request_repaint();
                }
            }

            tracing::debug!("stream closed");
        });

        (rx, Some(handle))
    }

    fn cleanup_session(&self, _session_id: String) {
        // OpenAI backend doesn't maintain persistent connections per session
        // No cleanup needed
    }

    fn interrupt_session(&self, _session_id: String, _ctx: egui::Context) {
        // OpenAI backend doesn't support interrupts - requests complete atomically
        // The JoinHandle can be aborted from the session side if needed
    }

    fn set_permission_mode(&self, _session_id: String, _mode: PermissionMode, _ctx: egui::Context) {
        // OpenAI backend doesn't support permission modes / plan mode
        tracing::warn!("Plan mode is not supported with the OpenAI backend");
    }
}
