//! Snapshot pngs (`crates/*/tests/snapshots/**/*.png`) live in Git LFS; real
//! assets don't, because builds read them without git-lfs: `assets/` is
//! compiled in with `include_bytes!`, and the Android mipmaps are packaged
//! into the APK. A rebase, a cherry-pick or a machine without git-lfs can
//! commit a raw png into an LFS path, or a pointer where the build expects an
//! image, and nothing else notices until a fetch 404s or an icon fails to
//! decode.
//!
//! This reads the index (its blobs, and `.gitattributes` as staged), never the
//! working tree, so it needs no git-lfs and runs in the plain
//! `cargo test --workspace`: `cargo test -p bin_testing --test lfs_snapshots`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The first line of every LFS pointer.
const POINTER_VERSION: &[u8] = b"version https://git-lfs.github.com/spec/v1\n";

/// Largest pointer file the LFS spec allows.
const MAX_POINTER_BYTES: usize = 1024;

/// One png in the index, as this check reads it.
struct IndexedPng {
    /// Repo-relative, `/`-separated.
    path: String,
    /// Whether the staged `.gitattributes` give it `filter=lfs`.
    lfs: bool,
    /// Whether its staged blob is an LFS pointer.
    pointer: bool,
}

/// The workspace root, where git runs.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `git <args>` in the repo with `input` on stdin, its stdout.
fn git(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("git runs");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = input.to_vec();
    // Written from a thread: `cat-file --batch` answers as it reads, so its
    // output could fill the pipe before all of the input is in.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().expect("git runs");
    writer
        .join()
        .expect("writer thread")
        .expect("git reads stdin");
    assert!(out.status.success(), "git {} failed", args.join(" "));
    out.stdout
}

/// Every png in the index with its attribute and whether it's a pointer.
fn indexed_pngs() -> Vec<IndexedPng> {
    // `<mode> <oid> <stage>\t<path>` per entry.
    let staged = git(&["ls-files", "--stage", "-z", "--", "*.png"], b"");
    let entries: Vec<(String, String)> = staged
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| {
            let e = std::str::from_utf8(e).expect("utf-8 index entry");
            let (meta, path) = e.split_once('\t').expect("a tab before the path");
            let oid = meta.split(' ').nth(1).expect("an oid");
            (oid.to_string(), path.to_string())
        })
        .collect();

    let paths: Vec<u8> = entries
        .iter()
        .flat_map(|(_, p)| [p.as_bytes(), b"\0"].concat())
        .collect();
    // `<path>\0filter\0<value>\0` per path, in input order.
    let attrs = git(
        &["check-attr", "--cached", "-z", "--stdin", "filter"],
        &paths,
    );
    let values: Vec<&[u8]> = attrs.split(|b| *b == 0).skip(2).step_by(3).collect();

    let oids: String = entries.iter().map(|(oid, _)| format!("{oid}\n")).collect();
    let blobs = git(&["cat-file", "--batch"], oids.as_bytes());
    let mut blobs = BufReader::new(&blobs[..]);

    entries
        .into_iter()
        .zip(values)
        .map(|((_, path), filter)| {
            let mut header = String::new();
            blobs.read_line(&mut header).expect("a cat-file header");
            let size: usize = header
                .trim_end()
                .rsplit(' ')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("a blob size in {header:?}"));
            // The blob, then the newline cat-file ends it with.
            let mut blob = vec![0; size + 1];
            blobs.read_exact(&mut blob).expect("the blob");
            blob.pop();
            IndexedPng {
                pointer: blob.len() <= MAX_POINTER_BYTES && blob.starts_with(POINTER_VERSION),
                lfs: filter == b"lfs",
                path,
            }
        })
        .collect()
}

/// Whether `path` is a kittest snapshot baseline.
fn is_snapshot(path: &str) -> bool {
    path.starts_with("crates/") && path.contains("/tests/snapshots/")
}

/// Whether `path` is an image a build reads, which must stay a plain blob.
fn is_build_asset(path: &str) -> bool {
    path.starts_with("assets/") || path.starts_with("crates/notedeck_chrome/android/")
}

#[test]
fn snapshot_pngs_are_lfs_pointers_and_assets_are_not() {
    let pngs = indexed_pngs();
    assert!(
        pngs.iter().any(|p| is_snapshot(&p.path)),
        "no snapshot png in the index: is this a checkout?"
    );
    let mut wrong = Vec::new();
    for png in &pngs {
        let path = &png.path;
        if is_snapshot(path) && !png.lfs {
            wrong.push(format!(
                "{path}: a snapshot png without filter=lfs; .gitattributes should cover it"
            ));
        }
        if is_build_asset(path) && png.lfs {
            wrong.push(format!(
                "{path}: a build asset with filter=lfs; builds read it without git-lfs, \
                 so .gitattributes must not cover it"
            ));
        }
        if png.lfs && !png.pointer {
            wrong.push(format!(
                "{path}: committed as a raw png into an LFS path; \
                 fix with `git add --renormalize {path}` (git-lfs installed)"
            ));
        }
        if !png.lfs && png.pointer {
            wrong.push(format!(
                "{path}: an LFS pointer at a path that isn't in LFS; \
                 commit the image, not its pointer"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}\n", wrong.join("\n"));
}
