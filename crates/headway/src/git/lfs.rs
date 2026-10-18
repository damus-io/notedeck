//! Git LFS pointers, so a review shows an LFS-tracked image rather than the
//! three-line text file git stores in its place.
//!
//! An LFS file's blob is a pointer: `version https://git-lfs.github.com/spec/v1`,
//! then `oid sha256:<hex>` and `size <bytes>`. Git diffs two of them as text and
//! `cat-file` returns one verbatim, so without this a re-recorded snapshot png
//! reads back as a pointer diff with no image to show. [`pointer_diffs_as_binary`]
//! turns such a diff back into the `Binary files … differ` git prints for a
//! plain png, and [`object_bytes`] reads the content a pointer names.
//!
//! Both work from the bytes alone, not from `.gitattributes`: which attributes
//! git would consult differs between a checkout and the bare review cache, and
//! a commit from before a path moved to LFS has a real blob there, which must
//! keep reading as itself.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{GitError, git};

/// The first line of every LFS pointer.
const VERSION_LINE: &[u8] = b"version https://git-lfs.github.com/spec/v1";

/// Largest pointer file the spec allows; any bigger blob is real content.
pub(super) const MAX_POINTER_BYTES: u64 = 1024;

/// An LFS pointer's fields, borrowed from the blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Pointer<'a> {
    /// The content's sha256, lowercase hex.
    pub oid: &'a str,
    /// The content's size in bytes.
    pub size: u64,
}

impl<'a> Pointer<'a> {
    /// `blob` read as an LFS pointer, or `None` when it's ordinary content.
    pub fn parse(blob: &'a [u8]) -> Option<Self> {
        if blob.len() as u64 > MAX_POINTER_BYTES || !blob.starts_with(VERSION_LINE) {
            return None;
        }
        let text = std::str::from_utf8(blob).ok()?;
        let mut oid = None;
        let mut size = None;
        for line in text.lines() {
            if let Some(hex) = line.strip_prefix("oid sha256:") {
                oid = Some(hex);
            } else if let Some(n) = line.strip_prefix("size ") {
                size = n.parse().ok();
            }
        }
        let oid = oid.filter(|o| o.len() == 64 && o.bytes().all(|b| b.is_ascii_hexdigit()))?;
        Some(Self { oid, size: size? })
    }
}

/// The content `pointer` names, for `path` in `repo_dir`.
///
/// Read straight from the repo's LFS store (`<common git dir>/lfs/objects`,
/// shared by every worktree) when it's there, so a fetched object needs no
/// `git-lfs` binary. Otherwise `git lfs smudge` fetches it from the remote's
/// LFS endpoint; that fails, as an error, when git-lfs isn't installed or
/// the repo has no endpoint (the bare review cache fetches by url, with no
/// remote to derive one from).
pub(super) fn object_bytes(
    repo_dir: &Path,
    pointer: Pointer<'_>,
    blob: &[u8],
    path: &str,
) -> Result<Vec<u8>, GitError> {
    if let Some(bytes) = stored_object(repo_dir, pointer) {
        return Ok(bytes);
    }
    smudge(repo_dir, blob, path)
}

/// `pointer`'s object from the local LFS store, when it's there whole.
fn stored_object(repo_dir: &Path, pointer: Pointer<'_>) -> Option<Vec<u8>> {
    let common = git(
        repo_dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    let oid = pointer.oid;
    let file: PathBuf = [&common, "lfs", "objects", &oid[..2], &oid[2..4], oid]
        .iter()
        .collect();
    let bytes = std::fs::read(file).ok()?;
    // A partial download is left under `tmp/`, never here, but a size check
    // costs nothing.
    (bytes.len() as u64 == pointer.size).then_some(bytes)
}

/// `blob` (a pointer) run through `git lfs smudge`, which downloads the
/// object when it isn't stored. Never prompts for credentials.
fn smudge(repo_dir: &Path, blob: &[u8], path: &str) -> Result<Vec<u8>, GitError> {
    let command = format!("lfs smudge -- {path}");
    let fail = |stderr: String| GitError {
        command: command.clone(),
        stderr,
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["lfs", "smudge", "--", path])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    // A pointer is under 1 KiB, so this can't fill the pipe and deadlock
    // against an unread stdout.
    let written = child.stdin.take().map(|mut stdin| stdin.write_all(blob));
    let out = child.wait_with_output().map_err(|e| fail(e.to_string()))?;
    if !out.status.success() {
        return Err(fail(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    if let Some(Err(e)) = written {
        return Err(fail(e.to_string()));
    }
    Ok(out.stdout)
}

/// `patch` (`git show` output) with every file whose hunks only change LFS
/// pointer lines rewritten the way git prints a binary file: its header, then
/// `Binary files <old> and <new> differ`, no hunks. Borrowed as-is when no
/// file is a pointer diff.
///
/// The diff parser then marks the file binary and the review pane reads its
/// sides as images, as it does for a png that isn't in LFS.
pub(super) fn pointer_diffs_as_binary(patch: &[u8]) -> Cow<'_, [u8]> {
    let files = FileSections { rest: patch };
    if !files.clone().any(is_pointer_diff) {
        return Cow::Borrowed(patch);
    }
    let mut out = Vec::with_capacity(patch.len());
    for file in files {
        if is_pointer_diff(file) {
            write_as_binary(file, &mut out);
        } else {
            out.extend_from_slice(file);
        }
    }
    Cow::Owned(out)
}

/// A patch split into its per-file sections, each starting at its
/// `diff --git` line (anything before the first one is a section too).
#[derive(Clone)]
struct FileSections<'a> {
    /// The patch not yet handed out.
    rest: &'a [u8],
}

impl<'a> Iterator for FileSections<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            return None;
        }
        // The next section starts at a later line beginning `diff --git `.
        let end = self
            .rest
            .windows(12)
            .skip(1)
            .position(|w| w == b"\ndiff --git ")
            .map_or(self.rest.len(), |i| i + 2);
        let (file, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(file)
    }
}

