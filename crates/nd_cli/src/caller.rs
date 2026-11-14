//! What nd reads from the process it runs in. A tool runs inside notedeck,
//! which can't see the shell that called it, so the arguments that name
//! the caller's side — an environment variable, a path relative to the
//! working directory, a file or stdin — are resolved here before the call.
//! [`Process`] reads the real ones; tests hand [`commands::resolve`] a
//! fake.
//!
//! [`commands::resolve`]: crate::commands::resolve

use std::io::Read;
use std::path::PathBuf;

/// The caller's environment, working directory and stdin.
pub trait Caller {
    /// An environment variable, or `None` when it isn't set.
    fn env(&self, var: &str) -> Option<String>;

    /// The working directory.
    fn cwd(&self) -> Result<PathBuf, String>;

    /// Everything on stdin. Read at most once a command line.
    fn stdin(&self) -> Result<String, String>;
}

/// The process nd runs in.
pub struct Process;

impl Caller for Process {
    fn env(&self, var: &str) -> Option<String> {
        std::env::var(var).ok()
    }

    fn cwd(&self) -> Result<PathBuf, String> {
        std::env::current_dir().map_err(|e| format!("reading the working directory: {e}"))
    }

    fn stdin(&self) -> Result<String, String> {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("reading stdin: {e}"))?;
        Ok(text)
    }
}
