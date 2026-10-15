//! The running notedeck's local socket: what goes over it and how to reach it.
//!
//! One notedeck per user listens on [`socket_path`]. A client connects, writes
//! one JSON [`Request`] line, reads one JSON [`Response`] line, and hangs up.
//! The chrome owns the listening end ([`Listener`]); `nd` and `notedeck-spawn`
//! are clients ([`Client`]).
//!
//! This crate depends on serde alone, so a client builds without notedeck.
//!
//! ```text
//! → {"v":1,"type":"list_tools"}
//! ← {"v":1,"notedeck":"0.11.0","status":"ok","tools":[{"name":"headway_open_review_queue",…}]}
//! → {"v":1,"type":"call_tool","name":"headway_open_review_queue","args":{}}
//! ← {"v":1,"notedeck":"0.11.0","status":"ok","intent":"applied"}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[cfg(unix)]
mod client;
#[cfg(unix)]
mod server;

#[cfg(unix)]
pub use client::Client;
#[cfg(unix)]
pub use server::{Connection, Incoming, Listener};

/// The protocol version this crate speaks, sent as `v` on every request and
/// response. A request carrying any other `v` is refused; one carrying none is
/// the pre-versioned `spawn_agent` of `notedeck-spawn`, and is served.
pub const PROTOCOL_VERSION: u32 = 1;

/// Overrides [`socket_path`] when set, so tests and a second notedeck can
/// listen somewhere other than the user's one socket.
pub const SOCKET_ENV: &str = "NOTEDECK_SOCKET";

/// The socket's file name, under a `notedeck` directory.
const SOCKET_FILE: &str = "notedeck.sock";

/// Where the running notedeck listens: `$NOTEDECK_SOCKET` if set, else
/// `$XDG_RUNTIME_DIR/notedeck/notedeck.sock` on Linux,
/// `~/Library/Application Support/notedeck/notedeck.sock` on macOS, and the
/// local data dir elsewhere. One per user: the last notedeck to start takes it.
pub fn socket_path() -> PathBuf {
    resolve_socket_path(
        std::env::var_os(SOCKET_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )
}

/// [`socket_path`] over the two variables it reads.
fn resolve_socket_path(
    socket_env: Option<std::ffi::OsString>,
    runtime_dir: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(path) = socket_env {
        return PathBuf::from(path);
    }

    if let Some(runtime_dir) = runtime_dir {
        return PathBuf::from(runtime_dir)
            .join("notedeck")
            .join(SOCKET_FILE);
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        return home
            .join("Library")
            .join("Application Support")
            .join("notedeck")
            .join(SOCKET_FILE);
    }

    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("notedeck")
        .join(SOCKET_FILE)
}

/// What a client asks of notedeck, tagged by `type` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Every tool the running apps offer, answered with [`Reply::Tools`].
    ListTools,
    /// Run the tool `name` with `args` (an object; omitted means none).
    CallTool {
        name: String,
        #[serde(default)]
        args: Value,
    },
    /// Open a Dave session in `cwd`, answered with [`Reply::Spawned`]. The
    /// request `notedeck-spawn` has always sent, kept until `nd spawn`
    /// replaces it.
    SpawnAgent { cwd: PathBuf },
}

/// A [`Request`] as it goes over the wire: `v` beside the tagged fields.
#[derive(Debug, Serialize, Deserialize)]
struct RequestLine {
    /// Absent only on a legacy `spawn_agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    v: Option<u32>,
    #[serde(flatten)]
    request: Request,
}

impl Request {
    /// This request as one wire line, without the newline.
    pub fn to_line(&self) -> String {
        let line = RequestLine {
            v: Some(PROTOCOL_VERSION),
            request: self.clone(),
        };
        serde_json::to_string(&line).expect("a request always serializes")
    }

    /// Parse one wire line. Refuses a `v` other than [`PROTOCOL_VERSION`], and
    /// a missing `v` on anything but `spawn_agent`.
    pub fn from_line(line: &str) -> Result<Self, String> {
        let RequestLine { v, request } =
            serde_json::from_str(line.trim()).map_err(|e| format!("invalid request: {e}"))?;
        match v {
            Some(PROTOCOL_VERSION) => Ok(request),
            Some(v) => Err(format!(
                "protocol version {v} is not supported; this notedeck speaks {PROTOCOL_VERSION}"
            )),
            None if matches!(request, Request::SpawnAgent { .. }) => Ok(request),
            None => Err(format!("missing protocol version \"v\":{PROTOCOL_VERSION}")),
        }
    }
}

/// One answer to one [`Request`]: the protocol version and the notedeck
/// version beside the outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always [`PROTOCOL_VERSION`].
    pub v: u32,
    /// The answering notedeck's version.
    pub notedeck: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Whether the request worked, tagged by `status` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok(Reply),
    Error { message: String },
}

/// What a request that worked answers with, told apart by its one field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    /// To [`Request::ListTools`].
    Tools { tools: Vec<ToolInfo> },
    /// A data tool's answer, or a UI tool's that returned data.
    Result { result: Value },
    /// The tool asked the window to act, and it did.
    Intent { intent: IntentStatus },
    /// To [`Request::SpawnAgent`]: the new Dave session.
    Spawned { session_id: u32 },
}

/// What became of a tool's request that the window act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    /// The window carried it out before answering.
    Applied,
}

