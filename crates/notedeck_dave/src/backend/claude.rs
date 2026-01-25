use crate::backend::traits::AiBackend;
use crate::messages::DaveApiResponse;
use crate::tools::Tool;
use crate::Message;
use claude_agent_sdk_rs::{query_stream, ContentBlock, Message as ClaudeMessage, TextBlock};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;

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
                Message::ToolCalls(_) | Message::ToolResponse(_) | Message::Error(_) => {
                    // Skip tool-related and error messages
                }
            }
        }

        // Get the last user message as the actual query
        if let Some(Message::User(user_msg)) = messages
            .iter()
            .rev()
            .find(|m| matches!(m, Message::User(_)))
        {
            user_msg.clone()
        } else {
            prompt
        }
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

        tokio::spawn(async move {
            let prompt = ClaudeBackend::messages_to_prompt(&messages);

            tracing::debug!(
                "Sending request to Claude Code: prompt length: {}",
                prompt.len()
            );

            let mut stream = match query_stream(prompt, None).await {
                Ok(stream) => stream,
                Err(err) => {
                    tracing::error!("Claude Code error: {}", err);
                    let _ = tx.send(DaveApiResponse::Failed(err.to_string()));
                    return;
                }
            };

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
