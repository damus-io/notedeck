//! `review`: gather a [`ReviewFields`] record for a commit from git, the host
//! and the caller's session, so an agent's done step is one line. Shared by
//! the `headway` CLI and the app's `headway_review` tool.
//!
//! Everything here reads the filesystem or runs git, and nothing touches a
//! board, so a caller runs it before any relay work: a bad rev, a directory
//! outside a repo or an unreadable source fails fast.

use std::collections::HashSet;
use std::path::Path;

use crate::event::{ReviewFields, ReviewSource, ReviewSourceError, ReviewSourceRole};
use crate::git;

/// The `review` flags as typed. Everything is optional; [`gather`] fills the
/// rest from git and the environment.
#[derive(Default)]
pub struct ReviewFlags {
    /// `--commit`: the rev to record. Defaults to `HEAD`.
    pub commit: Option<String>,
    /// `--explainer`: URL of the explainer page for the work.
    pub explainer: Option<String>,
    /// `--deploy`: URL where a build of the commit is live.
    pub deploy: Option<String>,
    /// `--agentium`: the session ref, refused when it isn't one.
    pub agentium: Option<String>,
    /// The caller's `$AGENTIUM_SESSION`, used when `agentium` isn't given
    /// and left out when it isn't a session ref.
    pub default_agentium: Option<String>,
    /// `--remote`: an explicit fetch URL for the commit.
    pub remote: Option<String>,
    /// `--repo-dir`: where to run git. Defaults to the current directory.
    pub repo_dir: Option<String>,
    /// `--explainer-source` / `--code-source`: files to store on the record,
    /// in the order given.
    pub sources: Vec<SourceFlag>,
}

/// One `--explainer-source` or `--code-source` as typed: the file to read
/// and the role the flag gives it.
#[derive(Clone)]
pub struct SourceFlag {
    /// Which flag named it.
    pub role: ReviewSourceRole,
    /// The path as typed, relative to the current directory.
    pub path: String,
}

/// Read each `--explainer-source` / `--code-source` file into the
/// [`ReviewSource`] `review` stores on its record, named by its basename.
///
/// Errors when a file can't be read or isn't UTF-8, when two files share a
/// basename (the second would replace the first), or when a file is over
/// [`crate::event::MAX_REVIEW_SOURCE_BYTES`] — the store would refuse it
/// silently, so it is refused here with the file's name and size.
pub fn read_sources(flags: &[SourceFlag]) -> Result<Vec<ReviewSource>, String> {
    let mut names = HashSet::new();
    flags
        .iter()
        .map(|flag| {
            let name = Path::new(&flag.path)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{}: not a file name", flag.path))?
                .to_string();
            if !names.insert(name.clone()) {
                return Err(format!(
                    "two sources named '{name}': each is stored by its file name"
                ));
            }
            let text =
                std::fs::read_to_string(&flag.path).map_err(|e| format!("{}: {e}", flag.path))?;
            ReviewSourceError::check(&name, &text).map_err(|e| format!("{}: {e}", flag.path))?;
            Ok(ReviewSource {
                role: flag.role,
                name,
                text,
            })
        })
        .collect()
}

/// Resolve `flags` into the record `review` writes: the commit and its subject,
/// branch, repo toplevel and repo identity from git in the repo dir; the host
/// from the OS; the agentium ref from `agentium`, else `default_agentium`.
///
/// Errors when the dir isn't in a git repo, the rev doesn't resolve, or an
/// explicit `agentium` isn't an `agentium:<word-id>` ref (a malformed
/// `default_agentium` is just left out). A dirty tree is fine: the commit is
/// what's recorded, not the working tree.
pub fn gather(flags: ReviewFlags) -> Result<ReviewFields, String> {
    let dir = flags.repo_dir.as_deref().unwrap_or(".");
    let dir = Path::new(dir);
    let rev = flags.commit.as_deref().unwrap_or("HEAD");
    let git_err = |e: git::GitError| e.to_string();
    let commit = git::resolve_commit(dir, rev).map_err(git_err)?;
    Ok(ReviewFields {
        title: Some(git::commit_title(dir, &commit).map_err(git_err)?),
        branch: git::current_branch(dir).map_err(git_err)?,
        host: git::host_name(),
        path: Some(git::toplevel(dir).map_err(git_err)?),
        repo: Some(git::repo_identity(dir, &commit).map_err(git_err)?),
        agentium: match flags.agentium {
            Some(given) => Some(
                agentium_ref(&given)
                    .ok_or_else(|| format!("--agentium wants agentium:<word-id>, got '{given}'"))?,
            ),
            None => flags.default_agentium.as_deref().and_then(agentium_ref),
        },
        explainer: flags.explainer,
        deploy: flags.deploy,
        remote: flags.remote,
        commit: Some(commit),
    })
}

