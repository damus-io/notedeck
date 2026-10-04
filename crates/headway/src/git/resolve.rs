//! The git resolver: given a card's review record, find the commit it names on
//! *this* host — fetching it from the host that recorded it when it isn't here —
//! and read it back as a patch.
//!
//! The problem it solves is that a record says where a commit lived on the
//! machine that made it (`host`, `path`), and a reviewer is usually somewhere
//! else, with their own checkout at their own path, or none at all. Resolution
//! runs in four steps, each described on the function that does it:
//!
//! 1. [`pick_target`]: which repo on this host the commit should end up in — the
//!    recorded path itself when it's a checkout here (whatever the recorded
//!    hostname), a known checkout of the same repo (matched by repo identity,
//!    the root commit), or a headway-owned bare cache.
//! 2. Is the commit already there? Then it's [`Found::Local`].
//! 3. [`fetch_source`] + [`fetch`]: where to fetch it from, and the fetch itself
//!    (by sha, then by branch when the server refuses a sha want).
//! 4. [`trailer_search`]: still missing — a rebase changed the hash, or the card
//!    predates review records — so find the commit by its
//!    `Headway: <card ref>` trailer instead.
//!
//! Everything here is blocking and UI-free: the CLI calls it inline, the GUI off
//! the UI thread. Fetches never prompt (see [`git_fetch`]).

use std::borrow::Cow;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{GitError, git};
use crate::event::ReviewFields;

/// What [`resolve`] needs to know about this host.
pub struct ResolveCtx<'a> {
    /// This machine's hostname, compared against a record's `host` to tell a
    /// record made here from one made elsewhere.
    pub local_host: &'a str,
    /// Where the bare cache repos live, one per repo identity:
    /// `<cache_root>/<repo>.git`. Created on first use.
    pub cache_root: &'a Path,
    /// Checkouts on this host to try, in order, before falling back to the
    /// cache. Only one whose repo identity matches the record's is used.
    pub known_checkouts: &'a [PathBuf],
    /// Upper bound on one `git fetch`; the fetch is killed past it.
    pub timeout: Duration,
}

/// Which kind of repo a [`Resolved`] commit sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The record's own `path`: the record was made on this host.
    Recorded,
    /// A checkout matched by repo identity: one of
    /// [`ResolveCtx::known_checkouts`], or the record's own `path` when the
    /// record names another host.
    Checkout,
    /// The headway-owned bare cache under [`ResolveCtx::cache_root`].
    Cache,
}

/// How a [`Resolved`] commit was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// It was already in the target repo.
    Local,
    /// It was fetched into the target repo from `from` (a URL, `host:path`, or
    /// a local path).
    Fetched { from: String },
    /// The recorded sha couldn't be had, so it was found by the card's
    /// `Headway:` trailer — the hash may differ from the record's.
    ByTrailer,
}

/// A commit [`resolve`] found: where it is and how it got there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The repo the commit is in: the one to run `git show` against.
    pub repo_dir: PathBuf,
    /// The commit's full sha. For [`Found::ByTrailer`] this is the trailer
    /// match, not the record's sha.
    pub sha: String,
    pub target: Target,
    pub how: Found,
}

impl Resolved {
    /// A few words saying where the commit came from, for a UI that shows the
    /// full [`Display`](fmt::Display) sentence only on hover: `local checkout`,
    /// `headway cache`, `fetched from <from>`, or `found by Headway trailer`.
    /// How it was found outranks where it sits, so a fetch or a trailer match
    /// names itself whatever repo it landed in.
    pub fn source_label(&self) -> Cow<'static, str> {
        match (&self.how, self.target) {
            (Found::Local, Target::Cache) => Cow::Borrowed("headway cache"),
            (Found::Local, Target::Recorded | Target::Checkout) => Cow::Borrowed("local checkout"),
            (Found::Fetched { from }, _) => Cow::Owned(format!("fetched from {from}")),
            (Found::ByTrailer, _) => Cow::Borrowed("found by Headway trailer"),
        }
    }
}

impl fmt::Display for Resolved {
    /// One line saying where the commit was found, e.g. `fetched from
    /// jex0:repos/notedeck into /x/cache/abc.git (headway cache)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dir = self.repo_dir.display();
        match &self.how {
            Found::Local => write!(f, "found locally in {dir}")?,
            Found::Fetched { from } => write!(f, "fetched from {from} into {dir}")?,
            Found::ByTrailer => write!(f, "found by its Headway trailer in {dir}")?,
        }
        if self.target == Target::Cache {
            write!(f, " (headway cache)")?;
        }
        Ok(())
    }
}

