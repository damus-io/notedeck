//! The Coding toolset's tools: Read, Write, Edit, Grep, Glob and Bash.
//!
//! [`Catalog::build`](super::Catalog::build) offers [`coding_tools`] to a
//! spec with a [`Toolset::Coding`](super::Toolset::Coding), and a runner on
//! the host or in a guest executes each call, parsed as a [`CodingCall`].
//!
//! ## The shapes are Claude Code's, on purpose
//!
//! Each tool's name and argument names are a compatibility surface, not a
//! design choice. Dave's coding-session UI dispatches on Claude Code's tool
//! names and reads the arguments by Claude Code's field names: the diff
//! renderer ([`FileUpdate::from_tool_call`]: Edit's `file_path`,
//! `old_string`, `new_string`; Write's `content`), the tool row summaries
//! (`format_tool_summary` in notedeck_dave), auto-accept rules (Bash's
//! `command`) and the permission card. A call of ours serializes exactly as
//! the claude CLI's would, so a session looks the same in the UI whichever
//! of them produced it, and none of those readers learns a second
//! vocabulary.
//!
//! ## A closed surface
//!
//! Six tools and no string-keyed escape hatch: a call is one of
//! [`CodingCall`]'s variants or it is refused, and a field the tool doesn't
//! take is refused too rather than dropped, so a model that asks for
//! behaviour we don't have hears so. That keeps the surface auditable, and
//! each tool declares its [`Capability`] so the approval table and the
//! session's permission mode apply:
//!
//! | tool | class |
//! |---|---|
//! | Read, Grep, Glob | `Observe` |
//! | Write, Edit | `MutateWorkspace` |
//! | Bash | `Shell`: writes anything, reaches the network |
//!
//! [`network_tools`] is derived from these classes, so a tool added here
//! that reaches the network is one [`AgentSpec::validate`](super::AgentSpec::validate)
//! keeps away from private data without a second list to update.
//!
//! ## What we don't have
//!
//! Our executor is not Claude Code's. These are the contracts, not years of
//! tool ergonomics: no line-numbered Read, no fuzzy edit matching, no image
//! or notebook handling, no smart truncation. Each argument here is one a
//! runner must honour; a gap is closed by adding an argument deliberately,
//! not discovered in the field.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::file_update::{FileUpdate, FileUpdateType};
use crate::tool::{Capability, ToolArg, ToolArgType, ToolSpec};

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

/// Read a file's text.
pub fn read() -> ToolSpec {
    ToolSpec::new(
        READ,
        "Read a file from the working tree and return its text. For a long file, pass \
         offset and limit to read part of it.",
        vec![
            ToolArg::new(
                "file_path",
                ToolArgType::String,
                "The file's absolute path.",
            )
            .required(true),
            ToolArg::new(
                "offset",
                ToolArgType::Integer,
                "The 1-based line to start reading from.",
            ),
            ToolArg::new("limit", ToolArgType::Integer, "How many lines to read."),
        ],
    )
    .read_only()
    .capability(Capability::Observe)
}

/// Write a whole file.
pub fn write() -> ToolSpec {
    ToolSpec::new(
        WRITE,
        "Write a file, replacing it if it exists. Prefer Edit to change part of an \
         existing file.",
        vec![
            ToolArg::new(
                "file_path",
                ToolArgType::String,
                "The file's absolute path.",
            )
            .required(true),
            ToolArg::new("content", ToolArgType::String, "The file's new content.").required(true),
        ],
    )
    .capability(Capability::MutateWorkspace)
}

/// Replace exact text in a file.
pub fn edit() -> ToolSpec {
    ToolSpec::new(
        EDIT,
        "Replace exact text in a file. old_string must match the file exactly, \
         whitespace included, and occur once unless replace_all is set.",
        vec![
            ToolArg::new(
                "file_path",
                ToolArgType::String,
                "The file's absolute path.",
            )
            .required(true),
            ToolArg::new("old_string", ToolArgType::String, "The text to replace.").required(true),
            ToolArg::new(
                "new_string",
                ToolArgType::String,
                "The text to put in its place.",
            )
            .required(true),
            ToolArg::new(
                "replace_all",
                ToolArgType::Boolean,
                "Replace every occurrence of old_string, not just one.",
            ),
        ],
    )
    .capability(Capability::MutateWorkspace)
}

/// Search file contents with a regular expression.
pub fn grep() -> ToolSpec {
    ToolSpec::new(
        GREP,
        "Search file contents with a regular expression. Returns the matching files by \
         default; set output_mode to see the matching lines or counts.",
        vec![
            ToolArg::new("pattern", ToolArgType::String, "The regular expression.").required(true),
            ToolArg::new(
                "path",
                ToolArgType::String,
                "The file or directory to search; the working tree when left out.",
            ),
            ToolArg::new(
                "glob",
                ToolArgType::String,
                "Search only files whose path matches this glob, e.g. \"*.rs\".",
            ),
            ToolArg::new(
                "output_mode",
                ToolArgType::Enum(vec!["files_with_matches", "content", "count"]),
                "What to return: the matching files, the matching lines, or a count per file.",
            ),
            ToolArg::new("-i", ToolArgType::Boolean, "Match case-insensitively."),
            ToolArg::new(
                "head_limit",
                ToolArgType::Integer,
                "Return at most this many results.",
            ),
        ],
    )
    .read_only()
    .capability(Capability::Observe)
}

