//! A parsed multi-file git patch: the `diff --git` sections of `git show
//! --patch --find-renames` output, split into files and hunks with running
//! line numbers.
//!
//! Parse once, render many times: [`GitPatch`] owns the patch text and every
//! line refers back into it by byte span, so the model is a handful of small
//! `Vec`s rather than a `String` per line.

use super::{DiffRow, DiffTag};
use std::ops::Range;

/// A whole patch, parsed. The commit header (sha, author, message) is not part
/// of it: callers that have one (headway's `CommitPatch`) carry it alongside.
#[derive(Debug, Clone, Default)]
pub struct GitPatch {
    text: String,
    files: Vec<FilePatch>,
    additions: usize,
    deletions: usize,
}

/// One file's section of the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// Path before the change. Equal to `new_path` unless the file was renamed.
    pub old_path: String,
    /// Path after the change.
    pub new_path: String,
    pub status: FileStatus,
    /// Git printed `Binary files … differ` (or a binary patch) instead of hunks.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    /// Every hunk's lines, in order; each [`Hunk::lines`] is a range into this.
    pub lines: Vec<PatchLine>,
    /// Number of `+` lines.
    pub additions: usize,
    /// Number of `-` lines.
    pub deletions: usize,
}

/// What happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    /// Content and/or mode changed in place.
    Modified,
    /// Moved, with git's similarity percentage (100 = moved unchanged).
    Renamed {
        similarity: u8,
    },
    /// Copied from `old_path`, with git's similarity percentage.
    Copied {
        similarity: u8,
    },
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based first old-file line (0 for a hunk that only adds to an empty file).
    pub old_start: u32,
    pub old_len: u32,
    /// 1-based first new-file line (0 for a hunk that deletes everything).
    pub new_start: u32,
    pub new_len: u32,
    /// The whole `@@ … @@ context` line; read it with [`GitPatch::text`].
    pub header: Span,
    /// This hunk's lines, as indices into [`FilePatch::lines`].
    pub lines: Range<usize>,
}

/// One line inside a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: LineKind,
    /// 1-based old-file line number; `None` for insertions and markers.
    pub old_no: Option<u32>,
    /// 1-based new-file line number; `None` for deletions and markers.
    pub new_no: Option<u32>,
    /// The content, without its `+`/`-`/` ` prefix or newline.
    pub text: Span,
}

/// What a [`PatchLine`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Delete,
    Insert,
    /// `\ No newline at end of file`, about the line before it. Its text is the
    /// whole marker line, backslash included.
    NoNewline,
}

impl LineKind {
    /// The renderer's tag for this line, `None` for a marker.
    pub fn diff_tag(self) -> Option<DiffTag> {
        match self {
            LineKind::Context => Some(DiffTag::Equal),
            LineKind::Delete => Some(DiffTag::Delete),
            LineKind::Insert => Some(DiffTag::Insert),
            LineKind::NoNewline => None,
        }
    }
}

/// A byte range into [`GitPatch`]'s text. `u32`s keep [`PatchLine`] small;
/// a patch over 4 GiB is not something we render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    start: u32,
    end: u32,
}

impl Span {
    fn new(range: Range<usize>) -> Self {
        Self {
            start: range.start as u32,
            end: range.end as u32,
        }
    }

    fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl GitPatch {
    /// Parse `git show --patch` (or `git diff`) output. Anything before the
    /// first `diff --git` line is ignored, and so is anything unrecognised, so
    /// a patch cut short mid-hunk (headway caps them) parses up to the cut.
    #[profiling::function]
    pub fn parse(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut parser = Parser::default();
        let mut offset = 0;
        for raw in text.split_inclusive('\n') {
            let start = offset;
            offset += raw.len();
            let line = raw.strip_suffix('\n').unwrap_or(raw);
            parser.line(line, start);
        }
        let files = parser.finish();
        let additions = files.iter().map(|f| f.additions).sum();
        let deletions = files.iter().map(|f| f.deletions).sum();
        Self {
            text,
            files,
            additions,
            deletions,
        }
    }

    /// The files, in patch order.
    pub fn files(&self) -> &[FilePatch] {
        &self.files
    }

    /// Total `+` lines across every file.
    pub fn additions(&self) -> usize {
        self.additions
    }

    /// Total `-` lines across every file.
    pub fn deletions(&self) -> usize {
        self.deletions
    }

    /// The index of the file named `path` (by the path it shows, see
    /// [`FilePatch::path`]), if the patch has one.
    pub fn file_named(&self, path: &str) -> Option<usize> {
        self.files.iter().position(|f| f.path() == path)
    }

    /// The text a [`Span`] refers to.
    pub fn text(&self, span: Span) -> &str {
        &self.text[span.range()]
    }

    /// `line` as a row for [`super::DiffLines`]; `None` for a
    /// [`LineKind::NoNewline`] marker.
    pub fn diff_row(&self, line: &PatchLine) -> Option<DiffRow<'_>> {
        Some(DiffRow {
            tag: line.kind.diff_tag()?,
            old_no: line.old_no.map(|n| n as usize),
            new_no: line.new_no.map(|n| n as usize),
            text: self.text(line.text),
        })
    }
}

impl FilePatch {
    /// The path to show for this file: the new path, or the old one when the
    /// file was deleted.
    pub fn path(&self) -> &str {
        if self.status == FileStatus::Deleted {
            &self.old_path
        } else {
            &self.new_path
        }
    }

    /// The lines of `hunk`, which must be one of this file's hunks.
    pub fn hunk_lines(&self, hunk: &Hunk) -> &[PatchLine] {
        &self.lines[hunk.lines.clone()]
    }

    /// The line numbers `lines` (indices into [`Self::lines`]) cover: the new
    /// file's, when any of them is in it (an added or context line), else the
    /// old file's, for a run of deletions only. `None` when they cover no
    /// numbered line (an empty range, or only a no-newline marker).
    pub fn line_span(&self, lines: Range<usize>) -> Option<LineSpan> {
        let picked = self.lines.get(lines)?;
        let span = |side, no: fn(&PatchLine) -> Option<u32>| {
            let mut numbers = picked.iter().filter_map(no);
            let first = numbers.next()?;
            let (start, end) = numbers.fold((first, first), |(a, b), n| (a.min(n), b.max(n)));
            Some(LineSpan { side, start, end })
        };
        span(DiffSide::New, |l| l.new_no).or_else(|| span(DiffSide::Old, |l| l.old_no))
    }

    /// The inverse of [`Self::line_span`]: the run of line indices whose
    /// numbers on `span`'s side fall in it, first to last. The old side takes
    /// only deleted lines, as [`Self::line_span`] only names it for those.
    /// `None` when no line matches (the span points outside this diff).
    pub fn lines_in(&self, span: LineSpan) -> Option<Range<usize>> {
        let hit = |l: &PatchLine| {
            let no = match span.side {
                DiffSide::New => l.new_no,
                DiffSide::Old if l.kind == LineKind::Delete => l.old_no,
                DiffSide::Old => None,
            };
            no.is_some_and(|n| (span.start..=span.end).contains(&n))
        };
        let first = self.lines.iter().position(hit)?;
        let last = self.lines.iter().rposition(hit)?;
        Some(first..last + 1)
    }
}

/// Which file of a diff a line number counts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffSide {
    /// The file after the change: added and context lines.
    New,
    /// The file before it: deleted lines.
    Old,
}

/// A run of line numbers (1-based, inclusive) in one side of a file's diff:
/// what a comment on some of its lines points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineSpan {
    pub side: DiffSide,
    pub start: u32,
    pub end: u32,
}

/// Where the parser is inside the current file.
#[derive(Debug, Default)]
struct Parser {
    files: Vec<FilePatch>,
    /// Inside a hunk: the old/new lines it still expects, and the next numbers.
    hunk: Option<HunkCursor>,
}

