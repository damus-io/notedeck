//! Importers: each reads one kind of source into a [`Batch`], which
//! [`write`] files as one journal segment.
//!
//! An importer writes **raw facts**: the leg it is sure of, a counter leg to
//! a structural default ([`crate::rules::DEFAULT_COUNTER_ACCOUNTS`]), and
//! what it read as `; raw-<key>:` metadata. Every transaction carries
//! `; id: <source>:<native-id>`, the source's own id wherever it has one, so
//! a re-import replaces a record rather than doubling it. Categorizing is the
//! derive step's job (see [`crate::books`]), never the importer's.
//!
//! The registry is a plain match on the name ([`importer`]); there is no
//! plugin system. A new importer is a module here, an arm in [`importer`] and
//! a name in [`NAMES`].

mod batch;
mod ledger;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ledgerdb::{Date, JournalStore, ParseError, SegmentHash, SegmentHeader, StoreError};

pub use batch::{Batch, BatchError, RawAmount, RawMeta, RawPosting, RawPrice, RawTxn};
pub use ledger::LedgerImporter;

/// Every importer's name, as `cashflow import <name>` takes it.
pub const NAMES: &[&str] = &["ledger"];

/// Reads one kind of source into a batch.
pub trait Importer {
    /// The name `cashflow import` takes, and the segment's default
    /// `; source:`.
    fn name(&self) -> &'static str;

    /// The layer its segments go in unless the run names one, so a reader
    /// can switch a whole kind of source on or off.
    fn layer(&self) -> &'static str;

    /// Read `input` into one batch.
    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError>;
}

/// What an import run hands its importer.
#[derive(Clone, Debug)]
pub struct ImportInput {
    /// The source name the segment is filed under, the prefix of the record
    /// ids an importer makes.
    pub source: String,
    /// The files or directories named on the command line. An importer that
    /// reads from the network ignores them.
    pub paths: Vec<PathBuf>,
}

/// The importer named `name`, if there is one.
pub fn importer(name: &str) -> Option<Box<dyn Importer>> {
    match name {
        "ledger" => Some(Box::new(LedgerImporter)),
        _ => None,
    }
}

/// Why an import failed. Nothing is written when it does.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The run's arguments don't fit the importer.
    #[error("{0}")]
    Usage(String),
    /// An input file could not be read.
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// An input ledger file did not parse cleanly.
    #[error("{}: {} parse error(s), first {}", path.display(), errors.len(), errors[0])]
    Parse {
        path: PathBuf,
        errors: Vec<ParseError>,
    },
    /// A transaction has no `; id:`.
    #[error("{}: the transaction on {date} ({payee:?}) has no `; id:`", path.display())]
    MissingId {
        path: PathBuf,
        date: Date,
        payee: String,
    },
    /// The importer's raw records didn't make a batch.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// The batch has no transactions or prices.
    #[error("nothing to import")]
    Empty,
    /// The segment could not be written.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What [`write`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// The segment's hash, its name in the journal.
    pub hash: SegmentHash,
    /// The header it was written with.
    pub header: SegmentHeader,
    /// False when the journal already had these exact bytes.
    pub new: bool,
    pub txns: usize,
    pub prices: usize,
}

/// Run `importer` over `input` and file the batch in `store` as one segment
/// in `layer` (the importer's own when `None`), stamped `imported` unix
/// seconds (now when `None`).
pub fn write(
    store: &JournalStore,
    importer: &dyn Importer,
    input: &ImportInput,
    layer: Option<&str>,
    imported: Option<u64>,
) -> Result<Written, ImportError> {
    let batch = importer.import(input)?;
    if batch.is_empty() {
        return Err(ImportError::Empty);
    }
    let header = SegmentHeader {
        source: input.source.clone(),
        imported: imported.unwrap_or_else(now),
        layer: layer.unwrap_or(importer.layer()).to_owned(),
    };
    let encoded = ledgerdb::Segment::encode(&header, &batch.journal).map_err(StoreError::from)?;
    let new = !store.contains(SegmentHash::of(encoded.as_bytes()));
    let hash = store.write_segment(&header, &batch.journal)?;
    Ok(Written {
        hash,
        header,
        new,
        txns: batch.txn_count(),
        prices: batch.price_count(),
    })
}

/// The current unix time in seconds.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(source: &str, paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: source.to_owned(),
            paths,
        }
    }

    #[test]
    fn every_name_resolves() {
        for name in NAMES {
            assert_eq!(importer(name).unwrap().name(), *name);
        }
        assert!(importer("frob").is_none());
    }

    #[test]
    fn ledger_import_writes_one_segment_once() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("in.ledger");
        std::fs::write(
            &file,
            "2024-01-02 * Shop\n  ; id: test:1\n  liabilities:card  -4.50 CAD\n  expenses:unknown\n",
        )
        .unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let ledger = importer("ledger").unwrap();

        let first = write(
            &store,
            &*ledger,
            &input("bank", vec![file.clone()]),
            None,
            Some(7),
        )
        .unwrap();
        assert!(first.new);
        assert_eq!((first.txns, first.prices), (1, 0));
        assert_eq!(first.header.layer, "ledger");
        assert_eq!(first.header.source, "bank");
        assert_eq!(store.list().unwrap(), [first.hash]);

        let again = write(&store, &*ledger, &input("bank", vec![file]), None, Some(7)).unwrap();
        assert_eq!(again.hash, first.hash);
        assert!(!again.new);
    }

    #[test]
    fn ledger_import_refuses_what_it_cannot_take() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let ledger = importer("ledger").unwrap();
        let file = |name: &str, text: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let run = |paths| write(&store, &*ledger, &input("x", paths), None, None);

        assert!(matches!(run(vec![]), Err(ImportError::Usage(_))));
        let no_id = file("a", "2024-01-02 Shop\n  a  1 CAD\n  b\n");
        assert!(matches!(
            run(vec![no_id]),
            Err(ImportError::MissingId { .. })
        ));
        let bad = file(
            "b",
            "2024-01-02 Shop\n  ; id: x:1\n  a  1 CAD\n  b  2 CAD\n",
        );
        assert!(matches!(run(vec![bad]), Err(ImportError::Parse { .. })));
        let empty = file("c", "; nothing\n");
        assert!(matches!(run(vec![empty]), Err(ImportError::Empty)));
        assert!(store.list().unwrap().is_empty());
    }
}
