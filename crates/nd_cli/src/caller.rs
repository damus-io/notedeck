//! What nd reads from the process it runs in. A tool runs inside notedeck,
//! which can't see the shell that called it, so the arguments that name
//! the caller's side — an environment variable, a path relative to the
//! working directory, a file or stdin — are resolved here before the call.
//! [`Process`] reads the real ones; tests hand [`commands::resolve`] a
//! fake.
//!
//! [`commands::resolve`]: crate::commands::resolve

use notedeck_ipc::Signer;
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

/// The comment key, as the `headway` CLI reads it: an agent's own key, which
/// signs its headway comments in place of the app's account.
pub const COMMENT_NSEC_VAR: &str = "HEADWAY_COMMENT_NSEC";

/// A file holding the comment key, read when [`COMMENT_NSEC_VAR`] is unset.
pub const COMMENT_NSEC_FILE_VAR: &str = "HEADWAY_COMMENT_NSEC_FILE";

/// The comment key the caller set, to send beside a call of a tool that
/// accepts one: `$HEADWAY_COMMENT_NSEC`, else the contents of the file
/// `$HEADWAY_COMMENT_NSEC_FILE` names (relative to the working directory),
/// else `None`.
///
/// A file that can't be read, or a key that is empty, is an error rather than
/// `None`: going on without it would sign the comment as the account, which
/// is what setting the variable was meant to prevent. notedeck parses the key
/// and refuses the call if it doesn't, so nd never needs to. Neither the
/// error nor anything else here prints the key.
pub fn comment_signer(caller: &dyn Caller) -> Result<Option<Signer>, String> {
    let key = match caller.env(COMMENT_NSEC_VAR) {
        Some(key) => key,
        None => {
            let Some(file) = caller.env(COMMENT_NSEC_FILE_VAR) else {
                return Ok(None);
            };
            let path = caller.cwd()?.join(file);
            std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "${COMMENT_NSEC_FILE_VAR}: can't read {}: {e}",
                    path.display()
                )
            })?
        }
    };
    let key = key.trim();
    if key.is_empty() {
        return Err(format!(
            "${COMMENT_NSEC_VAR} / ${COMMENT_NSEC_FILE_VAR} is set but holds no key"
        ));
    }
    Ok(Some(Signer {
        comment_nsec: key.to_owned(),
    }))
}

/// The signer to send with a call of `tool`: the caller's comment key when
/// the tool accepts one, else none, whatever the environment holds.
pub fn signer_for(
    tool: &notedeck_ipc::ToolInfo,
    caller: &dyn Caller,
) -> Result<Option<Signer>, String> {
    if !tool.accepts_signer {
        return Ok(None);
    }
    comment_signer(caller)
}
