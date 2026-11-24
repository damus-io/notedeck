//! Path attachments: files staged in a local session's composer as chips and
//! sent as context with the next message.
//!
//! A local session's agent runs on this machine and reads files itself, so
//! attaching a file needs no upload: the message only has to name its path.
//! [`ChatSession::pending_paths`] holds the staged paths; sending takes them
//! with the draft ([`ChatSession::take_draft`]) and appends one block listing
//! them ([`compose_message`]). A remote session's agent can't see this
//! machine's disk, so it never stages any ([`ChatSession::attach_path`]).

use super::ChatSession;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The line that opens the block of attached paths in a sent message.
pub const ATTACHED_FILES_HEADER: &str = "Attached files:";

/// The text a message with `paths` attached sends: `text`, a blank line, then
/// an [`ATTACHED_FILES_HEADER`] block with one `- <path>` line per path.
///
/// With no paths `text` comes back unchanged, and with no text the block is
/// the whole message, so a message of chips alone still says something.
pub fn compose_message(text: String, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return text;
    }
    let mut message = text;
    message.truncate(message.trim_end().len());
    if !message.trim_start().is_empty() {
        message.push_str("\n\n");
    } else {
        message.clear();
    }
    message.push_str(ATTACHED_FILES_HEADER);
    for path in paths {
        // Writing into a String can't fail.
        let _ = write!(message, "\n- {}", path.display());
    }
    message
}

/// Stage `path` onto a local session's `staged` chips. Returns whether it
/// was added: a relative path is refused (the agent's cwd is not the one the
/// path was taken in), and a path already staged isn't added twice.
///
/// [`ChatSession::attach_path`] is this plus the remote check. The composer
/// calls it directly on the chips it was handed, which it is handed only for
/// a local session (`ui/mod.rs`).
pub fn stage_path(staged: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if !path.is_absolute() || is_staged(staged, &path) {
        return false;
    }
    staged.push(path);
    true
}

/// Whether `path` is already among the `staged` chips.
fn is_staged(staged: &[PathBuf], path: &Path) -> bool {
    staged.iter().any(|p| p == path)
}

impl ChatSession {
    /// Stage `path` as a chip for the next message. Returns whether it was
    /// added: a remote session stages nothing (its agent can't read this
    /// machine's disk), and otherwise [`stage_path`]'s rules apply.
    pub fn attach_path(&mut self, path: PathBuf) -> bool {
        if self.is_remote() {
            return false;
        }
        stage_path(&mut self.pending_paths, path)
    }

    /// Take the draft to send: the typed text with the staged paths
    /// composed in ([`compose_message`]). The input and the chips are left
    /// empty.
    pub fn take_draft(&mut self) -> String {
        let paths = std::mem::take(&mut self.pending_paths);
        compose_message(std::mem::take(&mut self.input), &paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendType;
    use crate::config::AiMode;
    use crate::session::SessionSource;

    fn paths() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/home/me/shot one.png"),
            PathBuf::from("/var/log/app.log"),
        ]
    }

    fn local_session() -> ChatSession {
        ChatSession::new(
            1,
            PathBuf::from("/tmp/work"),
            AiMode::Agentic,
            BackendType::Claude,
        )
    }

    /// Text and paths: the text, a blank line, then the block.
    #[test]
    fn text_then_the_block() {
        assert_eq!(
            compose_message("why does this crash?\n".into(), &paths()),
            "why does this crash?\n\nAttached files:\n- /home/me/shot one.png\n- /var/log/app.log"
        );
    }

    /// Paths alone: the block is the message.
    #[test]
    fn paths_only_is_the_block() {
        assert_eq!(
            compose_message("  \n".into(), &paths()),
            "Attached files:\n- /home/me/shot one.png\n- /var/log/app.log"
        );
    }

    /// No paths: the text is sent exactly as typed.
    #[test]
    fn no_paths_leaves_the_text_alone() {
        assert_eq!(compose_message("  hi \n".into(), &[]), "  hi \n");
        assert_eq!(compose_message(String::new(), &[]), "");
    }

    /// Staging dedupes, refuses relative paths, and taking the draft empties
    /// both the input and the chips.
    #[test]
    fn attach_dedupes_and_take_draft_clears() {
        let mut session = local_session();
        assert!(session.attach_path(PathBuf::from("/var/log/app.log")));
        assert!(!session.attach_path(PathBuf::from("/var/log/app.log")));
        assert!(!session.attach_path(PathBuf::from("relative.txt")));
        session.input = "look".into();

        assert_eq!(
            session.take_draft(),
            "look\n\nAttached files:\n- /var/log/app.log"
        );
        assert!(session.input.is_empty());
        assert!(session.pending_paths.is_empty());
    }

    /// A remote session's agent can't read this machine's files, so nothing
    /// is staged on one.
    #[test]
    fn a_remote_session_stages_nothing() {
        let mut session = local_session();
        session.source = SessionSource::Remote;
        assert!(!session.attach_path(PathBuf::from("/var/log/app.log")));
        assert!(session.pending_paths.is_empty());
    }
}
