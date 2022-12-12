use crate::{Event, Filter};
use serde_json::json;

/// Messages sent by clients, received by relays
#[derive(Debug, Eq, PartialEq)]
pub enum ClientMessage {
    Event {
        event: Event,
    },
    Req {
        sub_id: String,
        filters: Vec<Filter>,
    },
    Close {
        sub_id: String,
    },
}

impl ClientMessage {
    pub fn event(event: Event) -> Self {
        ClientMessage::Event { event }
    }

    pub fn req(sub_id: String, filters: Vec<Filter>) -> Self {
        ClientMessage::Req { sub_id, filters }
    }

    pub fn close(sub_id: String) -> Self {
        ClientMessage::Close { sub_id }
    }

    pub fn to_json(&self) -> String {
        match self {
            Self::Event { event } => json!(["EVENT", event]).to_string(),
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
