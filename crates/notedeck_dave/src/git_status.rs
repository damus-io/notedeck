use agentium_core::inspector_snapshot::{GitFile, GitStatus, WorkTree};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

/// A session's working-tree status: read from git every few seconds for a
/// local session, or folded in from the host's inspector snapshot for a
/// remote one ([`Self::set_remote`]). Either way the status bar reads it from
/// here.
pub struct GitStatusCache {
    cwd: PathBuf,
    current: Option<GitStatus>,
    /// Bumped each time a read lands that differs from the one before, so
    /// the host knows its snapshot is stale.
    generation: u64,
    receiver: Option<mpsc::Receiver<GitStatus>>,
    last_fetch: Option<Instant>,
    /// Whether a fetch is currently in-flight
    fetching: bool,
    /// Whether the expanded file list is shown
    pub expanded: bool,
}

const REFRESH_INTERVAL_SECS: f64 = 5.0;

impl GitStatusCache {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            current: None,
            generation: 0,
            receiver: None,
            last_fetch: None,
            fetching: false,
            expanded: false,
        }
    }

    /// Request a fresh git status (non-blocking, spawns thread)
    pub fn request_refresh(&mut self) {
        if self.fetching {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cwd = self.cwd.clone();
        std::thread::spawn(move || {
            let result = run_git_status(&cwd);
            let _ = tx.send(result);
        });
        self.receiver = Some(rx);
        self.fetching = true;
        self.last_fetch = Some(Instant::now());
    }

    /// Poll for results (call each frame)
    pub fn poll(&mut self) {
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(result) => {
                    self.land(result);
                    self.fetching = false;
                    self.receiver = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.fetching = false;
                    self.receiver = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Check if auto-refresh is due and trigger if so
    pub fn maybe_auto_refresh(&mut self) {
        let should_refresh = match self.last_fetch {
            None => true,
            Some(t) => t.elapsed().as_secs_f64() >= REFRESH_INTERVAL_SECS,
        };
        if should_refresh {
            self.request_refresh();
        }
    }

    /// Take a read that came back, counting it as a change only when it
    /// differs from the last: an idle tree polled every few seconds must not
    /// republish the session's snapshot each time.
    fn land(&mut self, status: GitStatus) {
        if self.current.as_ref() != Some(&status) {
            self.generation += 1;
        }
        self.current = Some(status);
    }

    pub fn current(&self) -> Option<&GitStatus> {
        self.current.as_ref()
    }

    /// How many reads have changed the status, for the snapshot's key.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Land a read of a tree with `added` and `deleted` lines, as a poll
    /// would, for tests of what a change republishes.
    #[cfg(test)]
    pub(crate) fn land_for_test(&mut self, added: usize, deleted: usize) {
        self.land(GitStatus::WorkTree(WorkTree {
            added_lines: added,
            deleted_lines: deleted,
            ..Default::default()
        }));
    }

    /// Take a remote session's status from its host's snapshot. `None` (a
    /// host too old to send one) shows no git at all.
    pub fn set_remote(&mut self, status: Option<GitStatus>) {
        self.current = status;
    }

    /// Set the current status as if a fetch had returned `lines` added and
    /// deleted, for tests that draw a dirty worktree.
    #[cfg(test)]
    pub(crate) fn set_lines(&mut self, added: usize, deleted: usize) {
        self.current = Some(GitStatus::WorkTree(WorkTree {
            added_lines: added,
            deleted_lines: deleted,
            ..Default::default()
        }));
    }

    /// Mark cache as stale so next poll triggers a refresh
    pub fn invalidate(&mut self) {
        self.last_fetch = None;
    }
}

fn parse_git_status(output: &str) -> WorkTree {
    let mut tree = WorkTree::default();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // Branch line: "## main...origin/main" or "## HEAD (no branch)"
            let branch_name = rest
                .split("...")
                .next()
                .unwrap_or(rest)
                .split(' ')
                .next()
                .unwrap_or(rest);
            if branch_name != "HEAD" {
                tree.branch = Some(branch_name.to_string());
            }
        } else if line.len() >= 3 {
            // File entry: "XY path" where XY is 2-char status
            let file = GitFile {
                status: line[..2].to_string(),
                path: line[3..].to_string(),
            };
            count_file(&mut tree, file.status.as_bytes());
            tree.files.push(file);
        }
    }

    tree
}

/// Count one `XY` status code into `tree`'s per-kind totals.
fn count_file(tree: &mut WorkTree, xy: &[u8]) {
    let [x, y] = [xy[0], xy[1]];
    tree.changed += 1;
    if (x == b'M' || y == b'M') && x != b'?' && x != b'A' && x != b'D' {
        tree.modified += 1;
    }
    if x == b'A' {
        tree.added += 1;
    }
    if x == b'D' || y == b'D' {
        tree.deleted += 1;
    }
    if x == b'?' {
        tree.untracked += 1;
    }
}

