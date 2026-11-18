//! The six coding tools' calls: what the model asks for, and what crosses
//! to whichever executor runs it.
//!
//! Each tool's name and argument names are Claude Code's, on purpose: a
//! call of ours serializes exactly as the claude CLI's `tool_use` block
//! would, so Dave's UI (the diff renderer, the tool rows, the permission
//! card) reads one vocabulary whichever agent produced the call.
//! `agentium_core::agent_spec::coding` says why at length, and holds the
//! tool descriptions the model is shown.
//!
//! The surface is closed: a call is one of [`CodingCall`]'s variants or it
//! is refused, and an argument the tool doesn't take is refused too, rather
//! than dropped.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Read's name, as Claude Code spells it.
pub const READ: &str = "Read";
/// Write's name, as Claude Code spells it.
pub const WRITE: &str = "Write";
/// Edit's name, as Claude Code spells it.
pub const EDIT: &str = "Edit";
/// Grep's name, as Claude Code spells it.
pub const GREP: &str = "Grep";
/// Glob's name, as Claude Code spells it.
pub const GLOB: &str = "Glob";
/// Bash's name, as Claude Code spells it.
pub const BASH: &str = "Bash";

/// Read's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// Write's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteInput {
    pub file_path: String,
    pub content: String,
}

/// Edit's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_all: bool,
}

/// What a Grep call returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrepOutputMode {
    /// The paths of the files that match.
    #[default]
    FilesWithMatches,
    /// The matching lines.
    Content,
    /// How many lines match, per file.
    Count,
}

/// Grep's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrepInput {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_mode: Option<GrepOutputMode>,
    /// Claude Code's `-i`.
    #[serde(rename = "-i", default, skip_serializing_if = "std::ops::Not::not")]
    pub case_insensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_limit: Option<u64>,
}

/// Glob's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobInput {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Bash's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BashInput {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Milliseconds, as Claude Code's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// One call of a coding tool, parsed. It serializes as a Claude Code
/// `tool_use` block's `name` and `input`, so `serde_json::to_value` of an
/// Edit is `{"name":"Edit","input":{"file_path":…,"old_string":…,…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", content = "input")]
pub enum CodingCall {
    Read(ReadInput),
    Write(WriteInput),
    Edit(EditInput),
    Grep(GrepInput),
    Glob(GlobInput),
    Bash(BashInput),
}

/// Why a model's call isn't a [`CodingCall`].
#[derive(Debug, thiserror::Error)]
pub enum CodingCallError {
    /// No coding tool has that name.
    #[error("there is no coding tool named {0:?}")]
    UnknownTool(String),
    /// The arguments don't fit the tool: a field missing, mistyped, or one
    /// the tool doesn't take.
    #[error("bad arguments to {tool}: {source}")]
    BadInput {
        tool: &'static str,
        source: serde_json::Error,
    },
}

impl CodingCall {
    /// The call of the tool `name` with the arguments `input`, as the model
    /// sent them.
    pub fn parse(name: &str, input: &Value) -> Result<Self, CodingCallError> {
        fn arg<T: serde::de::DeserializeOwned>(
            tool: &'static str,
            input: &Value,
        ) -> Result<T, CodingCallError> {
            T::deserialize(input).map_err(|source| CodingCallError::BadInput { tool, source })
        }
        Ok(match name {
            READ => Self::Read(arg(READ, input)?),
            WRITE => Self::Write(arg(WRITE, input)?),
            EDIT => Self::Edit(arg(EDIT, input)?),
            GREP => Self::Grep(arg(GREP, input)?),
            GLOB => Self::Glob(arg(GLOB, input)?),
            BASH => Self::Bash(arg(BASH, input)?),
            _ => return Err(CodingCallError::UnknownTool(name.to_owned())),
        })
    }

