use crate::{Error, Note};
use nostrdb::Filter;
use serde_json::json;

/// Messages sent by clients, received by relays
#[derive(Debug)]
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

    pub fn to_json(&self) -> Result<String, Error> {
        Ok(match self {
            Self::Event { note } => json!(["EVENT", note]).to_string(),
            Self::Raw(raw) => raw.clone(),
            Self::Req { sub_id, filters } => {
                if filters.is_empty() {
                    format!("[\"REQ\",\"{}\",{{ }}]", sub_id)
                } else if filters.len() == 1 {
                    let filters_json_str = filters[0].json()?;
                    format!("[\"REQ\",\"{}\",{}]", sub_id, filters_json_str)
                } else {
                    let filters_json_str: Result<Vec<String>, Error> = filters
                        .iter()
                        .map(|f| f.json().map_err(Into::<Error>::into))
                        .collect();
                    format!("[\"REQ\",\"{}\",{}]", sub_id, filters_json_str?.join(","))
                }
            }
            Self::Close { sub_id } => json!(["CLOSE", sub_id]).to_string(),
        })
    }
}
