//! Journal sync: which segments this machine lacks, and which it has that
//! the vault doesn't know about.
//!
//! Segments move between machines as files, never through nostr; the
//! vault's [manifests](crate::manifest) are the index of what exists and
//! which host has it. [`Plan`] compares that index with the local journal.
//! `cashflow journal status` prints it, and the app warns with it so its
//! numbers are never silently partial.
//!
//! The files themselves move through a [`SegmentTransport`], one per remote
//! copy of the journal (`[[sync.transport]]` in `config.toml`, built by
//! [`transports`]). [`run`] fetches what a [`SyncJob`] made from the plan
//! lists as missing, into staging, admits it through the journal store's
//! hash check, and pushes each remote what it lacks: `cashflow sync`.

mod git;
mod rsync;
mod transport;

pub use git::GitTransport;
pub use rsync::RsyncTransport;
pub use transport::{
    FetchReport, KINDS, PushReport, SegmentTransport, SyncJob, SyncReport, TransportConfigError,
    TransportError, TransportReport, run, transports,
};

use ledgerdb::SegmentHash;

use crate::journal::SegmentSummary;
use crate::manifest::{self, Manifest, StoredManifest};

/// A segment the vault knows about that the local journal lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missing<'a> {
    /// The segment.
    pub hash: SegmentHash,
    /// Its manifest, which names the host that has it.
    pub manifest: &'a Manifest<'a>,
}

/// The [`Missing`] segments one host has, from [`Plan::missing_by_host`].
#[derive(Clone, Copy, Debug)]
pub struct HostMissing<'p, 'a> {
    /// The host that published their manifests.
    pub host: &'a str,
    /// The segments, oldest import first.
    pub segments: &'p [Missing<'a>],
}

/// The vault's manifests compared with the local journal.
#[derive(Debug, Default)]
pub struct Plan<'a> {
    /// Live segments the vault knows about that the journal lacks, by host,
    /// then oldest import first, then hash.
    pub missing_locally: Vec<Missing<'a>>,
    /// Local segments the vault has no manifest for, as listed:
    /// `cashflow journal publish` writes theirs.
    pub unpublished: Vec<&'a SegmentSummary>,
    /// Local segments whose manifest retires them. Loads leave them out
    /// already; `cashflow journal rm` moves the file to the trash here too.
    pub retired_here: Vec<&'a SegmentSummary>,
}