#[derive(Debug)]
struct HunkCursor {
    old_left: u32,
    new_left: u32,
    old_no: u32,
    new_no: u32,
}

impl Parser {
    fn line(&mut self, line: &str, start: usize) {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.hunk = None;
            let (old_path, new_path) = git_header_paths(rest).unwrap_or_default();
            self.files.push(FilePatch {
                old_path,
                new_path,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
                lines: Vec::new(),
                additions: 0,
                deletions: 0,
            });
            return;
        }
        let Some(file) = self.files.last_mut() else {
            return;
        };

        // `\` follows a line inside the hunk, including its last one.
        if line.starts_with('\\') {
            if !file.hunks.is_empty() {
                push_line(file, LineKind::NoNewline, None, None, start, line);
            }
            return;
        }

        if let Some(cur) = self.hunk.as_mut().filter(|c| c.old_left + c.new_left > 0) {
            let (kind, content) = match line.as_bytes().first() {
                Some(b'+') => (LineKind::Insert, &line[1..]),
                Some(b'-') => (LineKind::Delete, &line[1..]),
                Some(b' ') => (LineKind::Context, &line[1..]),
                // Some tools strip the space off an empty context line.
                None => (LineKind::Context, line),
                Some(_) => {
                    self.hunk = None;
                    return;
                }
            };
            let prefix = line.len() - content.len();
            let (old_no, new_no) = cur.advance(kind);
            push_line(file, kind, old_no, new_no, start + prefix, content);
            return;
        }

        if line.starts_with("@@ ") {
            let Some(h) = parse_hunk_header(line) else {
                self.hunk = None;
                return;
            };
            let at = file.lines.len();
            self.hunk = Some(HunkCursor {
                old_left: h.old_len,
                new_left: h.new_len,
                old_no: h.old_start,
                new_no: h.new_start,
            });
            file.hunks.push(Hunk {
                header: Span::new(start..start + line.len()),
                lines: at..at,
                ..h
            });
            return;
        }

        // Extended header lines, before the first hunk.
        if !file.hunks.is_empty() {
            return;
        }
        if line.starts_with("new file mode ") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode ") {
            file.status = FileStatus::Deleted;
        } else if let Some(pct) = line.strip_prefix("similarity index ") {
            let similarity = pct.trim_end_matches('%').parse().unwrap_or(0);
            file.status = match file.status {
                FileStatus::Copied { .. } => FileStatus::Copied { similarity },
                _ => FileStatus::Renamed { similarity },
            };
        } else if let Some(p) = line.strip_prefix("rename from ") {
            file.old_path = unquote(p);
            file.status = renamed(file.status);
        } else if let Some(p) = line.strip_prefix("rename to ") {
            file.new_path = unquote(p);
            file.status = renamed(file.status);
        } else if let Some(p) = line.strip_prefix("copy from ") {
            file.old_path = unquote(p);
            file.status = copied(file.status);
        } else if let Some(p) = line.strip_prefix("copy to ") {
            file.new_path = unquote(p);
            file.status = copied(file.status);
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if let Some(p) = line.strip_prefix("--- ") {
            if let Some(p) = marker_path(p, "a/") {
                file.old_path = p;
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if let Some(p) = marker_path(p, "b/") {
                file.new_path = p;
            }
        }
    }

    fn finish(self) -> Vec<FilePatch> {
        self.files
    }
}

impl HunkCursor {
    /// Number a line of `kind` and consume it from the hunk's counts.
    fn advance(&mut self, kind: LineKind) -> (Option<u32>, Option<u32>) {
        let old = matches!(kind, LineKind::Context | LineKind::Delete);
        let new = matches!(kind, LineKind::Context | LineKind::Insert);
        let nums = (old.then_some(self.old_no), new.then_some(self.new_no));
        if old {
            self.old_no += 1;
            self.old_left = self.old_left.saturating_sub(1);
        }
        if new {
            self.new_no += 1;
            self.new_left = self.new_left.saturating_sub(1);
        }
        nums
    }
}

