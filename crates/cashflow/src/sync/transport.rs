//! The seam journal segments move between machines through: a
//! [`SegmentTransport`] per remote copy of the journal, built from
//! `config.toml`'s `[[sync.transport]]` by [`transports`], and [`run`],
//! which fetches what the [`Plan`] says is missing and pushes what the
//! remotes lack.
//!
//! **A transport never writes into the journal directory.** It fetches into
//! `staging/<name>/`, and [`JournalStore::admit_from`] checks every staged
//! file's hash and header before filing it, so a broken or hostile remote
//! can at worst fail to deliver, never put wrong bytes in the journal. Sync
//! is a union of files whose names are their hashes, so there is nothing to
//! conflict.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use ledgerdb::{AdmitReport, JournalStore, SegmentHash, StoreError};

use super::Plan;
use super::git::GitTransport;
use super::rsync::RsyncTransport;
use crate::CashflowDir;
use crate::config::{Config, TransportConfig};
use crate::journal::SegmentSummary;

/// The `kind`s [`transports`] can build.
pub const KINDS: &[&str] = &["rsync", "git"];

/// One way of moving journal segments between machines.
///
/// `Send` so a caller can hand the transports to a blocking thread: every
/// method blocks on the network or a child process.
pub trait SegmentTransport: Send {
    /// What `sync --transport` and the reports call it; also its directory
    /// under `staging/`.
    fn name(&self) -> &str;

    /// The hashes the remote holds, if it can list them cheaply. rsync
    /// can't, and returns `None`: [`run`] then offers it every local
    /// segment and lets it skip what it has.
    fn list_remote(&self) -> Result<Option<HashSet<SegmentHash>>, TransportError>;

    /// Make the remote hold these local segments.
    fn push(
        &self,
        store: &JournalStore,
        hashes: &[SegmentHash],
    ) -> Result<PushReport, TransportError>;

    /// Fetch these segments into `staging`, filed as
    /// [`JournalStore::relative_path`] names them, and never into the
    /// journal directory. A wanted segment the remote lacks is left out of
    /// the report, not an error.
    fn fetch(&self, want: &[SegmentHash], staging: &Path) -> Result<FetchReport, TransportError>;
}

/// What [`SegmentTransport::push`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PushReport {
    /// The segments the remote didn't have and now does.
    pub sent: Vec<SegmentHash>,
}

/// What [`SegmentTransport::fetch`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FetchReport {
    /// The wanted segments now in staging, not yet checked.
    pub staged: Vec<SegmentHash>,
}

/// Why a transport, or one step of [`run`] on it, failed.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The program a transport shells out to isn't installed.
    #[error(
        "{program} is not on PATH: install it, or take the `{kind}` transport out of config.toml"
    )]
    NotInstalled {
        /// The program, `rsync`.
        program: &'static str,
        /// The transport kind that needs it.
        kind: &'static str,
    },
    /// The program ran and failed.
    #[error("{program} exited with {status}: {stderr}")]
    Failed {
        /// The program.
        program: &'static str,
        /// How it exited.
        status: std::process::ExitStatus,
        /// What it printed to stderr, trimmed.
        stderr: String,
    },
    /// Reading or writing a local file or directory failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What failed.
        source: io::Error,
    },
    /// The journal store failed to admit or read.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl TransportError {
    /// An [`Self::Io`] about `path`.
    pub fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

/// Why [`transports`] couldn't build the configured transports.
#[derive(Debug, thiserror::Error)]
pub enum TransportConfigError {
    /// `config.toml` has no `[[sync.transport]]`.
    #[error(
        "config.toml has no [[sync.transport]]; add one, e.g.\n\n    [[sync.transport]]\n    kind = \"rsync\"\n    remote = \"host:/path/to/journal\"\n\nor kind = \"git\" with remote = a git URL"
    )]
    NoTransports,
    /// A transport's `kind` isn't one this build has.
    #[error("transport {name}: unknown kind {kind:?} (known kinds: {})", KINDS.join(", "))]
    UnknownKind {
        /// The transport's name.
        name: String,
        /// The kind it gave.
        kind: String,
    },
    /// A transport lacks a key its kind needs.
    #[error("transport {name}: kind = {kind:?} needs `{key}`")]
    Missing {
        /// The transport's name.
        name: String,
        /// Its kind.
        kind: String,
        /// The missing key.
        key: &'static str,
    },
    /// A name that can't be a directory under `staging/`.
    #[error(
        "transport name {0:?} must be letters, digits, '.', '-' or '_', and not start with '.'"
    )]
    BadName(String),
    /// Two transports share a name.
    #[error("two transports are named {0:?}: give one a different `name`")]
    DuplicateName(String),
    /// `--transport` named none of the configured transports.
    #[error("no transport named {name:?} (configured: {})", configured.join(", "))]
    NotConfigured {
        /// The name asked for.
        name: String,
        /// The names config.toml has.
        configured: Vec<String>,
    },
}