/// Find the commit `record` names on this host, fetching it if needed, and
/// fall back to the card's `Headway: <card_ref>` trailer when the recorded sha
/// can't be had. `card_ref` is the card's `headway:<board>/<word-id>`.
///
/// A record without a `commit` goes straight to the trailer search. The error,
/// when nothing is found, is the fetch's own (git's stderr verbatim) if a fetch
/// was tried, else a short explanation of what was missing.
pub fn resolve(
    record: &ReviewFields,
    card_ref: &str,
    ctx: &ResolveCtx,
) -> Result<Resolved, GitError> {
    let (repo_dir, target) = pick_target(record, ctx)?;
    let found = |sha: String, how: Found| {
        // Pin whatever the cache resolved, so a later `gc` there can't drop it.
        if target == Target::Cache {
            pin(&repo_dir, &sha);
        }
        Resolved {
            repo_dir: repo_dir.clone(),
            sha,
            target,
            how,
        }
    };

    let mut fetch_err = None;
    // A branch tip the fetch fell back to, which the trailer search must also
    // walk: in a user checkout it's only in FETCH_HEAD, which `--all` misses.
    let mut fetched_tip = None;
    if let Some(sha) = record.commit.as_deref() {
        if !is_hex_sha(sha) {
            return Err(resolve_error(format!("'{sha}' is not a commit sha")));
        }
        if let Ok(full) = full_commit(&repo_dir, sha) {
            return Ok(found(full, Found::Local));
        }
        if let Some(src) = fetch_source(record, &repo_dir, ctx) {
            match fetch(&repo_dir, &src, sha, record.branch.as_deref(), ctx.timeout) {
                Ok(tip) => {
                    fetched_tip = tip;
                    if let Ok(full) = full_commit(&repo_dir, sha) {
                        return Ok(found(full, Found::Fetched { from: src }));
                    }
                }
                Err(e) => fetch_err = Some(e),
            }
        }
    }

    if let Some(sha) = trailer_search(&repo_dir, card_ref, fetched_tip.as_deref()) {
        return Ok(found(sha, Found::ByTrailer));
    }
    Err(fetch_err.unwrap_or_else(|| {
        resolve_error(match record.commit.as_deref() {
            Some(sha) => format!(
                "commit {sha} isn't in {} and there's nowhere to fetch it from \
                 (the record has no remote, and no host/path)",
                repo_dir.display()
            ),
            None => format!(
                "the record has no commit and nothing in {} carries a \
                 'Headway: {card_ref}' trailer",
                repo_dir.display()
            ),
        })
    }))
}

/// Find a card's commit by its `Headway: <card_ref>` trailer alone, for a card
/// with no review record (it finished before records existed). Searches each of
/// `repo_dirs` in order and returns the newest match in the first repo that has
/// one, as [`Found::ByTrailer`] in a [`Target::Checkout`].
pub fn resolve_by_trailer(card_ref: &str, repo_dirs: &[PathBuf]) -> Option<Resolved> {
    repo_dirs.iter().find_map(|dir| {
        let sha = trailer_search(dir, card_ref, None)?;
        Some(Resolved {
            repo_dir: dir.clone(),
            sha,
            target: Target::Checkout,
            how: Found::ByTrailer,
        })
    })
}

/// One commit read back for review: its header and its patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitPatch {
    pub sha: String,
    /// `Name <email>`.
    pub author: String,
    /// Author date, strict ISO 8601.
    pub date: String,
    /// The same author date as unix seconds, for a relative "3h ago".
    pub time: u64,
    /// The full commit message, subject and body.
    pub message: String,
    /// The `git show` patch (diff only, no header), cut at a line boundary when
    /// it ran past the cap.
    pub patch: String,
    /// Whether `patch` was cut short.
    pub truncated: bool,
}

impl CommitPatch {
    /// The author's name alone: [`author`](Self::author) without its
    /// ` <email>`, or all of it when there's no email to drop.
    pub fn author_name(&self) -> &str {
        self.author
            .split_once(" <")
            .map_or(self.author.as_str(), |(name, _)| name)
    }
}

/// Read commit `sha` in `repo_dir` as a [`CommitPatch`], its patch capped at
/// `max_bytes` (cut back to the last whole line, `truncated` set).
///
/// One `git show` produces both halves: the header fields NUL-separated (a
/// commit message can't carry a NUL, so the split is unambiguous), then the
/// patch after the last NUL.
pub fn commit_patch(repo_dir: &Path, sha: &str, max_bytes: usize) -> Result<CommitPatch, GitError> {
    let out = git_bytes(
        repo_dir,
        &[
            "show",
            "--no-color",
            "--no-ext-diff",
            "--patch",
            "--find-renames",
            "--format=%H%x00%an <%ae>%x00%aI%x00%at%x00%B%x00",
            "--end-of-options",
            sha,
        ],
    )?;
    let mut parts = out.splitn(6, |b| *b == 0);
    let mut field = || String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
    let (sha, author, date, time, message) = (field(), field(), field(), field(), field());
    let rest = parts.next().unwrap_or_default();
    // `%B` ends with a newline, and git puts a blank line before the patch.
    let start = rest.iter().position(|b| *b != b'\n').unwrap_or(rest.len());
    let rest = &rest[start..];
    let (patch, truncated) = cap_at_line(rest, max_bytes);
    Ok(CommitPatch {
        sha,
        author,
        date,
        time: time.parse().unwrap_or(0),
        message: message.trim_end().to_string(),
        patch: String::from_utf8_lossy(patch).into_owned(),
        truncated,
    })
}

