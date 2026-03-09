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
