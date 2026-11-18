//! Read, Write and Edit: plain file operations in this process.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::ExecError;

/// Lines a Read answers when it names no limit.
const DEFAULT_READ_LINES: u64 = 2000;

/// Most characters of one line a Read answers; the rest is cut.
const MAX_LINE_CHARS: usize = 2000;

/// The error for an I/O failure on `path`.
fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ExecError + '_ {
    move |source| ExecError::Io {
        path: path.to_owned(),
        source,
    }
}

/// `path`'s text, or why it has none.
fn text_of(path: &Path) -> Result<String, ExecError> {
    if path.is_dir() {
        return Err(ExecError::IsDirectory(path.to_owned()));
    }
    let bytes = std::fs::read(path).map_err(io(path))?;
    String::from_utf8(bytes).map_err(|_| ExecError::NotText(path.to_owned()))
}

/// Read `path`'s lines from the 1-based `offset`, at most `limit` of them,
/// each numbered as `cat -n` numbers it. A read that stops short of the end
/// says where it stopped, so the model knows to read on.
pub(super) fn read(
    path: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<String, ExecError> {
    let text = text_of(path)?;
    let total = text.lines().count();
    if total == 0 {
        return Ok(format!("{} is empty", path.display()));
    }
    let first = offset.unwrap_or(1).max(1) as usize;
    if first > total {
        return Ok(format!(
            "{} has {total} lines, so there is nothing from line {first}",
            path.display()
        ));
    }
    let limit = limit.unwrap_or(DEFAULT_READ_LINES).max(1) as usize;
    let mut out = String::new();
    let mut last = first - 1;
    for (i, line) in text.lines().enumerate().skip(first - 1).take(limit) {
        last = i + 1;
        let _ = write!(out, "{last:>6}\t");
        match line.char_indices().nth(MAX_LINE_CHARS) {
            Some((cut, _)) => {
                out.push_str(&line[..cut]);
                out.push_str(" [line cut]");
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    if last < total {
        let _ = writeln!(
            out,
            "[lines {}–{total} not shown: read on with offset {}]",
            last + 1,
            last + 1
        );
    }
    Ok(out)
}

/// Write `content` to `path`, making its directories as needed.
pub(super) fn write(path: &Path, content: &str) -> Result<String, ExecError> {
    if path.is_dir() {
        return Err(ExecError::IsDirectory(path.to_owned()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    let existed = path.exists();
    std::fs::write(path, content).map_err(io(path))?;
    let verb = if existed { "Updated" } else { "Created" };
    Ok(format!("{verb} {}", path.display()))
}

/// Replace `old` with `new` in `path`: its one occurrence, or every one
/// when `replace_all` is set.
pub(super) fn edit(
    path: &Path,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ExecError> {
    if old.is_empty() {
        return Err(ExecError::EmptyOldString);
    }
    if old == new {
        return Err(ExecError::NoChange);
    }
    let text = text_of(path)?;
    let count = text.matches(old).count();
    let edited = match count {
        0 => return Err(ExecError::NoMatch(path.to_owned())),
        1 => text.replacen(old, new, 1),
        _ if replace_all => text.replace(old, new),
        count => {
            return Err(ExecError::Ambiguous {
                path: PathBuf::from(path),
                count,
            })
        }
    };
    std::fs::write(path, edited).map_err(io(path))?;
    let times = if count == 1 {
        String::new()
    } else {
        format!(" ({count} replacements)")
    };
    Ok(format!("Edited {}{times}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file in a temp dir that lives as long as this does.
    struct TempFile {
        dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn file_with(text: &str) -> TempFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, text).unwrap();
        TempFile { dir, path }
    }

    /// Lines come numbered as `cat -n` numbers them, from the offset, and a
    /// read that stops early says where to read on.
    #[test]
    fn read_numbers_lines_and_says_where_it_stopped() {
        let TempFile { dir: _dir, path } = file_with("a\nb\nc\nd\n");
        assert_eq!(
            read(&path, None, None).unwrap(),
            "     1\ta\n     2\tb\n     3\tc\n     4\td\n"
        );
        assert_eq!(
            read(&path, Some(2), Some(2)).unwrap(),
            "     2\tb\n     3\tc\n[lines 4–4 not shown: read on with offset 4]\n"
        );
        assert!(read(&path, Some(9), None).unwrap().contains("has 4 lines"));
    }

    /// A long line is cut rather than flooding the context.
    #[test]
    fn read_cuts_a_long_line() {
        let TempFile { dir: _dir, path } = file_with(&"x".repeat(MAX_LINE_CHARS + 50));
        let out = read(&path, None, None).unwrap();
        assert!(out.ends_with(" [line cut]\n"));
        assert_eq!(out.matches('x').count(), MAX_LINE_CHARS);
    }

    /// A directory or a binary file is refused with a reason.
    #[test]
    fn read_refuses_a_directory_or_binary() {
        let TempFile { dir, path } = file_with("");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(
            read(&path, None, None),
            Err(ExecError::NotText(_))
        ));
        assert!(matches!(
            read(dir.path(), None, None),
            Err(ExecError::IsDirectory(_))
        ));
    }

    /// Write makes the directories it needs and says whether it made the
    /// file or replaced it.
    #[test]
    fn write_creates_parents_and_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c.txt");
        assert!(write(&path, "one").unwrap().starts_with("Created"));
        assert!(write(&path, "two").unwrap().starts_with("Updated"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    /// Edit replaces exactly one occurrence, refuses an ambiguous one
    /// unless told to replace all, and leaves the file alone when it fails.
    #[test]
    fn edit_replaces_one_or_all_and_refuses_the_rest() {
        let TempFile { dir: _dir, path } = file_with("let a = 1;\nlet a = 1;\nlet b = 2;\n");
        let content = || std::fs::read_to_string(&path).unwrap();

        edit(&path, "let b = 2;", "let b = 3;", false).unwrap();
        assert!(content().ends_with("let b = 3;\n"));

        let before = content();
        assert!(matches!(
            edit(&path, "let a = 1;", "let a = 9;", false),
            Err(ExecError::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            edit(&path, "nowhere", "x", false),
            Err(ExecError::NoMatch(_))
        ));
        assert!(matches!(
            edit(&path, "let", "let", false),
            Err(ExecError::NoChange)
        ));
        assert!(matches!(
            edit(&path, "", "x", false),
            Err(ExecError::EmptyOldString)
        ));
        assert_eq!(content(), before, "a refused edit changes nothing");

        let out = edit(&path, "let a = 1;", "let a = 9;", true).unwrap();
        assert!(out.ends_with("(2 replacements)"));
        assert_eq!(content(), "let a = 9;\nlet a = 9;\nlet b = 3;\n");
    }
}
