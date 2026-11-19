//! The running notedeck's local socket: what goes over it and how to reach it.
//!
//! One notedeck per user listens on [`socket_path`]. A client connects, writes
//! one JSON [`Request`] line, reads one JSON [`Response`] line, and hangs up.
//! The chrome owns the listening end ([`Listener`]); `nd` is the client
//! ([`Client`]).
//!
//! A [`Request::Subscribe`] (protocol v2) keeps the connection open instead:
//! the server writes one [`Reply::Event`] line per event until the stream ends
//! or the client hangs up, and a stream that can't start answers one error
//! line. Every other request is spelled in v1, so an older notedeck still
//! answers it, and each answer carries the version its request did.
//!
//! This crate depends on serde alone, so a client builds without notedeck.
//!
//! ```text
//! → {"v":1,"type":"list_tools"}
//! ← {"v":1,"notedeck":"0.11.0","status":"ok","tools":[{"name":"headway_open_review_queue",…}]}
//! → {"v":1,"type":"call_tool","name":"headway_open_review_queue","args":{}}
//! ← {"v":1,"notedeck":"0.11.0","status":"ok","intent":"applied"}
//! → {"v":2,"type":"subscribe","name":"agentium_follow_log","args":{"follow":true}}
//! ← {"v":2,"notedeck":"0.11.0","status":"ok","event":{"role":"user",…},"text":"…"}
//! ← {"v":2,"notedeck":"0.11.0","status":"ok","event":{"role":"assistant",…},"text":"…"}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[cfg(unix)]
mod client;
#[cfg(unix)]
mod server;

#[cfg(unix)]
pub use client::{probe, Client};
#[cfg(unix)]
pub use server::{Connection, EventQueue, Incoming, Listener, Pushed, Watch, DROPPED};

/// The newest protocol version this crate speaks. A request carries the
/// oldest version that spells it ([`Request::version`]), and is answered in
/// that version; a request carrying a version above this one, or none, is
/// refused.
pub const PROTOCOL_VERSION: u32 = 2;

/// The version that added [`Request::Subscribe`] and [`Reply::Event`].
pub const STREAMS_VERSION: u32 = 2;

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
        /// A key to sign the call's comments with in place of the app's
        /// account, for a tool that accepts one (see
        /// [`ToolInfo::accepts_signer`]); every other tool ignores it. It
        /// rides beside the args, never in them, so a model calling the
        /// tool through MCP or Dave can't set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signer: Option<Signer>,
    },
    /// Run the streaming tool `name` with `args` (see [`ToolInfo::streams`]),
    /// keeping the connection open: each event it emits is one
    /// [`Reply::Event`] line, until the stream ends (the server hangs up) or
    /// the client does. A stream that can't start answers one error line.
    /// Protocol v2.
    Subscribe {
        name: String,
        #[serde(default)]
        args: Value,
    },
}

/// Who signs a [`Request::CallTool`]'s comments, when not the app's account.
///
/// Its `Debug` hides the key, so logging a request never prints it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signer {
    /// The comment key, `nsec1…` or 64-char hex, as `nd` read it from
    /// `$HEADWAY_COMMENT_NSEC` or the file `$HEADWAY_COMMENT_NSEC_FILE`
    /// names. notedeck parses it and refuses the call if it doesn't parse,
    /// rather than signing as the account.
    pub comment_nsec: String,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("comment_nsec", &"<redacted>")
            .finish()
    }
}

/// A [`Request`] as it goes over the wire: `v` beside the tagged fields.
#[derive(Debug, Serialize, Deserialize)]
struct RequestLine {
    /// Absent only on a client older than the versioned protocol, which is
    /// refused rather than misread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    v: Option<u32>,
    #[serde(flatten)]
    request: Request,
}

/// A parsed request line: the request and the version it was spelled in,
/// which its answer carries back.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned {
    pub v: u32,
    pub request: Request,
}

impl Request {
    /// The oldest protocol version that spells this request, which
    /// [`to_line`](Self::to_line) sends: 1 for a list or a call, so a
    /// notedeck that predates streams still answers them, and
    /// [`STREAMS_VERSION`] for a subscribe.
    pub fn version(&self) -> u32 {
        match self {
            Request::ListTools | Request::CallTool { .. } => 1,
            Request::Subscribe { .. } => STREAMS_VERSION,
        }
    }

    /// This request as one wire line, without the newline.
    pub fn to_line(&self) -> String {
        let line = RequestLine {
            v: Some(self.version()),
            request: self.clone(),
        };
        serde_json::to_string(&line).expect("a request always serializes")
    }

    /// Parse one wire line. Refuses a `v` above [`PROTOCOL_VERSION`], a
    /// missing one, and a request newer than the `v` it came with.
    pub fn from_line(line: &str) -> Result<Self, String> {
        Self::versioned(line).map(|versioned| versioned.request)
    }

