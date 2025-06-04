use crate::tools::{ToolCall, ToolResponse};
use async_openai::types::*;
use nostrdb::{Ndb, Transaction};

#[derive(Debug, Clone)]
pub enum Message {
    System(String),
    Error(String),
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
    Failed(String),
}

impl Message {
    pub fn tool_error(id: String, msg: String) -> Self {
        Self::ToolResponse(ToolResponse::error(id, msg))
    }

    pub fn to_api_msg(&self, txn: &Transaction, ndb: &Ndb) -> Option<ChatCompletionRequestMessage> {
        match self {
            Message::Error(_err) => None,

            Message::User(msg) => Some(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    name: None,
                    content: ChatCompletionRequestUserMessageContent::Text(msg.clone()),
                },
            )),

            Message::Assistant(msg) => Some(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                        msg.clone(),
                    )),
                    ..Default::default()
                },
            )),

            Message::System(msg) => Some(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(msg.clone()),
                    ..Default::default()
                },
            )),

            Message::ToolCalls(calls) => Some(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    tool_calls: Some(calls.iter().map(|c| c.to_api()).collect()),
                    ..Default::default()
                },
            )),

            Message::ToolResponse(resp) => {
                let tool_response = resp.responses().format_for_dave(txn, ndb);

                Some(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        tool_call_id: resp.id().to_owned(),
                        content: ChatCompletionRequestToolMessageContent::Text(tool_response),
                    },
                ))
            }
        }
    }
}
