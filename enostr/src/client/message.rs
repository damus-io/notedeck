use crate::{Filter, Note};
use serde_json::json;

/// Messages sent by clients, received by relays
#[derive(Debug, Eq, PartialEq)]
pub enum ClientMessage {
    Event {
        note: Note,
    },
    Req {
        sub_id: String,
        filters: Vec<Filter>,
    },
    Close {
        sub_id: String,
    },
    Raw(String),
}

impl ClientMessage {
    pub fn event(note: Note) -> Self {
        ClientMessage::Event { note }
    }

    pub fn raw(raw: String) -> Self {
        ClientMessage::Raw(raw)
    }

    pub fn req(sub_id: String, filters: Vec<Filter>) -> Self {
        ClientMessage::Req { sub_id, filters }
    }

    pub fn close(sub_id: String) -> Self {
        ClientMessage::Close { sub_id }
    }

    pub fn to_json(&self) -> String {
        match self {
            Self::Event { note } => json!(["EVENT", note]).to_string(),
            Self::Raw(raw) => raw.clone(),
            Self::Req { sub_id, filters } => {
                let mut json = json!(["REQ", sub_id]);
                let mut filters = json!(filters);

                if let Some(json) = json.as_array_mut() {
                    if let Some(filters) = filters.as_array_mut() {
                        json.append(filters);
                    }
                }

                json.to_string()
            }
            Self::Close { sub_id } => json!(["CLOSE", sub_id]).to_string(),
        }
    }
}