    /// [`from_line`](Self::from_line), keeping the version the line carried.
    pub fn versioned(line: &str) -> Result<Versioned, String> {
        let RequestLine { v, request } =
            serde_json::from_str(line.trim()).map_err(|e| format!("invalid request: {e}"))?;
        let v = match v {
            Some(v @ 1..=PROTOCOL_VERSION) => v,
            Some(v) => {
                return Err(format!(
                    "protocol version {v} is not supported; this notedeck speaks 1 to {PROTOCOL_VERSION}"
                ));
            }
            None => return Err("missing protocol version \"v\"".to_owned()),
        };
        if v < request.version() {
            return Err(format!(
                "this request needs protocol version {}, and came with {v}",
                request.version()
            ));
        }
        Ok(Versioned { v, request })
    }
}

/// One answer to one [`Request`]: the protocol version and the notedeck
/// version beside the outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The version its request was spelled in: a server built here sends
    /// [`PROTOCOL_VERSION`] until [`Connection`] writes it as the request's.
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
    Result {
        result: Value,
        /// The same answer as text for a person, when the tool renders one:
        /// what `nd` prints unless asked for `--json`. Muted parts carry ANSI
        /// color for the client to keep or strip. Absent from an older
        /// notedeck, and ignored by an older nd.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// A line for the person beside the answer, not part of it: where a
        /// commit was found, say. nd prints it to stderr, so the text pipes
        /// clean. Absent from an older notedeck and most tools, and ignored
        /// by an older nd.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
    },
    /// The tool asked the window to act, and it did.
    Intent { intent: IntentStatus },
    /// One event of a [`Request::Subscribe`] stream.
    Event {
        event: Value,
        /// The event as text for a person, as [`Reply::Result`]'s `text`:
        /// what to append for a [`StreamMode::Append`] stream, the whole
        /// screen for a [`StreamMode::Redraw`] one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
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

    /// [`to_line`](Self::to_line), carrying version `v` in place of its own:
    /// an answer goes back in the version its request was spelled in.
    pub fn line_as(&self, v: u32) -> String {
        /// A [`Response`] borrowed, with another version.
        #[derive(Serialize)]
        struct Line<'a> {
            v: u32,
            notedeck: &'a str,
            #[serde(flatten)]
            outcome: &'a Outcome,
        }
        let line = Line {
            v,
            notedeck: &self.notedeck,
            outcome: &self.outcome,
        };
        serde_json::to_string(&line).expect("a response always serializes")
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

/// How a client shows a streaming tool's events (see [`ToolInfo::streams`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// Each event's text follows the last, as `tail -f` does: a log.
    Append,
    /// Each event's text is the whole screen, replacing the last: a
    /// dashboard.
    Redraw,
}

/// Whether a model may call a tool, and on what: the wire form of
/// `agentium_core::tool::ModelReach`, the rule `nd mcp` and an agent's
/// catalog share. Read it through [`ToolInfo::model_reach`], which derives
/// it for a notedeck that doesn't send it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelReach {
    /// A model can't call it: it streams, and a model's call answers once.
    Never,
    /// Reads or writes data and answers once.
    Data,
    /// Acts on the user's window; each caller decides whether to allow it.
    Window,
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
    /// The command line that runs it, a namespace then a verb:
    /// `["headway", "show"]` is `nd headway show`. Empty for a tool reachable
    /// only by name; absent on the wire then, and from an older notedeck.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Whether a call may carry a [`Signer`] for this tool to sign its
    /// comments with. `nd` sends one only then. Absent on the wire when
    /// false, and from an older notedeck.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_signer: bool,
    /// Set for a tool that streams: it is run with [`Request::Subscribe`],
    /// not called, and its events are shown as the mode says. Absent on the
    /// wire for any other tool, and from an older notedeck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<StreamMode>,
    /// Whether a model may call it, as the spec says. Absent from an older
    /// notedeck, so read it through [`model_reach`](Self::model_reach),
    /// which falls back to what `kind` and `streams` say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_reach: Option<ModelReach>,
}