/// Whether `file`'s section has hunks and every line in them is a pointer
/// line, with a `version` line among them: a diff of two pointers, or of
/// an added or deleted one.
fn is_pointer_diff(file: &[u8]) -> bool {
    let mut lines = file
        .split(|b| *b == b'\n')
        .skip_while(|l| !l.starts_with(b"@@ "));
    if lines.next().is_none() {
        return false;
    }
    let mut saw_version = false;
    for line in lines {
        if line.is_empty() || line.starts_with(b"@@ ") || line.starts_with(b"\\ ") {
            continue;
        }
        let Some((b' ' | b'+' | b'-', body)) = line.split_first() else {
            return false;
        };
        saw_version |= body == VERSION_LINE;
        let pointer_line = body == VERSION_LINE
            || body.starts_with(b"oid sha256:")
            || body.starts_with(b"size ")
            || body.starts_with(b"ext-");
        if !pointer_line {
            return false;
        }
    }
    saw_version
}

/// Append `file`'s header to `out` with its `---`/`+++` markers and hunks
/// replaced by a `Binary files <old> and <new> differ` line.
fn write_as_binary(file: &[u8], out: &mut Vec<u8>) {
    let mut old: &[u8] = b"a";
    let mut new: &[u8] = b"b";
    for line in file.split_inclusive(|b| *b == b'\n') {
        let bare = line.strip_suffix(b"\n").unwrap_or(line);
        if let Some(p) = bare.strip_prefix(b"--- ") {
            old = p;
        } else if let Some(p) = bare.strip_prefix(b"+++ ") {
            new = p;
        } else if bare.starts_with(b"@@ ") {
            break;
        } else {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(b"Binary files ");
    out.extend_from_slice(old);
    out.extend_from_slice(b" and ");
    out.extend_from_slice(new);
    out.extend_from_slice(b" differ\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pointer for content of `size` bytes whose oid is `fill` repeated.
    fn pointer(fill: char, size: u64) -> String {
        let oid: String = std::iter::repeat_n(fill, 64).collect();
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
    }

    #[test]
    fn parse_reads_a_pointer_and_nothing_else() {
        let p = pointer('a', 1234);
        let parsed = Pointer::parse(p.as_bytes()).unwrap();
        assert_eq!(parsed.oid, "a".repeat(64));
        assert_eq!(parsed.size, 1234);

        assert_eq!(Pointer::parse(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(
            Pointer::parse(b"version https://git-lfs.github.com/spec/v1\n"),
            None
        );
        let short_oid = "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 3\n";
        assert_eq!(Pointer::parse(short_oid.as_bytes()), None);
    }

    /// A pointer change becomes a binary file; a text file and a real
    /// binary file around it come through unchanged.
    #[test]
    fn pointer_diffs_read_as_binary_files() {
        let text = "diff --git a/notes.txt b/notes.txt\n\
            index 1111111..2222222 100644\n\
            --- a/notes.txt\n\
            +++ b/notes.txt\n\
            @@ -1 +1 @@\n\
            -size 3\n\
            +size 4\n";
        let lfs = format!(
            "diff --git a/shot.png b/shot.png\n\
            index 3333333..4444444 100644\n\
            --- a/shot.png\n\
            +++ b/shot.png\n\
            @@ -1,3 +1,3 @@\n \
            version https://git-lfs.github.com/spec/v1\n\
            -oid sha256:{a}\n\
            -size 10\n\
            +oid sha256:{b}\n\
            +size 20\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
        );
        let added = "diff --git a/new.png b/new.png\n\
            new file mode 100644\n\
            index 0000000..5555555\n\
            --- /dev/null\n\
            +++ b/new.png\n\
            @@ -0,0 +1,3 @@\n"
            .to_string()
            + &pointer('c', 7)
                .lines()
                .map(|l| format!("+{l}\n"))
                .collect::<String>();
        let raw = "diff --git a/raw.png b/raw.png\n\
            index 6666666..7777777 100644\n\
            Binary files a/raw.png and b/raw.png differ\n";
        let patch = format!("{text}{lfs}{added}{raw}");

        let out = pointer_diffs_as_binary(patch.as_bytes());
        let expected = format!(
            "{text}\
            diff --git a/shot.png b/shot.png\n\
            index 3333333..4444444 100644\n\
            Binary files a/shot.png and b/shot.png differ\n\
            diff --git a/new.png b/new.png\n\
            new file mode 100644\n\
            index 0000000..5555555\n\
            Binary files /dev/null and b/new.png differ\n\
            {raw}"
        );
        assert_eq!(String::from_utf8_lossy(&out), expected);
    }

    #[test]
    fn a_patch_without_pointers_is_borrowed() {
        let patch = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(matches!(
            pointer_diffs_as_binary(patch),
            Cow::Borrowed(p) if p == patch
        ));
    }
}
