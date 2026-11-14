//! Reaching notedeck over its socket, and starting one when none is running.
//!
//! When nothing listens on the socket, nd starts `notedeck --headless
//! --serve` itself, detached, and waits for its tools to list. That notedeck
//! runs the data apps only, and runs no agents. It exits once its socket goes
//! idle, and it never takes the socket from a notedeck that is running. So a
//! shell or an agent can call a tool without first asking whether the app is
//! open. `--no-start`, or `$ND_NO_START`, keeps the old answer: exit 2.

use crate::{Failure, NOT_RUNNING};
use notedeck_ipc::{Request, Response};
use std::path::PathBuf;
#[cfg(unix)]
use {
    notedeck_ipc::{Client, Outcome, Reply},
    std::io::ErrorKind,
    std::path::Path,
    std::process::{Child, Command, Stdio},
    std::time::{Duration, Instant},
};

/// Names the notedeck binary nd starts, in place of `notedeck` on `PATH`.
pub const BIN_ENV: &str = "NOTEDECK_BIN";

/// Set (to anything but empty or `0`) to keep nd from starting a notedeck,
/// as `--no-start` does.
pub const NO_START_ENV: &str = "ND_NO_START";

#[cfg(unix)]
/// How long nd waits for a notedeck it started to list its tools.
const START_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(unix)]
/// The first pause between polls of the socket; each doubles, up to
/// [`MAX_POLL`].
const FIRST_POLL: Duration = Duration::from_millis(25);
#[cfg(unix)]
const MAX_POLL: Duration = Duration::from_millis(500);

/// Whether nd may start a notedeck when none answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Start one, the default.
    IfNeeded,
    /// `--no-start` or `$ND_NO_START`: exit 2 instead.
    Never,
}

impl Start {
    /// `--no-start` when `flag`, else what `$ND_NO_START` says.
    pub fn resolve(flag: bool, env: Option<&str>) -> Self {
        let env_says_no = env.is_some_and(|v| !v.is_empty() && v != "0");
        if flag || env_says_no {
            Start::Never
        } else {
            Start::IfNeeded
        }
    }
}

/// Where nd reaches notedeck, and whether it may start one there.
pub struct Reach {
    path: PathBuf,
    start: Start,
}

#[cfg(unix)]
/// Why a socket couldn't be reached.
enum Unreached {
    /// Nothing listens there: no file, or a file nobody holds.
    Absent,
    /// Anything else, already worded.
    Failed(Failure),
}

impl Reach {
    /// The socket [`notedeck_ipc::socket_path`] names.
    pub fn new(start: Start) -> Self {
        Self {
            path: notedeck_ipc::socket_path(),
            start,
        }
    }

    /// There is no socket off Unix, so no notedeck to reach or start.
    #[cfg(not(unix))]
    pub fn exchange(&self, _request: &Request) -> Result<Response, Failure> {
        let _ = self.start;
        Err(Failure {
            code: NOT_RUNNING,
            message: "nd needs notedeck's Unix socket, which this platform doesn't have".to_owned(),
        })
    }

    /// One request and its response over a fresh connection, starting a
    /// notedeck first when none answers and [`Start`] allows it.
    #[cfg(unix)]
    pub fn exchange(&self, request: &Request) -> Result<Response, Failure> {
        let client = match connect(&self.path) {
            Ok(client) => client,
            Err(Unreached::Failed(failure)) => return Err(failure),
            Err(Unreached::Absent) => match self.start {
                Start::Never => {
                    return Err(self.not_running(&format!(
                        "--no-start / ${NO_START_ENV} keeps nd from starting one"
                    )));
                }
                Start::IfNeeded => {
                    self.start_notedeck()?;
                    connect(&self.path).map_err(|unreached| match unreached {
                        Unreached::Failed(failure) => failure,
                        Unreached::Absent => self.not_running("the notedeck nd started went away"),
                    })?
                }
            },
        };
        talk(client, request)
    }

    /// Exit 2, saying why.
    #[cfg(unix)]
    fn not_running(&self, why: &str) -> Failure {
        Failure {
            code: NOT_RUNNING,
            message: format!(
                "notedeck isn't running (no socket at {}), and {why}",
                self.path.display()
            ),
        }
    }