impl ToolInfo {
    /// Whether a model may call it: what notedeck sent, or, from a notedeck
    /// that predates the field, the same rule derived from the wire: a
    /// stream is [`Never`](ModelReach::Never), else a UI tool is
    /// [`Window`](ModelReach::Window) and a data tool
    /// [`Data`](ModelReach::Data).
    pub fn model_reach(&self) -> ModelReach {
        if let Some(reach) = self.model_reach {
            return reach;
        }
        if self.streams.is_some() {
            return ModelReach::Never;
        }
        match self.kind {
            ToolKind::Data => ModelReach::Data,
            ToolKind::Ui => ModelReach::Window,
        }
    }
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
            command: vec!["headway".to_owned(), "review".to_owned()],
            accepts_signer: false,
            streams: None,
            model_reach: None,
        }
    }

    /// A tool without a command line leaves `command` off the wire, and a
    /// tool list from a notedeck that predates the field still parses.
    #[test]
    fn command_is_optional_on_the_wire() {
        let bare = ToolInfo {
            command: Vec::new(),
            ..tool()
        };
        let line = serde_json::to_string(&bare).unwrap();
        assert!(!line.contains("command"), "{line}");
        assert_eq!(serde_json::from_str::<ToolInfo>(&line).unwrap(), bare);
    }

    #[test]
    fn requests_round_trip_with_the_version() {
        let requests = [
            Request::ListTools,
            Request::CallTool {
                name: "open".to_owned(),
                args: json!({"reference": "headway:notedeck/hill-measure-accuse"}),
                signer: None,
            },
            Request::CallTool {
                name: "headway_comment_card".to_owned(),
                args: json!({"card": "c", "body": "hi"}),
                signer: Some(Signer {
                    comment_nsec: "nsec1x".to_owned(),
                }),
            },
        ];
        for request in requests {
            let line = request.to_line();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["v"], json!(request.version()), "{line}");
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
                args: Value::Null,
                signer: None,
            })
        );
    }

    /// A call without a signer writes no `signer` key, so the wire is what
    /// it was before the field, and one with a signer reads back. Neither
    /// `Debug` prints the key.
    #[test]
    fn the_signer_is_optional_and_never_debug_printed() {
        let bare = Request::CallTool {
            name: "x".to_owned(),
            args: Value::Null,
            signer: None,
        };
        assert!(!bare.to_line().contains("signer"), "{}", bare.to_line());

        let line =
            r#"{"v":1,"type":"call_tool","name":"x","signer":{"comment_nsec":"nsec1secret"}}"#;
        let request = Request::from_line(line).unwrap();
        let Request::CallTool { signer, .. } = &request else {
            panic!("a call");
        };
        assert_eq!(signer.as_ref().unwrap().comment_nsec, "nsec1secret");
        let debug = format!("{request:?}");
        assert!(!debug.contains("nsec1secret"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    /// `accepts_signer` is off the wire when false, on it when true, and a
    /// tool list from a notedeck that predates it reads as false.
    #[test]
    fn accepts_signer_is_optional_on_the_wire() {
        let line = serde_json::to_string(&tool()).unwrap();
        assert!(!line.contains("accepts_signer"), "{line}");
        let signs = ToolInfo {
            accepts_signer: true,
            ..tool()
        };
        let line = serde_json::to_string(&signs).unwrap();
        assert!(line.contains(r#""accepts_signer":true"#), "{line}");
        assert_eq!(serde_json::from_str::<ToolInfo>(&line).unwrap(), signs);
    }

    #[test]
    fn a_request_needs_a_version() {
        let err = Request::from_line(r#"{"type":"spawn_agent","cwd":"/tmp"}"#).unwrap_err();
        assert!(
            err.contains("invalid request"),
            "the legacy spawn is gone: {err}"
        );
        let err = Request::from_line(r#"{"type":"list_tools"}"#).unwrap_err();
        assert!(err.contains("missing protocol version"), "{err}");
    }

    /// A list or a call is spelled in v1, so a notedeck that predates streams
    /// still answers it, and a subscribe in v2. A v2 list still reads; a
    /// subscribe that says v1 doesn't.
    #[test]
    fn each_request_carries_the_oldest_version_that_spells_it() {
        let subscribe = Request::Subscribe {
            name: "agentium_watch".to_owned(),
            args: json!({}),
        };
        assert_eq!(Request::ListTools.version(), 1);
        assert_eq!(subscribe.version(), STREAMS_VERSION);
        let line = subscribe.to_line();
        assert!(line.contains(r#""v":2"#), "{line}");
        assert_eq!(
            Request::versioned(&line),
            Ok(Versioned {
                v: 2,
                request: subscribe
            })
        );
        assert_eq!(
            Request::versioned(r#"{"v":2,"type":"list_tools"}"#),
            Ok(Versioned {
                v: 2,
                request: Request::ListTools
            })
        );
        let err = Request::from_line(r#"{"v":1,"type":"subscribe","name":"x"}"#).unwrap_err();
        assert!(err.contains("needs protocol version 2"), "{err}");
    }

    /// An answer is written in the version its request came with, so an nd
    /// that predates streams reads the v1 it expects.
    #[test]
    fn an_answer_carries_the_requests_version() {
        let response = Response::ok(Reply::Event {
            event: json!({"n": 1}),
            text: Some("one\n".to_owned()),
        });
        let line = response.line_as(1);
        let read = Response::from_line(&line).unwrap();
        assert_eq!(read.v, 1);
        assert_eq!(read.outcome, response.outcome);
        assert_eq!(
            Response::from_line(&response.to_line()).unwrap().v,
            PROTOCOL_VERSION
        );
    }

    /// `streams` is off the wire for a tool that answers once, and reads
    /// back for one that streams.
    #[test]
    fn streams_is_optional_on_the_wire() {
        let line = serde_json::to_string(&tool()).unwrap();
        assert!(!line.contains("streams"), "{line}");
        let streams = ToolInfo {
            streams: Some(StreamMode::Redraw),
            ..tool()
        };
        let line = serde_json::to_string(&streams).unwrap();
        assert!(line.contains(r#""streams":"redraw""#), "{line}");
        assert_eq!(serde_json::from_str::<ToolInfo>(&line).unwrap(), streams);
    }

    /// `model_reach` round-trips when sent, and a tool list from a
    /// notedeck that predates it still says what a model may reach,
    /// derived from `kind` and `streams`.
    #[test]
    fn model_reach_falls_back_to_kind_and_streams() {
        let sent = ToolInfo {
            kind: ToolKind::Data,
            model_reach: Some(ModelReach::Data),
            ..tool()
        };
        let line = serde_json::to_string(&sent).unwrap();
        assert!(line.contains(r#""model_reach":"data""#), "{line}");
        assert_eq!(serde_json::from_str::<ToolInfo>(&line).unwrap(), sent);

        let old = |kind: ToolKind, streams: Option<StreamMode>| {
            let tool = ToolInfo {
                kind,
                streams,
                model_reach: None,
                ..tool()
            };
            let line = serde_json::to_string(&tool).unwrap();
            assert!(!line.contains("model_reach"), "{line}");
            serde_json::from_str::<ToolInfo>(&line)
                .unwrap()
                .model_reach()
        };
        assert_eq!(old(ToolKind::Data, None), ModelReach::Data);
        assert_eq!(old(ToolKind::Ui, None), ModelReach::Window);
        assert_eq!(
            old(ToolKind::Data, Some(StreamMode::Append)),
            ModelReach::Never
        );
        assert_eq!(
            old(ToolKind::Ui, Some(StreamMode::Redraw)),
            ModelReach::Never
        );
    }

    #[test]
    fn another_version_or_type_is_refused() {
        let err = Request::from_line(r#"{"v":3,"type":"list_tools"}"#).unwrap_err();
        assert!(err.contains("version 3"), "{err}");
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
                text: None,
                notice: None,
            },
            Reply::Result {
                result: Value::Null,
                text: None,
                notice: None,
            },
            Reply::Result {
                result: json!([]),
                text: Some("Todo (0)\n".to_owned()),
                notice: Some("found locally in /x".to_owned()),
            },
            Reply::Intent {
                intent: IntentStatus::Applied,
            },
            Reply::Event {
                event: json!({"role": "user"}),
                text: Some("you: hi\n".to_owned()),
            },
            Reply::Event {
                event: json!(null),
                text: None,
            },
        ];
        for reply in replies {
            let response = Response::ok(reply);
            let line = response.to_line();
            assert_eq!(Response::from_line(&line), Ok(response), "{line}");
        }
        let response = Response::error("no such tool");
        assert_eq!(Response::from_line(&response.to_line()), Ok(response));
    }

    /// `text` and `notice` are optional both ways: a reply from a notedeck
    /// that predates them still reads, and one without them writes neither
    /// key.
    #[test]
    fn result_text_is_optional_on_the_wire() {
        let old = r#"{"v":1,"notedeck":"0","status":"ok","result":{"a":1}}"#;
        let Outcome::Ok(Reply::Result { result, text, .. }) =
            Response::from_line(old).unwrap().outcome
        else {
            panic!("a result");
        };
        assert_eq!((result, text), (json!({"a": 1}), None));

        let bare = Response::ok(Reply::Result {
            result: json!(1),
            text: None,
            notice: None,
        });
        assert!(!bare.to_line().contains("\"text\""), "{}", bare.to_line());
        assert!(!bare.to_line().contains("\"notice\""), "{}", bare.to_line());
        let with = Response::ok(Reply::Result {
            result: json!(1),
            text: Some("one\n".to_owned()),
            notice: None,
        });
        let value: Value = serde_json::from_str(&with.to_line()).unwrap();
        assert_eq!(value["text"], json!("one\n"));
    }

    #[test]
    fn responses_read_as_documented() {
        let line = Response::ok(Reply::Intent {
            intent: IntentStatus::Applied,
        })
        .to_line();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["v"], json!(PROTOCOL_VERSION));
        assert_eq!(value["status"], json!("ok"));
        assert_eq!(value["intent"], json!("applied"));
        assert_eq!(value["notedeck"], json!(env!("CARGO_PKG_VERSION")));

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