fn push_line(
    file: &mut FilePatch,
    kind: LineKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    at: usize,
    content: &str,
) {
    match kind {
        LineKind::Insert => file.additions += 1,
        LineKind::Delete => file.deletions += 1,
        LineKind::Context | LineKind::NoNewline => {}
    }
    file.lines.push(PatchLine {
        kind,
        old_no,
        new_no,
        text: Span::new(at..at + content.len()),
    });
    if let Some(hunk) = file.hunks.last_mut() {
        hunk.lines.end = file.lines.len();
    }
}

fn renamed(status: FileStatus) -> FileStatus {
    match status {
        FileStatus::Renamed { .. } => status,
        _ => FileStatus::Renamed { similarity: 0 },
    }
}

fn copied(status: FileStatus) -> FileStatus {
    match status {
        FileStatus::Copied { .. } => status,
        FileStatus::Renamed { similarity } => FileStatus::Copied { similarity },
        _ => FileStatus::Copied { similarity: 0 },
    }
}

/// `@@ -a[,b] +c[,d] @@ …` as a [`Hunk`] with empty header and lines.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let (old_start, old_len) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        header: Span::default(),
        lines: 0..0,
    })
}

/// `start[,len]`, where a missing length means 1.
fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// The two paths of `diff --git a/<old> b/<new>`, prefixes stripped.
///
/// Unquoted names can hold spaces, so `a/x b/y b/z` is ambiguous; git resolves
/// it the same way, by preferring the split where both halves name the same
/// path. Renames are ambiguous only here: their `rename from/to` lines, parsed
/// later, overwrite whatever this guessed.
fn git_header_paths(rest: &str) -> Option<(String, String)> {
    if rest.starts_with('"') {
        let (old, tail) = split_quoted(rest)?;
        let tail = tail.strip_prefix(' ')?;
        let new = if tail.starts_with('"') {
            split_quoted(tail)?.0
        } else {
            tail.to_string()
        };
        return Some((strip_prefix(old, "a/"), strip_prefix(new, "b/")));
    }
    if let Some(i) = rest.rfind(" \"b/") {
        let new = split_quoted(&rest[i + 1..])?.0;
        return Some((
            strip_prefix(rest[..i].to_string(), "a/"),
            strip_prefix(new, "b/"),
        ));
    }

    // Same path on both sides: `a/P b/P`, so the halves are equal length.
    let old = rest.strip_prefix("a/")?;
    if rest.len() % 2 == 1 {
        let half = (rest.len() - 1) / 2;
        let (a, b) = (&rest[..half], &rest[half + 1..]);
        if rest.as_bytes()[half] == b' ' && a.get(2..) == b.get(2..) && b.starts_with("b/") {
            return Some((a[2..].to_string(), b[2..].to_string()));
        }
    }
    let (old, new) = old.split_once(" b/")?;
    Some((old.to_string(), new.to_string()))
}

/// The path of a `---`/`+++` line with its `a/`/`b/` prefix stripped, or `None`
/// for `/dev/null`. Git appends a tab to names that contain a space.
fn marker_path(p: &str, prefix: &str) -> Option<String> {
    let p = p.strip_suffix('\t').unwrap_or(p);
    if p == "/dev/null" {
        return None;
    }
    Some(strip_prefix(unquote(p), prefix))
}

fn strip_prefix(mut path: String, prefix: &str) -> String {
    if path.starts_with(prefix) {
        path.drain(..prefix.len());
    }
    path
}

/// A path as git prints it: C-quoted when it holds unusual bytes, else as-is.
fn unquote(s: &str) -> String {
    match split_quoted(s) {
        Some((path, "")) => path,
        _ => s.to_string(),
    }
}

