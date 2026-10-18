//! Reading a commit's explain notes: the git notes `explain` writes beside a
//! commit, `refs/notes/explain` for its prose page and `refs/notes/explain-code`
//! for its code walk, one note per commit on each.
//!
//! Headway only reads them. [`read_notes`] asks the repo the commit is in;
//! [`fetch_notes`] falls back to the host the commit came from, the same
//! [`fetch_source`](super::fetch_source) the resolver fetched the commit from.
//!
//! The fetch keeps the resolver's rule for a user's checkout: nothing but
//! FETCH_HEAD is written there, never a ref, and never a forced
//! `+refs/notes/*:refs/notes/*` that would clobber the user's own unpushed
//! notes. So a fetched note is read straight out of the fetched notes commit's
//! tree rather than through `git notes`. Only the headway-owned bare cache
//! keeps what it fetched, under `refs/headway/notes/*`.

use std::path::Path;
use std::time::Duration;

use super::resolve::{git_fetch, git_remote, is_hex_sha};
use super::{GitError, Resolved, Target, git};

/// The notes ref holding a commit's explainer page.
pub const EXPLAIN_REF: &str = "refs/notes/explain";
/// The notes ref holding a commit's code walk.
pub const CODE_REF: &str = "refs/notes/explain-code";

/// Where the bare cache keeps the notes commits it fetched, by the notes ref's
/// last component: `refs/headway/notes/explain`, `.../explain-code`.
const CACHE_NOTES_PREFIX: &str = "refs/headway/notes/";

/// Which of a commit's two explain notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// The prose page, on [`EXPLAIN_REF`].
    Explain,
    /// The code walk, on [`CODE_REF`].
    Code,
}

impl NoteKind {
    /// Both kinds, page first.
    pub const ALL: [NoteKind; 2] = [NoteKind::Explain, NoteKind::Code];

    /// The kind a user names: `explain` or `code`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "explain" => Some(Self::Explain),
            "code" => Some(Self::Code),
            _ => None,
        }
    }

    /// The name [`parse`](Self::parse) takes back.
    pub fn name(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Code => "code",
        }
    }

    /// The notes ref this kind lives on.
    pub fn notes_ref(self) -> &'static str {
        match self {
            Self::Explain => EXPLAIN_REF,
            Self::Code => CODE_REF,
        }
    }

    /// Where the bare cache pins this kind's fetched notes commit.
    fn cache_ref(self) -> String {
        let tail = self.notes_ref().trim_start_matches("refs/notes/");
        format!("{CACHE_NOTES_PREFIX}{tail}")
    }
}

/// A commit's two explain notes, each `None` when the commit has none there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitNotes {
    /// The note on [`EXPLAIN_REF`]: the explainer page.
    pub explain: Option<String>,
    /// The note on [`CODE_REF`]: the code walk.
    pub code: Option<String>,
}

impl CommitNotes {
    /// The note of `kind`, if there is one.
    pub fn get(&self, kind: NoteKind) -> Option<&str> {
        self.slot(kind).as_deref()
    }

    /// Whether both notes are here, so there is nothing left to fetch.
    pub fn is_complete(&self) -> bool {
        self.explain.is_some() && self.code.is_some()
    }

    /// Whether neither note is here.
    pub fn is_empty(&self) -> bool {
        self.explain.is_none() && self.code.is_none()
    }

    /// The field holding `kind`'s note.
    fn slot(&self, kind: NoteKind) -> &Option<String> {
        match kind {
            NoteKind::Explain => &self.explain,
            NoteKind::Code => &self.code,
        }
    }

    /// The field holding `kind`'s note, to fill.
    fn slot_mut(&mut self, kind: NoteKind) -> &mut Option<String> {
        match kind {
            NoteKind::Explain => &mut self.explain,
            NoteKind::Code => &mut self.code,
        }
    }
}

/// Commit `sha`'s explain notes in the repo at `dir`, as `git notes
/// --ref=<ref> show <sha>` gives them. A commit without a note there, or a repo
/// without the notes ref at all, reads as `None`.
pub fn read_notes(dir: &Path, sha: &str) -> CommitNotes {
    let mut notes = CommitNotes::default();
    for kind in NoteKind::ALL {
        *notes.slot_mut(kind) = git(dir, &["notes", "--ref", kind.notes_ref(), "show", sha]).ok();
    }
    notes
}