impl<'a> Plan<'a> {
    /// Compare `manifests` (in hash order, as [`manifest::load`] returns
    /// them) with `local`, the journal's readable segments. A manifest whose
    /// id isn't a hash is ignored.
    pub fn new(manifests: &'a [StoredManifest<'a>], local: &'a [SegmentSummary]) -> Self {
        let mut plan = Plan::default();
        for summary in local {
            match manifest::find(manifests, summary.hash) {
                None => plan.unpublished.push(summary),
                Some(m) if m.body.retired => plan.retired_here.push(summary),
                Some(_) => {}
            }
        }
        // `local` is in import order: sort its hashes to look them up.
        let mut have: Vec<SegmentHash> = local.iter().map(|s| s.hash).collect();
        have.sort_unstable();
        plan.missing_locally = manifests
            .iter()
            .filter(|m| !m.body.retired)
            .filter_map(|m| {
                let hash = manifest::hash_of(m)?;
                have.binary_search(&hash).is_err().then_some(Missing {
                    hash,
                    manifest: &m.body,
                })
            })
            .collect();
        plan.missing_locally.sort_by(|a, b| {
            (a.manifest.host, a.manifest.imported, a.hash).cmp(&(
                b.manifest.host,
                b.manifest.imported,
                b.hash,
            ))
        });
        plan
    }

    /// [`Self::missing_locally`] grouped by the host that has them, hosts in
    /// name order.
    pub fn missing_by_host(&self) -> impl Iterator<Item = HostMissing<'_, 'a>> {
        self.missing_locally
            .chunk_by(|a, b| a.manifest.host == b.manifest.host)
            .map(|segments| HostMissing {
                host: segments[0].manifest.host,
                segments,
            })
    }

    /// Whether the journal and the vault agree: nothing missing, unpublished
    /// or left to retire.
    pub fn is_in_sync(&self) -> bool {
        self.missing_locally.is_empty()
            && self.unpublished.is_empty()
            && self.retired_here.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use crate::vault::Vault;
    use crate::{Books, CashflowDir, journal, kinds};
    use ledgerdb::{Edits, Journal, JournalStore, LayerFilter, SegmentHeader};
    use nostrdb::{Filter, Ndb, Transaction};

    const SECRET: [u8; 32] = [8u8; 32];

    /// One machine: its own journal dir and host name. Both share one ndb,
    /// standing in for two caches the relay has synced.
    struct Host {
        _tmp: tempfile::TempDir,
        store: JournalStore,
        name: &'static str,
    }

    impl Host {
        fn new(name: &'static str) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let store = journal::store(&CashflowDir::new(tmp.path()));
            Host {
                _tmp: tmp,
                store,
                name,
            }
        }

        /// Write a one-transaction segment of record `id`, filed under the
        /// id's source.
        fn import(&self, imported: u64, id: &str, qty: &str) -> SegmentHash {
            let header = SegmentHeader {
                source: id.split(':').next().unwrap().to_owned(),
                imported,
                layer: "cards".to_owned(),
            };
            let text = format!("2024-01-02 * Shop\n  ; id: {id}\n  a:b  -{qty} CAD\n  c:d\n");
            self.store
                .write_segment(&header, &Journal::parse(&text).journal)
                .unwrap()
        }

        /// Publish the manifest of every local segment the vault lacks, as
        /// `journal publish` does.
        async fn publish(&self, vault: &Vault, ndb: &Ndb) -> usize {
            let listing = journal::list(&self.store).unwrap();
            let sub = ndb
                .subscribe(&[Filter::new().kinds([kinds::SEGMENT as u64]).build()])
                .unwrap();
            let written = {
                let txn = Transaction::new(ndb).unwrap();
                let manifests = manifest::load(ndb, &txn, vault.author());
                let plan = Plan::new(&manifests, &listing.segments);
                for summary in &plan.unpublished {
                    let body = Manifest::of(summary, self.name);
                    manifest::write(vault, ndb, summary.hash, &body, 0).unwrap();
                }
                plan.unpublished.len()
            };
            if written > 0 {
                ndb.wait_for_all_notes_within(sub, written as u32, INGEST_TIMEOUT)
                    .await
                    .unwrap();
            }
            written
        }
    }

    /// Two hosts, one vault: each sees what the other published as missing
    /// locally, grouped under the other's name; a segment both have is in
    /// sync; and one host retiring a segment drops it from the other's load
    /// although the other still has the file.
    #[tokio::test]
    async fn two_hosts_see_each_others_segments_and_retirements() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let alpha = Host::new("alpha");
        let beta = Host::new("beta");

        let a1 = alpha.import(10, "alpha:1", "1");
        let a2 = alpha.import(20, "alpha:2", "2");
        let b1 = beta.import(15, "beta:1", "3");
        // Both have this one, as after a sync.
        let shared = alpha.import(30, "shared:1", "4");
        assert_eq!(beta.import(30, "shared:1", "4"), shared);

        // Nothing published yet: each host's segments are all unpublished.
        {
            let txn = Transaction::new(ndb).unwrap();
            let manifests = manifest::load(ndb, &txn, vault.author());
            let listing = journal::list(&alpha.store).unwrap();
            let plan = Plan::new(&manifests, &listing.segments);
            assert_eq!(plan.unpublished.len(), 3);
            assert!(plan.missing_locally.is_empty());
        }

        assert_eq!(alpha.publish(&vault, ndb).await, 3);
        // `shared` is already published, by alpha.
        assert_eq!(beta.publish(&vault, ndb).await, 1);
        assert_eq!(alpha.publish(&vault, ndb).await, 0);

        {
            let txn = Transaction::new(ndb).unwrap();
            let manifests = manifest::load(ndb, &txn, vault.author());
            let alpha_ls = journal::list(&alpha.store).unwrap();
            let plan = Plan::new(&manifests, &alpha_ls.segments);
            assert!(plan.unpublished.is_empty());
            let missing: Vec<_> = plan.missing_locally.iter().map(|m| m.hash).collect();
            assert_eq!(missing, [b1]);
            let groups: Vec<_> = plan.missing_by_host().collect();
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].host, "beta");
            assert_eq!(groups[0].segments[0].manifest.source, "beta");

            let beta_ls = journal::list(&beta.store).unwrap();
            let plan = Plan::new(&manifests, &beta_ls.segments);
            let missing: Vec<_> = plan.missing_locally.iter().map(|m| m.hash).collect();
            assert_eq!(missing, [a1, a2], "oldest import first");
            assert!(plan.missing_by_host().all(|g| g.host == "alpha"));
            assert!(!plan.is_in_sync());
        }

        // Alpha retires the shared segment: rewrites its manifest retired
        // and trashes its own copy, as `journal rm` does.
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::SEGMENT as u64]).build()])
            .unwrap();
        {
            let txn = Transaction::new(ndb).unwrap();
            let manifests = manifest::load(ndb, &txn, vault.author());
            let stored = manifest::find(&manifests, shared).unwrap();
            let retire = stored.body.retire();
            manifest::write(&vault, ndb, shared, &retire, stored.created_at).unwrap();
        }
        alpha.store.trash(shared).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let manifests = manifest::load(ndb, &txn, vault.author());
        let retired = manifest::retired(&manifests);
        assert_eq!(retired, HashSet::from([shared]));

        // Beta still has the file: the plan says to retire it here, and the
        // books leave it out regardless.
        let beta_ls = journal::list(&beta.store).unwrap();
        let plan = Plan::new(&manifests, &beta_ls.segments);
        let here: Vec<_> = plan.retired_here.iter().map(|s| s.hash).collect();
        assert_eq!(here, [shared]);
        assert!(plan.missing_locally.iter().all(|m| m.hash != shared));
        // Alpha, without the file, isn't told to fetch it.
        let alpha_ls = journal::list(&alpha.store).unwrap();
        let plan = Plan::new(&manifests, &alpha_ls.segments);
        assert!(plan.missing_locally.iter().all(|m| m.hash != shared));
        assert!(plan.retired_here.is_empty());

        let edits = Edits::default();
        let all = Books::load(
            &beta.store,
            LayerFilter::All,
            &HashSet::new(),
            &edits,
            "CAD",
        )
        .unwrap();
        assert!(all.ledger().find("shared:1").is_some());
        let books = Books::load(&beta.store, LayerFilter::All, &retired, &edits, "CAD").unwrap();
        assert!(books.ledger().find("shared:1").is_none());
        assert!(books.ledger().find("beta:1").is_some());
        assert_eq!(books.report.skipped, [shared]);
    }
}
