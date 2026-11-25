//! The e2e harness every `cashflow` test file shares (`mod harness;`): this
//! worktree's build of the binary, run in a fresh temp `$CASHFLOW_DIR` with
//! none of the developer's `CASHFLOW_*` environment leaking in.
//!
//! Every run also gets a dead relay ([`DEAD_RELAY`]) and a temp
//! `$XDG_DATA_HOME`, so a vault command can neither publish to the
//! developer's running notedeck nor touch their real `cashflow-cli` cache.
//!
//! A later card adds a command's cases as a new test file, or to `e2e.rs`,
//! building a [`Cashflow`] with [`Cashflow::knowing`] its command so a stale
//! sibling build without it is never picked.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A relay URL nothing listens on: vault commands work offline against the
/// temp cache. A test that wants a relay passes `--relay`.
pub const DEAD_RELAY: &str = "ws://127.0.0.1:1";

/// A `cashflow` binary and the temp data dir its runs use.
pub struct Cashflow {
    bin: PathBuf,
    /// Dropped with the harness, taking the data dir with it.
    tmp: TempDir,
}

impl Cashflow {
    /// A harness for a build that has the `config` command.
    // Each test file compiles its own copy of the harness and uses only part.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::knowing("config")
    }

    /// A harness for a build whose help has a page for `command`. See
    /// [`bin_testing::worktree_bin`] for why this isn't just
    /// `env!("CARGO_BIN_EXE_cashflow")`.
    ///
    /// The probe looks for the page's own header, not just a zero exit:
    /// cashflow_cli's unit-test binary is a `deps/cashflow-<hash>` built
    /// from the same sources, and it takes `help <command>` as test filters
    /// and exits 0.
    pub fn knowing(command: &str) -> Self {
        let header = format!("cashflow {command} —");
        let bin = bin_testing::worktree_bin(
            Path::new(env!("CARGO_BIN_EXE_cashflow")),
            "cashflow_cli",
            |bin| {
                Command::new(bin)
                    .args(["help", command])
                    .output()
                    .is_ok_and(|out| {
                        out.status.success()
                            && String::from_utf8_lossy(&out.stderr).starts_with(&header)
                    })
            },
        );
        let tmp = tempfile::tempdir().expect("temp dir");
        Self { bin, tmp }
    }

    /// The `$CASHFLOW_DIR` every run gets.
    pub fn dir(&self) -> &Path {
        self.tmp.path()
    }

    /// The data dir every run resolves (`dirs::data_dir`), so the CLI's
    /// default nostrdb (`<data-dir>/cashflow-cli`) is under the temp dir,
    /// shared by every run of this harness: `$XDG_DATA_HOME` on Linux, and
    /// `$HOME/Library/Application Support` on macOS, which ignores XDG.
    pub fn data_home(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home().join("Library").join("Application Support")
        } else {
            self.tmp.path().join("xdg")
        }
    }

    /// The `$HOME` every run gets.
    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    /// A `cashflow` command with `$CASHFLOW_DIR` set to [`dir`](Self::dir),
    /// `$CASHFLOW_RELAY` to [`DEAD_RELAY`], `$XDG_DATA_HOME` to
    /// [`data_home`](Self::data_home), `$HOME` to a temp dir, and every other `CASHFLOW_*` variable
    /// and `$AGENTIUM_SESSION` removed, ready for more `.env`s. Inside an
    /// agent session the runner's own ref would otherwise be stamped on
    /// every rule a test adds.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        for (var, _) in std::env::vars_os() {
            if var.to_string_lossy().starts_with("CASHFLOW_") {
                command.env_remove(&var);
            }
        }
        command.env_remove("AGENTIUM_SESSION");
        command.env("CASHFLOW_DIR", self.dir());
        command.env("CASHFLOW_RELAY", DEAD_RELAY);
        command.env("XDG_DATA_HOME", self.data_home());
        // macOS's data dir is `$HOME/Library/Application Support`, which
        // ignores $XDG_DATA_HOME: without this every run there used the
        // developer's real `cashflow-cli` and `headway-cli` caches, and read
        // back what earlier runs wrote into them.
        command.env("HOME", self.home());
        // Windows reads its data dir from the shell's known folders, not the
        // environment, so every run there shared the runner's real
        // `%APPDATA%\cashflow-cli` cache, across tests running in parallel.
        // A later `--db` of the test's own still wins.
        if cfg!(windows) {
            command
                .arg("--db")
                .arg(self.data_home().join("cashflow-cli"));
        }
        command
    }

    /// The args that point `goals` at the headway cache under
    /// [`data_home`](Self::data_home): none where the default path lands
    /// there, `--headway-db` on Windows, whose data dir no environment
    /// variable moves.
    #[allow(dead_code)]
    pub fn headway_db_args(&self) -> Vec<String> {
        if !cfg!(windows) {
            return Vec::new();
        }
        let db = self.data_home().join("headway-cli");
        vec!["--headway-db".into(), db.to_string_lossy().into_owned()]
    }

    /// Run `cashflow <args>`.
    #[allow(dead_code)]
    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Output {
        self.command().args(args).output().expect("run cashflow")
    }

    /// Run `cashflow <args>` with `input` on stdin, then closed.
    #[allow(dead_code)]
    pub fn run_with_stdin<S: AsRef<OsStr>>(&self, args: &[S], input: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cashflow");
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
        drop(stdin);
        child.wait_with_output().expect("run cashflow")
    }

    /// Run `cashflow <args>`, panic unless it succeeds, and return its stdout.
    #[allow(dead_code)]
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
