//! Grep and Glob, over ripgrep's own directory walker, so a search skips
//! what `.gitignore` skips (a `target/` or `node_modules/` would otherwise
//! bury every answer) and Grep's `glob` reads as ripgrep's `--glob` does.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::overrides::OverrideBuilder;
use ignore::{Walk, WalkBuilder};
use regex::RegexBuilder;

use super::ExecError;
use notedeck_agent_proto::call::{GrepInput, GrepOutputMode};

/// Most files a Glob answers, newest first; the rest are counted.
const MAX_GLOB_FILES: usize = 100;

/// Grep skips a file bigger than this rather than read it whole.
const MAX_GREP_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Most characters a Grep answers; a longer answer is cut, and says so.
const MAX_GREP_CHARS: usize = 30_000;

/// The files under `root` a search looks at: what git wouldn't ignore,
/// hidden files included but never `.git` itself, in path order. With
/// `glob`, only the files it matches, as ripgrep's `--glob` matches them.
fn walk(root: &Path, glob: Option<&str>) -> Result<Walk, ExecError> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        // A .gitignore counts outside a git checkout too.
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .sort_by_file_path(|a, b| a.cmp(b));
    if let Some(pattern) = glob {
        let bad = |err: ignore::Error| ExecError::Glob {
            pattern: pattern.to_owned(),
            reason: err.to_string(),
        };
        let mut overrides = OverrideBuilder::new(root);
        overrides.add(pattern).map_err(bad)?;
        builder.overrides(overrides.build().map_err(bad)?);
    }
    Ok(builder.build())
}

/// The regular files `walk` yields, skipping what it can't read.
fn files(walk: Walk) -> impl Iterator<Item = PathBuf> {
    walk.filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
}

/// Search the files under `dir` (or the one file it names) for the call's
/// pattern, answering as its output mode asks.
pub(super) fn grep(input: &GrepInput, dir: &Path) -> Result<String, ExecError> {
    let re = RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive)
        .build()?;
    let mode = input.output_mode.unwrap_or_default();
    let limit = input.head_limit.map_or(usize::MAX, |n| n.max(1) as usize);
    let mut results: Vec<String> = Vec::new();
    for path in files(walk(dir, input.glob.as_deref())?) {
        if results.len() >= limit {
            break;
        }
        let too_big = std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_GREP_FILE_BYTES);
        if too_big {
            continue;
        }
        // A file that isn't UTF-8 text is binary to a search.
        let Ok(Ok(text)) = std::fs::read(&path).map(String::from_utf8) else {
            continue;
        };
        match mode {
            GrepOutputMode::FilesWithMatches => {
                if re.is_match(&text) {
                    results.push(path.display().to_string());
                }
            }
            GrepOutputMode::Count => {
                let n = text.lines().filter(|line| re.is_match(line)).count();
                if n > 0 {
                    results.push(format!("{}:{n}", path.display()));
                }
            }
            GrepOutputMode::Content => {
                let lines = text.lines().enumerate();
                let hits = lines.filter(|(_, line)| re.is_match(line));
                let room = limit - results.len();
                results.extend(
                    hits.take(room)
                        .map(|(i, line)| format!("{}:{}:{line}", path.display(), i + 1)),
                );
            }
        }
    }
    if results.is_empty() {
        return Ok("No matches found".to_owned());
    }
    Ok(cut(results.join("\n"), MAX_GREP_CHARS))
}

/// `text`, cut to `max` characters with a note saying so.
fn cut(mut text: String, max: usize) -> String {
    let Some((at, _)) = text.char_indices().nth(max) else {
        return text;
    };
    let dropped = text[at..].chars().count();
    text.truncate(at);
    text.push_str(&format!(
        "\n[{dropped} more characters cut: narrow the search, or set head_limit]"
    ));
    text
}