/// A file's content at one commit, as [`blob_bytes`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    /// The rev has no such path: the parent side of an added file, the
    /// commit side of a deleted one, or any path at a root commit's parent.
    Missing,
    /// Bigger than the cap, so not read: its size in bytes.
    TooLarge(u64),
    /// Its content.
    Bytes(Vec<u8>),
}

/// File `path` as of `rev` in `repo_dir` (`git cat-file blob <rev>:<path>`),
/// unless it's over `max_bytes`: its size is checked first, so a huge file is
/// never read. A `rev` or `path` that doesn't name a blob is
/// [`Blob::Missing`], not an error; git failing to run is one.
pub fn blob_bytes(
    repo_dir: &Path,
    rev: &str,
    path: &str,
    max_bytes: u64,
) -> Result<Blob, GitError> {
    let spec = format!("{rev}:{path}");
    // Non-zero and silent when `spec` names nothing: a missing path, or a
    // root commit's `^`.
    let Ok(oid) = git(repo_dir, &["rev-parse", "--verify", "--quiet", &spec]) else {
        return Ok(Blob::Missing);
    };
    let size = git(repo_dir, &["cat-file", "-s", &oid])?;
    let size: u64 = size.parse().map_err(|_| GitError {
        command: format!("cat-file -s {oid}"),
        stderr: format!("not a size: {size:?}"),
    })?;
    if size > max_bytes {
        return Ok(Blob::TooLarge(size));
    }
    git_bytes(repo_dir, &["cat-file", "blob", &oid]).map(Blob::Bytes)
}

/// `bytes` cut to at most `max` bytes, backed up to just after the last newline
/// so the patch never ends mid-line. Returns whether anything was cut.
fn cap_at_line(bytes: &[u8], max: usize) -> (&[u8], bool) {
    if bytes.len() <= max {
        return (bytes, false);
    }
    let head = &bytes[..max];
    let end = head.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    (&bytes[..end], true)
}

/// Step 1: the repo on this host the commit should be found or fetched into,
/// first match wins:
///
/// 1. the record's own `path`, when it is a checkout of the same repo on this
///    machine — [`Target::Recorded`] when the record's `host` is this one,
///    [`Target::Checkout`] when it isn't;
/// 2. the first of [`ResolveCtx::known_checkouts`] whose repo identity matches;
/// 3. the bare cache `<cache_root>/<repo>.git`, created on first use.
///
/// The recorded path is tried whatever its `host` says, because a hostname is
/// only a hint: macOS renames a machine with its network, so a record made
/// here can name a host this machine no longer answers to. The repo identity
/// is the check that counts, and a same-repo checkout at that path is as good
/// a target as any known checkout even when it is a different machine's
/// namesake.
///
/// A record with no repo identity can't be matched or cached, so any checkout
/// is taken as a guess; with none it's an error.
fn pick_target(record: &ReviewFields, ctx: &ResolveCtx) -> Result<(PathBuf, Target), GitError> {
    let repo = record.repo.as_deref();
    if let Some(path) = record.path.as_deref().map(PathBuf::from)
        && same_repo(&path, repo)
    {
        let recorded_here = record
            .host
            .as_deref()
            .is_some_and(|h| host_matches(h, ctx.local_host));
        let target = if recorded_here {
            Target::Recorded
        } else {
            Target::Checkout
        };
        return Ok((path, target));
    }
    if let Some(dir) = ctx.known_checkouts.iter().find(|d| same_repo(d, repo)) {
        return Ok((dir.clone(), Target::Checkout));
    }
    let Some(repo) = repo.filter(|r| is_hex_sha(r)) else {
        return Err(resolve_error(
            "the record has no repo identity and there is no local checkout to use".to_string(),
        ));
    };
    let dir = ctx.cache_root.join(format!("{repo}.git"));
    if !dir.join("HEAD").is_file() {
        std::fs::create_dir_all(&dir).map_err(|e| GitError {
            command: format!("mkdir {}", dir.display()),
            stderr: e.to_string(),
        })?;
        git(&dir, &["init", "--bare", "--quiet"])?;
    }
    Ok((dir, Target::Cache))
}

/// Whether `dir` is a checkout of the repo whose identity is `repo`: one of the
/// roots of its `HEAD` history. With no identity to compare, any repo passes.
fn same_repo(dir: &Path, repo: Option<&str>) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let Some(repo) = repo else {
        return git(dir, &["rev-parse", "--git-dir"]).is_ok();
    };
    git(dir, &["rev-list", "--max-parents=0", "HEAD"])
        .is_ok_and(|roots| roots.lines().any(|r| r.trim() == repo))
}

