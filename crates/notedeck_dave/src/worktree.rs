use std::path::{Path, PathBuf};
use std::process::Command;

/// Apply Windows-specific flag to suppress console window creation.
#[cfg(target_os = "windows")]
fn configure_cmd(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_cmd(_cmd: &mut Command) {}

/// Identity of the project a session's cwd belongs to.
///
/// A "project" groups every git worktree of one repository (plus the main
/// checkout) under a single sidebar entry, Codex-style, instead of scattering
/// each worktree as its own top-level cwd group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectId {
    /// Grouping key: the git repo root shared by all the project's worktrees, or
    /// the cwd itself when it isn't inside a git repo.
    pub root: PathBuf,
    /// Display slug: the basename of `root`.
    pub slug: String,
}

/// Resolve the project a `cwd` belongs to.
///
/// Runs `git rev-parse --path-format=absolute --git-common-dir`, so every linked
/// worktree of a repo resolves to the *same* shared `.git` and hence the same
/// project root — the whole point of project grouping. A cwd that isn't inside a
/// git repo becomes its own single-workspace project. This spawns `git`, so call
/// it at session-creation time and persist the result; never from a per-frame
/// UI path.
pub fn project_for(cwd: &Path) -> ProjectId {
    match git_common_dir(cwd) {
        Some(common) => project_from_common_dir(&common, cwd),
        None => project_from_cwd(cwd),
    }
}

/// Get the shared git dir (`--git-common-dir`) for `cwd`, absolute.
///
/// For the main checkout this is `<repo>/.git`; for a linked worktree it is the
/// *main* repo's `.git`, which is exactly what makes worktrees converge onto one
/// project root.
fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd);
    configure_cmd(&mut cmd);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() {
        return None;
    }
    Some(PathBuf::from(path_str))
}

/// Derive a [`ProjectId`] from a `--git-common-dir` path.
///
/// The common dir is typically `<root>/.git`; the project root is its parent.
/// Factored out (pure) so it can be unit-tested without a real git repo.
fn project_from_common_dir(common_dir: &Path, cwd: &Path) -> ProjectId {
    let root = if common_dir.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common_dir.parent().map(Path::to_path_buf)
    } else {
        Some(common_dir.to_path_buf())
    };
    match root {
        Some(root) => ProjectId {
            slug: slug_for(&root),
            root,
        },
        // Degenerate common dir (e.g. `.git` with no parent) — fall back to cwd.
        None => project_from_cwd(cwd),
    }
}

/// A cwd that isn't inside a git repo is its own single-workspace project.
fn project_from_cwd(cwd: &Path) -> ProjectId {
    ProjectId {
        slug: slug_for(cwd),
        root: cwd.to_path_buf(),
    }
}

/// The display slug for a project root: its basename, or the full path when the
/// root has no final component (e.g. `/`).
fn slug_for(root: &Path) -> String {
    root.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

/// Get the git repository root for the given directory.
pub fn git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"]).current_dir(cwd);
    configure_cmd(&mut cmd);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(PathBuf::from(path_str))
}

/// List local branches in the repo at `cwd`.
pub fn list_branches(cwd: &Path) -> Vec<String> {
    let mut cmd = Command::new("git");
    cmd.args(["branch", "--list", "--no-color"])
        .current_dir(cwd);
    configure_cmd(&mut cmd);

    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim_start_matches('*').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Check if `path` is a linked git worktree (not the main repo).
/// In linked worktrees, `.git` is a file pointing to the main repo's
/// `.git/worktrees/` directory, rather than being a directory itself.
pub fn is_linked_worktree(path: &Path) -> bool {
    let dot_git = path.join(".git");
    dot_git.is_file()
}

/// Remove the git worktree at `path`, forcibly (even if dirty).
pub fn remove_git_worktree(path: &Path) -> Result<(), String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "worktree path contains invalid UTF-8".to_string())?;

    let mut cmd = Command::new("git");
    cmd.args(["worktree", "remove", "--force", path_str])
        .current_dir(path.parent().unwrap_or(path));
    configure_cmd(&mut cmd);

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Create a git worktree at `worktree_path`.
///
/// If `is_new_branch` is true, passes `-b <branch>` to create a new branch.
/// Otherwise checks out the existing branch.
pub fn create_git_worktree(
    cwd: &Path,
    worktree_path: &Path,
    branch: &str,
    is_new_branch: bool,
) -> Result<(), String> {
    let path_str = worktree_path
        .to_str()
        .ok_or_else(|| "worktree path contains invalid UTF-8".to_string())?;

    let mut cmd = Command::new("git");
    if is_new_branch {
        cmd.args(["worktree", "add", "-b", branch, path_str]);
    } else {
        cmd.args(["worktree", "add", path_str, branch]);
    }
    cmd.current_dir(cwd);
    configure_cmd(&mut cmd);

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_checkout_root_is_common_dir_parent() {
        let p = project_from_common_dir(
            Path::new("/home/dev/notedeck/.git"),
            Path::new("/home/dev/notedeck"),
        );
        assert_eq!(p.root, PathBuf::from("/home/dev/notedeck"));
        assert_eq!(p.slug, "notedeck");
    }

    #[test]
    fn worktree_converges_on_the_main_repo_root() {
        // A linked worktree reports the *main* repo's `.git` as its common dir,
        // so both the worktree and the main checkout produce the same project.
        let from_worktree = project_from_common_dir(
            Path::new("/home/dev/notedeck/.git"),
            Path::new("/home/dev/notedeck-dave"),
        );
        let from_main = project_from_common_dir(
            Path::new("/home/dev/notedeck/.git"),
            Path::new("/home/dev/notedeck"),
        );
        assert_eq!(from_worktree, from_main);
        assert_eq!(from_worktree.slug, "notedeck");
    }

    #[test]
    fn non_dot_git_common_dir_is_kept_as_root() {
        let p = project_from_common_dir(Path::new("/srv/repo.git"), Path::new("/srv/repo.git"));
        assert_eq!(p.root, PathBuf::from("/srv/repo.git"));
        assert_eq!(p.slug, "repo.git");
    }

    #[test]
    fn non_git_cwd_is_its_own_project() {
        let p = project_from_cwd(Path::new("/home/dev/scratch"));
        assert_eq!(p.root, PathBuf::from("/home/dev/scratch"));
        assert_eq!(p.slug, "scratch");
    }
}