    /// Start `notedeck --headless --serve` detached, and wait until it lists
    /// its tools. The registry fills on the notedeck's first tick, so an
    /// empty list means "not yet".
    #[cfg(unix)]
    fn start_notedeck(&self) -> Result<(), Failure> {
        let log = log_path(&self.path);
        let mut child = spawn_detached(&log).map_err(|why| self.not_running(&why))?;

        let deadline = Instant::now() + START_TIMEOUT;
        let mut pause = FIRST_POLL;
        let mut answered = false;
        loop {
            match tool_count(&self.path) {
                Some(0) => answered = true,
                Some(_) => return Ok(()),
                None => {}
            }
            // A notedeck that exited without a socket answering crashed or
            // refused; one that exited cleanly found another notedeck there,
            // which the next connect reaches.
            if let Ok(Some(status)) = child.try_wait() {
                if !status.success() && !answered {
                    return Err(self.not_running(&format!(
                        "the notedeck nd started exited ({status}); see {}",
                        log.display()
                    )));
                }
            }
            if Instant::now() >= deadline {
                // It answers, with no tools: let the call say what it will.
                if answered {
                    return Ok(());
                }
                return Err(self.not_running(&format!(
                    "the notedeck nd started didn't answer within {}s; see {}",
                    START_TIMEOUT.as_secs(),
                    log.display()
                )));
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(MAX_POLL);
        }
    }
}

#[cfg(unix)]
/// How many tools the notedeck on `path` lists, or `None` when none answers
/// with a list.
fn tool_count(path: &Path) -> Option<usize> {
    let client = connect(path).ok()?;
    match talk(client, &Request::ListTools).ok()?.outcome {
        Outcome::Ok(Reply::Tools { tools }) => Some(tools.len()),
        _ => None,
    }
}

#[cfg(unix)]
/// Connect to the socket at `path`.
fn connect(path: &Path) -> Result<Client, Unreached> {
    match Client::connect(path) {
        Ok(client) => Ok(client),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            Err(Unreached::Absent)
        }
        Err(e) => Err(Unreached::Failed(Failure::new(format!(
            "connecting to {}: {e}",
            path.display()
        )))),
    }
}

#[cfg(unix)]
/// Send `request` on `client` and read its response.
fn talk(mut client: Client, request: &Request) -> Result<Response, Failure> {
    client
        .request(request)
        .map_err(|e| Failure::new(format!("talking to notedeck: {e}")))
}

#[cfg(unix)]
/// Where a started notedeck's stdout and stderr go: beside the socket, as
/// `notedeck.log` for `notedeck.sock`, holding the last start's output only.
/// Its own rolling log is in its data dir as always; this catches what comes
/// before that, and any panic.
fn log_path(socket: &Path) -> PathBuf {
    socket.with_extension("log")
}

#[cfg(unix)]
/// Start the notedeck binary with `--headless --serve` in a session of its
/// own, so closing the terminal that ran nd doesn't stop it, with its output
/// in `log`. Returns why it couldn't, worded for [`Reach::not_running`].
fn spawn_detached(log: &Path) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;

    let bin = std::env::var_os(BIN_ENV)
        .filter(|bin| !bin.is_empty())
        .unwrap_or_else(|| "notedeck".into());
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log)
        .map_err(|e| format!("can't open the log {}: {e}", log.display()))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("can't open the log {}: {e}", log.display()))?;

    let mut command = Command::new(&bin);
    command
        .args(["--headless", "--serve"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    // SAFETY: setsid is async-signal-safe and touches no state of this
    // process, which is all that may run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map_err(|e| match e.kind() {
        ErrorKind::NotFound => format!(
            "there is no `{}` to start (put notedeck on PATH, or name it with ${BIN_ENV})",
            bin.to_string_lossy()
        ),
        _ => format!("starting {}: {e}", bin.to_string_lossy()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_or_the_env_keeps_nd_from_starting() {
        assert_eq!(Start::resolve(false, None), Start::IfNeeded);
        assert_eq!(Start::resolve(true, None), Start::Never);
        assert_eq!(Start::resolve(false, Some("1")), Start::Never);
        assert_eq!(Start::resolve(false, Some("yes")), Start::Never);
        assert_eq!(Start::resolve(false, Some("0")), Start::IfNeeded);
        assert_eq!(Start::resolve(false, Some("")), Start::IfNeeded);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_sits_beside_the_socket() {
        assert_eq!(
            log_path(Path::new("/run/user/1/notedeck/notedeck.sock")),
            PathBuf::from("/run/user/1/notedeck/notedeck.log")
        );
    }
}
