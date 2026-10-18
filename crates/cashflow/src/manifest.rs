//! Segment manifests: what the vault knows about each journal segment.
//!
//! The journal's segments move between machines by file sync, outside
//! nostr (see [`crate::sync`]); the vault says which ones exist. Each
//! segment gets one manifest record, kind [`kinds::SEGMENT`],
//! `d = seg:<sha256>` (the prefix keeps nostrdb from storing the 64-hex
//! hash as an id a string read misses), its fields as tags:
//!
//! - `source`, `layer`, `imported`: the segment's header;
//! - `first`, `last`: its earliest and latest transaction or price date,
//!   absent for an empty segment;
//! - `txns`, `prices`, `bytes`: what it holds and how big the file is;
//! - `host`: the machine that published it, so a machine missing the file
//!   knows where to fetch it from;
//! - `retired`: `"1"` once `cashflow journal rm` retired it.
//!
//! `cashflow import` publishes a manifest as soon as it has written a
//! segment, and `cashflow journal publish` backfills the ones a segment
//! written before then, or offline, lacks. Comparing the manifests with the
//! local journal is [`crate::sync::Plan`].
//!
//! ## Retiring
//!
//! A journal is write-once and its sync is a union of files, so a delete
//! can't travel as a missing file. It travels as the manifest instead:
//! retiring rewrites it with `["retired", "1"]`, and every reader loads the
//! journal without the [`retired`] segments, whether or not the file is
//! still there ([`ledgerdb::Ledger::load_without`]).
//!
//! A reader asks only for the tags it knows, so a field a later writer adds
//! never hides a manifest; there is deliberately no schema-version check,
//! since a manifest that didn't read would look unpublished and be
//! backfilled over.

use std::collections::HashSet;

use ledgerdb::{Date, JournalStore, SegmentHash, StoreError};
use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use crate::journal::{self, SegmentSummary};
use crate::kinds;
use crate::vault::{self, Record, Stored, TagWriter, Vault, VaultError, Written};

/// One segment as the vault describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest<'a> {
    /// The source the segment's batch came from.
    pub source: &'a str,
    /// The layer it belongs to.
    pub layer: &'a str,
    /// When it was imported, in unix seconds.
    pub imported: u64,
    /// Its earliest transaction or price date; `None` when it is empty.
    pub first: Option<Date>,
    /// Its latest.
    pub last: Option<Date>,
    /// Transactions in it.
    pub txns: u64,
    /// `P` directives in it.
    pub prices: u64,
    /// The file's size.
    pub bytes: u64,
    /// The host that published the manifest, which has the file.
    pub host: &'a str,
    /// Whether the segment is retired: every reader leaves it out.
    pub retired: bool,
}

/// A manifest as read back from the vault; its id is the segment's hash.
pub type StoredManifest<'a> = Stored<'a, Manifest<'a>>;

impl<'a> Manifest<'a> {
    /// The live manifest of the local segment `summary`, published from
    /// `host`.
    pub fn of(summary: &'a SegmentSummary, host: &'a str) -> Self {
        Manifest {
            source: &summary.header.source,
            layer: &summary.header.layer,
            imported: summary.header.imported,
            first: summary.first,
            last: summary.last,
            txns: summary.txns as u64,
            prices: summary.prices as u64,
            bytes: summary.bytes,
            host,
            retired: false,
        }
    }

    /// This manifest, retired.
    pub fn retire(self) -> Self {
        Manifest {
            retired: true,
            ..self
        }
    }
}

