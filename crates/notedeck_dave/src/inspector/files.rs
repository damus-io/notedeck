//! The Files card's state, part one: the session's Claude Code temp dir.
//!
//! Claude Code gives every session a private directory under its per-user
//! temp root, holding `scratchpad/` (the agent's own scratch files) and
//! `tasks/<id>.output` (each background task's log). Dave never sees these
//! files go by in the stream, so the card lists the directory itself, off the
//! UI thread, every few seconds while it is drawn.
//!
//! The path rule is the CLI's own, read from Claude Code 2.1.288's bundle:
//!
//! ```text
//! (CLAUDE_CODE_TMPDIR || os.tmpdir()) / claude-<uid> / <slug of cwd> / <session id>
//! ```
//!
//! where the slug replaces every character outside `[A-Za-z0-9]` with `-`
//! (dots and underscores too), and a slug over 200 characters is cut there and
//! suffixed with a hash of the path. See [`project_slug`].

use super::background::OutputTails;
use super::LabelMemo;
use poll_promise::Promise;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How long a listing is trusted before the directory is read again.
const LIST_REFRESH: Duration = Duration::from_secs(3);

/// The longest slug Claude Code keeps whole (its `Pie`).
const SLUG_MAX: usize = 200;

/// The most entries a group lists; the rest are only counted.
pub const GROUP_CAP: usize = 50;

/// Claude Code's per-user temp root: `$CLAUDE_CODE_TMPDIR/claude-<uid>`, or
/// the system temp dir's when that is unset.
///
/// Dave's environment is the CLI's (the backend spawns it with Dave's), so
/// reading it here finds the same root the CLI chose.
pub fn claude_temp_root() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CODE_TMPDIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("claude-{}", current_uid()))
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Claude Code uses 0 where the platform has no uid.
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// A session's temp dir under `root`: `<root>/<slug of cwd>/<cli session id>`.
pub fn session_temp_dir(root: &Path, cwd: &Path, cli_session_id: &str) -> PathBuf {
    root.join(project_slug(&cwd.to_string_lossy()))
        .join(cli_session_id)
}

/// The directory name Claude Code gives a working directory: every character
/// outside `[A-Za-z0-9]` becomes `-`, and past 200 characters the slug is cut
/// and suffixed with `-<hash>`, the path's Java-style string hash in base 36.
///
/// The CLI works in UTF-16 code units, so a character outside the BMP becomes
/// two dashes, and the hash runs over the units too.
pub fn project_slug(cwd: &str) -> String {
    let mut slug = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else {
            for _ in 0..c.len_utf16() {
                slug.push('-');
            }
        }
    }
    if slug.len() <= SLUG_MAX {
        return slug;
    }
    // The slug is ASCII, so a byte cut is a character cut.
    slug.truncate(SLUG_MAX);
    slug.push('-');
    slug.push_str(&base36(java_string_hash(cwd).unsigned_abs() as u64));
    slug
}

/// JavaScript's `(h << 5) - h + charCodeAt(i) | 0` over the string's UTF-16
/// units: Java's `String.hashCode`.
fn java_string_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, unit| h.wrapping_mul(31).wrapping_add(unit as i32))
}

/// `n` in lowercase base 36, as JavaScript's `toString(36)`.
fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base-36 digits are ASCII")
}

/// One entry of a temp-dir group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempFile {
    /// The file name, lossily decoded.
    pub name: String,
    pub path: PathBuf,
    /// Bytes; zero for a directory.
    pub size: u64,
    /// Last modified, in Unix seconds; zero if unknown.
    pub modified: u64,
    /// A directory rather than a file (the scratchpad may hold some).
    pub is_dir: bool,
}

/// One subdirectory's entries, newest first, at most [`GROUP_CAP`] of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TempGroup {
    pub files: Vec<TempFile>,
    /// How many entries the directory holds, listed or not.
    pub total: usize,
}

