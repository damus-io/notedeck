//! The git [`SegmentTransport`]: the journal's segments as files in a git
//! repository, so the remote keeps a history of every push and can be any
//! git URL (a bare repo over ssh, a self-hosted forge, a private hosted
//! one).
//!
//! The transport keeps **its own clone** at `sync/git/<name>/` and works
//! only there: the journal directory is never a git worktree, and fetched
//! files go to staging like every transport's. The clone is a cache of the
//! remote's `main` branch. Each step fetches and resets it to the remote's
//! tip, so a clone left odd by a failed run heals on the next one, and a
//! commit that never got pushed is simply made again: what to push is
//! worked out from the remote's tree every time.
//!
//! Segments are add-only files named by their hashes, so two machines
//! pushing at once never conflict: the loser rebases its one commit onto
//! the winner's and pushes again.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ledgerdb::{JournalStore, SegmentHash};

use super::transport::{FetchReport, PushReport, SegmentTransport, TransportError};

/// The program this transport runs.
const PROGRAM: &str = "git";

/// The branch the segments live on, in every clone and on the remote.
const BRANCH: &str = "main";

/// The clone's copy of the remote's [`BRANCH`].
const TRACKING: &str = "refs/remotes/origin/main";

/// A git remote holding the journal, as `[[sync.transport]] kind = "git"`
/// configures it.
#[derive(Clone, Debug)]
pub struct GitTransport {
    name: String,
    remote: String,
    clone: PathBuf,
    host: String,
}

impl GitTransport {
    /// The transport called `name` for the repository at `remote` (any URL
    /// `git` takes), working in its own clone at `clone`; `host` names
    /// this machine in its commit messages.
    pub fn new(name: String, remote: String, clone: PathBuf, host: String) -> Self {
        GitTransport {
            name,
            remote,
            clone,
            host,
        }
    }

