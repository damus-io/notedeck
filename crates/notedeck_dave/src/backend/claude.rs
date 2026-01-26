use crate::backend::traits::AiBackend;
use crate::messages::{DaveApiResponse, PendingPermission, PermissionRequest, PermissionResponse};
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{
    ClaudeAgentOptions, ClaudeClient, ContentBlock, Message as ClaudeMessage, PermissionMode,
    PermissionResult, PermissionResultAllow, PermissionResultDeny, TextBlock,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::oneshot;
use uuid::Uuid;

pub struct ClaudeBackend {
    api_key: String,
}

impl ClaudeBackend {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Convert our messages to a prompt for Claude Code
    fn messages_to_prompt(messages: &[Message]) -> String {
        let mut prompt = String::new();

        // Include system message if present
        for msg in messages {
            if let Message::System(content) = msg {
                prompt.push_str(content);
                prompt.push_str("\n\n");
                break;
            }
        }

        // Format conversation history
        for msg in messages {
            match msg {
                Message::System(_) => {} // Already handled
                Message::User(content) => {
                    prompt.push_str("Human: ");
                    prompt.push_str(content);
                    prompt.push_str("\n\n");
                }
                Message::Assistant(content) => {
                    prompt.push_str("Assistant: ");
                    prompt.push_str(content);
                    prompt.push_str("\n\n");
                }
                Message::ToolCalls(_)
                | Message::ToolResponse(_)
                | Message::Error(_)
                | Message::PermissionRequest(_) => {
                    // Skip tool-related, error, and permission messages
                }
            }
        }

        prompt
    }
}

impl AiBackend for ClaudeBackend {
    fn stream_request(
        &self,
        messages: Vec<Message>,
        _tools: Arc<HashMap<String, Tool>>,
        _model: String,
        _user_id: String,
        ctx: egui::Context,
    ) -> mpsc::Receiver<DaveApiResponse> {
        let (tx, rx) = mpsc::channel();
        let _api_key = self.api_key.clone();

        let tx_for_callback = tx.clone();
        let ctx_for_callback = ctx.clone();

        tokio::spawn(async move {
            let prompt = ClaudeBackend::messages_to_prompt(&messages);

            tracing::debug!(
                "Sending request to Claude Code: prompt length: {}, preview: {:?}",
                prompt.len(),
                &prompt[..prompt.len().min(100)]
            );

            // A stderr callback is needed to prevent the subprocess from blocking
            // when stderr buffer fills up. We log the output for debugging.
            let stderr_callback = |msg: String| {
                tracing::trace!("Claude CLI stderr: {}", msg);
            };

            // Permission callback - sends requests to UI and waits for user response
            let can_use_tool: Arc<
                dyn Fn(
                        String,
                        serde_json::Value,
                        claude_agent_sdk_rs::ToolPermissionContext,
                    ) -> BoxFuture<'static, PermissionResult>
                    + Send
                    + Sync,
            > = Arc::new({
                let tx = tx_for_callback;
                let ctx = ctx_for_callback;
                move |tool_name: String,
                      tool_input: serde_json::Value,
                      _context: claude_agent_sdk_rs::ToolPermissionContext| {
                    let tx = tx.clone();
                    let ctx = ctx.clone();
                    Box::pin(async move {
                        let (response_tx, response_rx) = oneshot::channel();

                        let request = PermissionRequest {
                            id: Uuid::new_v4(),
                            tool_name: tool_name.clone(),
                            tool_input: tool_input.clone(),
                            response: None,
                        };

                        let pending = PendingPermission {
                            request,
                            response_tx,
                        };

                        // Send permission request to UI
                        if tx
                            .send(DaveApiResponse::PermissionRequest(pending))
                            .is_err()
                        {
                            tracing::error!("Failed to send permission request to UI");
                            return PermissionResult::Deny(PermissionResultDeny {
                                message: "UI channel closed".to_string(),
                                interrupt: true,
                            });
                        }

                        ctx.request_repaint();

                        // Wait for user response
                        match response_rx.await {
                            Ok(PermissionResponse::Allow) => {
                                tracing::debug!("User allowed tool: {}", tool_name);
                                PermissionResult::Allow(PermissionResultAllow::default())
                            }
                            Ok(PermissionResponse::Deny { reason }) => {
                                tracing::debug!("User denied tool {}: {}", tool_name, reason);
                                PermissionResult::Deny(PermissionResultDeny {
                                    message: reason,
                                    interrupt: false,
                                })
                            }
                            Err(_) => {
                                tracing::error!("Permission response channel closed");
                                PermissionResult::Deny(PermissionResultDeny {
                                    message: "Permission request cancelled".to_string(),
                                    interrupt: true,
                                })
                            }
                        }
                    })
                }
            });

            let options = ClaudeAgentOptions::builder()
                .permission_mode(PermissionMode::Default)
                .stderr_callback(Arc::new(stderr_callback))
                .can_use_tool(can_use_tool)
                .build();

            // Use ClaudeClient instead of query_stream to enable control protocol
            // for can_use_tool callbacks
            let mut client = ClaudeClient::new(options);
            if let Err(err) = client.connect().await {
                tracing::error!("Claude Code connection error: {}", err);
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                return;
            }
            if let Err(err) = client.query(&prompt).await {
                tracing::error!("Claude Code query error: {}", err);
                let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                return;
            }
            let mut stream = client.receive_response();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(message) => match message {
                        ClaudeMessage::Assistant(assistant_msg) => {
                            for block in &assistant_msg.message.content {
                                if let ContentBlock::Text(TextBlock { text }) = block {
                                    if let Err(err) = tx.send(DaveApiResponse::Token(text.clone()))
                                    {
                                        tracing::error!("Failed to send token to UI: {}", err);
                                        return;
                                    }
                                    ctx.request_repaint();
                                }
                            }
                        }
                        ClaudeMessage::Result(result_msg) => {
                            if result_msg.is_error {
                                let error_text = result_msg
                                    .result
                                    .unwrap_or_else(|| "Unknown error".to_string());
                                let _ = tx.send(DaveApiResponse::Failed(error_text));
                            }
                            break;
                        }
                        _ => {}
                    },
                    Err(err) => {
                        tracing::error!("Claude stream error: {}", err);
                        let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                        return;
                    }
                }
            }

            tracing::debug!("Claude stream closed");
        });

        rx
    }
}