impl<'a> Record<'a> for Manifest<'a> {
    const KIND: u32 = kinds::SEGMENT;
    const PREFIX: &'static str = "seg";

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        tags = tags
            .tag("source", &[self.source])
            .tag("layer", &[self.layer])
            .tag("imported", &[&self.imported.to_string()]);
        if let (Some(first), Some(last)) = (self.first, self.last) {
            tags = tags
                .tag("first", &[&first.to_string()])
                .tag("last", &[&last.to_string()]);
        }
        tags = tags
            .tag("txns", &[&self.txns.to_string()])
            .tag("prices", &[&self.prices.to_string()])
            .tag("bytes", &[&self.bytes.to_string()])
            .tag("host", &[self.host]);
        if self.retired {
            tags = tags.tag("retired", &["1"]);
        }
        tags
    }

    /// `None` without the header fields or the host; a missing count reads
    /// as 0.
    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        let number = |name| {
            vault::tag_value(tags, name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        let date = |name| vault::tag_value(tags, name).and_then(|v| Date::parse(v).ok());
        Some(Manifest {
            source: vault::tag_value(tags, "source")?,
            layer: vault::tag_value(tags, "layer")?,
            imported: vault::tag_value(tags, "imported")?.parse().ok()?,
            first: date("first"),
            last: date("last"),
            txns: number("txns"),
            prices: number("prices"),
            bytes: number("bytes"),
            host: vault::tag_value(tags, "host")?,
            retired: vault::tag_value(tags, "retired") == Some("1"),
        })
    }
}

/// Every manifest `author` published, borrowed from `txn`, in hash order.
pub fn load<'a>(ndb: &Ndb, txn: &'a Transaction, author: &Pubkey) -> Vec<StoredManifest<'a>> {
    vault::latest_all(ndb, txn, author)
}

/// The segment a stored manifest describes, or `None` when its id isn't a
/// hash.
pub fn hash_of(stored: &StoredManifest<'_>) -> Option<SegmentHash> {
    stored.id.parse().ok()
}

/// The segments `manifests` retire: what every load leaves out.
pub fn retired(manifests: &[StoredManifest<'_>]) -> HashSet<SegmentHash> {
    manifests
        .iter()
        .filter(|m| m.body.retired)
        .filter_map(hash_of)
        .collect()
}

/// The stored manifest of `hash` in `manifests`, which is in hash order.
pub fn find<'s, 'a>(
    manifests: &'s [StoredManifest<'a>],
    hash: SegmentHash,
) -> Option<&'s StoredManifest<'a>> {
    let id = hash.to_string();
    manifests
        .binary_search_by(|m| m.id.cmp(id.as_str()))
        .ok()
        .map(|at| &manifests[at])
}

/// Write `manifest` as segment `hash`'s newest manifest, sealed into the
/// vault and ingested; publishing the frame is the caller's.
///
/// `after` is the `created_at` of the manifest it replaces, if the caller
/// read one (0 if not). Pass it whenever a read transaction is still open:
/// nostrdb opens one transaction per thread, so the write can't look the
/// stored revision up itself then, and would stamp the same second.
pub fn write(
    vault: &Vault,
    ndb: &Ndb,
    hash: SegmentHash,
    manifest: &Manifest<'_>,
    after: u64,
) -> Result<Written, VaultError> {
    vault.write(ndb, &hash.to_string(), manifest, after)
}

/// Why [`publish_segment`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The segment couldn't be read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The manifest couldn't be written.
    #[error(transparent)]
    Vault(#[from] VaultError),
}