    /// Run git in the clone with `args`, whatever its exit status. It never
    /// prompts: a remote that wants a password it can't get fails instead.
    fn output(&self, args: &[&str]) -> Result<Output, TransportError> {
        Command::new(PROGRAM)
            .arg("-C")
            .arg(&self.clone)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => TransportError::NotInstalled {
                    program: PROGRAM,
                    kind: "git",
                },
                _ => TransportError::io(Path::new(PROGRAM), err),
            })
    }

    /// [`Self::output`], failing unless git exits 0.
    fn git(&self, args: &[&str]) -> Result<Output, TransportError> {
        let out = self.output(args)?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(failed(&out))
        }
    }

    /// Make the clone if it isn't there, and point its `origin` at the
    /// configured remote (which may have changed since).
    ///
    /// The clone gets its own identity, no signing and no hooks: its
    /// commits are the machine's, and a global `commit.gpgsign` or hook
    /// must not stall a sync nobody is watching.
    fn ensure_clone(&self) -> Result<(), TransportError> {
        if !self.clone.join(".git").is_dir() {
            std::fs::create_dir_all(&self.clone)
                .map_err(|err| TransportError::io(&self.clone, err))?;
            self.git(&["init", "-q"])?;
            let email = format!("cashflow@{}", self.host);
            for (key, value) in [
                ("user.name", "cashflow"),
                ("user.email", email.as_str()),
                ("commit.gpgsign", "false"),
                ("core.hooksPath", "/dev/null"),
                ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ] {
                self.git(&["config", key, value])?;
            }
        }
        self.git(&["config", "remote.origin.url", &self.remote])?;
        Ok(())
    }

    /// Fetch the remote and reset the clone to its `main`, or to an empty
    /// `main` when the remote has none yet, and return the segments that
    /// leaves in the tree.
    fn refresh(&self) -> Result<HashSet<SegmentHash>, TransportError> {
        self.ensure_clone()?;
        // A rebase a failed push left half done would block the checkout.
        let _ = self.output(&["rebase", "--abort"]);
        self.git(&["fetch", "-q", "--prune", "origin"])?;
        let has_tip = self
            .output(&["rev-parse", "-q", "--verify", TRACKING])?
            .status
            .success();
        if has_tip {
            self.git(&["checkout", "-q", "-f", "-B", BRANCH, TRACKING])?;
        } else {
            self.git(&["symbolic-ref", "HEAD", &format!("refs/heads/{BRANCH}")])?;
            self.git(&["update-ref", "-d", &format!("refs/heads/{BRANCH}")])?;
            self.git(&["read-tree", "--empty"])?;
        }
        self.git(&["clean", "-q", "-f", "-d", "-x"])?;
        if has_tip {
            self.tree(TRACKING)
        } else {
            Ok(HashSet::new())
        }
    }

    /// The segments in `rev`'s tree: every file filed where
    /// [`JournalStore::relative_path`] would put it. Anything else in the
    /// repository, a README say, is ignored.
    fn tree(&self, rev: &str) -> Result<HashSet<SegmentHash>, TransportError> {
        let out = self.git(&["ls-tree", "-r", "--name-only", "-z", rev])?;
        Ok(out
            .stdout
            .split(|&b| b == 0)
            .filter_map(|path| segment_at(std::str::from_utf8(path).ok()?))
            .collect())
    }

    /// Copy the `hashes` the remote's tree lacks from `store` into the
    /// clone, returning those copied.
    fn stage(
        &self,
        store: &JournalStore,
        hashes: &[SegmentHash],
        remote: &HashSet<SegmentHash>,
    ) -> Result<Vec<SegmentHash>, TransportError> {
        let mut added = Vec::new();
        for &hash in hashes.iter().filter(|hash| !remote.contains(hash)) {
            let to = self.clone.join(JournalStore::relative_path(hash));
            copy(&store.path_of(hash), &to)?;
            added.push(hash);
        }
        Ok(added)
    }

    /// Commit everything staged in the clone as `count` segments from this
    /// host.
    fn commit(&self, count: usize) -> Result<(), TransportError> {
        let noun = if count == 1 { "segment" } else { "segments" };
        let message = format!("cashflow: add {count} {noun} from {}", self.host);
        self.git(&["add", "-A", "."])?;
        self.git(&["commit", "-q", "-m", &message])?;
        Ok(())
    }

    /// Push the clone's `main`. A rejected push (another machine pushed
    /// since the fetch) fetches, rebases onto the new tip and tries once
    /// more; the rebase can't conflict, as both sides only add files named
    /// by their hashes, and a commit whose files the remote already has
    /// drops out of it. Returns which of `added` the remote lacked.
    fn push_retrying(&self, added: Vec<SegmentHash>) -> Result<Vec<SegmentHash>, TransportError> {
        let refspec = format!("HEAD:refs/heads/{BRANCH}");
        let push = ["push", "-q", "origin", refspec.as_str()];
        if self.git(&push).is_ok() {
            return Ok(added);
        }
        self.git(&["fetch", "-q", "--prune", "origin"])?;
        let theirs = self.tree(TRACKING)?;
        if let Err(err) = self.git(&["rebase", "-q", TRACKING]) {
            let _ = self.output(&["rebase", "--abort"]);
            return Err(err);
        }
        self.git(&push)?;
        Ok(added
            .into_iter()
            .filter(|hash| !theirs.contains(hash))
            .collect())
    }
}

impl SegmentTransport for GitTransport {
    fn name(&self) -> &str {
        &self.name
    }

    /// The segments on the remote's `main`, from the clone after a fetch.
    fn list_remote(&self) -> Result<Option<HashSet<SegmentHash>>, TransportError> {
        self.refresh().map(Some)
    }

    /// Copy what the remote lacks into the clone, commit it as one commit
    /// and push.
    fn push(
        &self,
        store: &JournalStore,
        hashes: &[SegmentHash],
    ) -> Result<PushReport, TransportError> {
        let remote = self.refresh()?;
        let added = self.stage(store, hashes, &remote)?;
        if added.is_empty() {
            return Ok(PushReport::default());
        }
        self.commit(added.len())?;
        Ok(PushReport {
            sent: self.push_retrying(added)?,
        })
    }