/// `value` as an `agentium:<word-id>` session ref, or `None` when it isn't one
/// (empty, a bare word-id, junk), so a malformed value is never recorded.
pub fn agentium_ref(value: &str) -> Option<String> {
    let word_id = value.trim().strip_prefix("agentium:")?;
    let valid = !word_id.is_empty()
        && word_id
            .split('-')
            .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()));
    valid.then(|| format!("agentium:{word_id}"))
}

/// What a `review` recorded, one field per line, so the transcript of the
/// agent that ran it shows exactly what was captured.
pub fn recorded_text(review: &ReviewFields, sources: &[ReviewSource]) -> String {
    let mut out = String::new();
    for tag in review.tags() {
        let Some(value) = tag.value else { continue };
        out.push_str(&format!("  {:<11}{value}\n", tag.name));
    }
    for source in sources {
        out.push_str(&format!(
            "  {:<11}{} ({}, {} bytes)\n",
            "source",
            source.name,
            source.role.as_str(),
            source.text.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sources are read by basename with their flag's role; a shared basename
    /// or a file over the cap is refused naming the file.
    #[test]
    fn read_sources_names_by_basename_and_refuses_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path.to_string_lossy().into_owned()
        };
        let flag = |role, path: &str| SourceFlag {
            role,
            path: path.to_string(),
        };
        let md = write("book/a-b-c.md", "# A");
        let code = write("book/a-b-c.code.md", "walk");

        let sources = read_sources(&[
            flag(ReviewSourceRole::Explainer, &md),
            flag(ReviewSourceRole::Code, &code),
        ])
        .unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(
            (
                sources[0].role,
                sources[0].name.as_str(),
                sources[0].text.as_str()
            ),
            (ReviewSourceRole::Explainer, "a-b-c.md", "# A")
        );
        assert_eq!(
            (sources[1].role, sources[1].name.as_str()),
            (ReviewSourceRole::Code, "a-b-c.code.md")
        );

        let clash = write("other/a-b-c.md", "# B");
        let err = read_sources(&[
            flag(ReviewSourceRole::Explainer, &md),
            flag(ReviewSourceRole::Explainer, &clash),
        ])
        .unwrap_err();
        assert!(err.contains("two sources named 'a-b-c.md'"), "{err}");

        let big = write(
            "big.md",
            &"x".repeat(crate::event::MAX_REVIEW_SOURCE_BYTES + 1),
        );
        let err = read_sources(&[flag(ReviewSourceRole::Explainer, &big)]).unwrap_err();
        assert!(err.contains("big.md"), "{err}");

        let missing = dir.path().join("nope.md").to_string_lossy().into_owned();
        assert!(read_sources(&[flag(ReviewSourceRole::Explainer, &missing)]).is_err());
    }

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

    /// An explicit malformed `agentium` is refused; a malformed default is
    /// left out, so a stray `$AGENTIUM_SESSION` never fails a review.
    #[test]
    fn gather_refuses_a_bad_agentium_but_drops_a_bad_default() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "first",
        ]);
        let repo_dir = Some(dir.path().to_string_lossy().into_owned());

        let fields = gather(ReviewFlags {
            repo_dir: repo_dir.clone(),
            default_agentium: Some("junk".to_owned()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(fields.agentium, None);
        assert_eq!(fields.title.as_deref(), Some("first"));

        let err = gather(ReviewFlags {
            repo_dir,
            agentium: Some("junk".to_owned()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("--agentium"), "{err}");
    }
}
