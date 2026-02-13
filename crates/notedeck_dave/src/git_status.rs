use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

/// A single file entry from git status --short
#[derive(Debug, Clone)]
pub struct GitFileEntry {
    /// Two-character status code (e.g., "M ", " M", "??", "A ")
    pub status: String,
    /// File path relative to repo root
    pub path: String,
}

/// Parsed result of git status --short --branch
#[derive(Debug, Clone)]
pub struct GitStatusData {
    /// Current branch name (None if detached HEAD)
    pub branch: Option<String>,
    /// List of file entries
    pub files: Vec<GitFileEntry>,
    /// When this data was fetched
    pub fetched_at: Instant,
}

impl GitStatusData {
    pub fn modified_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| {
                let b = f.status.as_bytes();
                (b[0] == b'M' || b[1] == b'M') && b[0] != b'?' && b[0] != b'A' && b[0] != b'D'
            })
            .count()
    }

    pub fn added_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.status.starts_with('A'))
            .count()
    }

    pub fn deleted_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| {
                let b = f.status.as_bytes();
                b[0] == b'D' || b[1] == b'D'
            })
            .count()
    }

    pub fn untracked_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| f.status.starts_with('?'))
            .count()
    }

    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum GitStatusError {
    NotARepo,
    CommandFailed(String),
}

pub type GitStatusResult = Result<GitStatusData, GitStatusError>;

/// Manages periodic git status checks for a session
pub struct GitStatusCache {
    cwd: PathBuf,
    current: Option<GitStatusResult>,
    receiver: Option<mpsc::Receiver<GitStatusResult>>,
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
                    self.current = Some(result);
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

    pub fn current(&self) -> Option<&GitStatusResult> {
        self.current.as_ref()
    }

    /// Mark cache as stale so next poll triggers a refresh
    pub fn invalidate(&mut self) {
        self.last_fetch = None;
    }
}

fn parse_git_status(output: &str) -> GitStatusData {
    let mut branch = None;
    let mut files = Vec::new();

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
                branch = Some(branch_name.to_string());
            }
        } else if line.len() >= 3 {
            // File entry: "XY path" where XY is 2-char status
            let status = line[..2].to_string();
            let path = line[3..].to_string();
            files.push(GitFileEntry { status, path });
        }
    }

    GitStatusData {
        branch,
        files,
        fetched_at: Instant::now(),
    }
}

fn run_git_status(cwd: &Path) -> GitStatusResult {
    let output = std::process::Command::new("git")
        .args(["status", "--short", "--branch"])
        .current_dir(cwd)
        .output()
        .map_err(|e| GitStatusError::CommandFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Err(GitStatusError::NotARepo);
        }
        return Err(GitStatusError::CommandFailed(stderr.into_owned()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_status(&stdout))
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
        assert_eq!(data.modified_count(), 2);
        assert_eq!(data.added_count(), 1);
        assert_eq!(data.untracked_count(), 1);
        assert_eq!(data.deleted_count(), 0);
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
        assert_eq!(data.deleted_count(), 1);
    }
}
