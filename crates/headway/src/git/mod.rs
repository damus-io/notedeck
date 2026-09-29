//! Thin `git` plumbing for review records: resolve a commit and read the facts a
//! [`ReviewFields`](crate::event::ReviewFields) carries about it (subject, branch,
//! repo toplevel, repo identity).
//!
//! Every helper shells out to the `git` binary with `-C <dir>`, the same way
//! Dave's worktree code does, so there is no libgit dependency and the answers
//! match what the user sees on the command line. Failures keep git's own stderr
//! (see [`GitError`]) because "not a git repository" or "unknown revision" is
//! exactly what the caller needs to show.
//!
//! [`resolve`] (the `resolve` submodule) builds on these to find — or fetch —
//! the commit a review record names and turn it into a [`CommitPatch`].

mod resolve;

pub use resolve::{
    CommitPatch, Found, ResolveCtx, Resolved, Target, commit_patch, remote_host, resolve,
    resolve_by_trailer,
};

use std::fmt;
use std::path::Path;
use std::process::Command;

/// A git invocation that failed: either `git` couldn't be run at all, or it ran
/// and exited non-zero. `stderr` is git's own message, trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    /// The git arguments that were run, space-joined, for context.
    pub command: String,
    /// git's stderr (or the spawn error when git couldn't be started).
    pub stderr: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git {} failed: {}", self.command, self.stderr)
    }
}

impl std::error::Error for GitError {}

/// Run `git -C <dir> <args>` and return its stdout, trimmed. A non-zero exit is
/// a [`GitError`] carrying git's stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let command = args.join(" ");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| GitError {
            command: command.clone(),
            stderr: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(GitError {
            command,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The full sha of the commit `rev` names in the repo at `dir`
/// (`git rev-parse --verify <rev>^{commit}`). Errors when `dir` isn't in a repo
/// or `rev` doesn't resolve to a commit.
pub fn resolve_commit(dir: &Path, rev: &str) -> Result<String, GitError> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .map_err(|mut e| {
        // `--quiet` suppresses the message for an unknown rev; say it ourselves.
        if e.stderr.is_empty() {
            e.stderr = format!("'{rev}' is not a commit");
        }
        e
    })
}

/// The repo toplevel containing `dir` (`git rev-parse --show-toplevel`). For a
/// worktree this is the worktree's own root, which is what a host needs to find
/// the checkout again.
pub fn toplevel(dir: &Path) -> Result<String, GitError> {
    git(dir, &["rev-parse", "--show-toplevel"])
}

/// The subject line of commit `sha` (`git log -1 --format=%s`).
pub fn commit_title(dir: &Path, sha: &str) -> Result<String, GitError> {
    git(dir, &["log", "-1", "--format=%s", sha])
}

/// The branch checked out at `dir`, or `None` on a detached `HEAD` (where
/// `git rev-parse --abbrev-ref HEAD` prints the literal `HEAD`).
pub fn current_branch(dir: &Path) -> Result<Option<String>, GitError> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|b| branch_name(&b))
}

/// Interpret `git rev-parse --abbrev-ref HEAD` output: a detached head prints
/// `HEAD`, which is not a branch.
fn branch_name(abbrev: &str) -> Option<String> {
    (!abbrev.is_empty() && abbrev != "HEAD").then(|| abbrev.to_string())
}

/// The repo identity of the history containing `sha`: its root commit
/// (`git rev-list --max-parents=0 <sha>`). A history with several roots (a
/// merged-in unrelated history) takes the lexicographically smallest, so every
/// clone and worktree agrees on one value without any configured mapping.
pub fn repo_identity(dir: &Path, sha: &str) -> Result<String, GitError> {
    let roots = git(dir, &["rev-list", "--max-parents=0", sha])?;
    smallest_root(&roots).ok_or_else(|| GitError {
        command: format!("rev-list --max-parents=0 {sha}"),
        stderr: "no root commit".to_string(),
    })
}

/// The smallest non-empty line of `rev-list --max-parents=0` output.
fn smallest_root(roots: &str) -> Option<String> {
    roots
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .min()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A detached head reads back as no branch; a named branch as itself.
    #[test]
    fn detached_head_is_not_a_branch() {
        assert_eq!(branch_name("HEAD"), None);
        assert_eq!(branch_name(""), None);
        assert_eq!(branch_name("headway"), Some("headway".to_string()));
    }

    /// Several roots pick the smallest, so every clone agrees.
    #[test]
    fn repo_identity_takes_the_smallest_root() {
        assert_eq!(smallest_root("bbb\naaa\nccc\n"), Some("aaa".to_string()));
        assert_eq!(smallest_root("abc"), Some("abc".to_string()));
        assert_eq!(smallest_root("\n"), None);
    }

    /// Outside a repo every helper errors with git's own message kept.
    #[test]
    fn outside_a_repo_keeps_gits_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = toplevel(dir.path()).unwrap_err();
        assert!(err.stderr.contains("not a git repository"), "{err}");
    }

    /// Against a real one-commit repo: the commit resolves, and its title,
    /// toplevel, branch and identity (its own sha, being the root) read back.
    #[test]
    fn reads_a_real_commit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| git(p, args).unwrap();
        run(&["init", "-q", "-b", "trunk"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first commit",
        ]);

        let sha = resolve_commit(p, "HEAD").unwrap();
        assert_eq!(sha.len(), 40);
        assert_eq!(commit_title(p, &sha).unwrap(), "first commit");
        assert_eq!(repo_identity(p, &sha).unwrap(), sha);
        assert_eq!(current_branch(p).unwrap(), Some("trunk".to_string()));
        let top = std::fs::canonicalize(toplevel(p).unwrap()).unwrap();
        assert_eq!(top, std::fs::canonicalize(p).unwrap());

        let err = resolve_commit(p, "nope").unwrap_err();
        assert!(err.stderr.contains("'nope' is not a commit"), "{err}");

        run(&["checkout", "-q", "--detach"]);
        assert_eq!(current_branch(p).unwrap(), None);
    }
}