/// Step 3a: where to fetch the record's commit from, first match wins:
///
/// 1. the record's explicit `remote`;
/// 2. made on this host: the record's `path` itself, when it's a directory
///    other than the target (a local fetch into a checkout or the cache);
/// 3. a remote of the target repo whose URL host is the record's `host` — so
///    jb55's `jex0` remote (`jex0:repos/notedeck`) serves a record from `jex0`;
/// 4. the scp-style `<host>:<path>`.
///
/// `None` when the record says too little to reach its host.
fn fetch_source(record: &ReviewFields, repo_dir: &Path, ctx: &ResolveCtx) -> Option<String> {
    if let Some(remote) = record.remote.as_deref() {
        return Some(remote.to_string());
    }
    let host = record.host.as_deref()?;
    if host_matches(host, ctx.local_host) {
        let path = record.path.as_deref()?;
        let other = !same_dir(Path::new(path), repo_dir);
        return (Path::new(path).is_dir() && other).then(|| path.to_string());
    }
    if let Some(url) = remote_urls(repo_dir)
        .into_iter()
        .find(|url| remote_host(url).is_some_and(|h| host_matches(h, host)))
    {
        return Some(url);
    }
    record.path.as_deref().map(|path| format!("{host}:{path}"))
}

/// Whether two paths are the same directory (after resolving symlinks).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The fetch URLs of every remote configured in `repo_dir` (none for the bare
/// cache, or when git fails).
fn remote_urls(repo_dir: &Path) -> Vec<String> {
    let Ok(out) = git(repo_dir, &["config", "--get-regexp", r"^remote\..*\.url$"]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.split_once(' ').map(|(_, url)| url.trim().to_string()))
        .collect()
}

/// The ssh host a git remote URL points at, or `None` when it isn't an ssh URL
/// (https, git://, file://, a local path).
///
/// Understands `ssh://[user@]host[:port]/path` (and the `git+ssh`/`ssh+git`
/// spellings) and scp-style `[user@]host:path`, git's rule for which being that
/// the part before the first `:` has no `/`.
pub fn remote_host(url: &str) -> Option<&str> {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "ssh" | "git+ssh" | "ssh+git") {
            return None;
        }
        let authority = rest.split('/').next()?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        return non_empty(strip_port(host));
    }
    let (before, _) = url.split_once(':')?;
    if before.contains('/') {
        return None;
    }
    let host = before.rsplit_once('@').map_or(before, |(_, h)| h);
    non_empty(host.trim_start_matches('[').trim_end_matches(']'))
}

/// `host[:port]` without the port; a bracketed IPv6 `[addr]:port` keeps `addr`.
fn strip_port(host: &str) -> &str {
    if let Some(inner) = host.strip_prefix('[') {
        return inner.split(']').next().unwrap_or(inner);
    }
    host.split(':').next().unwrap_or(host)
}

/// `Some(s)` unless `s` is empty.
fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Whether a record's host and this host (or a remote's host) are the same
/// machine: equal ignoring case, or equal on the first DNS label, so `jex0`
/// matches `jex0.lan`.
fn host_matches(a: &str, b: &str) -> bool {
    let label = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    a.eq_ignore_ascii_case(b) || label(a) == label(b)
}

/// Step 3b: fetch `sha` from `src` into `repo_dir`, to FETCH_HEAD only — never
/// creating a ref in a user's checkout (the cache pins what it keeps
/// separately, see [`pin`]).
///
/// When the sha want fails (a server that only serves advertised refs, or a
/// rebased commit that no longer exists there) and the record named a branch,
/// fetch that branch instead and return its tip, which the trailer search then
/// walks. `Ok(None)` means the sha fetch itself succeeded. An error keeps both
/// attempts' stderr.
fn fetch(
    repo_dir: &Path,
    src: &str,
    sha: &str,
    branch: Option<&str>,
    timeout: Duration,
) -> Result<Option<String>, GitError> {
    let by_sha = match git_fetch(repo_dir, src, sha, timeout) {
        Ok(()) => return Ok(None),
        Err(e) => e,
    };
    let Some(branch) = branch.filter(|b| is_plain_branch(b)) else {
        return Err(by_sha);
    };
    match git_fetch(repo_dir, src, branch, timeout) {
        Ok(()) => Ok(git(
            repo_dir,
            &["rev-parse", "--verify", "--quiet", "FETCH_HEAD"],
        )
        .ok()),
        Err(by_branch) => Err(GitError {
            command: by_sha.command,
            stderr: format!(
                "{}\nthen fetching branch '{branch}': {}",
                by_sha.stderr, by_branch.stderr
            ),
        }),
    }
}

/// Whether `branch` is safe to hand `git fetch` as a refspec: a plain name, no
/// `src:dst` (which would write a ref), no force `+`, no leading `-`, no glob.
fn is_plain_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with(['-', '+'])
        && !branch.contains([':', '*', ' ', '\t', '\n', '^', '~', '?', '['])
        && !branch.contains("..")
}