/// The transports `config` lists, in order, or only the one named `only`.
/// Each is built by a match on its `kind`; there is no registry. A
/// transport that keeps state of its own (git's clone) keeps it under
/// `dir`'s [`CashflowDir::sync`], and `host` names this machine to the
/// remote where a transport records it (git's commit messages).
pub fn transports(
    config: &Config,
    dir: &CashflowDir,
    host: &str,
    only: Option<&str>,
) -> Result<Vec<Box<dyn SegmentTransport>>, TransportConfigError> {
    let configured = config.sync.as_ref().map_or(&[][..], |s| &s.transport[..]);
    if configured.is_empty() {
        return Err(TransportConfigError::NoTransports);
    }
    let mut names = Vec::with_capacity(configured.len());
    for entry in configured {
        let name = name_of(entry);
        check_name(name)?;
        if names.contains(&name) {
            return Err(TransportConfigError::DuplicateName(name.to_owned()));
        }
        names.push(name);
    }
    if let Some(only) = only
        && !names.contains(&only)
    {
        return Err(TransportConfigError::NotConfigured {
            name: only.to_owned(),
            configured: names.iter().map(|n| (*n).to_owned()).collect(),
        });
    }
    configured
        .iter()
        .filter(|entry| only.is_none_or(|only| name_of(entry) == only))
        .map(|entry| build(entry, dir, host))
        .collect()
}

/// A transport's name: its `name`, else its `kind`.
fn name_of(entry: &TransportConfig) -> &str {
    entry.name.as_deref().unwrap_or(&entry.kind)
}

/// Whether `name` is safe as one directory under `staging/`: never empty,
/// `.`/`..`, hidden, or holding a separator.
fn check_name(name: &str) -> Result<(), TransportConfigError> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(TransportConfigError::BadName(name.to_owned()))
    }
}

/// Build one configured transport.
fn build(
    entry: &TransportConfig,
    dir: &CashflowDir,
    host: &str,
) -> Result<Box<dyn SegmentTransport>, TransportConfigError> {
    let name = name_of(entry).to_owned();
    let missing = |key| TransportConfigError::Missing {
        name: name.clone(),
        kind: entry.kind.clone(),
        key,
    };
    match entry.kind.as_str() {
        "rsync" => {
            let remote = entry.remote.clone().ok_or_else(|| missing("remote"))?;
            Ok(Box::new(RsyncTransport::new(name, remote)))
        }
        "git" => {
            let remote = entry.remote.clone().ok_or_else(|| missing("remote"))?;
            let clone = dir.sync().join("git").join(&name);
            Ok(Box::new(GitTransport::new(
                name,
                remote,
                clone,
                host.to_owned(),
            )))
        }
        _ => Err(TransportConfigError::UnknownKind {
            name,
            kind: entry.kind.clone(),
        }),
    }
}

/// What [`run`] moves, copied out of a [`Plan`] so it outlives the
/// transaction the plan borrows: a sync blocks on the network for as long
/// as it takes, and must not hold the txn (or an await) across it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncJob {
    /// The segments to fetch: the plan's `missing_locally`, which never
    /// holds a retired one.
    pub fetch: Vec<SegmentHash>,
    /// The local segments to offer each remote: every readable one but
    /// those the vault retires.
    pub push: Vec<SegmentHash>,
}

impl SyncJob {
    /// The job for `plan`, made from `local`, the listing it compared.
    pub fn new(plan: &Plan<'_>, local: &[SegmentSummary]) -> Self {
        let retired: HashSet<SegmentHash> = plan.retired_here.iter().map(|s| s.hash).collect();
        SyncJob {
            fetch: plan.missing_locally.iter().map(|m| m.hash).collect(),
            push: local
                .iter()
                .map(|s| s.hash)
                .filter(|hash| !retired.contains(hash))
                .collect(),
        }
    }
}