/// Find files by name.
pub fn glob() -> ToolSpec {
    ToolSpec::new(
        GLOB,
        "Find files whose path matches a glob pattern, e.g. \"src/**/*.rs\".",
        vec![
            ToolArg::new("pattern", ToolArgType::String, "The glob pattern.").required(true),
            ToolArg::new(
                "path",
                ToolArgType::String,
                "The directory to search; the working tree when left out.",
            ),
        ],
    )
    .read_only()
    .capability(Capability::Observe)
}

/// Run a shell command.
pub fn bash() -> ToolSpec {
    ToolSpec::new(
        BASH,
        "Run a shell command in the working tree and return its output and exit status.",
        vec![
            ToolArg::new("command", ToolArgType::String, "The command to run.").required(true),
            ToolArg::new(
                "description",
                ToolArgType::String,
                "What the command does, in a few words, for the user reading along.",
            ),
            ToolArg::new(
                "timeout",
                ToolArgType::Integer,
                "How long it may run, in milliseconds.",
            ),
        ],
    )
    .capability(Capability::Shell)
}

/// The Coding toolset's tools, in the order the model is shown them.
pub fn coding_tools() -> [ToolSpec; 6] {
    [read(), write(), edit(), grep(), glob(), bash()]
}

/// The coding tools that can reach the network: those whose class
/// [reaches it](Capability::reaches_network). What a spec must make
/// impossible before a coding toolset may sit beside private data.
pub fn network_tools() -> impl Iterator<Item = ToolSpec> {
    coding_tools()
        .into_iter()
        .filter(|t| t.capability_class().reaches_network())
}

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

    /// The file change this call makes, for the diff renderer: what an
    /// executed Edit or Write's `ExecutedTool::file_update` carries. The
    /// same value [`FileUpdate::from_tool_call`] reads from
    /// [`input`](Self::input), built without the round trip.
    pub fn file_update(&self) -> Option<FileUpdate> {
        match self {
            Self::Edit(e) => Some(FileUpdate::new(
                e.file_path.clone(),
                FileUpdateType::Edit {
                    old_string: e.old_string.clone(),
                    new_string: e.new_string.clone(),
                },
            )),
            Self::Write(w) => Some(FileUpdate::new(
                w.file_path.clone(),
                FileUpdateType::Write {
                    content: w.content.clone(),
                },
            )),
            Self::Read(_) | Self::Grep(_) | Self::Glob(_) | Self::Bash(_) => None,
        }
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

    fn spec_of(name: &str) -> ToolSpec {
        coding_tools()
            .into_iter()
            .find(|t| t.name() == name)
            .unwrap()
    }

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        keys
    }

    /// The schema the model is shown and the struct a call parses into
    /// name the same fields, so nothing the model is told about is dropped
    /// and nothing it may send is left undescribed.
    #[test]
    fn each_schema_names_exactly_its_inputs_fields() {
        for call in every_argument() {
            let params = spec_of(call.name()).json_schema();
            assert_eq!(
                keys(&params["properties"]),
                keys(&call.input()),
                "{}",
                call.name()
            );
            // And the schema's required fields are all a call needs.
            let required: serde_json::Map<String, Value> = params["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| {
                    let k = k.as_str().unwrap();
                    (k.to_owned(), call.input()[k].clone())
                })
                .collect();
            CodingCall::parse(call.name(), &Value::Object(required))
                .unwrap_or_else(|e| panic!("{}: {e}", call.name()));
        }
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

    /// The diff renderer reads an Edit or Write of ours exactly as it reads
    /// the CLI's, and the call's own file update is the same one.
    #[test]
    fn the_diff_renderer_reads_our_edits_and_writes() {
        for call in every_argument() {
            let read_back = FileUpdate::from_tool_call(call.name(), &call.input());
            let ours = call.file_update();
            assert_eq!(
                read_back.as_ref().map(|u| u.to_wire()),
                ours.as_ref().map(|u| u.to_wire()),
                "{}",
                call.name()
            );
            assert_eq!(
                ours.is_some(),
                matches!(call, CodingCall::Edit(_) | CodingCall::Write(_))
            );
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

    /// Reads observe, edits mutate the workspace, and the shell is its own
    /// class: the one tool that reaches the network.
    #[test]
    fn each_tool_declares_its_class() {
        let class = |name| spec_of(name).capability_class();
        for name in [READ, GREP, GLOB] {
            assert_eq!(class(name), Capability::Observe, "{name}");
        }
        for name in [WRITE, EDIT] {
            assert_eq!(class(name), Capability::MutateWorkspace, "{name}");
        }
        assert_eq!(class(BASH), Capability::Shell);
        let network: Vec<&str> = network_tools().map(|t| t.name()).collect();
        assert_eq!(network, [BASH]);
    }
}
