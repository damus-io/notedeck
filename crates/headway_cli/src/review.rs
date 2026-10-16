//! `headway review`: gather a [`ReviewFields`] record for a commit from git, the
//! host and the environment, so an agent's done step is one line.
//!
//! Gathering runs at parse time (like `--desc-file`'s read), before any relay
//! work, so a bad rev or a directory outside a repo fails fast rather than after
//! a full sync.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use headway::event::{ReviewFields, ReviewSource, ReviewSourceError, ReviewSourceRole};
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
    /// `--deploy`: URL where a build of the commit is live.
    pub(crate) deploy: Option<String>,
    /// `--agentium`: the session ref; defaults to `$AGENTIUM_SESSION`.
    pub(crate) agentium: Option<String>,
    /// `--remote`: an explicit fetch URL for the commit.
    pub(crate) remote: Option<String>,
    /// `--repo-dir`: where to run git. Defaults to the current directory.
    pub(crate) repo_dir: Option<String>,
    /// `--explainer-source` / `--code-source`: files to store on the record,
    /// in the order given.
    pub(crate) sources: Vec<SourceFlag>,
}

/// One `--explainer-source` or `--code-source` as typed: the file to read
/// and the role the flag gives it.
pub(crate) struct SourceFlag {
    /// Which flag named it.
    pub(crate) role: ReviewSourceRole,
    /// The path as typed, relative to the current directory.
    pub(crate) path: String,
}

/// Read each `--explainer-source` / `--code-source` file into the
/// [`ReviewSource`] `review` stores on its record, named by its basename.
///
/// Errors before any relay work when a file can't be read or isn't UTF-8, when
/// two files share a basename (the second would replace the first), or when a
/// file is over [`headway::event::MAX_REVIEW_SOURCE_BYTES`] — the store would
/// refuse it silently, so it is refused here with the file's name and size.
pub(crate) fn read_sources(flags: &[SourceFlag]) -> Result<Vec<ReviewSource>> {
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
                return Err(
                    format!("two sources named '{name}': each is stored by its file name").into(),
                );
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
        host: git::host_name(),
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
        deploy: flags.deploy,
        remote: flags.remote,
        commit: Some(commit),
    })
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
pub(crate) fn print_recorded(review: &ReviewFields, sources: &[ReviewSource]) {
    for tag in review.tags() {
        let Some(value) = tag.value else { continue };
        println!("  {:<11}{value}", tag.name);
    }
    for source in sources {
        println!(
            "  {:<11}{} ({}, {} bytes)",
            "source",
            source.name,
            source.role.as_str(),
            source.text.len()
        );
    }
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
        .err()
        .unwrap()
        .to_string();
        assert!(err.contains("two sources named 'a-b-c.md'"), "{err}");

        let big = write(
            "big.md",
            &"x".repeat(headway::event::MAX_REVIEW_SOURCE_BYTES + 1),
        );
        let err = read_sources(&[flag(ReviewSourceRole::Explainer, &big)])
            .err()
            .unwrap()
            .to_string();
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
}