impl Response {
    /// A successful answer from this build.
    pub fn ok(reply: Reply) -> Self {
        Self::new(Outcome::Ok(reply))
    }

    /// A failed answer from this build.
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Outcome::Error {
            message: message.into(),
        })
    }

    fn new(outcome: Outcome) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            notedeck: env!("CARGO_PKG_VERSION").to_owned(),
            outcome,
        }
    }

    /// This response as one wire line, without the newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a response always serializes")
    }

    /// Parse one wire line.
    pub fn from_line(line: &str) -> Result<Self, String> {
        serde_json::from_str(line.trim()).map_err(|e| format!("invalid response: {e}"))
    }
}

/// Whether a tool only reads or may also drive the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reads or writes data; runs without a window.
    Data,
    /// Acts on the window, as a key binding would.
    Ui,
}

/// One tool as [`Request::ListTools`] lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    /// The short name a menu shows.
    pub title: String,
    pub description: String,
    /// The group a command bar files it under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub kind: ToolKind,
    /// Changes nothing when called.
    pub read_only: bool,
    /// A JSON Schema object for `args`.
    pub input_schema: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool() -> ToolInfo {
        ToolInfo {
            name: "headway_open_review_queue".to_owned(),
            title: "Open review queue".to_owned(),
            description: "Show the cards in review".to_owned(),
            category: Some("Headway".to_owned()),
            kind: ToolKind::Ui,
            read_only: false,
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn requests_round_trip_with_the_version() {
        let requests = [
            Request::ListTools,
            Request::CallTool {
                name: "chrome_open".to_owned(),
                args: json!({"reference": "headway:notedeck/hill-measure-accuse"}),
            },
            Request::SpawnAgent {
                cwd: PathBuf::from("/tmp"),
            },
        ];
        for request in requests {
            let line = request.to_line();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["v"], json!(PROTOCOL_VERSION), "{line}");
            assert_eq!(Request::from_line(&line), Ok(request));
        }
    }

    #[test]
    fn the_wire_shape_is_flat() {
        assert_eq!(
            Request::from_line(r#"{"v":1,"type":"list_tools"}"#),
            Ok(Request::ListTools)
        );
        assert_eq!(
            Request::from_line(r#"{"v":1,"type":"call_tool","name":"x"}"#),
            Ok(Request::CallTool {
                name: "x".to_owned(),
                args: Value::Null
            })
        );
    }

    #[test]
    fn a_legacy_spawn_needs_no_version() {
        assert_eq!(
            Request::from_line(r#"{"type":"spawn_agent","cwd":"/tmp"}"#),
            Ok(Request::SpawnAgent {
                cwd: PathBuf::from("/tmp")
            })
        );
        let err = Request::from_line(r#"{"type":"list_tools"}"#).unwrap_err();
        assert!(err.contains("missing protocol version"), "{err}");
    }

    #[test]
    fn another_version_or_type_is_refused() {
        let err = Request::from_line(r#"{"v":2,"type":"list_tools"}"#).unwrap_err();
        assert!(err.contains("version 2"), "{err}");
        let err = Request::from_line(r#"{"v":1,"type":"reboot"}"#).unwrap_err();
        assert!(err.contains("invalid request"), "{err}");
        let err = Request::from_line("not json").unwrap_err();
        assert!(err.contains("invalid request"), "{err}");
    }

    #[test]
    fn every_reply_round_trips() {
        let replies = [
            Reply::Tools {
                tools: vec![tool()],
            },
            Reply::Result {
                result: json!({"cards": 3}),
            },
            Reply::Result {
                result: Value::Null,
            },
            Reply::Intent {
                intent: IntentStatus::Applied,
            },
            Reply::Spawned { session_id: 7 },
        ];
        for reply in replies {
            let response = Response::ok(reply);
            let line = response.to_line();
            assert_eq!(Response::from_line(&line), Ok(response), "{line}");
        }
        let response = Response::error("no such tool");
        assert_eq!(Response::from_line(&response.to_line()), Ok(response));
    }

    #[test]
    fn responses_read_as_documented() {
        let line = Response::ok(Reply::Intent {
            intent: IntentStatus::Applied,
        })
        .to_line();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["v"], json!(1));
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(value["intent"], json!("applied"));
        assert_eq!(value["notedeck"], json!(env!("CARGO_PKG_VERSION")));

        // notedeck-spawn reads exactly these two fields.
        let value: Value =
            serde_json::from_str(&Response::ok(Reply::Spawned { session_id: 4 }).to_line())
                .unwrap();
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(value["session_id"], json!(4));

        let value: Value = serde_json::from_str(&Response::error("bad").to_line()).unwrap();
        assert_eq!(value["status"], json!("error"));
        assert_eq!(value["message"], json!("bad"));
    }

    #[test]
    fn the_env_override_wins() {
        assert_eq!(
            resolve_socket_path(Some("/tmp/x.sock".into()), Some("/run/user/1".into())),
            PathBuf::from("/tmp/x.sock")
        );
        assert_eq!(
            resolve_socket_path(None, Some("/run/user/1".into())),
            PathBuf::from("/run/user/1/notedeck/notedeck.sock")
        );
        assert!(resolve_socket_path(None, None).ends_with("notedeck/notedeck.sock"));
    }
}
