//! Find *this* worktree's build of a CLI binary, for end-to-end tests that run
//! the real executable.
//!
//! `env!("CARGO_BIN_EXE_x")` names the top-level `target/debug/x`. When several
//! git worktrees share one `target/` dir, that is a *single* path every worktree
//! hardlinks its own `x` onto. Under `cargo test --workspace` cargo compiles the
//! bin but records its uplift as already done and won't re-link it, so a sibling
//! worktree's older `x` can own that path, and a concurrent sibling build can
//! re-clobber it at any instant *during* the test. `cargo test -p <crate>`
//! re-roots the package and forces the uplift, which is why such a test passes in
//! isolation and fails in the workspace run.
//!
//! [`worktree_bin`] goes to the per-fingerprint artifact cargo actually built,
//! `target/debug/deps/x-<hash>`, instead. That name is unique to a
//! source + features fingerprint, so a sibling never overwrites it with
//! *different* code.
//!
//! Std-only on purpose: the CLI crates that use it are lean, and pulling in
//! `notedeck_testing` (egui, eframe, notedeck) as their dev-dep would not be.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Resolve the binary this worktree built for `uplifted`
/// (= `env!("CARGO_BIN_EXE_x")`).
///
/// Picks the newest `deps/x-<hash>` executable next to `uplifted` whose dep-info
/// (`x-<hash>.d`) says it was built through *this* worktree's `deps` dir and from
/// sources under `crate_dir` (e.g. `"headway_cli"`), and that `is_current`
/// accepts. `is_current` is the caller's check that a binary is a current build,
/// typically "its usage mentions the subcommand under test".
///
/// `crate_dir` matters when a library crate shares the binary's name, as the
/// `headway` lib does with the `headway` bin: the lib's unit-test harness is also
/// `deps/headway-<hash>`, and without the filter `is_current` would run that
/// whole test suite once per stale hash.
///
/// Falls back to `uplifted` itself when the scan identifies nothing, as long as
/// `is_current` accepts it. The scan reads cargo's dep-info, whose layout is not
/// contractual (on Windows CI it identifies nothing at all), and the hazard it
/// guards against needs sibling worktrees, which a CI runner's single fresh
/// checkout doesn't have.
///
/// # Panics
///
/// When neither the scan nor the uplifted binary yields a current build.
pub fn worktree_bin(
    uplifted: &Path,
    crate_dir: &str,
    is_current: impl Fn(&Path) -> bool,
) -> PathBuf {
    let deps = uplifted
        .parent()
        .expect("the uplifted binary has a target/<profile> parent")
        .join("deps");
    let name = exe_stem(uplifted).expect("the uplifted binary has a UTF-8 file name");

    let newest = std::fs::read_dir(&deps)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_bin_artifact(path, name))
        .filter(|path| built_here(path, &deps, crate_dir))
        .filter(|path| is_current(path))
        .max_by_key(|path| modified(path));
    if let Some(path) = newest {
        return path;
    }

    if is_current(uplifted) {
        return uplifted.to_path_buf();
    }
    panic!(
        "no current `{name}` under {} and none at {} — build it first \
         (e.g. `cargo build --bin {name}`)",
        deps.display(),
        uplifted.display()
    )
}

/// The file name without [`std::env::consts::EXE_SUFFIX`] (`.exe` on Windows,
/// empty elsewhere).
fn exe_stem(path: &Path) -> Option<&str> {
    path.file_name()?
        .to_str()?
        .strip_suffix(std::env::consts::EXE_SUFFIX)
}

/// A runnable `deps/<name>-<hash>` executable, as opposed to the `.d`, `.rmeta`
/// and `.o` files cargo drops next to it under the same stem.
///
/// The suffix is stripped before rejecting dots, or Windows' `x-<hash>.exe`
/// would be thrown out with the build artifacts.
fn is_bin_artifact(path: &Path, name: &str) -> bool {
    exe_stem(path)
        .and_then(|stem| stem.strip_prefix(name))
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hash| !hash.is_empty() && !hash.contains('.'))
        && path.is_file()
}

/// Whether the artifact's dep-info names this worktree's `deps` dir and a source
/// under `crate_dir`.
///
/// Cargo writes the dep-info's first line as `<abs path to target>: <sources>`,
/// with the path taken through the *building* worktree's `target`. A hash shared
/// with a sibling means byte-identical source, so a miss here is never wrong,
/// only deferred to an owned twin or the fallback.
fn built_here(bin: &Path, deps: &Path, crate_dir: &str) -> bool {
    let Ok(depinfo) = std::fs::read_to_string(bin.with_extension("d")) else {
        return false;
    };
    let first = depinfo.lines().next().unwrap_or("");
    let in_this_worktree = first
        .strip_prefix(&*deps.to_string_lossy())
        .is_some_and(|rest| rest.starts_with(['/', '\\']));
    in_this_worktree && first.contains(crate_dir)
}

