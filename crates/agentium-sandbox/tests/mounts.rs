//! A real git checkout seen through exactly the mounts a [`MountPlan`]
//! names, and nothing else of the repository.
//!
//! The guest here is bubblewrap rather than a microVM, so it runs on any
//! Linux box with user namespaces (no KVM): `--bind` and `--ro-bind` give a
//! guest the same view a microsandbox bind mount does, at the same path,
//! with `EROFS` on a readonly one. The rest of the repository's temp
//! directory is hidden under a tmpfs, so the main checkout's working tree
//! isn't visible from a worktree's guest, as it wouldn't be in a VM.
//!
//! Where bubblewrap isn't installed or can't make a namespace, each test
//! says so on stderr and passes without checking anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agentium_sandbox::mounts::{GitAccess, GuestEnv, Mount, MountPlan, RepoLayout};

/// A main repository with one commit, a linked worktree beside it, and a
/// bare remote, all in one temp dir.
struct Fixture {
    /// Held so the dir lives as long as the fixture.
    _root: tempfile::TempDir,
    /// The temp dir's real path. git writes canonical paths into a linked
    /// worktree's `.git` file, and on macOS the temp dir is under `/var`, a
    /// symlink to `/private/var`, so paths built from the uncanonical one
    /// would never match what the planner reads back.
    base: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let base = real_path(root.path());
        let f = Fixture { _root: root, base };
        let main = f.main();
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "master"]);
        std::fs::write(main.join("a.txt"), "one\n").unwrap();
        git(&main, &["add", "a.txt"]);
        git(&main, &["commit", "-q", "-m", "first"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                f.worktree().to_str().unwrap(),
            ],
        );
        git(
            &f.base,
            &["init", "-q", "--bare", f.remote().to_str().unwrap()],
        );
        git(
            &main,
            &["remote", "add", "origin", f.remote().to_str().unwrap()],
        );
        git(&main, &["push", "-q", "origin", "master"]);
        git(&main, &["fetch", "-q", "origin"]);
        f
    }

    fn main(&self) -> PathBuf {
        self.base.join("main")
    }

    fn worktree(&self) -> PathBuf {
        self.base.join("wt")
    }

    fn remote(&self) -> PathBuf {
        self.base.join("remote.git")
    }
}

/// `path` with its symlinks resolved, on Unix. Windows is left alone:
/// `canonicalize` there gives a `\\?\` verbatim path, which git can't make
/// a worktree under, and the temp dir has no symlink to resolve.
fn real_path(path: &Path) -> PathBuf {
    if cfg!(unix) {
        path.canonicalize().expect("canonical tempdir")
    } else {
        path.to_path_buf()
    }
}

/// git with no user or system config, so a developer's hooks, signing or
/// aliases can't change what's measured.
fn git_cmd(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    cmd
}

fn git(cwd: &Path, args: &[&str]) {
    let out = git_cmd(cwd).args(args).output().expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Whether bubblewrap can run here; prints why not.
fn bwrap_works() -> bool {
    let ok = Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("SKIPPED: bubblewrap missing or can't create a namespace here");
    }
    ok
}

/// Run `sh -c script` in a "guest" that sees the host read-only, `hide`
/// emptied, and then `mounts` bound back in at their own paths, in the
/// plan's order. The working directory is `cwd`. `/tmp` is the guest's
/// own and writable, as on a microVM's root disk.
fn in_guest(hide: &Path, mounts: &[Mount], cwd: &Path, script: &str) -> Output {
    in_guest_with(hide, mounts, cwd, &[], script)
}

/// [`in_guest`], with `env` set for every command, as a sandbox's env is.
fn in_guest_with(
    hide: &Path,
    mounts: &[Mount],
    cwd: &Path,
    env: &[GuestEnv],
    script: &str,
) -> Output {
    let mut cmd = Command::new("bwrap");
    cmd.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
    cmd.args(["--tmpfs", "/tmp"]);
    cmd.arg("--tmpfs").arg(hide);
    for m in mounts {
        cmd.arg(if m.readonly { "--ro-bind" } else { "--bind" })
            .arg(&m.path)
            .arg(&m.path);
    }
    cmd.arg("--chdir").arg(cwd);
    cmd.args(["--", "sh", "-c", script])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    for e in env {
        cmd.env(e.key, &e.value);
    }
    cmd.output().expect("run bwrap")
}

