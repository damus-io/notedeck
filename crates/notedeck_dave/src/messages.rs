use crate::tools::{ToolCall, ToolResponse};
use async_openai::types::*;
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Clone)]
pub enum Message {
    System(String),
    User(String),
    Assistant(String),
    ToolCalls(Vec<ToolCall>),
    ToolResponse(ToolResponse),
}

/// The ai backends response. Since we are using streaming APIs these are
/// represented as individual tokens or tool calls
pub enum DaveApiResponse {
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
                let tool_response = resp.responses().format_for_dave(txn, ndb);

                ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
                    tool_call_id: resp.id().to_owned(),
                    content: ChatCompletionRequestToolMessageContent::Text(tool_response),
                })
            }
        }
    }
}
