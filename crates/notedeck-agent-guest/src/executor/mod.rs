//! The six coding tools, run where this process runs: in notedeck on the
//! host (agentium-core's `Runner::LocalExecutor`), or in the
//! `notedeck-agent` binary inside a guest ([`serve`](crate::serve)).
//!
//! ```text
//! CodingCall ─▶ Executor::run ─▶ text for the model, or an ExecError it reads instead
//!                  │
//!                  Read · Write · Edit ── std::fs, in this process
//!                  Grep · Glob ────────── ripgrep's walker: skips what .gitignore skips
//!                  Bash ──────────────── `bash -c` in its own process group;
//!                                        a timeout or cancel kills the group
//! ```
//!
//! A runner gets a [`CodingCall`] (already parsed, so already one of the six
//! tools with arguments it knows) and answers the text the model reads, or
//! an [`ExecError`]. What the user sees of a call (the tool row, the diff)
//! comes from the call itself, not from here.
//!
//! ## On the host, in process
//!
//! On the host notedeck calls [`Executor::run`] directly rather than
//! spawning the binary: a separate process would have the user's
//! privileges either way, and would be one more thing to find at run time.
//! So the host does what Claude Code does: file tools run in notedeck, and
//! only Bash is a child process. The binary is for a guest, where the tools
//! have to run on the far side of a VM boundary.
//!
//! ## Not a boundary
//!
//! Nothing here confines a call. A path may be absolute and anywhere, Bash
//! can do what the process can, and [`Executor::root`] is only where a
//! relative path and a command start. On the host the session's permission
//! mode is the only guard; in a guest, the VM is.
//!
//! ## A Bash child's environment
//!
//! Bash inherits this process's environment, as Claude Code's does, with
//! what [`ChildEnv`] adds and removes. On the host, notedeck's own
//! environment holds things a session must not see (a key it was launched
//! with) and lacks things a session needs (its own `AGENTIUM_SESSION`), so
//! the caller sets both through [`Executor::with_env`].
//!
//! ## What we don't have
//!
//! Claude Code's tools carry years of ergonomics these don't: no fuzzy edit
//! matching, no image or notebook reading, no persistent shell (each Bash
//! call starts afresh in the root, so a `cd` doesn't carry to the next
//! call), no background commands. Read numbers its lines as `cat -n` does,
//! which is the shape models are tuned to quote from.

mod bash;
mod files;
mod search;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};

use notedeck_agent_proto::call::CodingCall;
use notedeck_agent_proto::wire::OutputStream;

/// Watches a call's output as it is written: which stream, and the next
/// piece of it, always ending on a character boundary. Only Bash calls it,
/// and only for its first [`STREAM_BUDGET`] bytes per stream.
pub type OnOutput<'a> = &'a (dyn Fn(OutputStream, &str) + Send + Sync);

/// How many bytes of each stream a Bash call hands to [`OnOutput`]. Past
/// it the command runs on and its answer still keeps its head and tail;
/// only the live view stops, so a flood (`yes`, a runaway log) can't push
/// megabytes across the guest boundary that nobody will read.
pub const STREAM_BUDGET: usize = 512 * 1024;

/// Runs coding tool calls against one working tree.
#[derive(Debug, Clone)]
pub struct Executor {
    root: PathBuf,
    /// How a Bash child's environment differs from this process's.
    env: ChildEnv,
}

/// How a Bash child's environment differs from the process it starts
/// from: variables it gets set, and variables it doesn't inherit.
#[derive(Debug, Clone, Default)]
pub struct ChildEnv {
    /// Set in the child, over any inherited value.
    pub set: BTreeMap<String, String>,
    /// Left out of the child even when this process has them. A name in
    /// `set` too is set: removal only stops inheritance.
    pub remove: Vec<String>,
}

/// Why a coding call didn't do what it asked. Its text is what the model
/// reads instead of an answer.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// Reading or writing a file failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file isn't UTF-8 text.
    #[error("{} isn't a text file", .0.display())]
    NotText(PathBuf),
    /// A file tool was pointed at a directory.
    #[error("{} is a directory: use Glob, or ls through Bash, to see what's in it", .0.display())]
    IsDirectory(PathBuf),
    /// Edit's `old_string` isn't in the file.
    #[error("old_string isn't in {}: it must match the file exactly, whitespace included", .0.display())]
    NoMatch(PathBuf),
    /// Edit's `old_string` is in the file more than once and `replace_all`
    /// isn't set.
    #[error(
        "old_string occurs {count} times in {}: add surrounding lines to make it unique, \
         or set replace_all",
        path.display()
    )]
    Ambiguous { path: PathBuf, count: usize },
    /// Edit's `old_string` is empty.
    #[error("old_string is empty: use Write to create a file")]
    EmptyOldString,
    /// Edit's `old_string` and `new_string` are the same.
    #[error("old_string and new_string are the same, so the edit would change nothing")]
    NoChange,
    /// Grep's pattern isn't a regular expression.
    #[error("bad regular expression: {0}")]
    Pattern(#[from] regex::Error),
    /// Grep's or Glob's glob isn't a glob.
    #[error("bad glob {pattern:?}: {reason}")]
    Glob { pattern: String, reason: String },
    /// The shell (`/bin/sh`) couldn't be started.
    #[error("couldn't start /bin/sh: {0}")]
    Spawn(std::io::Error),
    /// A command ran past its timeout, and it and its children were killed.
    #[error("the command ran past {ms} ms and was killed. Its output so far:\n{output}")]
    TimedOut { ms: u64, output: String },
    /// The call was cancelled before it finished.
    #[error("cancelled")]
    Cancelled,
}