/// What the session's temp dir held when it was last read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TempListing {
    /// The session's temp dir.
    pub dir: PathBuf,
    /// `scratchpad/`.
    pub scratchpad: TempGroup,
    /// `tasks/`: background-task output logs.
    pub tasks: TempGroup,
}

/// List a session's temp dir. A missing dir or subdir lists as empty: a
/// session that never wrote a scratch file or ran a background task has
/// neither.
pub fn list_session_temp(dir: &Path) -> TempListing {
    TempListing {
        dir: dir.to_path_buf(),
        scratchpad: list_group(&dir.join("scratchpad")),
        tasks: list_group(&dir.join("tasks")),
    }
}

/// One directory's entries, newest first (ties by name).
fn list_group(dir: &Path) -> TempGroup {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return TempGroup::default();
    };
    let mut files: Vec<TempFile> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let meta = entry.metadata().ok();
            TempFile {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                size: meta.as_ref().filter(|m| m.is_file()).map_or(0, |m| m.len()),
                modified: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs()),
                is_dir: meta.is_some_and(|m| m.is_dir()),
            }
        })
        .collect();
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    let total = files.len();
    files.truncate(GROUP_CAP);
    TempGroup { files, total }
}

/// The session a listing belongs to, so a session that changes its cwd or
/// CLI session id is listed afresh.
#[derive(Debug, PartialEq, Eq)]
struct ListedFor {
    cwd: PathBuf,
    cli_session_id: String,
}

/// The Files card's temp-dir state. See the module docs.
#[derive(Default)]
pub struct FilesState {
    /// Claude Code's temp root; [`claude_temp_root`] on first use.
    root: Option<PathBuf>,
    /// The session the listing is of.
    listed_for: Option<ListedFor>,
    /// The latest listing; `None` before the first read lands.
    listing: Option<TempListing>,
    /// Bumped each time a read lands, so rows built from it know to rebuild.
    generation: u64,
    /// A read in flight.
    reading: Option<Promise<TempListing>>,
    /// When the last read started.
    read_at: Option<Instant>,
    /// The display strings, keyed by the generation and the second they
    /// were built in, since ages move with the clock.
    pub rows: LabelMemo<(u64, u64), FileRows>,
    /// Tails of task output files the card has opened.
    pub tails: OutputTails,
    /// The output file whose tail is shown, if any.
    pub tail_open: Option<PathBuf>,
}

impl std::fmt::Debug for FilesState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesState")
            .field("root", &self.root)
            .field("listed_for", &self.listed_for)
            .field("listing", &self.listing)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl FilesState {
    /// Read temp dirs under `root` instead of Claude Code's: for tests.
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            ..Default::default()
        }
    }

    /// The listing of the temp dir of the session at `cwd` with CLI session
    /// `cli_session_id`, as last read, starting a fresh read off the UI thread
    /// if the last is older than a few seconds. `None` until the first read
    /// lands.
    pub fn poll(&mut self, cwd: &Path, cli_session_id: &str, now: Instant) -> Option<&TempListing> {
        let same_session = self
            .listed_for
            .as_ref()
            .is_some_and(|l| l.cwd == cwd && l.cli_session_id == cli_session_id);
        if !same_session {
            self.listed_for = Some(ListedFor {
                cwd: cwd.to_path_buf(),
                cli_session_id: cli_session_id.to_string(),
            });
            self.listing = None;
            self.reading = None;
            self.read_at = None;
            self.tail_open = None;
            self.generation += 1;
        }

        if let Some(listing) = self.reading.as_mut().and_then(|p| p.ready_mut()) {
            self.listing = Some(std::mem::take(listing));
            self.reading = None;
            self.generation += 1;
        }
        let stale = self
            .read_at
            .is_none_or(|at| now.duration_since(at) >= LIST_REFRESH);
        if self.reading.is_none() && stale {
            let root = self.root.get_or_insert_with(claude_temp_root);
            let dir = session_temp_dir(root, cwd, cli_session_id);
            self.reading = Some(Promise::spawn_thread("dave-temp-files", move || {
                list_session_temp(&dir)
            }));
            self.read_at = Some(now);
        }
        self.listing.as_ref()
    }

    /// The listing beside the row memo, tails and open tail, so the card can
    /// build rows from the one while borrowing the others. Call after
    /// [`Self::poll`].
    pub fn parts_mut(&mut self) -> FilesParts<'_> {
        FilesParts {
            listing: self.listing.as_ref(),
            generation: self.generation,
            rows: &mut self.rows,
            tails: &mut self.tails,
            tail_open: &mut self.tail_open,
        }
    }

    /// Show `path`'s tail, or hide it if it is the one shown.
    pub fn toggle_tail(tail_open: &mut Option<PathBuf>, path: &Path) {
        if tail_open.as_deref() == Some(path) {
            *tail_open = None;
        } else {
            *tail_open = Some(path.to_path_buf());
        }
    }

    /// How long until the listing should be read again, so the card can ask
    /// for a repaint then rather than every frame.
    pub fn refresh_after() -> Duration {
        LIST_REFRESH
    }
}

