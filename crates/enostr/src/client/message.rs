use crate::Error;
use nostrdb::{Filter, Note};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct EventClientMessage {
    pub note_json: String,
}

impl EventClientMessage {
    pub fn to_json(&self) -> String {
        format!("[\"EVENT\", {}]", self.note_json)
    }
}

impl<'a> TryFrom<&'a Note<'a>> for EventClientMessage {
    type Error = Error;

    fn try_from(value: &'a Note<'a>) -> Result<Self, Self::Error> {
        Ok(Self {
            note_json: value.json()?,
        })
    }
}

fn neg_open_to_json(
    sub_id: &str,
    filter_json: &str,
    initial_message: &str,
) -> Result<String, Error> {
    let sub_id = serde_json::to_string(sub_id)?;
    let initial_message = serde_json::to_string(initial_message)?;
    Ok(format!(
        r#"["NEG-OPEN",{sub_id},{filter_json},{initial_message}]"#
    ))
}

/// Messages sent by clients, received by relays.
#[derive(Debug, Clone)]
pub struct ClientMessage(ClientMessageKind);

#[derive(Debug, Clone)]
enum ClientMessageKind {
    Event(EventClientMessage),
    Req {
        sub_id: String,
        filters: Vec<Filter>,
    },
    Close {
        sub_id: String,
    },
    /// Open one NIP-77 negentropy session.
    NegOpen {
        sub_id: String,
        filter_json: String,
        initial_message: String,
    },
    /// Continue one NIP-77 negentropy session.
    NegMsg {
        sub_id: String,
        message: String,
    },
    /// Close one NIP-77 negentropy session.
    NegClose {
        sub_id: String,
    },
}

impl ClientMessage {
    pub fn event(note: &Note) -> Result<Self, Error> {
        Ok(Self(ClientMessageKind::Event(EventClientMessage {
            note_json: note.json()?,
        })))
    }

    pub fn event_json(note_json: String) -> Result<Self, Error> {
        Ok(Self(ClientMessageKind::Event(EventClientMessage {
            note_json,
        })))
    }

    pub fn req(sub_id: String, filters: Vec<Filter>) -> Self {
        Self(ClientMessageKind::Req { sub_id, filters })
    }

    pub fn close(sub_id: String) -> Self {
        Self(ClientMessageKind::Close { sub_id })
    }

    /// Construct a NIP-77 `NEG-OPEN` message from an already serialized filter.
    pub(crate) fn neg_open_from_json(
        sub_id: String,
        filter_json: String,
        initial_message: String,
    ) -> Self {
        Self(ClientMessageKind::NegOpen {
            sub_id,
            filter_json,
            initial_message,
        })
    }

    /// Construct a NIP-77 `NEG-MSG` message.
    pub(crate) fn neg_msg(sub_id: String, message: String) -> Self {
        Self(ClientMessageKind::NegMsg { sub_id, message })
    }

    /// Construct a NIP-77 `NEG-CLOSE` message.
    pub(crate) fn neg_close(sub_id: String) -> Self {
        Self(ClientMessageKind::NegClose { sub_id })
    }

    pub fn to_json(&self) -> Result<String, Error> {
        Ok(match &self.0 {
            ClientMessageKind::Event(ecm) => ecm.to_json(),
            ClientMessageKind::Req { sub_id, filters } => {
                if filters.is_empty() {
                    format!("[\"REQ\",\"{sub_id}\",{{ }}]")
                } else if filters.len() == 1 {
                    let filters_json_str = filters[0].json()?;
                    format!("[\"REQ\",\"{sub_id}\",{filters_json_str}]")
                } else {
                    let filters_json_str: Result<Vec<String>, Error> = filters
                        .iter()
                        .map(|f| f.json().map_err(Into::<Error>::into))
                        .collect();
                    format!("[\"REQ\",\"{}\",{}]", sub_id, filters_json_str?.join(","))
                }
            }
            ClientMessageKind::Close { sub_id } => json!(["CLOSE", sub_id]).to_string(),
            ClientMessageKind::NegOpen {
                sub_id,
                filter_json,
                initial_message,
            } => neg_open_to_json(sub_id, filter_json, initial_message)?,
            ClientMessageKind::NegMsg { sub_id, message } => {
                json!(["NEG-MSG", sub_id, message]).to_string()
            }
            ClientMessageKind::NegClose { sub_id } => json!(["NEG-CLOSE", sub_id]).to_string(),
        })
    }
}

impl From<EventClientMessage> for ClientMessage {
    fn from(value: EventClientMessage) -> Self {
        Self(ClientMessageKind::Event(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negentropy_client_messages_serialize_to_nip77_frames() {
        let open = ClientMessage::neg_open_from_json(
            "sub-1".to_owned(),
            r#"{"kinds":[1]}"#.to_owned(),
            "abcd".to_owned(),
        );
        assert_eq!(
            open.to_json().expect("serialize NEG-OPEN"),
            r#"["NEG-OPEN","sub-1",{"kinds":[1]},"abcd"]"#
        );

        let msg = ClientMessage::neg_msg("sub-1".to_owned(), "deadbeef".to_owned());
        assert_eq!(
            msg.to_json().expect("serialize NEG-MSG"),
            r#"["NEG-MSG","sub-1","deadbeef"]"#
        );

        let close = ClientMessage::neg_close("sub-1".to_owned());
        assert_eq!(
            close.to_json().expect("serialize NEG-CLOSE"),
            r#"["NEG-CLOSE","sub-1"]"#
        );
    }
}