/// What [`run`] did with one transport.
#[derive(Debug, Default)]
pub struct TransportReport {
    /// The transport's name.
    pub name: String,
    /// The segments it didn't have and now does; dry run, the ones it would
    /// be offered (for rsync, every local segment).
    pub pushed: Vec<SegmentHash>,
    /// The wanted segments it staged; dry run, the ones it would be asked
    /// for.
    pub fetched: Vec<SegmentHash>,
    /// What the store did with the staged files. Rejected files stay in
    /// `staging/<name>/` until the next sync, to be looked at.
    pub admit: AdmitReport,
    /// What failed: the push, the fetch, or both. Each is tried whatever
    /// the other did, and [`run`] goes on to the next transport.
    pub errors: Vec<TransportError>,
}

/// What [`run`] did.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Whether nothing was moved, only reported.
    pub dry_run: bool,
    /// One per transport, in the order they ran.
    pub transports: Vec<TransportReport>,
    /// Wanted segments no transport delivered intact.
    pub still_missing: Vec<SegmentHash>,
}

impl SyncReport {
    /// Every segment admitted into the journal, from any transport.
    pub fn admitted(&self) -> impl Iterator<Item = SegmentHash> + '_ {
        self.transports
            .iter()
            .flat_map(|t| t.admit.admitted.iter().copied())
    }

    /// Whether any transport failed a step.
    pub fn has_errors(&self) -> bool {
        self.transports.iter().any(|t| !t.errors.is_empty())
    }
}

/// Run `job` over `transports` in order: push each the local segments it
/// lacks, then fetch from it the wanted segments still missing into
/// `staging_root/<name>/` and admit them. A segment admitted from one
/// remote is offered to the remotes after it, so one run spreads it
/// everywhere.
///
/// Pushing first is what lets a sync start a brand-new remote: rsync
/// creates the remote directory on the way in, and a fetch from one that
/// doesn't exist yet fails.
///
/// A transport that fails is reported and the rest still run. Blocking:
/// call it off the UI thread (and off an async executor's worker). With
/// `dry_run` nothing is fetched, admitted or pushed, though a transport may
/// still list its remote.
///
/// Publishing manifests for what was admitted is the caller's: this module
/// never touches the vault.
#[profiling::function]
pub fn run(
    transports: &[Box<dyn SegmentTransport>],
    store: &JournalStore,
    staging_root: &Path,
    job: &SyncJob,
    dry_run: bool,
) -> SyncReport {
    let mut want: Vec<SegmentHash> = job
        .fetch
        .iter()
        .copied()
        .filter(|hash| !store.contains(*hash))
        .collect();
    let mut offer = job.push.clone();
    let mut report = SyncReport {
        dry_run,
        ..SyncReport::default()
    };
    for transport in transports {
        let mut done = TransportReport {
            name: transport.name().to_owned(),
            ..TransportReport::default()
        };
        let step = Step {
            transport: transport.as_ref(),
            store,
            staging_root,
            dry_run,
        };
        if let Err(err) = step.push(&offer, &mut done) {
            done.errors.push(err);
        }
        if let Err(err) = step.fetch(&mut want, &mut done) {
            done.errors.push(err);
        }
        offer.extend(done.admit.admitted.iter().copied());
        report.transports.push(done);
    }
    report.still_missing = want;
    report
}

/// One transport's turn in [`run`].
struct Step<'a> {
    transport: &'a dyn SegmentTransport,
    store: &'a JournalStore,
    staging_root: &'a Path,
    dry_run: bool,
}

impl Step<'_> {
    /// Push what of `offer` the remote lacks into `done.pushed`.
    fn push(
        &self,
        offer: &[SegmentHash],
        done: &mut TransportReport,
    ) -> Result<(), TransportError> {
        let remote = self.transport.list_remote()?;
        let lacking: Vec<SegmentHash> = offer
            .iter()
            .copied()
            .filter(|hash| remote.as_ref().is_none_or(|have| !have.contains(hash)))
            .collect();
        if self.dry_run {
            done.pushed = lacking;
        } else if !lacking.is_empty() {
            done.pushed = self.transport.push(self.store, &lacking)?.sent;
        }
        Ok(())
    }

    /// Fetch and admit what's in `want`, dropping from it what arrives.
    fn fetch(
        &self,
        want: &mut Vec<SegmentHash>,
        done: &mut TransportReport,
    ) -> Result<(), TransportError> {
        if want.is_empty() {
            return Ok(());
        }
        if self.dry_run {
            done.fetched = want.clone();
            return Ok(());
        }
        let staging = self.staging_root.join(self.transport.name());
        fresh_dir(&staging)?;
        done.fetched = self.transport.fetch(want, &staging)?.staged;
        done.admit = self.store.admit_from(&staging)?;
        clear_admitted(&staging, &done.admit);
        want.retain(|hash| !self.store.contains(*hash));
        Ok(())
    }
}

