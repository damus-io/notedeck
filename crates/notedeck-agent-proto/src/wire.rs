//! The stdio protocol: newline-delimited JSON, one message per line, calls
//! correlated by `id` so several can be in flight at once.
//!
//! A line on the wire reads as the message it is:
//!
//! ```text
//! {"type":"call","id":3,"call":{"name":"Read","input":{"file_path":"src/lib.rs"}}}
//! {"type":"result","id":3,"text":"     1\tfn main() {}\n"}
//! ```
//!
//! ## The handshake is a lockstep check
//!
//! The host patches the guest binary in from its own build, so host and guest
//! should be the same commit by construction. That only holds if the binary
//! patched in is the one just built, and a stale one fails in confusing ways
//! (a field one side has and the other refuses). So the host's first message
//! is a [`HostMessage::Ping`], the guest answers its [`Version`], and the host
//! refuses to go on unless [`Version::check`] passes: one clear error in place
//! of a class of strange ones.

use serde::{Deserialize, Serialize};

use crate::call::CodingCall;

/// The protocol's version. Bump it on any change to a message's shape, so
/// [`Version::check`] refuses a guest built before the change even when the
/// build ids are unset or equal.
pub const PROTOCOL_VERSION: u32 = 1;

/// A message from the host to the executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Ask the executor for its [`Version`]; answered by
    /// [`GuestMessage::Pong`] with the same id.
    Ping { id: u64 },
    /// Run a tool call; answered by a [`GuestMessage::Result`],
    /// [`GuestMessage::Error`] or [`GuestMessage::Cancelled`] with the same
    /// id.
    Call { id: u64, call: CodingCall },
    /// Stop the call `id`. A Bash call's whole process group is killed, and
    /// the call is answered [`GuestMessage::Cancelled`]. A call that already
    /// finished, or an id that never ran, is ignored: the cancel raced the
    /// answer.
    Cancel { id: u64 },
    /// Cancel every call in flight, answer them, and exit. Closing the
    /// executor's stdin does the same.
    Shutdown,
}

/// A message from the executor to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMessage {
    /// The executor's version, answering [`HostMessage::Ping`].
    Pong { id: u64, version: Version },
    /// The call `id` finished; `text` is what the model reads.
    Result { id: u64, text: String },
    /// The call `id` failed; `message` is what the model reads instead.
    Error { id: u64, message: String },
    /// The call `id` was cancelled before it finished.
    Cancelled { id: u64 },
    /// A line the executor couldn't read as a [`HostMessage`] and couldn't
    /// find an id in to answer.
    Malformed { message: String },
}

/// Which protocol, and which build, one end speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    /// The sender's [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// The commit the sender was built from, as its build stamped it.
    pub build: String,
}

/// Why the host won't talk to an executor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    /// The executor speaks another version of this protocol.
    #[error(
        "the agent executor speaks protocol {theirs} and notedeck speaks {ours}: \
         rebuild it from this checkout (scripts/build-guest-agent)"
    )]
    Protocol { ours: u32, theirs: u32 },
    /// The executor was built from another commit.
    #[error(
        "the agent executor was built from {theirs:?} and notedeck from {ours:?}: \
         rebuild it from this checkout (scripts/build-guest-agent)"
    )]
    Build { ours: String, theirs: String },
}

impl Version {
    /// This build's version: the current protocol and `build`, the commit
    /// it was built from.
    pub fn new(build: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            build: build.into(),
        }
    }

    /// Whether the host, built from `expected_build`, may talk to the
    /// executor that answered `self`: the same protocol, and the same build.
    pub fn check(&self, expected_build: &str) -> Result<(), HandshakeError> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(HandshakeError::Protocol {
                ours: PROTOCOL_VERSION,
                theirs: self.protocol,
            });
        }
        if self.build != expected_build {
            return Err(HandshakeError::Build {
                ours: expected_build.to_owned(),
                theirs: self.build.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::ReadInput;
    use serde_json::json;

    /// The messages read on the wire as the module docs show them, so a
    /// host in another language, or a person reading a log, can follow.
    #[test]
    fn messages_have_the_documented_shape() {
        let call = HostMessage::Call {
            id: 3,
            call: CodingCall::Read(ReadInput {
                file_path: "src/lib.rs".into(),
                offset: None,
                limit: None,
            }),
        };
        assert_eq!(
            serde_json::to_value(&call).unwrap(),
            json!({"type": "call", "id": 3,
                   "call": {"name": "Read", "input": {"file_path": "src/lib.rs"}}})
        );
        let pong = GuestMessage::Pong {
            id: 0,
            version: Version::new("abc"),
        };
        assert_eq!(
            serde_json::to_value(&pong).unwrap(),
            json!({"type": "pong", "id": 0, "version": {"protocol": PROTOCOL_VERSION, "build": "abc"}})
        );
        assert_eq!(
            serde_json::to_value(HostMessage::Shutdown).unwrap(),
            json!({"type": "shutdown"})
        );
    }

    /// Every message survives a trip through one line of JSON.
    #[test]
    fn messages_round_trip() {
        let host = [
            HostMessage::Ping { id: 1 },
            HostMessage::Cancel { id: 2 },
            HostMessage::Shutdown,
        ];
        for m in host {
            let line = serde_json::to_string(&m).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(serde_json::from_str::<HostMessage>(&line).unwrap(), m);
        }
        let guest = [
            GuestMessage::Result {
                id: 1,
                text: "a\nb\n".into(),
            },
            GuestMessage::Error {
                id: 2,
                message: "no".into(),
            },
            GuestMessage::Cancelled { id: 3 },
            GuestMessage::Malformed {
                message: "eh".into(),
            },
        ];
        for m in guest {
            let line = serde_json::to_string(&m).unwrap();
            assert!(!line.contains('\n'), "a multi-line text stays one line");
            assert_eq!(serde_json::from_str::<GuestMessage>(&line).unwrap(), m);
        }
    }

    /// The handshake passes only for the same protocol and the same build.
    #[test]
    fn the_handshake_refuses_another_build_or_protocol() {
        assert_eq!(Version::new("abc").check("abc"), Ok(()));
        assert!(matches!(
            Version::new("abc").check("def"),
            Err(HandshakeError::Build { .. })
        ));
        let old = Version {
            protocol: PROTOCOL_VERSION + 1,
            build: "abc".into(),
        };
        assert!(matches!(
            old.check("abc"),
            Err(HandshakeError::Protocol { .. })
        ));
    }
}