/// The file's mtime, or the epoch when it can't be read.
fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    /// A fake `target/debug` with its `deps` dir, and the uplifted `headway` path.
    struct FakeTarget {
        _dir: tempfile::TempDir,
        deps: PathBuf,
        uplifted: PathBuf,
    }

    impl FakeTarget {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let debug = dir.path().join("target").join("debug");
            let deps = debug.join("deps");
            std::fs::create_dir_all(&deps).expect("deps dir");
            let uplifted = debug.join(format!("headway{}", std::env::consts::EXE_SUFFIX));
            Self {
                _dir: dir,
                deps,
                uplifted,
            }
        }

        /// Drop `headway-<hash>` plus a dep-info whose first line starts at
        /// `target_deps` and lists `source`, with the binary's mtime `age` ago.
        fn artifact(&self, hash: &str, target_deps: &Path, source: &str, age: u64) -> PathBuf {
            let bin = self
                .deps
                .join(format!("headway-{hash}{}", std::env::consts::EXE_SUFFIX));
            let file = File::create(&bin).expect("fake bin");
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .expect("set mtime");
            let depinfo = format!(
                "{}: {source}\n\n{source}:\n",
                target_deps.join(format!("headway-{hash}.d")).display()
            );
            std::fs::write(bin.with_extension("d"), depinfo).expect("fake dep-info");
            bin
        }

        fn resolve(&self, is_current: impl Fn(&Path) -> bool) -> PathBuf {
            worktree_bin(&self.uplifted, "headway_cli", is_current)
        }
    }

    const CLI_SRC: &str = "crates/headway_cli/src/main.rs";

    #[test]
    fn picks_our_cli_build() {
        let t = FakeTarget::new();
        let ours = t.artifact("aaaa", &t.deps, CLI_SRC, 10);
        assert_eq!(t.resolve(|_| true), ours);
    }

    #[test]
    fn rejects_a_same_named_lib_test_harness() {
        let t = FakeTarget::new();
        let ours = t.artifact("aaaa", &t.deps, CLI_SRC, 100);
        // Newer, built here, but from the `headway` lib's sources.
        t.artifact("bbbb", &t.deps, "crates/headway/src/lib.rs", 1);
        assert_eq!(t.resolve(|_| true), ours);
    }

    #[test]
    fn rejects_a_sibling_worktrees_build() {
        let t = FakeTarget::new();
        let ours = t.artifact("aaaa", &t.deps, CLI_SRC, 100);
        // Newer, same crate, but cargo built it through another worktree.
        let sibling = Path::new("/elsewhere/notedeck-other/target/debug/deps");
        t.artifact("cccc", sibling, CLI_SRC, 1);
        // A worktree whose path merely extends ours is a sibling, too.
        let longer = PathBuf::from(format!("{}-other", t.deps.display()));
        t.artifact("dddd", &longer, CLI_SRC, 1);
        assert_eq!(t.resolve(|_| true), ours);
    }

    #[test]
    fn newest_current_build_wins() {
        let t = FakeTarget::new();
        t.artifact("aaaa", &t.deps, CLI_SRC, 100);
        let newer = t.artifact("bbbb", &t.deps, CLI_SRC, 10);
        let stale = t.artifact("cccc", &t.deps, CLI_SRC, 1);
        assert_eq!(t.resolve(|p| p != stale), newer);
    }

    #[test]
    fn ignores_build_artifacts_sharing_the_stem() {
        let t = FakeTarget::new();
        let ours = t.artifact("aaaa", &t.deps, CLI_SRC, 100);
        File::create(t.deps.join("headway-aaaa.rmeta")).expect("rmeta");
        File::create(t.deps.join("headway_cli-eeee")).expect("other crate");
        assert_eq!(t.resolve(|_| true), ours);
    }

    #[test]
    fn falls_back_to_the_uplifted_binary() {
        let t = FakeTarget::new();
        t.artifact(
            "cccc",
            Path::new("/elsewhere/target/debug/deps"),
            CLI_SRC,
            1,
        );
        assert_eq!(t.resolve(|_| true), t.uplifted);
    }

    #[test]
    #[should_panic(expected = "no current `headway`")]
    fn panics_when_nothing_is_current() {
        let t = FakeTarget::new();
        t.artifact("aaaa", &t.deps, CLI_SRC, 1);
        t.resolve(|_| false);
    }
}
