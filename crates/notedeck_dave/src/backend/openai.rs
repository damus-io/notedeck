use crate::backend::traits::AiBackend;
use crate::messages::DaveApiResponse;
use crate::tools::{PartialToolCall, Tool, ToolCall, ToolCalls, ToolResponses};
use crate::Message;
use async_openai::{config::OpenAIConfig, types::*, Client};
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
        model: Option<String>,
        user_id: String,
        _session_id: String,
        _cwd: Option<PathBuf>,
        _resume_session_id: Option<String>,
        ctx: egui::Context,
    ) -> (
        Option<mpsc::Receiver<DaveApiResponse>>,
        Option<tokio::task::JoinHandle<()>>,
    ) {
        let (tx, rx) = mpsc::channel();

        let api_messages: Vec<ChatCompletionRequestMessage> = {
            let txn = Transaction::new(&self.ndb).expect("txn");
            messages
                .iter()
                .filter_map(|c| message_to_api(c, &txn, &self.ndb))
                .collect()
        };

        let client = self.client.clone();
        let tool_list: Vec<_> = tools.values().map(tool_to_api).collect();

        let handle = tokio::spawn(async move {
            // Timeout for the initial API connection (creating the stream).
            const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

            let mut token_stream = match tokio::time::timeout(
                CONNECT_TIMEOUT,
                client.chat().create_stream(CreateChatCompletionRequest {
                    model: model.unwrap_or_else(|| "gpt-4.1-mini".to_string()),
                    stream: Some(true),
                    messages: api_messages,
                    tools: Some(tool_list),
                    user: Some(user_id),
                    ..Default::default()
                }),
            )
            .await
            {
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

            if !parsed_tool_calls.is_empty()
                && tx
                    .send(DaveApiResponse::ToolCalls(parsed_tool_calls))
                    .is_ok()
            {
                ctx.request_repaint();
            }

            tracing::debug!("stream closed");
        });

        (Some(rx), Some(handle))
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
                content: ChatCompletionRequestSystemMessageContent::Text(m.clone()),
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
        Message::PermissionRequest(_)
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
/// [`Tool::parameters_schema`]; this wraps them in async_openai's envelope.
fn tool_to_api(tool: &Tool) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: tool.name().to_owned(),
            description: Some(tool.description().to_owned()),
            strict: Some(false),
            parameters: Some(tool.parameters_schema()),
        },
    }
}