/// [`FilesState`] split for drawing: see [`FilesState::parts_mut`].
pub struct FilesParts<'a> {
    pub listing: Option<&'a TempListing>,
    pub generation: u64,
    pub rows: &'a mut LabelMemo<(u64, u64), FileRows>,
    pub tails: &'a mut OutputTails,
    pub tail_open: &'a mut Option<PathBuf>,
}

/// The Files card's display strings, built when the listing or the second
/// changes.
#[derive(Debug, Default)]
pub struct FileRows {
    /// The temp dir, for the card's path line.
    pub dir: String,
    /// `scratchpad/`'s heading and rows.
    pub scratchpad: GroupRows,
    /// `tasks/`'s heading and rows.
    pub tasks: GroupRows,
    /// What an empty group shows.
    pub empty: String,
    /// The Open, Tail and Hide buttons.
    pub open: String,
    pub tail: String,
    pub hide: String,
}

/// One group's display strings.
#[derive(Debug, Default)]
pub struct GroupRows {
    /// "scratchpad/ · 3"
    pub heading: String,
    /// One per listed entry, in listing order.
    pub rows: Vec<FileRow>,
    /// "5 older not shown", for a group past [`GROUP_CAP`].
    pub more: Option<String>,
}

/// One entry's display strings.
#[derive(Debug, Default)]
pub struct FileRow {
    /// The name, with a trailing `/` for a directory.
    pub name: String,
    /// "12.3 KB · 4m"
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn slug_replaces_everything_but_ascii_alphanumerics() {
        assert_eq!(
            project_slug("/home/jb55/dev/notedeck-dave"),
            "-home-jb55-dev-notedeck-dave"
        );
        assert_eq!(project_slug("/tmp/my.repo_v2"), "-tmp-my-repo-v2");
        assert_eq!(project_slug("/srv/café"), "-srv-caf-");
        // Outside the BMP is two UTF-16 units, so two dashes.
        assert_eq!(project_slug("/a/🦀"), "-a---");
    }

    /// Past 200 characters the CLI cuts the slug and hashes the path.
    #[test]
    fn a_long_slug_is_cut_and_hashed() {
        let cwd = format!("/{}", "a".repeat(250));
        let slug = project_slug(&cwd);
        let (head, hash) = slug.split_at(SLUG_MAX);
        assert_eq!(head, &format!("-{}", "a".repeat(199)));
        // Java's "a".hashCode() is 97; check the hash against a known value
        // rather than the function under test.
        assert_eq!(java_string_hash("a"), 97);
        assert_eq!(java_string_hash("ab"), 97 * 31 + 98);
        assert_eq!(base36(97), "2p");
        assert_eq!(
            hash,
            format!("-{}", base36(java_string_hash(&cwd).unsigned_abs() as u64))
        );
        assert_eq!(project_slug(&"b".repeat(200)), "b".repeat(200));
    }

