//! The rsync [`SegmentTransport`]: a copy of the journal anywhere rsync
//! reaches (`host:/path` over ssh, or a local directory such as a mounted
//! drive), moved with `rsync -a --ignore-existing`.
//!
//! Both directions name the exact files with `--files-from`, so a push
//! never carries the local `.trash/` or a temp file, and a fetch asks only
//! for what the plan wants. `--ignore-existing` is safe because a name is a
//! hash: a file the other side already has is the same bytes.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use ledgerdb::{JournalStore, SegmentHash};

use super::transport::{FetchReport, PushReport, SegmentTransport, TransportError};

/// The program this transport runs.
const PROGRAM: &str = "rsync";

/// rsync's "partial transfer": some named files were missing on the
/// sending side. For a fetch that is a remote lacking a wanted segment.
const PARTIAL: i32 = 23;

/// rsync's "some source files vanished" while it ran.
const VANISHED: i32 = 24;

/// A journal copy rsync can reach, as `[[sync.transport]] kind = "rsync"`
/// configures it.
#[derive(Clone, Debug)]
pub struct RsyncTransport {
    name: String,
    remote: String,
}

impl RsyncTransport {
    /// The transport called `name` for the journal at `remote`, anything
    /// rsync takes as a path: `host:/path`, `user@host:path`, `/mnt/usb/j`.
    pub fn new(name: String, remote: String) -> Self {
        RsyncTransport { name, remote }
    }

    /// `remote`, ending in `/` so rsync copies into it rather than under it.
    fn remote_dir(&self) -> OsString {
        dir_arg(self.remote.as_ref())
    }

    /// Copy `hashes` from the journal at `from` to the one at `to`, naming
    /// them on stdin, and return rsync's output.
    fn copy(
        &self,
        hashes: &[SegmentHash],
        from: OsString,
        to: OsString,
    ) -> Result<Output, TransportError> {
        let mut list = String::with_capacity(hashes.len() * 76);
        for &hash in hashes {
            list.push_str(&JournalStore::relative_path(hash).to_string_lossy());
            list.push('\n');
        }
        let mut child = Command::new(PROGRAM)
            .args([
                "-a",
                "--ignore-existing",
                "--files-from=-",
                "--out-format=%n",
            ])
            .arg("--")
            .arg(from)
            .arg(to)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => TransportError::NotInstalled {
                    program: PROGRAM,
                    kind: "rsync",
                },
                _ => TransportError::io(Path::new(PROGRAM), err),
            })?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // Feed the list from its own thread: rsync may fill stdout before it
        // has read all of stdin.
        std::thread::scope(|scope| {
            scope.spawn(move || {
                // A failed write shows up as rsync's own error.
                let _ = stdin.write_all(list.as_bytes());
            });
            child
                .wait_with_output()
                .map_err(|err| TransportError::io(Path::new(PROGRAM), err))
        })
    }
}

impl SegmentTransport for RsyncTransport {
    fn name(&self) -> &str {
        &self.name
    }

    /// rsync has no cheap listing: `None`, so every local segment is
    /// offered and `--ignore-existing` skips what the remote has.
    fn list_remote(&self) -> Result<Option<HashSet<SegmentHash>>, TransportError> {
        Ok(None)
    }

    fn push(
        &self,
        store: &JournalStore,
        hashes: &[SegmentHash],
    ) -> Result<PushReport, TransportError> {
        let out = self.copy(hashes, dir_arg(store.dir()), self.remote_dir())?;
        if !out.status.success() {
            return Err(failed(&out));
        }
        // `--out-format=%n` names each file it sent (and each shard it made).
        let sent = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                line.rsplit('/')
                    .next()?
                    .strip_suffix(".ledger")?
                    .parse()
                    .ok()
            })
            .collect();
        Ok(PushReport { sent })
    }

    fn fetch(&self, want: &[SegmentHash], staging: &Path) -> Result<FetchReport, TransportError> {
        let out = self.copy(want, self.remote_dir(), dir_arg(staging))?;
        // A wanted file the remote lacks makes a partial transfer, not a
        // failure: the report leaves it out.
        if !out.status.success() && !matches!(out.status.code(), Some(PARTIAL | VANISHED)) {
            return Err(failed(&out));
        }
        let staged = want
            .iter()
            .copied()
            .filter(|&hash| staging.join(JournalStore::relative_path(hash)).is_file())
            .collect();
        Ok(FetchReport { staged })
    }
}

/// `dir` as an rsync argument ending in `/`.
fn dir_arg(dir: &Path) -> OsString {
    let mut arg = dir.as_os_str().to_owned();
    if !arg.to_string_lossy().ends_with('/') {
        arg.push("/");
    }
    arg
}

/// rsync's failure, with its stderr.
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

    /// Whether `rsync` runs here; the e2e skips when it doesn't.
    fn have_rsync() -> bool {
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

    /// Two data dirs and a hub directory on localhost: alpha pushes its
    /// segments (not its trash), beta fetches the one it wants plus one
    /// the hub lacks, and pushes its own back. Skips without rsync.
    #[test]
    fn rsync_between_two_dirs_through_a_hub() {
        if !have_rsync() {
            eprintln!("skipping: rsync is not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let alpha = CashflowDir::new(tmp.path().join("alpha"));
        let beta = CashflowDir::new(tmp.path().join("beta"));
        let hub = tmp.path().join("hub");
        let hub_store = JournalStore::new(&hub);
        let (alpha_store, beta_store) = (journal::store(&alpha), journal::store(&beta));
        let a1 = import(&alpha_store, 1, "a:1");
        let a2 = import(&alpha_store, 2, "a:2");
        let gone = import(&alpha_store, 3, "a:3");
        alpha_store.trash(gone).unwrap();
        let b1 = import(&beta_store, 4, "b:1");
        let never = import(&JournalStore::new(tmp.path().join("nowhere")), 5, "n:1");

        let transports: Vec<Box<dyn SegmentTransport>> = vec![Box::new(RsyncTransport::new(
            "hub".to_owned(),
            hub.to_string_lossy().into_owned(),
        ))];
        let report = run(
            &transports,
            &alpha_store,
            &alpha.staging(),
            &SyncJob {
                fetch: vec![],
                push: vec![a1, a2],
            },
            false,
        );
        assert!(!report.has_errors(), "{report:?}");
        let mut pushed = report.transports[0].pushed.clone();
        pushed.sort_unstable();
        let mut expect = vec![a1, a2];
        expect.sort_unstable();
        assert_eq!(pushed, expect);
        let mut on_hub = hub_store.list().unwrap();
        on_hub.sort_unstable();
        assert_eq!(on_hub, expect);
        assert!(!hub.join(".trash").exists());

        let report = run(
            &transports,
            &beta_store,
            &beta.staging(),
            &SyncJob {
                fetch: vec![a1, never],
                push: vec![b1],
            },
            false,
        );
        assert!(!report.has_errors(), "{report:?}");
        let t = &report.transports[0];
        assert_eq!(t.fetched, [a1]);
        assert_eq!(t.admit.admitted, [a1]);
        assert_eq!(t.pushed, [b1]);
        assert_eq!(report.still_missing, [never]);
        beta_store.read(a1).unwrap();
        assert!(!beta_store.contains(a2));
        hub_store.read(b1).unwrap();

        // Pushing again sends nothing: the hub has it all.
        let report = run(
            &transports,
            &alpha_store,
            &alpha.staging(),
            &SyncJob {
                fetch: vec![],
                push: vec![a1, a2],
            },
            false,
        );
        assert!(report.transports[0].pushed.is_empty(), "{report:?}");
    }
}