/// Publish the manifest of local segment `hash` from `host`, unless the
/// vault already has a live one: what `import` runs on each segment it
/// writes. A retired manifest is replaced, since importing the same bytes
/// again is asking for them back. `None` when nothing needed writing.
pub fn publish_segment(
    vault: &Vault,
    ndb: &Ndb,
    manifests: &[StoredManifest<'_>],
    store: &JournalStore,
    hash: SegmentHash,
    host: &str,
) -> Result<Option<Written>, PublishError> {
    let stored = find(manifests, hash);
    if stored.is_some_and(|m| !m.body.retired) {
        return Ok(None);
    }
    let summary = journal::summarize(store, hash)?;
    let after = stored.map_or(0, |m| m.created_at);
    Ok(Some(write(
        vault,
        ndb,
        hash,
        &Manifest::of(&summary, host),
        after,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::CashflowDir;
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use ledgerdb::{Journal, SegmentHeader};
    use nostrdb::Filter;

    const SECRET: [u8; 32] = [7u8; 32];

    fn segment(store: &JournalStore, source: &str, imported: u64, id: &str) -> SegmentHash {
        let header = SegmentHeader {
            source: source.to_owned(),
            imported,
            layer: "cards".to_owned(),
        };
        let text = format!(
            "2024-03-02 * Shop\n  ; id: {id}\n  a:b  -1 CAD\n  c:d\n\nP 2024-03-01 X 2 CAD\n"
        );
        store
            .write_segment(&header, &Journal::parse(&text).journal)
            .unwrap()
    }

    fn manifests_sub(ndb: &Ndb) -> nostrdb::Subscription {
        ndb.subscribe(&[Filter::new().kinds([kinds::SEGMENT as u64]).build()])
            .unwrap()
    }

    /// A manifest round-trips every field, `publish_segment` writes one only
    /// when the vault lacks a live one, and a retired one is replaced.
    #[tokio::test]
    async fn publish_then_read_and_retire() {
        let tmp = tempfile::tempdir().unwrap();
        let store = journal::store(&CashflowDir::new(tmp.path()));
        let hash = segment(&store, "bank", 10, "bank:1");
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);

        let sub = manifests_sub(ndb);
        let written = publish_segment(&vault, ndb, &[], &store, hash, "alpha").unwrap();
        assert!(written.is_some());
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let summary = journal::summarize(&store, hash).unwrap();
        let sub = manifests_sub(ndb);
        {
            let txn = Transaction::new(ndb).unwrap();
            let all = load(ndb, &txn, vault.author());
            assert_eq!(all.len(), 1);
            assert_eq!(hash_of(&all[0]), Some(hash));
            assert_eq!(all[0].body, Manifest::of(&summary, "alpha"));
            assert_eq!(all[0].body.first, Date::parse("2024-03-01").ok());
            assert_eq!(all[0].body.last, Date::parse("2024-03-02").ok());
            assert_eq!((all[0].body.txns, all[0].body.prices), (1, 1));
            assert!(all[0].body.bytes > 0);
            assert!(retired(&all).is_empty());
            // Already published: nothing to write.
            let again = publish_segment(&vault, ndb, &all, &store, hash, "alpha").unwrap();
            assert!(again.is_none());

            write(&vault, ndb, hash, &all[0].body.retire(), all[0].created_at).unwrap();
        }
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let sub = manifests_sub(ndb);
        {
            let txn = Transaction::new(ndb).unwrap();
            let all = load(ndb, &txn, vault.author());
            assert!(all[0].body.retired);
            assert_eq!(retired(&all), HashSet::from([hash]));
            assert!(find(&all, hash).is_some());

            // Importing the same bytes again un-retires them.
            let back = publish_segment(&vault, ndb, &all, &store, hash, "beta").unwrap();
            assert!(back.is_some());
        }
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();
        let txn = Transaction::new(ndb).unwrap();
        let all = load(ndb, &txn, vault.author());
        assert!(!all[0].body.retired);
        assert_eq!(all[0].body.host, "beta");
    }

    /// An empty segment has no dates, and its manifest reads back without
    /// them.
    #[tokio::test]
    async fn an_empty_segment_has_no_dates() {
        let tmp = tempfile::tempdir().unwrap();
        let store = journal::store(&CashflowDir::new(tmp.path()));
        let header = SegmentHeader {
            source: "x".to_owned(),
            imported: 1,
            layer: "y".to_owned(),
        };
        let hash = store
            .write_segment(&header, &Journal::parse("").journal)
            .unwrap();
        let summary = journal::summarize(&store, hash).unwrap();
        assert_eq!((summary.first, summary.last), (None, None));

        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = manifests_sub(ndb);
        write(&vault, ndb, hash, &Manifest::of(&summary, "h"), 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();
        let txn = Transaction::new(ndb).unwrap();
        let all = load(ndb, &txn, vault.author());
        assert_eq!(all[0].body, Manifest::of(&summary, "h"));
    }
}