fn report(what: &str, out: &Output) -> String {
    format!(
        "{what}: rc={:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The plan for a linked worktree names the main repo's `.git`, found by
/// following `.git` -> gitdir -> commondir.
#[test]
fn a_linked_worktree_plans_its_common_git_dir() {
    let f = Fixture::new();
    let plan = MountPlan::for_checkout(&f.worktree(), GitAccess::Review).unwrap();
    let main_git = f.main().join(".git");
    assert_eq!(
        plan.layout,
        RepoLayout::Linked {
            git_dir: main_git.join("worktrees").join("wt"),
            common_dir: main_git.clone(),
        }
    );
    assert_eq!(
        plan.mounts,
        vec![
            Mount {
                path: f.worktree(),
                readonly: false
            },
            Mount {
                path: main_git,
                readonly: true
            },
        ]
    );
    assert_eq!(plan.checkout_str(), f.worktree().to_str().unwrap());
}

/// A directory that isn't a repository is mounted on its own; one that
/// doesn't exist is an error.
#[test]
fn not_a_repo_and_missing_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let plan = MountPlan::for_checkout(dir.path(), GitAccess::Review).unwrap();
    assert_eq!(plan.layout, RepoLayout::NotARepo);
    assert_eq!(plan.mounts.len(), 1);
    assert!(MountPlan::for_checkout(&dir.path().join("nope"), GitAccess::Review).is_err());
}

/// A worktree whose main repository is gone plans to an error rather than
/// a guest where git silently doesn't work.
#[test]
fn a_dangling_worktree_is_an_error() {
    let f = Fixture::new();
    std::fs::remove_dir_all(f.main()).unwrap();
    let err = MountPlan::for_checkout(&f.worktree(), GitAccess::Review).unwrap_err();
    assert!(err.to_string().contains("doesn't exist"), "{err}");
}

/// The spike's case A: the worktree alone is not a repository in the guest.
#[test]
fn the_worktree_alone_is_not_a_repository() {
    if !bwrap_works() {
        return;
    }
    let f = Fixture::new();
    let plan = MountPlan::for_checkout(&f.worktree(), GitAccess::Commit).unwrap();
    let only_wt = &plan.mounts[..1];
    let out = in_guest(&f.base, only_wt, &f.worktree(), "git status");
    assert!(!out.status.success(), "{}", report("status", &out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a git repository"),
        "{}",
        report("status", &out)
    );
}

/// What each profile allows, measured: the read side works under both,
/// everything that writes shared git state fails under Review.
#[test]
fn review_refuses_git_writes_and_commit_allows_them() {
    if !bwrap_works() {
        return;
    }
    /// One git operation and whether Review lets it through.
    struct Op {
        name: &'static str,
        script: &'static str,
        review_ok: bool,
    }
    let op = |name, script, review_ok| Op {
        name,
        script,
        review_ok,
    };
    let table = [
        op("status", "git status --porcelain", true),
        op("log", "git log --oneline", true),
        op("diff", "echo two >> a.txt && git diff --stat", true),
        op("rev-parse", "git rev-parse HEAD", true),
        op("ls-files", "git ls-files", true),
        op(
            "build output",
            "mkdir -p target && echo x > target/out",
            true,
        ),
        op("add", "echo two >> a.txt && git add a.txt", false),
        op(
            "commit",
            "echo two >> a.txt && git commit -qam second",
            false,
        ),
        op("checkout -b", "git checkout -q -b other", false),
        op("stash", "echo two >> a.txt && git stash -q", false),
        op("gc", "git gc -q", false),
    ];

    for access in [GitAccess::Review, GitAccess::Commit] {
        for Op {
            name,
            script,
            review_ok,
        } in &table
        {
            // A fresh repo per op, so one op's writes can't change the next.
            let f = Fixture::new();
            let plan = MountPlan::for_checkout(&f.worktree(), access).unwrap();
            let out = in_guest(&f.base, &plan.mounts, &f.worktree(), script);
            let want = access == GitAccess::Commit || *review_ok;
            assert_eq!(
                out.status.success(),
                want,
                "{access:?} {}",
                report(name, &out)
            );
            if !want {
                assert!(
                    String::from_utf8_lossy(&out.stderr).contains("Read-only file system"),
                    "{access:?} {}",
                    report(name, &out)
                );
            }
        }
    }
}

/// A commit made in the guest is the host's commit: same object store, same
/// branch, nothing to copy back.
#[test]
fn a_guest_commit_is_visible_on_the_host() {
    if !bwrap_works() {
        return;
    }
    let f = Fixture::new();
    let plan = MountPlan::for_checkout(&f.worktree(), GitAccess::Commit).unwrap();
    let out = in_guest(
        &f.base,
        &plan.mounts,
        &f.worktree(),
        "echo two >> a.txt && git commit -qam from-guest",
    );
    assert!(out.status.success(), "{}", report("commit", &out));
    let log = git_cmd(&f.main())
        .args(["log", "-1", "--format=%s", "wt"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "from-guest");
}

/// A main checkout under Review gets its `.git` as a readonly mount nested
/// in the read-write checkout, and that nesting holds.
#[test]
fn a_main_checkout_under_review_nests_a_readonly_git_dir() {
    if !bwrap_works() {
        return;
    }
    let f = Fixture::new();
    let plan = MountPlan::for_checkout(&f.main(), GitAccess::Review).unwrap();
    assert_eq!(plan.mounts.len(), 2);
    let ok = in_guest(
        &f.base,
        &plan.mounts,
        &f.main(),
        "echo two >> a.txt && git status --porcelain",
    );
    assert!(ok.status.success(), "{}", report("status", &ok));
    let refused = in_guest(&f.base, &plan.mounts, &f.main(), "git add a.txt");
    assert!(!refused.status.success(), "{}", report("add", &refused));
}

/// The caveat on the Review profile: a readonly git dir does NOT stop a
/// push. Push only reads local objects; the remote gets the branch, and
/// only the remote-tracking ref update afterwards hits the readonly mount.
/// What stops a guest pushing is no credentials and the egress allowlist.
///
/// The remote here is a directory bound read-write into the guest, which
/// stands in for a reachable, authenticated remote.
#[test]
fn push_lands_through_a_readonly_git_dir() {
    if !bwrap_works() {
        return;
    }
    let f = Fixture::new();
    // A commit to push, made on the host.
    std::fs::write(f.worktree().join("a.txt"), "pushed\n").unwrap();
    git(&f.worktree(), &["commit", "-qam", "to-push"]);
    let head = String::from_utf8(
        git_cmd(&f.worktree())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    let plan = MountPlan::for_checkout(&f.worktree(), GitAccess::Review).unwrap();
    let mut mounts = plan.mounts.clone();
    mounts.push(Mount {
        path: f.remote(),
        readonly: false,
    });
    let out = in_guest(&f.base, &mounts, &f.worktree(), "git push origin wt");
    eprintln!("{}", report("push under Review", &out));

    let remote_head = git_cmd(&f.remote())
        .args(["rev-parse", "refs/heads/wt"])
        .output()
        .unwrap();
    assert!(
        remote_head.status.success(),
        "the push didn't land: {}",
        report("push", &out)
    );
    assert_eq!(String::from_utf8_lossy(&remote_head.stdout), head);
}

/// The guest doesn't own the host's files, so git there sees every mounted
/// repository as someone else's. `GIT_TEST_ASSUME_DIFFERENT_OWNER` is git's
/// own switch for that case: with it, git refuses the checkout as `dubious
/// ownership` until the plan's [`MountPlan::git_env`] names it, and still
/// refuses any other repository. No guest needed.
#[test]
fn git_env_trusts_the_checkout_and_nothing_else() {
    let f = Fixture::new();
    for checkout in [f.worktree(), f.main()] {
        let plan = MountPlan::for_checkout(&checkout, GitAccess::Review).unwrap();
        let rev_parse = |dir: &Path, trusted: bool| {
            let mut cmd = git_cmd(dir);
            cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
                .args(["rev-parse", "HEAD"]);
            if trusted {
                for e in plan.git_env() {
                    cmd.env(e.key, e.value);
                }
            }
            cmd.output().expect("run git")
        };

        let refused = rev_parse(&checkout, false);
        assert!(
            !refused.status.success()
                && String::from_utf8_lossy(&refused.stderr).contains("dubious ownership"),
            "{}",
            report("untrusted", &refused)
        );
        let trusted = rev_parse(&checkout, true);
        assert!(trusted.status.success(), "{}", report("trusted", &trusted));
        let elsewhere = rev_parse(&f.remote(), true);
        assert!(
            !elsewhere.status.success(),
            "{}",
            report("another repo", &elsewhere)
        );
    }
}

/// A directory that isn't a repository needs no trust.
#[test]
fn git_env_is_empty_outside_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    let plan = MountPlan::for_checkout(dir.path(), GitAccess::Review).unwrap();
    assert_eq!(plan.layout, RepoLayout::NotARepo);
    assert!(plan.git_env().is_empty());
}

/// Whether git-lfs is installed; prints why not.
fn git_lfs_works() -> bool {
    let ok = Command::new("git-lfs")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("SKIPPED: git-lfs isn't installed");
    }
    ok
}

/// `git status` in a Review guest of a repository with an LFS file. Its
/// stat info no longer matches the index, as every file's doesn't in a real
/// guest, so git runs the LFS clean filter, and git-lfs writes a temp file
/// under `.git/lfs/tmp`: `read-only file system` under Review, and git dies.
/// The plan's [`MountPlan::git_env`] moves LFS's storage into the guest's
/// own `/tmp`. Commit's git dir is writable, so it isn't moved there.
#[test]
fn review_git_status_survives_the_lfs_clean_filter() {
    if !bwrap_works() || !git_lfs_works() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("lfs");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "master"]);
    // `git lfs install --local`, by hand: user and system config are off.
    for (key, value) in [
        ("filter.lfs.process", "git-lfs filter-process"),
        ("filter.lfs.clean", "git-lfs clean -- %f"),
        ("filter.lfs.smudge", "git-lfs smudge -- %f"),
        ("filter.lfs.required", "true"),
    ] {
        git(&repo, &["config", key, value]);
    }
    std::fs::write(repo.join(".gitattributes"), "*.png filter=lfs -text\n").unwrap();
    std::fs::write(repo.join("a.png"), b"not really a png").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "lfs"]);
    let pointer = git_cmd(&repo)
        .args(["cat-file", "-p", "HEAD:a.png"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&pointer.stdout).starts_with("version https://git-lfs"),
        "a.png wasn't committed through LFS"
    );
    // Same content, new stat info: git has to re-hash it.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(repo.join("a.png"), b"not really a png").unwrap();

    let review = MountPlan::for_checkout(&repo, GitAccess::Review).unwrap();
    let status = "git status --porcelain";

    let bare = in_guest(root.path(), &review.mounts, &repo, status);
    assert!(
        !bare.status.success()
            && String::from_utf8_lossy(&bare.stderr).contains("read-only file system"),
        "{}",
        report("Review without git_env", &bare)
    );

    let fixed = in_guest_with(
        root.path(),
        &review.mounts,
        &repo,
        &review.git_env(),
        &format!("{status} && git diff --stat"),
    );
    assert!(fixed.status.success(), "{}", report("Review", &fixed));
    assert_eq!(
        String::from_utf8_lossy(&fixed.stdout),
        "",
        "{}",
        report("Review: an unchanged LFS file showed as changed", &fixed)
    );

    let commit = MountPlan::for_checkout(&repo, GitAccess::Commit).unwrap();
    assert!(
        commit.git_env().iter().all(|e| e.value != "lfs.storage"),
        "Commit moved LFS storage out of the repository"
    );
    let out = in_guest_with(
        root.path(),
        &commit.mounts,
        &repo,
        &commit.git_env(),
        status,
    );
    assert!(out.status.success(), "{}", report("Commit", &out));
}
