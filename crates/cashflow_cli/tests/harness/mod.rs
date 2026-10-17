//! The e2e harness every `cashflow` test file shares (`mod harness;`): this
//! worktree's build of the binary, run in a fresh temp `$CASHFLOW_DIR` with
//! none of the developer's `CASHFLOW_*` environment leaking in.
//!
//! A later card adds a command's cases as a new test file, or to `e2e.rs`,
//! building a [`Cashflow`] with [`Cashflow::knowing`] its command so a stale
//! sibling build without it is never picked.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A `cashflow` binary and the temp data dir its runs use.
pub struct Cashflow {
    bin: PathBuf,
    /// Dropped with the harness, taking the data dir with it.
    tmp: TempDir,
}

impl Cashflow {
    /// A harness for a build that has the `config` command.
    pub fn new() -> Self {
        Self::knowing("config")
    }

    /// A harness for a build whose help has a page for `command`. See
    /// [`bin_testing::worktree_bin`] for why this isn't just
    /// `env!("CARGO_BIN_EXE_cashflow")`.
    pub fn knowing(command: &str) -> Self {
        let bin = bin_testing::worktree_bin(
            Path::new(env!("CARGO_BIN_EXE_cashflow")),
            "cashflow_cli",
            |bin| {
                Command::new(bin)
                    .args(["help", command])
                    .output()
                    .is_ok_and(|out| out.status.success())
            },
        );
        let tmp = tempfile::tempdir().expect("temp dir");
        Self { bin, tmp }
    }

    /// The `$CASHFLOW_DIR` every run gets.
    pub fn dir(&self) -> &Path {
        self.tmp.path()
    }

    /// A `cashflow` command with `$CASHFLOW_DIR` set to [`dir`](Self::dir) and
    /// every other `CASHFLOW_*` variable removed, ready for more `.env`s.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        for (var, _) in std::env::vars_os() {
            if var.to_string_lossy().starts_with("CASHFLOW_") {
                command.env_remove(&var);
            }
        }
        command.env("CASHFLOW_DIR", self.dir());
        command
    }

    /// Run `cashflow <args>`.
    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Output {
        self.command().args(args).output().expect("run cashflow")
    }

    /// Run `cashflow <args>`, panic unless it succeeds, and return its stdout.
    pub fn ok<S: AsRef<OsStr>>(&self, args: &[S]) -> String {
        expect_success(self.run(args))
    }
}

/// The stdout of a run that must have succeeded; panics with both streams
/// otherwise.
pub fn expect_success(out: Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "cashflow failed ({}):\nstdout:\n{stdout}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// The stderr of a run that must have failed; panics otherwise.
pub fn expect_failure(out: Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "cashflow unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}