    /// Bring the clone up to the remote's tip and copy the wanted segments
    /// it holds into staging.
    fn fetch(&self, want: &[SegmentHash], staging: &Path) -> Result<FetchReport, TransportError> {
        let remote = self.refresh()?;
        let mut staged = Vec::new();
        for &hash in want.iter().filter(|hash| remote.contains(hash)) {
            let rel = JournalStore::relative_path(hash);
            copy(&self.clone.join(&rel), &staging.join(&rel))?;
            staged.push(hash);
        }
        Ok(FetchReport { staged })
    }
}

/// The segment a repository path names, if it is filed as
/// [`JournalStore::relative_path`] files one.
fn segment_at(path: &str) -> Option<SegmentHash> {
    let hash: SegmentHash = Path::new(path)
        .file_name()?
        .to_str()?
        .strip_suffix(".ledger")?
        .parse()
        .ok()?;
    (JournalStore::relative_path(hash) == Path::new(path)).then_some(hash)
}

/// Copy the file at `from` to `to`, making `to`'s directory.
fn copy(from: &Path, to: &Path) -> Result<(), TransportError> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|err| TransportError::io(dir, err))?;
    }
    std::fs::copy(from, to).map_err(|err| TransportError::io(from, err))?;
    Ok(())
}

/// git's failure, with its stderr.
fn failed(out: &Output) -> TransportError {
    TransportError::Failed {
        program: PROGRAM,
        status: out.status,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::transport::{SyncJob, run};
    use crate::{CashflowDir, journal};
    use ledgerdb::{Journal, SegmentHeader};

    /// Whether `git` runs here; the tests skip when it doesn't.
    fn have_git() -> bool {
        Command::new(PROGRAM)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    fn import(store: &JournalStore, imported: u64, id: &str) -> SegmentHash {
        let header = SegmentHeader {
            source: "test".to_owned(),
            imported,
            layer: "cards".to_owned(),
        };
        let text = format!("2024-01-02 * Shop\n  ; id: {id}\n  a:b  -1 CAD\n  c:d\n");
        store
            .write_segment(&header, &Journal::parse(&text).journal)
            .unwrap()
    }

    /// One machine: a data dir and its git transport to `remote`.
    struct Machine {
        dir: CashflowDir,
        store: JournalStore,
        transport: GitTransport,
    }

    impl Machine {
        fn new(root: &Path, host: &str, remote: &Path) -> Self {
            let dir = CashflowDir::new(root.join(host));
            let transport = GitTransport::new(
                "hub".to_owned(),
                remote.to_string_lossy().into_owned(),
                dir.sync().join("git").join("hub"),
                host.to_owned(),
            );
            Machine {
                store: journal::store(&dir),
                dir,
                transport,
            }
        }

        fn sync(
            &self,
            fetch: Vec<SegmentHash>,
            push: Vec<SegmentHash>,
        ) -> super::super::SyncReport {
            let transports: Vec<Box<dyn SegmentTransport>> = vec![Box::new(self.transport.clone())];
            let report = run(
                &transports,
                &self.store,
                &self.dir.staging(),
                &SyncJob { fetch, push },
                false,
            );
            assert!(!report.has_errors(), "{report:?}");
            report
        }
    }

    /// A bare repository at `root/remote.git`.
    fn bare(root: &Path) -> PathBuf {
        let remote = root.join("remote.git");
        let out = Command::new(PROGRAM)
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .output()
            .unwrap();
        assert!(out.status.success());
        remote
    }

    /// The remote's `main` history, newest first, as commit subjects.
    fn history(remote: &Path) -> Vec<String> {
        let out = Command::new(PROGRAM)
            .arg("--git-dir")
            .arg(remote)
            .args(["log", "--format=%s", "main"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn sorted(mut hashes: Vec<SegmentHash>) -> Vec<SegmentHash> {
        hashes.sort_unstable();
        hashes
    }

    /// Two machines through one bare repository: alpha's segments reach
    /// beta and beta's reach alpha, each push is one commit naming its
    /// host, a push with nothing new makes no commit, and neither journal
    /// directory becomes a git worktree.
    #[test]
    fn git_between_two_machines_through_a_bare_repo() {
        if !have_git() {
            eprintln!("skipping: git is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let remote = bare(tmp.path());
        let alpha = Machine::new(tmp.path(), "alpha", &remote);
        let beta = Machine::new(tmp.path(), "beta", &remote);
        let a1 = import(&alpha.store, 1, "a:1");
        let a2 = import(&alpha.store, 2, "a:2");
        let b1 = import(&beta.store, 3, "b:1");

        // Alpha starts the empty remote: nothing to fetch, both go up.
        let report = alpha.sync(vec![b1], vec![a1, a2]);
        assert_eq!(
            sorted(report.transports[0].pushed.clone()),
            sorted(vec![a1, a2])
        );
        assert_eq!(report.still_missing, [b1]);
        assert_eq!(history(&remote), ["cashflow: add 2 segments from alpha"]);

        // Beta pushes its own and fetches a1 (not a2: its plan doesn't
        // want it).
        let report = beta.sync(vec![a1], vec![b1]);
        let t = &report.transports[0];
        assert_eq!(t.pushed, [b1]);
        assert_eq!(t.admit.admitted, [a1]);
        beta.store.read(a1).unwrap();
        assert!(!beta.store.contains(a2));

        // Alpha fetches b1 back; offering what the remote has sends nothing.
        let report = alpha.sync(vec![b1], vec![a1, a2]);
        assert!(report.transports[0].pushed.is_empty());
        assert_eq!(report.transports[0].admit.admitted, [b1]);
        alpha.store.read(b1).unwrap();
        assert_eq!(
            history(&remote),
            [
                "cashflow: add 1 segment from beta",
                "cashflow: add 2 segments from alpha"
            ]
        );
        for machine in [&alpha, &beta] {
            assert!(!machine.dir.journal().join(".git").exists());
        }
    }

    /// A push the remote rejects because another machine pushed since the
    /// fetch rebases onto theirs and lands: both commits are on the remote,
    /// and a segment both added counts as sent only by the first.
    #[test]
    fn a_raced_push_rebases_and_retries() {
        if !have_git() {
            eprintln!("skipping: git is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let remote = bare(tmp.path());
        let alpha = Machine::new(tmp.path(), "alpha", &remote);
        let beta = Machine::new(tmp.path(), "beta", &remote);
        let shared = import(&alpha.store, 1, "s:1");
        std::fs::create_dir_all(beta.store.path_of(shared).parent().unwrap()).unwrap();
        std::fs::copy(alpha.store.path_of(shared), beta.store.path_of(shared)).unwrap();
        let a1 = import(&alpha.store, 2, "a:1");
        let b1 = import(&beta.store, 3, "b:1");
        alpha.sync(vec![], vec![a1]);

        // Beta fetches and commits, then alpha pushes before beta does.
        let t = &beta.transport;
        let theirs = t.refresh().unwrap();
        let added = t.stage(&beta.store, &[shared, b1], &theirs).unwrap();
        assert_eq!(added, [shared, b1]);
        t.commit(added.len()).unwrap();
        alpha.sync(vec![], vec![shared]);

        let sent = t.push_retrying(added).unwrap();
        assert_eq!(sent, [b1]);
        assert_eq!(
            sorted(t.refresh().unwrap().into_iter().collect()),
            sorted(vec![shared, a1, b1])
        );
        assert_eq!(
            history(&remote),
            [
                "cashflow: add 2 segments from beta",
                "cashflow: add 1 segment from alpha",
                "cashflow: add 1 segment from alpha"
            ]
        );
    }

    /// Only files filed as the journal files them are segments.
    #[test]
    fn segment_paths() {
        let hash: SegmentHash = "ab".repeat(32).parse().unwrap();
        let path = JournalStore::relative_path(hash);
        assert_eq!(segment_at(path.to_str().unwrap()), Some(hash));
        assert_eq!(segment_at(&format!("{hash}.ledger")), None);
        assert_eq!(segment_at(&format!("cd/{hash}.ledger")), None);
        assert_eq!(segment_at("README.md"), None);
    }
}