/// Empty `dir`, creating it if needed: last run's rejected files go, so a
/// fixed remote's good copy isn't shadowed by a bad one left over.
fn fresh_dir(dir: &Path) -> Result<(), TransportError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(TransportError::io(dir, err)),
    }
    std::fs::create_dir_all(dir).map_err(|err| TransportError::io(dir, err))
}

/// Delete the staged copies of what the store now holds, keeping the
/// rejected ones. Best effort: a copy left behind goes on the next run.
fn clear_admitted(staging: &Path, admit: &AdmitReport) {
    for &hash in admit.admitted.iter().chain(&admit.present) {
        let _ = std::fs::remove_file(staging.join(JournalStore::relative_path(hash)));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;
    use crate::config::SyncConfig;
    use crate::{CashflowDir, journal};
    use ledgerdb::{Journal, SegmentHeader};

    /// A remote journal held in memory: hash → bytes. `tampered` hashes are
    /// served with other bytes, as a broken or hostile remote would.
    #[derive(Default)]
    struct Fake {
        name: String,
        files: RefCell<HashMap<SegmentHash, Vec<u8>>>,
        tampered: HashSet<SegmentHash>,
    }

    impl Fake {
        fn new(name: &str) -> Self {
            Fake {
                name: name.to_owned(),
                ..Fake::default()
            }
        }
    }

    impl SegmentTransport for Fake {
        fn name(&self) -> &str {
            &self.name
        }

        fn list_remote(&self) -> Result<Option<HashSet<SegmentHash>>, TransportError> {
            Ok(Some(self.files.borrow().keys().copied().collect()))
        }

        fn push(
            &self,
            store: &JournalStore,
            hashes: &[SegmentHash],
        ) -> Result<PushReport, TransportError> {
            let mut sent = Vec::new();
            for &hash in hashes {
                let path = store.path_of(hash);
                let bytes = std::fs::read(&path).map_err(|e| TransportError::io(&path, e))?;
                if self.files.borrow_mut().insert(hash, bytes).is_none() {
                    sent.push(hash);
                }
            }
            Ok(PushReport { sent })
        }

        fn fetch(
            &self,
            want: &[SegmentHash],
            staging: &Path,
        ) -> Result<FetchReport, TransportError> {
            let mut staged = Vec::new();
            for &hash in want {
                let Some(mut bytes) = self.files.borrow().get(&hash).cloned() else {
                    continue;
                };
                if self.tampered.contains(&hash) {
                    bytes.extend_from_slice(b"; tampered\n");
                }
                let path = staging.join(JournalStore::relative_path(hash));
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, bytes).map_err(|e| TransportError::io(&path, e))?;
                staged.push(hash);
            }
            Ok(FetchReport { staged })
        }
    }

    /// A data dir with its journal.
    struct Machine {
        _tmp: tempfile::TempDir,
        dir: CashflowDir,
        store: JournalStore,
    }

    impl Machine {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let dir = CashflowDir::new(tmp.path());
            let store = journal::store(&dir);
            Machine {
                _tmp: tmp,
                dir,
                store,
            }
        }

        fn import(&self, imported: u64, id: &str) -> SegmentHash {
            let header = SegmentHeader {
                source: "test".to_owned(),
                imported,
                layer: "cards".to_owned(),
            };
            let text = format!("2024-01-02 * Shop\n  ; id: {id}\n  a:b  -1 CAD\n  c:d\n");
            self.store
                .write_segment(&header, &Journal::parse(&text).journal)
                .unwrap()
        }

        fn run(
            &self,
            transports: &[Box<dyn SegmentTransport>],
            job: &SyncJob,
            dry_run: bool,
        ) -> SyncReport {
            run(transports, &self.store, &self.dir.staging(), job, dry_run)
        }
    }

    /// The bytes of `hash` as `machine` stores them.
    fn bytes_of(machine: &Machine, hash: SegmentHash) -> Vec<u8> {
        std::fs::read(machine.store.path_of(hash)).unwrap()
    }

    /// One remote between two machines: alpha pushes, beta fetches what the
    /// plan names and pushes its own back; a second run moves nothing.
    #[test]
    fn push_then_fetch_through_one_remote() {
        let alpha = Machine::new();
        let beta = Machine::new();
        let a1 = alpha.import(1, "a:1");
        let a2 = alpha.import(2, "a:2");
        let b1 = beta.import(3, "b:1");
        let hub: Vec<Box<dyn SegmentTransport>> = vec![Box::new(Fake::new("hub"))];

        let report = alpha.run(
            &hub,
            &SyncJob {
                fetch: vec![],
                push: vec![a1, a2],
            },
            false,
        );
        assert!(!report.has_errors());
        assert_eq!(report.transports[0].pushed.len(), 2);

        // Beta's plan says it lacks a1 (a2 is retired, so never asked for).
        let report = beta.run(
            &hub,
            &SyncJob {
                fetch: vec![a1],
                push: vec![b1],
            },
            false,
        );
        let t = &report.transports[0];
        assert_eq!(t.fetched, [a1]);
        assert_eq!(t.admit.admitted, [a1]);
        assert_eq!(t.pushed, [b1]);
        assert!(report.still_missing.is_empty());
        assert!(beta.store.contains(a1));
        assert!(!beta.store.contains(a2));
        assert_eq!(bytes_of(&beta, a1), bytes_of(&alpha, a1));
        // The admitted copy left staging.
        assert!(
            !beta
                .dir
                .staging()
                .join("hub")
                .join(JournalStore::relative_path(a1))
                .exists()
        );

        // Again: nothing to fetch, nothing the remote lacks.
        let report = beta.run(
            &hub,
            &SyncJob {
                fetch: vec![a1],
                push: vec![a1, b1],
            },
            false,
        );
        let t = &report.transports[0];
        assert!(t.fetched.is_empty() && t.pushed.is_empty());
    }

    /// A remote serving bytes that don't match their name gets nothing into
    /// the journal: the file is rejected and left in staging, the segment is
    /// still missing, and the next transport delivers the good copy.
    #[test]
    fn a_tampered_file_is_rejected_and_another_remote_fills_in() {
        let origin = Machine::new();
        let beta = Machine::new();
        let a1 = origin.import(1, "a:1");

        let mut bad = Fake::new("bad");
        bad.files.get_mut().insert(a1, bytes_of(&origin, a1));
        bad.tampered.insert(a1);
        let good = Fake::new("good");
        good.files.borrow_mut().insert(a1, bytes_of(&origin, a1));
        let job = SyncJob {
            fetch: vec![a1],
            push: vec![],
        };

        let only_bad: Vec<Box<dyn SegmentTransport>> = vec![Box::new(bad)];
        let report = beta.run(&only_bad, &job, false);
        let t = &report.transports[0];
        assert_eq!(t.fetched, [a1]);
        assert!(t.admit.admitted.is_empty());
        assert!(matches!(
            t.admit.rejected.as_slice(),
            [StoreError::HashMismatch { .. }]
        ));
        assert_eq!(report.still_missing, [a1]);
        assert!(!beta.store.contains(a1));
        assert!(beta.store.list().unwrap().is_empty());
        assert!(
            beta.dir
                .staging()
                .join("bad")
                .join(JournalStore::relative_path(a1))
                .is_file(),
            "a rejected file stays in staging to be looked at"
        );

        let both: Vec<Box<dyn SegmentTransport>> =
            vec![only_bad.into_iter().next().unwrap(), Box::new(good)];
        let report = beta.run(&both, &job, false);
        assert_eq!(report.transports[0].admit.rejected.len(), 1);
        assert_eq!(report.transports[1].admit.admitted, [a1]);
        assert!(report.still_missing.is_empty());
        assert_eq!(bytes_of(&beta, a1), bytes_of(&origin, a1));
        beta.store.read(a1).unwrap();
    }

    /// What one remote delivers is pushed on to the next, and the remote it
    /// came from isn't offered it back: its push ran before its fetch.
    #[test]
    fn admitted_segments_spread_to_later_remotes() {
        let origin = Machine::new();
        let beta = Machine::new();
        let a1 = origin.import(1, "a:1");
        let first = Fake::new("first");
        first.files.borrow_mut().insert(a1, bytes_of(&origin, a1));
        let transports: Vec<Box<dyn SegmentTransport>> =
            vec![Box::new(first), Box::new(Fake::new("second"))];

        let report = beta.run(
            &transports,
            &SyncJob {
                fetch: vec![a1],
                push: vec![],
            },
            false,
        );
        assert_eq!(report.transports[0].admit.admitted, [a1]);
        assert!(report.transports[0].pushed.is_empty());
        assert!(report.transports[1].fetched.is_empty());
        assert_eq!(report.transports[1].pushed, [a1]);
    }

    /// A dry run reports what would move and moves nothing.
    #[test]
    fn dry_run_moves_nothing() {
        let origin = Machine::new();
        let beta = Machine::new();
        let a1 = origin.import(1, "a:1");
        let b1 = beta.import(2, "b:1");
        let hub = Fake::new("hub");
        hub.files.borrow_mut().insert(a1, bytes_of(&origin, a1));
        let transports: Vec<Box<dyn SegmentTransport>> = vec![Box::new(hub)];

        let report = beta.run(
            &transports,
            &SyncJob {
                fetch: vec![a1],
                push: vec![b1],
            },
            true,
        );
        let t = &report.transports[0];
        assert_eq!(t.fetched, [a1]);
        assert_eq!(t.pushed, [b1]);
        assert!(!beta.store.contains(a1));
        assert!(!beta.dir.staging().join("hub").exists());
        assert_eq!(report.still_missing, [a1]);
    }

    /// Config: rsync builds, an unknown kind names the known ones, a
    /// missing remote, a bad or repeated name, and a `--transport` nothing
    /// is called are each an error.
    #[test]
    fn transports_from_config() {
        let config = |entries: Vec<TransportConfig>| Config {
            sync: Some(SyncConfig { transport: entries }),
            ..Config::default()
        };
        let rsync = |name: Option<&str>| TransportConfig {
            name: name.map(str::to_owned),
            kind: "rsync".to_owned(),
            remote: Some("host:/journal".to_owned()),
        };
        let dir = CashflowDir::new("/data/cashflow");
        let transports = |c: &Config, only| transports(c, &dir, "here", only);
        let err = |c: &Config, only| transports(c, only).err().unwrap().to_string();

        assert!(matches!(
            transports(&Config::default(), None),
            Err(TransportConfigError::NoTransports)
        ));
        let two = config(vec![rsync(None), rsync(Some("backup"))]);
        let names = |only| -> Vec<String> {
            transports(&two, only)
                .unwrap()
                .iter()
                .map(|t| t.name().to_owned())
                .collect()
        };
        assert_eq!(names(None), ["rsync", "backup"]);
        assert_eq!(names(Some("backup")), ["backup"]);
        let msg = err(&two, Some("nope"));
        assert!(msg.contains("rsync, backup"), "{msg}");

        let blossom = config(vec![TransportConfig {
            kind: "blossom".to_owned(),
            ..rsync(None)
        }]);
        let msg = err(&blossom, None);
        assert!(
            msg.contains("blossom") && msg.contains("known kinds: rsync, git"),
            "{msg}"
        );

        let git = config(vec![TransportConfig {
            kind: "git".to_owned(),
            ..rsync(Some("history"))
        }]);
        assert_eq!(transports(&git, None).unwrap()[0].name(), "history");

        let no_remote = config(vec![TransportConfig {
            remote: None,
            ..rsync(None)
        }]);
        assert!(err(&no_remote, None).contains("`remote`"));
        for bad in ["", "..", ".hidden", "a/b"] {
            assert!(matches!(
                transports(&config(vec![rsync(Some(bad))]), None),
                Err(TransportConfigError::BadName(_))
            ));
        }
        assert!(matches!(
            transports(&config(vec![rsync(None), rsync(None)]), None),
            Err(TransportConfigError::DuplicateName(_))
        ));

        let parsed = Config::parse(
            "[[sync.transport]]\nkind = \"rsync\"\nremote = \"h:/j\"\n\n[[sync.transport]]\nname = \"usb\"\nkind = \"rsync\"\nremote = \"/mnt/usb/journal\"\n",
        )
        .unwrap();
        assert_eq!(parsed.sync.as_ref().unwrap().transport.len(), 2);
        assert_eq!(Config::parse(&parsed.to_toml()).unwrap(), parsed);
        assert!(Config::parse("[[sync.transport]]\nkind = \"rsync\"\nremot = \"x\"\n").is_err());
    }
}