/// Commit `resolved.sha`'s explain notes: the ones in `resolved.repo_dir`
/// first, then any still missing from `src`, the host the commit came from
/// (see [`fetch_source`](super::fetch_source); `None` reads locally only).
///
/// The lookup is by the resolved sha, which after a rebase is the commit the
/// `Headway:` trailer matched: that is where `notes.rewriteRef` moved the note.
///
/// The fetch asks `src` which notes commits its two refs point at
/// (`ls-remote`), fetches those by sha into FETCH_HEAD only, and reads the note
/// out of each one's tree, so a user's checkout gains no ref. In the bare cache
/// the fetched notes commits are pinned under `refs/headway/notes/*`, and read
/// from there on the next call before going back to `src`.
///
/// A failed fetch is an error only when nothing was found locally either;
/// otherwise what was found is returned.
pub fn fetch_notes(
    resolved: &Resolved,
    src: Option<&str>,
    timeout: Duration,
) -> Result<CommitNotes, GitError> {
    let dir = resolved.repo_dir.as_path();
    let sha = resolved.sha.as_str();
    let cache = resolved.target == Target::Cache;
    let mut notes = read_notes(dir, sha);
    if cache {
        fill(&mut notes, |kind| note_in_tree(dir, &kind.cache_ref(), sha));
    }
    let Some(src) = src.filter(|_| !notes.is_complete()) else {
        return Ok(notes);
    };
    match remote_notes(dir, src, timeout) {
        Ok(tips) => {
            fill(&mut notes, |kind| {
                let tip = tips.get(kind)?;
                if cache {
                    let _ = git(dir, &["update-ref", &kind.cache_ref(), tip]);
                }
                note_in_tree(dir, tip, sha)
            });
            Ok(notes)
        }
        Err(e) if notes.is_empty() => Err(e),
        Err(_) => Ok(notes),
    }
}

/// Fill each of `notes`' empty slots with what `find` gives for its kind.
fn fill(notes: &mut CommitNotes, mut find: impl FnMut(NoteKind) -> Option<String>) {
    for kind in NoteKind::ALL {
        let slot = notes.slot_mut(kind);
        if slot.is_none() {
            *slot = find(kind);
        }
    }
}

/// The notes commits `src`'s two explain notes refs point at, fetched into
/// `dir` by sha (FETCH_HEAD only). A ref `src` doesn't have is `None`; one
/// already in `dir` isn't fetched again.
fn remote_notes(dir: &Path, src: &str, timeout: Duration) -> Result<CommitNotes, GitError> {
    let listing = git_remote(
        dir,
        &["ls-remote", "--end-of-options", src, EXPLAIN_REF, CODE_REF],
        timeout,
    )?;
    let tips = notes_tips(&listing);
    let missing: Vec<&str> = NoteKind::ALL
        .iter()
        .filter_map(|kind| tips.get(*kind))
        .filter(|tip| git(dir, &["cat-file", "-e", &format!("{tip}^{{commit}}")]).is_err())
        .collect();
    if !missing.is_empty() {
        git_fetch(dir, src, &missing, timeout)?;
    }
    Ok(tips)
}

/// The notes commit each explain ref points at, out of `git ls-remote` output
/// (`<sha>\t<ref>` lines). `ls-remote` matches its patterns on a ref's tail, so
/// only the exact ref names count; a value that isn't a sha is dropped.
fn notes_tips(listing: &str) -> CommitNotes {
    let mut tips = CommitNotes::default();
    for line in listing.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        let Some(kind) = NoteKind::ALL.into_iter().find(|k| k.notes_ref() == name) else {
            continue;
        };
        if is_hex_sha(sha) {
            *tips.slot_mut(kind) = Some(sha.to_string());
        }
    }
    tips
}

/// The note for commit `sha` in notes commit `notes_rev`, read from its tree
/// without any notes ref. Notes trees fan out by leading hex pairs
/// (`ab/cdef…`, `ab/cd/ef…`) depending on how many notes they hold, so the
/// note is the blob whose path, slashes removed, is `sha`.
fn note_in_tree(dir: &Path, notes_rev: &str, sha: &str) -> Option<String> {
    let listing = git(dir, &["ls-tree", "-r", notes_rev]).ok()?;
    let oid = note_blob(&listing, sha)?;
    git(dir, &["cat-file", "blob", oid]).ok()
}

