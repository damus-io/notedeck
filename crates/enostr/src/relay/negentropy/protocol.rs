use std::borrow::Borrow;

use crate::ClientMessage;

/// Relay-facing NIP-77 session identifier used in NEG-* protocol messages.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NegSessionId(String);

impl NegSessionId {
    /// Wraps one relay-facing negentropy session id.
    pub(crate) fn new(raw: String) -> Self {
        Self(raw)
    }

    /// Borrows the protocol string form used on the wire.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for NegSessionId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

/// Builds a `NEG-OPEN` message for one relay-local session.
pub(super) fn neg_open_msg(
    session_id: &NegSessionId,
    filter_json: String,
    init_hex: &str,
) -> ClientMessage {
    ClientMessage::neg_open_from_json(
        session_id.as_str().to_owned(),
        filter_json,
        init_hex.to_owned(),
    )
}

/// Builds a follow-up `NEG-MSG` for one relay-local session.
pub(super) fn neg_msg(session_id: &NegSessionId, payload_hex: &str) -> ClientMessage {
    ClientMessage::neg_msg(session_id.as_str().to_owned(), payload_hex.to_owned())
}

/// Builds a `NEG-CLOSE` for one relay-local session.
pub(super) fn neg_close_msg(session_id: &NegSessionId) -> ClientMessage {
    ClientMessage::neg_close(session_id.as_str().to_owned())
}
