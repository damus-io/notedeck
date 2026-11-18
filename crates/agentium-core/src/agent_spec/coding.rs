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
//! tool ergonomics: no fuzzy edit matching, no image or notebook handling,
//! no persistent shell. Each argument here is one a runner must honour
//! ([`local_executor`](super::local_executor) runs them on the host, and the
//! same executor runs them in a sandbox guest); a gap is closed by adding an
//! argument deliberately, not discovered in the field.
//!
//! The call types themselves ([`CodingCall`] and its inputs) live in
//! `notedeck-agent-proto` and are re-exported here: the guest is built from
//! that crate too, so both ends read one definition.

use crate::file_update::{FileUpdate, FileUpdateType};
use crate::tool::{Capability, ToolArg, ToolArgType, ToolSpec};

pub use notedeck_agent_proto::call::{BASH, EDIT, GLOB, GREP, READ, WRITE};

/// Read a file's text.
pub fn read() -> ToolSpec {
    ToolSpec::new(
        READ,
        "Read a file and return its lines, numbered as cat -n numbers them. For a long \
         file, pass offset and limit to read part of it.",
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

pub use notedeck_agent_proto::call::{
    BashInput, CodingCall, CodingCallError, EditInput, GlobInput, GrepInput, GrepOutputMode,
    ReadInput, WriteInput,
};

/// The file change `call` makes, for the diff renderer: what an
/// executed Edit or Write's `ExecutedTool::file_update` carries. The
/// same value [`FileUpdate::from_tool_call`] reads from
/// [`input`](CodingCall::input), built without the round trip.
pub fn file_update(call: &CodingCall) -> Option<FileUpdate> {
    match call {
        CodingCall::Edit(e) => Some(FileUpdate::new(
            e.file_path.clone(),
            FileUpdateType::Edit {
                old_string: e.old_string.clone(),
                new_string: e.new_string.clone(),
            },
        )),
        CodingCall::Write(w) => Some(FileUpdate::new(
            w.file_path.clone(),
            FileUpdateType::Write {
                content: w.content.clone(),
            },
        )),
        CodingCall::Read(_) | CodingCall::Grep(_) | CodingCall::Glob(_) | CodingCall::Bash(_) => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

    /// The diff renderer reads an Edit or Write of ours exactly as it reads
    /// the CLI's, and the call's own file update is the same one.
    #[test]
    fn the_diff_renderer_reads_our_edits_and_writes() {
        for call in every_argument() {
            let read_back = FileUpdate::from_tool_call(call.name(), &call.input());
            let ours = file_update(&call);
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