    /// The tool's name, as Claude Code spells it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read(_) => READ,
            Self::Write(_) => WRITE,
            Self::Edit(_) => EDIT,
            Self::Grep(_) => GREP,
            Self::Glob(_) => GLOB,
            Self::Bash(_) => BASH,
        }
    }

    /// The call's arguments as Claude Code's tool input: what a
    /// `ToolCalls`/`ExecutedTool` event carries and the UI reads.
    pub fn input(&self) -> Value {
        let input = match self {
            Self::Read(i) => serde_json::to_value(i),
            Self::Write(i) => serde_json::to_value(i),
            Self::Edit(i) => serde_json::to_value(i),
            Self::Grep(i) => serde_json::to_value(i),
            Self::Glob(i) => serde_json::to_value(i),
            Self::Bash(i) => serde_json::to_value(i),
        };
        input.expect("a coding input is plain strings, numbers and bools")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One call of every tool with every argument set.
    fn every_argument() -> [CodingCall; 6] {
        [
            CodingCall::Read(ReadInput {
                file_path: "/w/src/lib.rs".into(),
                offset: Some(10),
                limit: Some(20),
            }),
            CodingCall::Write(WriteInput {
                file_path: "/w/new.rs".into(),
                content: "fn main() {}\n".into(),
            }),
            CodingCall::Edit(EditInput {
                file_path: "/w/src/lib.rs".into(),
                old_string: "a".into(),
                new_string: "b".into(),
                replace_all: true,
            }),
            CodingCall::Grep(GrepInput {
                pattern: "fn main".into(),
                path: Some("/w".into()),
                glob: Some("*.rs".into()),
                output_mode: Some(GrepOutputMode::Content),
                case_insensitive: true,
                head_limit: Some(5),
            }),
            CodingCall::Glob(GlobInput {
                pattern: "**/*.rs".into(),
                path: Some("/w".into()),
            }),
            CodingCall::Bash(BashInput {
                command: "cargo test".into(),
                description: Some("Run the tests".into()),
                timeout: Some(60_000),
            }),
        ]
    }

    /// A call round-trips through the name and input the model sends.
    #[test]
    fn a_call_round_trips_through_name_and_input() {
        for call in every_argument() {
            assert_eq!(CodingCall::parse(call.name(), &call.input()).unwrap(), call);
        }
    }

    /// Calls as the claude CLI writes them in a transcript parse unchanged,
    /// and ours serialize as the same `tool_use` name and input.
    #[test]
    fn claude_codes_own_tool_use_blocks_parse() {
        let blocks = [
            json!({"name": "Edit", "input": {
                "file_path": "/w/a.rs", "old_string": "x", "new_string": "y"}}),
            json!({"name": "Bash", "input": {
                "command": "ls", "description": "List files"}}),
            json!({"name": "Grep", "input": {
                "pattern": "todo", "-i": true, "output_mode": "count"}}),
            json!({"name": "Read", "input": {"file_path": "/w/a.rs"}}),
        ];
        for block in blocks {
            let call: CodingCall = serde_json::from_value(block.clone()).unwrap();
            assert_eq!(
                CodingCall::parse(block["name"].as_str().unwrap(), &block["input"]).unwrap(),
                call
            );
            assert_eq!(serde_json::to_value(&call).unwrap(), block);
            assert_eq!(call.input(), block["input"]);
        }
    }

    /// The surface is closed: an unknown tool, or an argument the tool
    /// doesn't take, is refused rather than ignored.
    #[test]
    fn an_unknown_tool_or_argument_is_refused() {
        assert!(matches!(
            CodingCall::parse("NotebookEdit", &json!({})),
            Err(CodingCallError::UnknownTool(_))
        ));
        assert!(matches!(
            CodingCall::parse(BASH, &json!({"command": "ls", "run_in_background": true})),
            Err(CodingCallError::BadInput { tool: BASH, .. })
        ));
        assert!(matches!(
            CodingCall::parse(EDIT, &json!({"file_path": "/w/a.rs", "old_string": "x"})),
            Err(CodingCallError::BadInput { tool: EDIT, .. })
        ));
    }
}