/// The files under `dir` whose path from `dir` matches `pattern`, newest
/// first. `*` stops at a `/` and `**` crosses any number of them, so
/// `*.rs` is the top level's Rust files and `**/*.rs` is all of them.
pub(super) fn glob(pattern: &str, dir: &Path) -> Result<String, ExecError> {
    let matcher = globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| ExecError::Glob {
            pattern: pattern.to_owned(),
            reason: err.kind().to_string(),
        })?
        .compile_matcher();
    let mut found: Vec<(SystemTime, PathBuf)> = files(walk(dir, None)?)
        .filter(|path| {
            path.strip_prefix(dir)
                .is_ok_and(|rel| matcher.is_match(rel))
        })
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    if found.is_empty() {
        return Ok("No files found".to_owned());
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let more = found.len().saturating_sub(MAX_GLOB_FILES);
    let mut out: Vec<String> = found
        .iter()
        .take(MAX_GLOB_FILES)
        .map(|(_, path)| path.display().to_string())
        .collect();
    if more > 0 {
        out.push(format!(
            "[{more} more files not shown: narrow the pattern or the path]"
        ));
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree with a gitignored build directory, a .git directory, a hidden
    /// file and Rust files at two depths.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let put = |rel: &str, text: &str| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        put(".gitignore", "target/\n");
        put("target/debug/out.rs", "fn needle() {}\n");
        put(".git/config", "needle\n");
        put(".github/ci.yml", "needle: yes\n");
        put("main.rs", "fn main() {\n    needle();\n}\n");
        put("src/lib.rs", "pub fn needle() {}\n// Needle\n");
        put("README.md", "no match here\n");
        dir
    }

    /// `line` with `dir`'s path and its separator taken off the front of
    /// each path in it, and the rest of the path written with `/`: what
    /// the search answers relative to the tree, on any OS.
    fn rel(dir: &Path, line: &str) -> String {
        let prefix = format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR);
        let line = line.replace(&prefix, "");
        if cfg!(windows) {
            line.replace('\\', "/")
        } else {
            line
        }
    }

    fn grep_in(dir: &Path, input: GrepInput) -> Vec<String> {
        let lines = grep(&input, dir).unwrap();
        lines.lines().map(|line| rel(dir, line)).collect()
    }

    fn input(pattern: &str) -> GrepInput {
        GrepInput {
            pattern: pattern.into(),
            path: None,
            glob: None,
            output_mode: None,
            case_insensitive: false,
            head_limit: None,
        }
    }

    /// Grep finds the files that match, skipping gitignored and .git
    /// files but not other hidden ones.
    #[test]
    fn grep_lists_matching_files_git_would_see() {
        let dir = tree();
        assert_eq!(
            grep_in(dir.path(), input("needle")),
            [".github/ci.yml", "main.rs", "src/lib.rs"]
        );
    }

    /// The output modes answer lines with their numbers, or counts, and
    /// -i, glob and head_limit narrow it.
    #[test]
    fn grep_modes_and_filters() {
        let dir = tree();
        let content = GrepInput {
            output_mode: Some(GrepOutputMode::Content),
            glob: Some("*.rs".into()),
            ..input("needle")
        };
        assert_eq!(
            grep_in(dir.path(), content.clone()),
            ["main.rs:2:    needle();", "src/lib.rs:1:pub fn needle() {}"]
        );
        let count = GrepInput {
            output_mode: Some(GrepOutputMode::Count),
            case_insensitive: true,
            glob: Some("*.rs".into()),
            ..input("needle")
        };
        assert_eq!(grep_in(dir.path(), count), ["main.rs:1", "src/lib.rs:2"]);
        let first = GrepInput {
            head_limit: Some(1),
            ..content
        };
        assert_eq!(grep_in(dir.path(), first), ["main.rs:2:    needle();"]);
        assert_eq!(
            grep(&input("absent"), dir.path()).unwrap(),
            "No matches found"
        );
        assert!(matches!(
            grep(&input("("), dir.path()),
            Err(ExecError::Pattern(_))
        ));
    }

    /// `*` stays in one directory, `**` crosses them, and ignored files
    /// aren't found.
    #[test]
    fn glob_matches_paths_from_the_dir() {
        let dir = tree();
        let found = |pattern| {
            let mut paths: Vec<String> = glob(pattern, dir.path())
                .unwrap()
                .lines()
                .map(|l| rel(dir.path(), l))
                .collect();
            paths.sort();
            paths
        };
        assert_eq!(found("*.rs"), ["main.rs"]);
        assert_eq!(found("**/*.rs"), ["main.rs", "src/lib.rs"]);
        assert_eq!(found("*.nothing"), ["No files found"]);
    }

    /// A long answer is cut, and says how much was.
    #[test]
    fn a_long_answer_is_cut() {
        let out = cut("é".repeat(10), 4);
        assert_eq!(
            out,
            "éééé\n[6 more characters cut: narrow the search, or set head_limit]"
        );
        assert_eq!(cut("short".into(), 10), "short");
    }
}