impl Executor {
    /// An executor whose relative paths and commands start in `root`, the
    /// session's working tree.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            env: ChildEnv::default(),
        }
    }

    /// The same executor, with `env` applied to every Bash child.
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = env;
        self
    }

    /// Where a relative path and a Bash command start.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run `call` and answer what the model reads. A Bash call stops, its
    /// whole process group killed, when `cancelled` resolves; the file and
    /// search tools finish on their own.
    pub async fn run(
        &self,
        call: &CodingCall,
        cancelled: impl Future<Output = ()>,
    ) -> Result<String, ExecError> {
        self.run_streaming(call, cancelled, &|_, _| {}).await
    }

    /// [`Executor::run`], also handing a Bash call's output to `on_output`
    /// as the command writes it. The answer is the same either way.
    pub async fn run_streaming(
        &self,
        call: &CodingCall,
        cancelled: impl Future<Output = ()>,
        on_output: OnOutput<'_>,
    ) -> Result<String, ExecError> {
        match call {
            CodingCall::Read(r) => files::read(&self.resolve(&r.file_path), r.offset, r.limit),
            CodingCall::Write(w) => files::write(&self.resolve(&w.file_path), &w.content),
            CodingCall::Edit(e) => files::edit(
                &self.resolve(&e.file_path),
                &e.old_string,
                &e.new_string,
                e.replace_all,
            ),
            CodingCall::Grep(g) => search::grep(g, &self.resolve_dir(g.path.as_deref())),
            CodingCall::Glob(g) => search::glob(&g.pattern, &self.resolve_dir(g.path.as_deref())),
            CodingCall::Bash(b) => bash::bash(&self.root, &self.env, b, cancelled, on_output).await,
        }
    }

    /// `path` as the call means it: absolute as given, relative from the
    /// root.
    fn resolve(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    /// A search's optional `path`: the root when left out.
    fn resolve_dir(&self, path: Option<&str>) -> PathBuf {
        path.map_or_else(|| self.root.clone(), |p| self.resolve(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck_agent_proto::call::{ReadInput, WriteInput};
    use std::future::pending;

    /// A relative path starts at the root and an absolute one is taken as
    /// given, so a model that drops the root still lands in the tree.
    #[tokio::test]
    async fn a_relative_path_starts_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let exec = Executor::new(dir.path());
        let write = CodingCall::Write(WriteInput {
            file_path: "src/new.rs".into(),
            content: "fn main() {}\n".into(),
        });
        exec.run(&write, pending()).await.unwrap();
        assert!(dir.path().join("src/new.rs").is_file());

        let read = CodingCall::Read(ReadInput {
            file_path: dir.path().join("src/new.rs").display().to_string(),
            offset: None,
            limit: None,
        });
        let text = exec.run(&read, pending()).await.unwrap();
        assert_eq!(text, "     1\tfn main() {}\n");
    }

    /// A Bash child gets what [`ChildEnv`] sets and loses what it
    /// removes; the rest it inherits. Unix only: Bash runs `/bin/sh`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_bash_child_gets_its_env() {
        let dir = tempfile::tempdir().unwrap();
        let env = ChildEnv {
            set: BTreeMap::from([("AGENTIUM_SESSION".into(), "agentium:a-b-c".into())]),
            remove: vec!["HOME".into()],
        };
        let exec = Executor::new(dir.path()).with_env(env);
        let call = CodingCall::Bash(notedeck_agent_proto::call::BashInput {
            command: "echo \"$AGENTIUM_SESSION ${HOME-unset} ${PATH:+path}\"".into(),
            description: None,
            timeout: None,
        });
        let text = exec.run(&call, pending()).await.unwrap();
        assert!(text.contains("agentium:a-b-c unset path"), "{text}");
    }

    /// A call the model sends, as Claude Code's tool_use block, runs
    /// through parse and run with nothing in between.
    #[tokio::test]
    async fn a_tool_use_block_runs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let exec = Executor::new(dir.path());
        let call = CodingCall::parse(
            "Edit",
            &serde_json::json!({
                "file_path": "a.txt", "old_string": "hello", "new_string": "goodbye"
            }),
        )
        .unwrap();
        exec.run(&call, pending()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "goodbye\n"
        );
    }
}