/// `git fetch --no-tags <src> <what>`, non-interactively and under `timeout`.
///
/// It must never block on a human: `GIT_TERMINAL_PROMPT=0` stops https
/// credential prompts, and unless the user has set their own `GIT_SSH_COMMAND`
/// ssh runs with `BatchMode=yes` (no password or host-key prompt) and a connect
/// timeout. Past `timeout` git is killed; an ssh it spawned outlives it by at
/// most its own connect timeout. `ext::` transports are refused outright, since
/// `src` comes from a record someone else wrote.
fn git_fetch(repo_dir: &Path, src: &str, what: &str, timeout: Duration) -> Result<(), GitError> {
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "fetch",
        "--no-tags",
        "--quiet",
        "--end-of-options",
        src,
        what,
    ];
    let command = format!("fetch --no-tags {src} {what}");
    let err = |stderr: String| GitError {
        command: command.clone(),
        stderr,
    };

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        );
    }
    let mut child = cmd.spawn().map_err(|e| err(e.to_string()))?;

    // Drain stderr on its own thread so a chatty git can't fill the pipe and
    // stall; it's left detached if we give up on the child below.
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(format!("timed out after {}s", timeout.as_secs())));
            }
            Err(e) => return Err(err(e.to_string())),
        }
    };
    let stderr = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(err(stderr.trim().to_string()))
    }
}

/// Keep `sha` alive in the bare cache with `refs/headway/review/<sha>`, since a
/// commit only in FETCH_HEAD is fair game for `gc`. Best-effort: an unpinned
/// commit still resolves now, it just may need fetching again later.
fn pin(cache: &Path, sha: &str) {
    let _ = git(
        cache,
        &["update-ref", &format!("refs/headway/review/{sha}"), sha],
    );
}