/// The blob in `git ls-tree -r` output (`<mode> blob <oid>\t<path>` lines)
/// whose path is `sha` once its fanout slashes are taken out.
fn note_blob<'a>(listing: &'a str, sha: &str) -> Option<&'a str> {
    listing.lines().find_map(|line| {
        let (meta, path) = line.split_once('\t')?;
        let mut meta = meta.split_whitespace();
        let (_mode, kind, oid) = (meta.next()?, meta.next()?, meta.next()?);
        let flat = path.chars().filter(|c| *c != '/').eq(sha.chars());
        (kind == "blob" && flat).then_some(oid)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReviewFields;
    use crate::git::{Found, ResolveCtx, resolve};
    use std::path::PathBuf;

    /// Run git in `dir` with a throwaway identity (notes need one), panicking
    /// on failure.
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

    /// A fresh repo at `dir` with one root commit naming `dir` (so two
    /// fixtures never share an identity) and a commit on top; returns the top
    /// commit's sha.
    fn repo_with_commit(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        run(dir, &["init", "-q", "-b", "trunk"]);
        let root = format!("root of {}", dir.display());
        run(dir, &["commit", "-q", "--allow-empty", "-m", &root]);
        run(dir, &["commit", "-q", "--allow-empty", "-m", "work"]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// Add `text` as `sha`'s note on `notes_ref` in `dir`.
    fn add_note(dir: &Path, notes_ref: &str, sha: &str, text: &str) {
        run(dir, &["notes", "--ref", notes_ref, "add", "-m", text, sha]);
    }

    /// A record of `sha` made in `theirs` on another host, reachable only by
    /// its `remote`.
    fn record(theirs: &Path, sha: &str) -> ReviewFields {
        ReviewFields {
            commit: Some(sha.to_string()),
            branch: Some("trunk".to_string()),
            host: Some("elsewhere".to_string()),
            path: Some("/nonexistent/elsewhere/repo".to_string()),
            remote: Some(theirs.to_str().unwrap().to_string()),
            repo: Some(crate::git::repo_identity(theirs, sha).unwrap()),
            ..Default::default()
        }
    }

    /// A commit [`resolve_notes`] found, with its notes.
    struct Lookup {
        resolved: Resolved,
        notes: CommitNotes,
    }

    /// Resolve `rec` and read its notes the way `headway diff --note` does.
    fn resolve_notes(rec: &ReviewFields, ctx: &ResolveCtx) -> Lookup {
        let resolved = resolve(rec, "headway:b/a-b-c", ctx).unwrap();
        let src = crate::git::fetch_source(rec, &resolved.repo_dir, ctx);
        let notes = fetch_notes(&resolved, src.as_deref(), ctx.timeout).unwrap();
        Lookup { resolved, notes }
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

    /// Both notes on a local commit read back; a commit without notes, in a
    /// repo with no notes refs at all, reads as none.
    #[test]
    fn read_notes_locally() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sha = repo_with_commit(&repo);
        assert_eq!(read_notes(&repo, &sha), CommitNotes::default());

        add_note(&repo, EXPLAIN_REF, &sha, "# the page");
        add_note(&repo, CODE_REF, &sha, "the walk");
        let notes = read_notes(&repo, &sha);
        assert_eq!(notes.get(NoteKind::Explain), Some("# the page"));
        assert_eq!(notes.get(NoteKind::Code), Some("the walk"));

        let parent = run(&repo, &["rev-parse", "HEAD^"]);
        assert!(read_notes(&repo, &parent).is_empty());
    }

    /// A commit and its note in "theirs", resolved into the "mine" checkout,
    /// has its note read from theirs without a single ref appearing in mine.
    /// The code ref theirs doesn't have reads as none, not an error.
    #[test]
    fn fetched_note_creates_no_refs_in_a_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        let mine = tmp.path().join("mine");
        repo_with_commit(&theirs);
        run(
            tmp.path(),
            &[
                "clone",
                "-q",
                theirs.to_str().unwrap(),
                mine.to_str().unwrap(),
            ],
        );
        run(
            &theirs,
            &["commit", "-q", "--allow-empty", "-m", "their work"],
        );
        let sha = run(&theirs, &["rev-parse", "HEAD"]);
        add_note(&theirs, EXPLAIN_REF, &sha, "their page");
        let refs_before = run(&mine, &["for-each-ref"]);

        let checkouts = [mine.clone()];
        let cache = tmp.path().join("cache");
        let Lookup { resolved, notes } =
            resolve_notes(&record(&theirs, &sha), &ctx(&cache, &checkouts));
        assert_eq!(resolved.repo_dir, mine);
        assert_eq!(notes.get(NoteKind::Explain), Some("their page"));
        assert_eq!(notes.get(NoteKind::Code), None);
        assert_eq!(
            run(&mine, &["for-each-ref"]),
            refs_before,
            "no refs created"
        );
        assert!(!cache.exists(), "cache untouched");
    }

    /// A note already in the checkout is used as is, without asking the
    /// source: a source that doesn't exist would fail the fetch.
    #[test]
    fn complete_local_notes_skip_the_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sha = repo_with_commit(&repo);
        add_note(&repo, EXPLAIN_REF, &sha, "page");
        add_note(&repo, CODE_REF, &sha, "walk");
        let resolved = Resolved {
            repo_dir: repo.clone(),
            sha: sha.clone(),
            target: Target::Checkout,
            how: Found::Local,
        };
        let missing = tmp.path().join("missing");
        let notes = fetch_notes(&resolved, missing.to_str(), Duration::from_secs(30)).unwrap();
        assert!(notes.is_complete());

        // With nothing local, the same bad source is an error.
        run(&repo, &["update-ref", "-d", EXPLAIN_REF]);
        run(&repo, &["update-ref", "-d", CODE_REF]);
        assert!(fetch_notes(&resolved, missing.to_str(), Duration::from_secs(30)).is_err());
    }

    /// With no checkout the commit and its notes land in the bare cache, the
    /// notes commits pinned under `refs/headway/notes/*`; the next read finds
    /// them there even once the source is gone.
    #[test]
    fn the_cache_pins_fetched_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let theirs = tmp.path().join("theirs");
        let sha = repo_with_commit(&theirs);
        add_note(&theirs, EXPLAIN_REF, &sha, "page");
        add_note(&theirs, CODE_REF, &sha, "walk");
        let cache = tmp.path().join("cache");
        let rec = record(&theirs, &sha);

        let Lookup { resolved, notes } = resolve_notes(&rec, &ctx(&cache, &[]));
        assert_eq!(resolved.target, Target::Cache);
        assert!(notes.is_complete(), "{notes:?}");
        let pinned = run(
            &resolved.repo_dir,
            &["rev-parse", "refs/headway/notes/explain"],
        );
        assert_eq!(pinned, run(&theirs, &["rev-parse", EXPLAIN_REF]));
        assert!(
            run(&resolved.repo_dir, &["for-each-ref", "refs/notes/"]).is_empty(),
            "the cache writes only its own namespace"
        );

        std::fs::remove_dir_all(&theirs).unwrap();
        let again = fetch_notes(&resolved, rec.remote.as_deref(), Duration::from_secs(30));
        assert_eq!(again.unwrap(), notes);
    }

    /// A note is found under every fanout depth a notes tree uses; another
    /// commit's note, or a tree entry, doesn't match.
    #[test]
    fn note_blob_reads_through_fanout() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let flat = format!("100644 blob aaa\t{sha}");
        let one = format!("100644 blob bbb\t01/{}", &sha[2..]);
        let two = format!("100644 blob ccc\t01/23/{}", &sha[4..]);
        assert_eq!(note_blob(&flat, sha), Some("aaa"));
        assert_eq!(note_blob(&one, sha), Some("bbb"));
        assert_eq!(note_blob(&two, sha), Some("ccc"));
        let other = format!("100644 blob ddd\tff/{}\n040000 tree eee\t{sha}", &sha[2..]);
        assert_eq!(note_blob(&other, sha), None);
    }

    /// `ls-remote` output yields each exact ref's tip; a tail match on a
    /// longer ref name, or a value that isn't a sha, is dropped.
    #[test]
    fn notes_tips_take_exact_refs_only() {
        let listing = "aaaa1111\trefs/notes/explain\n\
                       bbbb2222\trefs/foo/refs/notes/explain-code\n\
                       zzzz\trefs/notes/explain-code\n";
        let tips = notes_tips(listing);
        assert_eq!(tips.get(NoteKind::Explain), Some("aaaa1111"));
        assert_eq!(tips.get(NoteKind::Code), None);
    }

    /// The kinds a user names round-trip; anything else is refused.
    #[test]
    fn note_kinds_parse_by_name() {
        for kind in NoteKind::ALL {
            assert_eq!(NoteKind::parse(kind.name()), Some(kind));
        }
        assert_eq!(NoteKind::parse("page"), None);
        assert_eq!(
            NoteKind::Code.cache_ref(),
            "refs/headway/notes/explain-code"
        );
    }
}