/// Decode a leading C-style quoted string (`"caf\303\251"`), returning it and
/// the text after the closing quote. Octal escapes are raw bytes, so a UTF-8
/// name round-trips.
fn split_quoted(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let path = String::from_utf8_lossy(&out).into_owned();
                return Some((path, &body[i + 1..]));
            }
            b'\\' => {
                let esc = *bytes.get(i + 1)?;
                i += 2;
                out.push(match esc {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'v' => 0x0b,
                    b'r' => b'\r',
                    b'0'..=b'7' => {
                        let digits = bytes.get(i - 1..i + 2)?;
                        i += 2;
                        digits
                            .iter()
                            .fold(0u8, |n, d| n.wrapping_mul(8) + (d - b'0'))
                    }
                    other => other,
                });
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI: &str = include_str!("testdata/multi.patch");
    const TRAPS: &str = include_str!("testdata/renames_and_traps.patch");

    fn file<'a>(patch: &'a GitPatch, path: &str) -> &'a FilePatch {
        patch
            .files()
            .iter()
            .find(|f| f.path() == path)
            .unwrap_or_else(|| panic!("no file {path}"))
    }

    /// `(kind, old_no, new_no, text)` for every line of `f`.
    fn lines<'a>(
        patch: &'a GitPatch,
        f: &FilePatch,
    ) -> Vec<(LineKind, Option<u32>, Option<u32>, &'a str)> {
        f.lines
            .iter()
            .map(|l| (l.kind, l.old_no, l.new_no, patch.text(l.text)))
            .collect()
    }

    #[test]
    fn splits_files_in_order_with_statuses() {
        let patch = GitPatch::parse(MULTI);
        let got: Vec<_> = patch
            .files()
            .iter()
            .map(|f| (f.old_path.as_str(), f.new_path.as_str(), f.status, f.binary))
            .collect();
        assert_eq!(
            got,
            [
                ("added file.rs", "added file.rs", FileStatus::Added, false),
                ("blob.bin", "blob.bin", FileStatus::Modified, true),
                ("gone.txt", "gone.txt", FileStatus::Deleted, false),
                ("keep.txt", "keep.txt", FileStatus::Modified, false),
                ("long.txt", "long.txt", FileStatus::Modified, false),
                ("main.rs", "main.rs", FileStatus::Modified, false),
                (
                    "old name.txt",
                    "new name.txt",
                    FileStatus::Renamed { similarity: 86 },
                    false
                ),
                ("noeol.txt", "noeol.txt", FileStatus::Modified, false),
            ]
        );
        assert_eq!((patch.additions(), patch.deletions()), (8, 5));
    }

    #[test]
    fn added_and_deleted_files_number_one_side() {
        let patch = GitPatch::parse(MULTI);
        let added = file(&patch, "added file.rs");
        assert_eq!(
            lines(&patch, added),
            [
                (LineKind::Insert, None, Some(1), "fresh"),
                (LineKind::Insert, None, Some(2), "file"),
            ]
        );
        let gone = file(&patch, "gone.txt");
        assert_eq!((gone.hunks[0].new_start, gone.hunks[0].new_len), (0, 0));
        assert_eq!(
            lines(&patch, gone),
            [(LineKind::Delete, Some(1), None, "bye")]
        );
    }

    #[test]
    fn binary_and_mode_only_files_have_no_hunks() {
        let patch = GitPatch::parse(MULTI);
        assert!(file(&patch, "blob.bin").hunks.is_empty());
        let keep = file(&patch, "keep.txt");
        assert!(keep.hunks.is_empty() && !keep.binary);
    }

    #[test]
    fn line_numbers_restart_at_each_hunk_header() {
        let patch = GitPatch::parse(MULTI);
        let long = file(&patch, "long.txt");
        assert_eq!(long.hunks.len(), 2);
        assert_eq!(
            patch.text(long.hunks[1].header),
            "@@ -32,7 +32,7 @@ line 31"
        );
        let second = long.hunk_lines(&long.hunks[1]);
        let nums: Vec<_> = second.iter().map(|l| (l.old_no, l.new_no)).collect();
        assert_eq!(
            nums,
            [
                (Some(32), Some(32)),
                (Some(33), Some(33)),
                (Some(34), Some(34)),
                (Some(35), None),
                (None, Some(35)),
                (Some(36), Some(36)),
                (Some(37), Some(37)),
                (Some(38), Some(38)),
            ]
        );
        assert_eq!((long.additions, long.deletions), (2, 2));
    }

    #[test]
    fn unequal_hunk_lengths_number_correctly() {
        let patch = GitPatch::parse(MULTI);
        let main = file(&patch, "main.rs");
        assert_eq!(
            lines(&patch, main),
            [
                (LineKind::Context, Some(1), Some(1), "fn main() {"),
                (LineKind::Delete, Some(2), None, "    old();"),
                (LineKind::Insert, None, Some(2), "    new();"),
                (LineKind::Insert, None, Some(3), "    more();"),
                (LineKind::Context, Some(3), Some(4), "}"),
            ]
        );
    }

    #[test]
    fn no_newline_marker_follows_its_line() {
        let patch = GitPatch::parse(MULTI);
        let noeol = file(&patch, "noeol.txt");
        assert_eq!(
            lines(&patch, noeol),
            [
                (LineKind::Delete, Some(1), None, "no eol"),
                (
                    LineKind::NoNewline,
                    None,
                    None,
                    "\\ No newline at end of file"
                ),
                (LineKind::Insert, None, Some(1), "no eol now"),
                (
                    LineKind::NoNewline,
                    None,
                    None,
                    "\\ No newline at end of file"
                ),
            ]
        );
        assert!(patch.diff_row(&noeol.lines[1]).is_none());
    }

    #[test]
    fn spaced_paths_drop_the_trailing_tab() {
        let patch = GitPatch::parse(MULTI);
        let renamed = file(&patch, "new name.txt");
        assert_eq!(renamed.old_path, "old name.txt");
        assert_eq!(renamed.hunks[0].old_start, 4);
        assert_eq!(renamed.additions, 1);
    }

    #[test]
    fn pure_rename_quoted_path_and_prefix_lookalikes() {
        let patch = GitPatch::parse(TRAPS);
        let files = patch.files();
        assert_eq!(files.len(), 3);

        assert_eq!(
            (
                files[0].old_path.as_str(),
                files[0].new_path.as_str(),
                files[0].status
            ),
            ("a.txt", "b.txt", FileStatus::Renamed { similarity: 100 })
        );
        assert!(files[0].hunks.is_empty());

        assert_eq!(files[1].new_path, "café.txt");
        assert_eq!(files[1].old_path, "café.txt");

        // `--- header` and `+++ plus` are content, not file markers: the
        // hunk's line counts say so.
        let sql = file(&patch, "q.sql");
        assert_eq!(
            lines(&patch, sql),
            [
                (LineKind::Delete, Some(1), None, "-- header"),
                (LineKind::Context, Some(2), Some(1), "select 1;"),
                (LineKind::Insert, None, Some(2), "++ plus"),
            ]
        );
    }

    #[test]
    fn truncated_patch_keeps_what_arrived() {
        let cut = &MULTI[..MULTI.find("+    more();").unwrap()];
        let patch = GitPatch::parse(cut);
        let main = patch.files().last().unwrap();
        assert_eq!(main.path(), "main.rs");
        assert_eq!(main.lines.len(), 3);
        assert_eq!(main.hunks[0].lines, 0..3);
    }

    #[test]
    fn header_paths_split_on_equal_halves() {
        let split = |s| git_header_paths(s).unwrap();
        assert_eq!(split("a/x b/y b/x b/y"), ("x b/y".into(), "x b/y".into()));
        assert_eq!(split("a/old b/new"), ("old".into(), "new".into()));
        assert_eq!(
            split(r#""a/tab\there" "b/tab\there""#),
            ("tab\there".into(), "tab\there".into())
        );
    }

    #[test]
    fn ignores_text_before_the_first_file() {
        let patch = GitPatch::parse("commit abc\n\n    msg\n\n");
        assert!(patch.files().is_empty());
    }
}
