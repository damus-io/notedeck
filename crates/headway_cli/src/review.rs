//! `headway review`: gather a [`ReviewFields`] record for a commit from git, the
//! host and the environment, so an agent's done step is one line.
//!
//! Gathering runs at parse time (like `--desc-file`'s read), before any relay
//! work, so a bad rev or a directory outside a repo fails fast rather than after
//! a full sync.

use std::path::PathBuf;

use headway::event::ReviewFields;
use headway::git;

use nostrdb_net::relay::sync::Result;

/// The `review` flags as typed. Everything is optional; [`gather`] fills the
/// rest from git and the environment.
#[derive(Default)]
pub(crate) struct ReviewFlags {
    /// `--commit`: the rev to record. Defaults to `HEAD`.
    pub(crate) commit: Option<String>,
    /// `--explainer`: URL of the explainer page for the work.
    pub(crate) explainer: Option<String>,
    /// `--agentium`: the session ref; defaults to `$AGENTIUM_SESSION`.
    pub(crate) agentium: Option<String>,
    /// `--remote`: an explicit fetch URL for the commit.
    pub(crate) remote: Option<String>,
    /// `--repo-dir`: where to run git. Defaults to the current directory.
    pub(crate) repo_dir: Option<String>,
}

/// Resolve `flags` into the record `review` writes: the commit and its subject,
/// branch, repo toplevel and repo identity from git in the repo dir; the host
/// from the OS; the agentium ref from `--agentium` or `$AGENTIUM_SESSION`.
///
/// Errors when the dir isn't in a git repo, the rev doesn't resolve, or an
/// explicit `--agentium` isn't an `agentium:<word-id>` ref (a malformed
/// `$AGENTIUM_SESSION` is just left out). A dirty
/// tree is fine: the commit is what's recorded, not the working tree.
pub(crate) fn gather(flags: ReviewFlags) -> Result<ReviewFields> {
    let dir = flags
        .repo_dir
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let rev = flags.commit.as_deref().unwrap_or("HEAD");
    let commit = git::resolve_commit(&dir, rev)?;
    Ok(ReviewFields {
        title: Some(git::commit_title(&dir, &commit)?),
        branch: git::current_branch(&dir)?,
        host: host_name(),
        path: Some(git::toplevel(&dir)?),
        repo: Some(git::repo_identity(&dir, &commit)?),
        agentium: match flags.agentium {
            Some(given) => Some(
                agentium_ref(&given)
                    .ok_or_else(|| format!("--agentium wants agentium:<word-id>, got '{given}'"))?,
            ),
            None => std::env::var("AGENTIUM_SESSION")
                .ok()
                .and_then(|s| agentium_ref(&s)),
        },
        explainer: flags.explainer,
        remote: flags.remote,
        commit: Some(commit),
    })
}

/// This machine's hostname, or `None` when the OS gives back something unusable.
fn host_name() -> Option<String> {
    let host = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    (!host.is_empty()).then_some(host)
}

/// `value` as an `agentium:<word-id>` session ref, or `None` when it isn't one
/// (empty, a bare word-id, junk), so a malformed value is never recorded.
fn agentium_ref(value: &str) -> Option<String> {
    let word_id = value.trim().strip_prefix("agentium:")?;
    let valid = !word_id.is_empty()
        && word_id
            .split('-')
            .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()));
    valid.then(|| format!("agentium:{word_id}"))
}

/// Print what a `review` recorded, one field per line, so the transcript of the
/// agent that ran it shows exactly what was captured.
pub(crate) fn print_recorded(review: &ReviewFields) {
    for (name, value) in review.tags() {
        let Some(value) = value else { continue };
        println!("  {name:<11}{value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a well-formed `agentium:<word-id>` is taken from the environment.
    #[test]
    fn agentium_session_parse() {
        assert_eq!(
            agentium_ref("agentium:like-outdoor-lizard"),
            Some("agentium:like-outdoor-lizard".to_string())
        );
        assert_eq!(
            agentium_ref(" agentium:a-b-c\n"),
            Some("agentium:a-b-c".to_string())
        );
        for bad in [
            "",
            "like-outdoor-lizard",
            "agentium:",
            "agentium:Bad-Case",
            "agentium:a--b",
            "headway:headway/a-b-c",
        ] {
            assert_eq!(agentium_ref(bad), None, "{bad:?}");
        }
    }
}