    #[test]
    fn session_dir_joins_root_slug_and_id() {
        assert_eq!(
            session_temp_dir(
                Path::new("/tmp/claude-1000"),
                Path::new("/home/jb55/dev/notedeck-dave"),
                "438410d8-f269-49e6-abfe-9304042beed7"
            ),
            Path::new(
                "/tmp/claude-1000/-home-jb55-dev-notedeck-dave/438410d8-f269-49e6-abfe-9304042beed7"
            )
        );
    }

    fn touch(path: &Path, body: &str, modified: u64) {
        std::fs::write(path, body).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified))
            .unwrap();
    }

    /// The lister reads both groups newest first, with sizes, and marks
    /// directories.
    #[test]
    fn lists_both_groups_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let dir = session_temp_dir(root.path(), Path::new("/w/repo"), "sess");
        std::fs::create_dir_all(dir.join("scratchpad/notes")).unwrap();
        std::fs::create_dir_all(dir.join("tasks")).unwrap();
        touch(&dir.join("scratchpad/old.txt"), "old", 1_000);
        touch(&dir.join("scratchpad/new.md"), "newer file", 3_000);
        touch(&dir.join("tasks/b1.output"), "x", 2_000);
        touch(&dir.join("tasks/b2.output"), "xyz", 2_500);

        let listing = list_session_temp(&dir);
        let names = |g: &TempGroup| g.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        // The subdirectory's mtime is now, so it sorts first.
        assert_eq!(names(&listing.scratchpad), ["notes", "new.md", "old.txt"]);
        assert!(listing.scratchpad.files[0].is_dir);
        assert_eq!(listing.scratchpad.files[1].size, 10);
        assert_eq!(listing.scratchpad.files[1].modified, 3_000);
        assert_eq!(names(&listing.tasks), ["b2.output", "b1.output"]);
        assert_eq!(listing.tasks.total, 2);
    }

    /// A session that never wrote anything has no dir: an empty listing,
    /// not an error.
    #[test]
    fn a_missing_dir_lists_empty() {
        let root = tempfile::tempdir().unwrap();
        let listing = list_session_temp(&root.path().join("nope"));
        assert_eq!(listing.scratchpad, TempGroup::default());
        assert_eq!(listing.tasks, TempGroup::default());
    }

    /// A big group lists the newest [`GROUP_CAP`] and counts the rest.
    #[test]
    fn a_big_group_is_capped_but_counted() {
        let root = tempfile::tempdir().unwrap();
        let tasks = root.path().join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        for i in 0..(GROUP_CAP + 5) {
            touch(&tasks.join(format!("t{i}.output")), "", i as u64 + 1);
        }
        let listing = list_session_temp(root.path());
        assert_eq!(listing.tasks.files.len(), GROUP_CAP);
        assert_eq!(listing.tasks.total, GROUP_CAP + 5);
        assert_eq!(
            listing.tasks.files[0].name,
            format!("t{}.output", GROUP_CAP + 4)
        );
    }

    /// Polling reads off-thread, lands the listing, and starts over when
    /// the session changes.
    #[test]
    fn poll_lands_a_listing_and_resets_on_a_new_session() {
        let root = tempfile::tempdir().unwrap();
        let dir = session_temp_dir(root.path(), Path::new("/w/repo"), "s1");
        std::fs::create_dir_all(dir.join("tasks")).unwrap();
        touch(&dir.join("tasks/b1.output"), "hi", 5);

        let mut state = FilesState::with_root(root.path().to_path_buf());
        let cwd = Path::new("/w/repo");
        let now = Instant::now();
        assert!(state.poll(cwd, "s1", now).is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        let listing = loop {
            if let Some(listing) = state.poll(cwd, "s1", now) {
                break listing.clone();
            }
            assert!(Instant::now() < deadline, "the listing never landed");
            std::thread::yield_now();
        };
        assert_eq!(listing.dir, dir);
        assert_eq!(listing.tasks.files[0].name, "b1.output");

        assert!(state.poll(cwd, "s2", now).is_none());
    }
}