/// Lines a diff adds and deletes in total, as `git diff --numstat` counts
/// them.
#[derive(Debug, Default, PartialEq, Eq)]
struct LineCounts {
    /// Inserted lines.
    added: usize,
    /// Deleted lines.
    deleted: usize,
}

/// Parse `git diff --numstat` output into total added and deleted line counts.
/// Each line is `added<TAB>deleted<TAB>path`; binary files report `-` and are
/// counted as zero.
fn parse_numstat(output: &str) -> LineCounts {
    let mut added = 0;
    let mut deleted = 0;
    for line in output.lines() {
        let mut parts = line.split('\t');
        let a = parts.next().unwrap_or("");
        let d = parts.next().unwrap_or("");
        added += a.parse::<usize>().unwrap_or(0);
        deleted += d.parse::<usize>().unwrap_or(0);
    }
    LineCounts { added, deleted }
}

/// Apply the platform-specific flags we want on every git invocation.
pub(crate) fn configure_git_command(cmd: &mut std::process::Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// Total inserted/deleted lines in tracked files relative to HEAD. Returns
/// zero counts if the diff can't be computed (e.g. a repo with no commits yet).
fn run_git_numstat(cwd: &Path) -> LineCounts {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-c", "color.ui=never", "diff", "--numstat", "HEAD"])
        .current_dir(cwd);
    configure_git_command(&mut cmd);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            parse_numstat(&String::from_utf8_lossy(&output.stdout))
        }
        _ => LineCounts::default(),
    }
}

fn run_git_status(cwd: &Path) -> GitStatus {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-c", "color.ui=never", "status", "--short", "--branch"])
        .current_dir(cwd);
    configure_git_command(&mut cmd);

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) => return GitStatus::Failed(e.to_string()),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return GitStatus::NotARepo;
        }
        return GitStatus::Failed(stderr.into_owned());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tree = parse_git_status(&stdout);
    let LineCounts { added, deleted } = run_git_numstat(cwd);
    tree.added_lines = added;
    tree.deleted_lines = deleted;
    GitStatus::WorkTree(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_repo() {
        let output = "## main...origin/main\n";
        let data = parse_git_status(output);
        assert_eq!(data.branch.as_deref(), Some("main"));
        assert!(data.is_clean());
    }

    #[test]
    fn test_parse_dirty_repo() {
        let output = "## dave...origin/dave\n M src/ui/dave.rs\n M src/session.rs\nA  src/git_status.rs\n?? src/ui/git_status_ui.rs\n";
        let data = parse_git_status(output);
        assert_eq!(data.branch.as_deref(), Some("dave"));
        assert_eq!(data.files.len(), 4);
        assert_eq!(data.changed, 4);
        assert_eq!(data.modified, 2);
        assert_eq!(data.added, 1);
        assert_eq!(data.untracked, 1);
        assert_eq!(data.deleted, 0);
    }

    #[test]
    fn test_parse_detached_head() {
        let output = "## HEAD (no branch)\n M file.rs\n";
        let data = parse_git_status(output);
        assert!(data.branch.is_none());
        assert_eq!(data.files.len(), 1);
    }

    #[test]
    fn test_parse_deleted_file() {
        let output = "## main\n D deleted.rs\n";
        let data = parse_git_status(output);
        assert_eq!(data.deleted, 1);
    }

    /// Only a read that changes the status moves the generation.
    #[test]
    fn generation_moves_only_when_the_status_does() {
        let mut cache = GitStatusCache::new(PathBuf::from("/tmp/project"));
        let tree = || GitStatus::WorkTree(parse_git_status("## main\n M a.rs\n"));
        cache.land(tree());
        assert_eq!(cache.generation(), 1);
        cache.land(tree());
        assert_eq!(cache.generation(), 1, "the same tree again is no change");
        cache.land(GitStatus::WorkTree(parse_git_status("## main\n")));
        assert_eq!(cache.generation(), 2);
    }

    #[test]
    fn test_parse_numstat() {
        let output = "12\t3\tsrc/a.rs\n0\t7\tsrc/b.rs\n-\t-\tassets/logo.png\n";
        let LineCounts { added, deleted } = parse_numstat(output);
        assert_eq!(added, 12);
        assert_eq!(deleted, 10);
    }

    #[test]
    fn test_parse_numstat_empty() {
        assert_eq!(parse_numstat(""), LineCounts::default());
    }
}