/// Step 4: the newest commit in `repo_dir` carrying the trailer line
/// `Headway: <card_ref>`, searching every ref plus `extra_tip` (a fetched
/// branch that only FETCH_HEAD points at).
///
/// The match is a whole line, so `…/soft-exclude-cruel` doesn't also match a
/// longer word-id it happens to prefix.
fn trailer_search(repo_dir: &Path, card_ref: &str, extra_tip: Option<&str>) -> Option<String> {
    let grep = format!("--grep=^Headway: {}$", escape_basic_regex(card_ref));
    let mut args = vec![
        "log",
        "--format=%H",
        "--max-count=1",
        grep.as_str(),
        "--all",
    ];
    args.extend(extra_tip);
    let out = git(repo_dir, &args).ok()?;
    out.lines()
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// `s` with basic-regex metacharacters backslash-escaped, so a card ref is
/// matched literally inside the anchored trailer pattern.
fn escape_basic_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '.' | '[' | ']' | '*' | '^' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The full sha of `sha` (which may be a prefix) as a commit in `repo_dir`,
/// erroring when it isn't there.
fn full_commit(repo_dir: &Path, sha: &str) -> Result<String, GitError> {
    super::resolve_commit(repo_dir, sha)
}

/// Whether `s` looks like a (possibly abbreviated) object id: 4 to 64 hex
/// digits. Guards every record-supplied sha before it reaches git's argv or a
/// cache path.
fn is_hex_sha(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A [`GitError`] for a resolution that failed without a git command to blame.
fn resolve_error(stderr: String) -> GitError {
    GitError {
        command: "resolve".to_string(),
        stderr,
    }
}

/// Run `git -C <dir> <args>` and return its raw stdout (the patch may not be
/// UTF-8, and trimming would eat its trailing newline).
fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| GitError {
            command: args.join(" "),
            stderr: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(GitError {
            command: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card ref every fixture commit's trailer names.
    const CARD: &str = "headway:headway/soft-exclude-cruel";

    /// Run git in `dir` with a throwaway identity, panicking on failure.
    fn run(dir: &Path, args: &[&str]) -> String {
        let mut full = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        git(dir, &full).unwrap()
    }

    /// A fresh repo at `dir` on branch `trunk` with one root commit. The root
    /// names `dir`, so two fixture repos never share an identity by accident
    /// (same content in the same second is the same sha).
    fn init_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        run(dir, &["init", "-q", "-b", "trunk"]);
        commit(dir, "root", &format!("root of {}", dir.display()))
    }

    /// Commit a file `name` whose content is `msg`, with message `msg`;
    /// returns the new sha.
    fn commit(dir: &Path, name: &str, msg: &str) -> String {
        std::fs::write(dir.join(name), format!("{msg}\n")).unwrap();
        run(dir, &["add", name]);
        run(dir, &["commit", "-q", "-m", msg]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// A clone of `src` at `dst` (same repo identity, all current commits).
    fn clone(src: &Path, dst: &Path) {
        let parent = dst.parent().unwrap();
        run(
            parent,
            &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()],
        );
    }

    /// A review record as `headway review` would write it for `sha` in `dir`,
    /// claiming to come from `host`.
    fn record(dir: &Path, sha: &str, host: &str) -> ReviewFields {
        ReviewFields {
            commit: Some(sha.to_string()),
            branch: Some("trunk".to_string()),
            host: Some(host.to_string()),
            path: Some(dir.to_str().unwrap().to_string()),
            repo: Some(super::super::repo_identity(dir, sha).unwrap()),
            ..Default::default()
        }
    }

    /// `rec` as another machine would have recorded it: its `path` is moved
    /// to one that doesn't exist here, so only its `remote` reaches it.
    fn from_elsewhere(mut rec: ReviewFields) -> ReviewFields {
        rec.path = Some("/nonexistent/elsewhere/repo".to_string());
        rec
    }

    /// A resolve context for host `here` with `cache` and `checkouts`.
    fn ctx<'a>(cache: &'a Path, checkouts: &'a [PathBuf]) -> ResolveCtx<'a> {
        ResolveCtx {
            local_host: "here",
            cache_root: cache,
            known_checkouts: checkouts,
            timeout: Duration::from_secs(30),
        }
    }

    /// ssh-ish URLs yield their host; everything else doesn't.
    #[test]
    fn remote_host_parses_ssh_urls_only() {
        assert_eq!(remote_host("jex0:repos/notedeck"), Some("jex0"));
        assert_eq!(
            remote_host("git@github.com:damus-io/notedeck"),
            Some("github.com")
        );
        assert_eq!(remote_host("ssh://jex0/home/jb55/notedeck"), Some("jex0"));
        assert_eq!(remote_host("ssh://git@jex0:2222/repo.git"), Some("jex0"));
        assert_eq!(remote_host("git+ssh://u@box.lan/r"), Some("box.lan"));
        assert_eq!(remote_host("ssh://u@[::1]:22/r"), Some("::1"));
        assert_eq!(remote_host("https://github.com/damus-io/notedeck"), None);
        assert_eq!(remote_host("git://github.com/x"), None);
        assert_eq!(remote_host("file:///tmp/x"), None);
        assert_eq!(remote_host("/home/jb55/notedeck"), None);
        assert_eq!(remote_host("./a:b"), None);
        assert_eq!(remote_host(":path"), None);
    }

    /// Hostnames match case-insensitively and on their first label.
    #[test]
    fn hosts_match_on_first_label() {
        assert!(host_matches("jex0", "JEX0"));
        assert!(host_matches("jex0", "jex0.lan"));
        assert!(!host_matches("jex0", "jex1"));
    }

    /// Only plain branch names are fetched; a `src:dst` would write a ref.
    #[test]
    fn only_plain_branches_are_fetched() {
        assert!(is_plain_branch("headway"));
        assert!(is_plain_branch("feat/x-y"));
        for bad in ["", "-x", "+main", "a:b", "a..b", "ma*"] {
            assert!(!is_plain_branch(bad), "{bad:?}");
        }
    }

    /// A commit already in the recorded checkout on this host is found there.
    #[test]
    fn local_hit_in_recorded_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let sha = commit(&repo, "a", "work");

        let got = resolve(&record(&repo, &sha, "here"), CARD, &ctx(tmp.path(), &[])).unwrap();
        assert_eq!(got.how, Found::Local);
        assert_eq!(got.target, Target::Recorded);
        assert_eq!(got.sha, sha);
    }

    /// A record whose host has since been renamed (a Mac picks its hostname
    /// up from the network) is still found in its recorded path, which is a
    /// checkout of the same repo here — no fetch from the old name, no cache.
    #[test]
    fn renamed_host_finds_the_commit_in_its_recorded_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let sha = commit(&repo, "a", "work");
        let cache = tmp.path().join("cache");

        let rec = record(&repo, &sha, "J497044J94.local");
        let got = resolve(&rec, CARD, &ctx(&cache, &[])).unwrap();
        assert_eq!(got.how, Found::Local);
        assert_eq!(got.target, Target::Checkout);
        assert_eq!(got.repo_dir, repo);
        assert_eq!(got.sha, sha);
        assert!(!cache.exists(), "cache untouched");
    }

    /// A record from another host whose commit this checkout lacks is fetched
    /// into the checkout (FETCH_HEAD only, no new refs), from `record.remote`.
    #[test]
    fn fetches_into_a_checkout_missing_the_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        let mine = tmp.path().join("mine");
        init_repo(&theirs);
        clone(&theirs, &mine);
        let sha = commit(&theirs, "a", "their work");
        let refs_before = run(&mine, &["for-each-ref"]);

        let mut rec = from_elsewhere(record(&theirs, &sha, "elsewhere"));
        rec.remote = Some(theirs.to_str().unwrap().to_string());
        let checkouts = [mine.clone()];
        let got = resolve(&rec, CARD, &ctx(&tmp.path().join("cache"), &checkouts)).unwrap();

        assert_eq!(got.target, Target::Checkout);
        assert_eq!(got.repo_dir, mine);
        assert!(matches!(got.how, Found::Fetched { .. }), "{got:?}");
        assert_eq!(
            run(&mine, &["for-each-ref"]),
            refs_before,
            "no refs created"
        );
        assert!(!tmp.path().join("cache").exists(), "cache untouched");
    }

    /// With no checkout of the repo, the commit lands in the bare cache and is
    /// pinned there; a second resolve then finds it locally.
    #[test]
    fn fetches_into_the_bare_cache_without_a_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        init_repo(&theirs);
        let sha = commit(&theirs, "a", "their work");
        let cache = tmp.path().join("cache");

        let mut rec = from_elsewhere(record(&theirs, &sha, "elsewhere"));
        rec.remote = Some(theirs.to_str().unwrap().to_string());
        let got = resolve(&rec, CARD, &ctx(&cache, &[])).unwrap();
        assert_eq!(got.target, Target::Cache);
        assert_eq!(
            got.repo_dir,
            cache.join(format!("{}.git", rec.repo.as_deref().unwrap()))
        );
        assert!(matches!(got.how, Found::Fetched { .. }), "{got:?}");
        let pinned = run(
            &got.repo_dir,
            &["rev-parse", &format!("refs/headway/review/{sha}")],
        );
        assert_eq!(pinned, sha);

        let again = resolve(&rec, CARD, &ctx(&cache, &[])).unwrap();
        assert_eq!(again.how, Found::Local);
    }

    /// A record made on this host whose path isn't a checkout of the recorded
    /// repo isn't used as the target, but its path is still a local fetch
    /// source for the cache.
    #[test]
    fn local_host_record_fetches_from_its_path_into_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        init_repo(&src);
        let sha = commit(&src, "a", "work");
        let cache = tmp.path().join("cache");
        let mut rec = record(&src, &sha, "here");
        // Pretend the record's identity is a different root, so `src` doesn't
        // qualify as the target and the cache is used — then `src` is still a
        // valid local fetch source.
        let root = rec.repo.clone().unwrap();
        rec.repo = Some(root.chars().rev().collect());
        let got = resolve(&rec, CARD, &ctx(&cache, &[])).unwrap();
        assert_eq!(got.target, Target::Cache);
        assert_eq!(
            got.how,
            Found::Fetched {
                from: src.to_str().unwrap().to_string()
            }
        );
    }

    /// When the server refuses a sha want (protocol v0, not a ref tip), the
    /// record's branch is fetched instead and the commit is found in it.
    #[test]
    fn falls_back_to_fetching_the_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        let mine = tmp.path().join("mine");
        init_repo(&theirs);
        clone(&theirs, &mine);
        let sha = commit(&theirs, "a", "reviewed work");
        commit(&theirs, "b", "later work"); // `sha` is no longer the tip
        run(&mine, &["config", "protocol.version", "0"]);

        let mut rec = from_elsewhere(record(&theirs, &sha, "elsewhere"));
        rec.remote = Some(theirs.to_str().unwrap().to_string());
        let checkouts = [mine.clone()];
        let got = resolve(&rec, CARD, &ctx(&tmp.path().join("cache"), &checkouts)).unwrap();
        assert_eq!(got.sha, sha);
        assert!(matches!(got.how, Found::Fetched { .. }), "{got:?}");
    }

    /// An amended commit (new hash, same trailer) is found by its trailer once
    /// the recorded hash is gone for good.
    #[test]
    fn finds_an_amended_commit_by_its_trailer() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let old = commit(&repo, "a", &format!("work\n\nHeadway: {CARD}"));
        run(
            &repo,
            &[
                "commit",
                "-q",
                "--amend",
                "-m",
                &format!("work v2\n\nHeadway: {CARD}"),
            ],
        );
        let new = run(&repo, &["rev-parse", "HEAD"]);
        run(&repo, &["reflog", "expire", "--expire=now", "--all"]);
        run(&repo, &["gc", "-q", "--prune=now"]);
        // A decoy whose word-id merely starts with the card's must not match.
        commit(&repo, "b", &format!("decoy\n\nHeadway: {CARD}y"));

        let rec = record(&repo, &new, "here");
        let rec = ReviewFields {
            commit: Some(old),
            ..rec
        };
        let got = resolve(&rec, CARD, &ctx(tmp.path(), &[])).unwrap();
        assert_eq!(got.how, Found::ByTrailer);
        assert_eq!(got.sha, new);

        // The record-less path finds the same commit.
        let by_trailer = resolve_by_trailer(CARD, &[tmp.path().join("nope"), repo]).unwrap();
        assert_eq!(by_trailer.sha, new);
    }

    /// A checkout of a *different* repo is never used: the commit goes to the
    /// cache for its own identity instead.
    #[test]
    fn identity_mismatch_uses_the_cache_not_the_wrong_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        let unrelated = tmp.path().join("unrelated");
        init_repo(&theirs);
        init_repo(&unrelated);
        let sha = commit(&theirs, "a", "their work");

        let mut rec = from_elsewhere(record(&theirs, &sha, "elsewhere"));
        rec.remote = Some(theirs.to_str().unwrap().to_string());
        let checkouts = [unrelated.clone()];
        let got = resolve(&rec, CARD, &ctx(&tmp.path().join("cache"), &checkouts)).unwrap();
        assert_eq!(got.target, Target::Cache);
        assert!(
            git(
                &unrelated,
                &["cat-file", "-e", &format!("{sha}^{{commit}}")]
            )
            .is_err()
        );
    }

    /// A failed fetch surfaces git's own stderr.
    #[test]
    fn a_failed_fetch_keeps_gits_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        init_repo(&theirs);
        let sha = commit(&theirs, "a", "their work");
        let mut rec = from_elsewhere(record(&theirs, &sha, "elsewhere"));
        rec.remote = Some(tmp.path().join("missing").to_str().unwrap().to_string());
        let err = resolve(&rec, CARD, &ctx(&tmp.path().join("cache"), &[])).unwrap_err();
        assert!(
            err.stderr
                .contains("does not appear to be a git repository"),
            "{err}"
        );
    }

    /// The patch reads back with its header, and a cap cuts it at a line.
    #[test]
    fn commit_patch_reads_header_and_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.join("big"), &body).unwrap();
        run(&repo, &["add", "big"]);
        run(&repo, &["commit", "-q", "-m", "big file\n\nwith a body"]);
        let sha = run(&repo, &["rev-parse", "HEAD"]);

        let full = commit_patch(&repo, &sha, usize::MAX).unwrap();
        assert_eq!(full.sha, sha);
        assert_eq!(full.author, "t <t@t>");
        assert_eq!(full.author_name(), "t");
        assert!(full.time > 0, "author time parsed: {}", full.time);
        assert_eq!(full.message, "big file\n\nwith a body");
        assert!(
            full.patch.starts_with("diff --git a/big b/big\n"),
            "{:?}",
            &full.patch[..40]
        );
        assert!(full.patch.ends_with("+line 199\n"));
        assert!(!full.truncated);

        let cut = commit_patch(&repo, &sha, 300).unwrap();
        assert!(cut.truncated);
        assert!(cut.patch.len() <= 300);
        assert!(cut.patch.ends_with('\n'));
        assert!(full.patch.starts_with(&cut.patch));
    }

    /// A committed file reads back at its commit and, changed, at the one
    /// before; a path the rev lacks is missing (so is anything at a root
    /// commit's parent), and a blob over the cap is reported by size unread.
    #[test]
    fn blob_bytes_reads_each_side_and_respects_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let root = init_repo(&repo);
        let old: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        let new: Vec<u8> = (0..=255).collect();
        std::fs::write(repo.join("shot.png"), &old).unwrap();
        run(&repo, &["add", "shot.png"]);
        run(&repo, &["commit", "-q", "-m", "add shot"]);
        std::fs::write(repo.join("shot.png"), &new).unwrap();
        run(&repo, &["commit", "-q", "-am", "change shot"]);
        let sha = run(&repo, &["rev-parse", "HEAD"]);

        let read = |rev: &str, path: &str, max: u64| blob_bytes(&repo, rev, path, max).unwrap();
        assert_eq!(read(&sha, "shot.png", 1024), Blob::Bytes(new.clone()));
        assert_eq!(read(&format!("{sha}^"), "shot.png", 1024), Blob::Bytes(old));
        assert_eq!(read(&format!("{sha}^^"), "shot.png", 1024), Blob::Missing);
        assert_eq!(read(&sha, "nope.png", 1024), Blob::Missing);
        assert_eq!(read(&format!("{root}^"), "shot.png", 1024), Blob::Missing);
        assert_eq!(read(&sha, "shot.png", 255), Blob::TooLarge(256));
        assert_eq!(read(&sha, "shot.png", 256), Blob::Bytes(new));
    }

    /// Every [`Found`] × [`Target`] pair gets its short label: a local find
    /// says which kind of repo, a fetch or trailer match says how, whatever
    /// the repo.
    #[test]
    fn source_label_names_each_find() {
        let fetched = || Found::Fetched {
            from: "jex0:repos/notedeck".to_string(),
        };
        let cases = [
            (Found::Local, Target::Recorded, "local checkout"),
            (Found::Local, Target::Checkout, "local checkout"),
            (Found::Local, Target::Cache, "headway cache"),
            (
                fetched(),
                Target::Recorded,
                "fetched from jex0:repos/notedeck",
            ),
            (
                fetched(),
                Target::Checkout,
                "fetched from jex0:repos/notedeck",
            ),
            (fetched(), Target::Cache, "fetched from jex0:repos/notedeck"),
            (
                Found::ByTrailer,
                Target::Recorded,
                "found by Headway trailer",
            ),
            (
                Found::ByTrailer,
                Target::Checkout,
                "found by Headway trailer",
            ),
            (Found::ByTrailer, Target::Cache, "found by Headway trailer"),
        ];
        for (how, target, label) in cases {
            let resolved = Resolved {
                repo_dir: PathBuf::from("/x/repo"),
                sha: "a".repeat(40),
                target,
                how,
            };
            assert_eq!(resolved.source_label(), label, "{resolved:?}");
        }
    }
}
