//! Importers: each reads one kind of source into a [`Batch`], which
//! [`write`] files as one journal segment.
//!
//! An importer writes **raw facts**: the leg it is sure of, a counter leg to
//! a structural default ([`crate::rules::DEFAULT_COUNTER_ACCOUNTS`]), and
//! what it read as `; raw-<key>:` metadata. Every transaction carries
//! `; id: <source>:<native-id>`, the source's own id wherever it has one, so
//! a re-import replaces a record rather than doubling it, and [`write`]
//! leaves out the rows the journal already holds. Categorizing is the
//! derive step's job (see [`crate::books`]), never the importer's.
//!
//! The registry is a plain match on the name ([`importer`]); there is no
//! plugin system. A new importer is a module here, an arm in [`importer`] and
//! a name in [`NAMES`]. One with settings reads its own table of
//! [`Config`].

mod amex;
mod appstore;
mod batch;
mod btc;
mod cln;
mod csv_rows;
mod ledger;
mod payroll;
mod rbc;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ledgerdb::{
    Journal, JournalStore, LayerFilter, Ledger, Novelty, ParseError, Parsed, SegmentHash,
    SegmentHeader, StoreError, TxnId, WriteOptions,
};

pub use amex::AmexImporter;
pub use appstore::{AppStoreImporter, ReportSource};
pub use batch::{
    Batch, BatchError, LeftOut, OtherLayer, RawAmount, RawMeta, RawPosting, RawPrice, RawTxn,
    Skipped,
};
pub use btc::{BitcoindHttp, BtcImporter, Rpc};
pub use cln::{ClnImporter, Commando, Node, NodeError};
pub use ledger::LedgerImporter;
pub use payroll::PayrollImporter;
pub use rbc::RbcImporter;

use crate::settings::{Setting, Settings};
use crate::{CashflowDir, Config};

/// Every importer's name, as `cashflow import <name>` takes it.
pub const NAMES: &[&str] = &["amex", "appstore", "btc", "cln", "ledger", "payroll", "rbc"];

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

    /// [`import`](Self::import), knowing what the journal already `held`,
    /// loaded with all its layers. An importer that can ask its source for
    /// only the newer records overrides this to find where the journal
    /// leaves off, so the cursor travels with the journal rather than in
    /// saved state of its own. Every other importer ignores it.
    fn import_held(&self, input: &ImportInput, held: &Ledger) -> Result<Batch, ImportError> {
        let _ = held;
        self.import(input)
    }
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

/// What the vault's synced settings tell the importers: the values every
/// machine shares, beside `config.toml`'s per-machine tables. Empty when
/// the run didn't open the vault.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// [`Setting::Timezone`], when it was set: the day `cln` files an
    /// event under.
    pub timezone: Option<String>,
}

impl Synced {
    /// The settings an importer reads, copied out of `settings`.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            timezone: settings.value(Setting::Timezone).map(str::to_owned),
        }
    }
}

/// The importer named `name`, if there is one, set up by its table in
/// `config` and the `synced` settings. One that signs in to its source
/// reads its secrets from `dir` when it runs.
pub fn importer(
    name: &str,
    config: &Config,
    synced: &Synced,
    dir: &CashflowDir,
) -> Option<Box<dyn Importer>> {
    match name {
        "amex" => Some(Box::new(AmexImporter::new(config.amex.as_ref()))),
        "appstore" => Some(Box::new(AppStoreImporter::new(
            config.appstore.as_ref(),
            dir,
        ))),
        "btc" => Some(Box::new(BtcImporter::new(config.btc.as_ref(), dir))),
        "cln" => Some(Box::new(
            ClnImporter::new(config.cln.as_ref(), dir).synced(synced),
        )),
        "ledger" => Some(Box::new(LedgerImporter)),
        "payroll" => Some(Box::new(PayrollImporter::new(config.payroll.as_ref()))),
        "rbc" => Some(Box::new(RbcImporter::new(config.rbc.as_ref()))),
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
    /// A row of a CSV export could not be read. `row` counts from 1, the
    /// header being row 1.
    #[error("{}: row {row}: {message}", path.display())]
    Csv {
        path: PathBuf,
        row: usize,
        message: String,
    },
    /// The importer's raw records didn't make a batch.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// The batch has no transactions or prices. `skipped` is what the
    /// importer read but left out, so a file where nothing parsed says why.
    #[error("nothing to import{}", first_skip(skipped))]
    Empty { skipped: Vec<Skipped> },
    /// The segment could not be written, or the journal not read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A source read over the network failed or answered with something the
    /// importer can't read. `from` names the call, such as `bitcoind
    /// listwallets`.
    #[error("{from}: {error}")]
    Fetch { from: String, error: String },
    /// The batch's new and changed rows, written out without the unchanged
    /// ones, did not read back. The writer's output always should,
    /// so this is a bug in ledgerdb.
    #[error("the batch's new rows did not read back: {0}")]
    Reread(ParseError),
}

/// What [`write`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// The segment's hash, its name in the journal, or `None` when the
    /// journal already held every transaction and the batch had no prices,
    /// so there was nothing to write.
    pub hash: Option<SegmentHash>,
    /// The header it was written with, or would have been.
    pub header: SegmentHeader,
    /// False when nothing was written: no segment, or the journal already
    /// had these exact bytes.
    pub new: bool,
    /// Transactions in the segment: the batch's new and changed ones.
    pub txns: usize,
    pub prices: usize,
    /// How the batch's transactions compared with the journal.
    pub txn_rows: Rows,
    /// How the batch's `P` directives compared with the rates the journal
    /// uses for their dates and pairs.
    pub price_rows: Rows,
    /// What the importer read but left out, from [`Batch::skipped`].
    pub skipped: Vec<Skipped>,
    /// What the importer deliberately left out, from [`Batch::left_out`].
    pub left_out: Vec<LeftOut>,
    /// What the importer said about the run, from [`Batch::notes`].
    pub notes: Vec<String>,
    /// The segments of the batch's [`Batch::other_layers`], each as this
    /// one, in the batch's order. Their `skipped`, `left_out`, `notes` and
    /// `other_layers` are empty: those are the run's, and are here.
    pub other_layers: Vec<Written>,
}

/// How a batch's transactions or prices compared with the journal they were
/// imported into; see [`ledgerdb::Novelty`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    /// Record ids (or, for a price, dates and pairs) the journal didn't
    /// have.
    pub new: usize,
    /// Ones the journal had with other contents, which this import
    /// replaces.
    pub changed: usize,
    /// Rows the journal already held exactly, left out of the segment.
    pub unchanged: usize,
}

/// Run `importer` over `input` and file what the journal doesn't already
/// hold as one segment in `layer` (the importer's own when `None`), stamped
/// `imported` unix seconds (now when `None`).
///
/// Every transaction is checked against the journal loaded with all its
/// layers but without the `retired` segments (whose rows no reader sees,
/// even where the file is still here), since record ids dedup across
/// layers, and only the new and
/// changed ones are written (see [`novel_rows`]). A daily pull of a source's
/// trailing 90 days so stores each row once, not ninety times, in a store
/// that can never delete a synced segment. A price is left out when the
/// journal already uses that rate for its date and pair.
///
/// The importer sees the same loaded journal ([`Importer::import_held`]).
/// Transactions it files in [other layers](Batch::other_layers) are written
/// first, each as a segment of its own, so an importer that resumes after
/// its newest record in `layer` never resumes past one it failed to write.
pub fn write(
    store: &JournalStore,
    retired: &HashSet<SegmentHash>,
    importer: &dyn Importer,
    input: &ImportInput,
    layer: Option<&str>,
    imported: Option<u64>,
) -> Result<Written, ImportError> {
    let ledger = Ledger::load_without(store, LayerFilter::All, retired.clone())?.ledger;
    let batch = importer.import_held(input, &ledger)?;
    if batch.is_empty() {
        return Err(ImportError::Empty {
            skipped: batch.skipped,
        });
    }
    let imported = imported.unwrap_or_else(now);
    let header = |layer: &str| SegmentHeader {
        source: input.source.clone(),
        imported,
        layer: layer.to_owned(),
    };
    let other_layers = batch
        .other_layers
        .into_iter()
        .map(|other| file(store, &ledger, header(&other.layer), other.journal))
        .collect::<Result<Vec<_>, _>>()?;
    let mut written = file(
        store,
        &ledger,
        header(layer.unwrap_or(importer.layer())),
        batch.journal,
    )?;
    written.skipped = batch.skipped;
    written.left_out = batch.left_out;
    written.notes = batch.notes;
    written.other_layers = other_layers;
    Ok(written)
}

/// Write what of `journal` the `ledger` doesn't hold as one segment under
/// `header`.
fn file(
    store: &JournalStore,
    ledger: &Ledger,
    header: SegmentHeader,
    journal: Journal,
) -> Result<Written, ImportError> {
    let Novel {
        journal,
        txn_rows,
        price_rows,
    } = novel_rows(ledger, journal)?;
    let mut written = Written {
        hash: None,
        header,
        new: false,
        txns: journal.txns.len(),
        prices: journal.prices.len(),
        txn_rows,
        price_rows,
        skipped: Vec::new(),
        left_out: Vec::new(),
        notes: Vec::new(),
        other_layers: Vec::new(),
    };
    if journal.txns.is_empty() && journal.prices.is_empty() {
        return Ok(written);
    }
    let put = store.put_segment(&written.header, &journal)?;
    written.hash = Some(put.hash);
    written.new = put.new;
    Ok(written)
}

/// A batch cut down to what the journal lacks, from [`novel_rows`].
struct Novel {
    journal: Journal,
    txn_rows: Rows,
    price_rows: Rows,
}

/// Keep the transactions and prices of `batch` that `ledger` doesn't hold
/// exactly.
///
/// When everything is kept the batch is returned as it is. Otherwise the
/// kept rows are written out and read back, so the segment's commodity
/// table holds only their precisions (a segment's hash depends on them), and
/// the batch's file comments are dropped, since they were placed between
/// transactions that may be gone.
#[profiling::function]
fn novel_rows(ledger: &Ledger, mut batch: Journal) -> Result<Novel, ImportError> {
    let mut txn_rows = Rows::default();
    let mut kept = Vec::with_capacity(batch.txns.len());
    for txn in (0..batch.txns.len() as u32).map(TxnId) {
        if txn_rows.count(ledger.novelty(&batch, txn)) {
            kept.push(txn);
        }
    }
    let mut price_rows = Rows::default();
    let keep_price: Vec<bool> = (0..batch.prices.len())
        .map(|at| price_rows.count(ledger.price_novelty(&batch, at)))
        .collect();
    if txn_rows.unchanged == 0 && price_rows.unchanged == 0 {
        return Ok(Novel {
            journal: batch,
            txn_rows,
            price_rows,
        });
    }

    let mut keep_price = keep_price.into_iter();
    batch.prices.retain(|_| keep_price.next().unwrap_or(false));
    let mut text = String::new();
    batch
        .write_prices(None, &mut text)
        .and_then(|()| batch.write_txns(kept, &mut text, WriteOptions::default()))
        .expect("writing to a String cannot fail");
    let Parsed { journal, errors } = Journal::parse(&text);
    if let Some(error) = errors.into_iter().next() {
        return Err(ImportError::Reread(error));
    }
    Ok(Novel {
        journal,
        txn_rows,
        price_rows,
    })
}

impl Rows {
    /// Count one row as `novelty` says, and return whether it is kept.
    fn count(&mut self, novelty: Novelty) -> bool {
        match novelty {
            Novelty::New => self.new += 1,
            Novelty::Changed => self.changed += 1,
            Novelty::Unchanged => self.unchanged += 1,
        }
        novelty != Novelty::Unchanged
    }
}

/// `: 3 lines skipped, first 1:1: …` for [`ImportError::Empty`], or
/// nothing when none were.
fn first_skip(skipped: &[Skipped]) -> String {
    match skipped.first() {
        Some(first) => format!(": {} skipped, first {first}", skipped.len()),
        None => String::new(),
    }
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
            assert_eq!(
                importer(
                    name,
                    &Config::default(),
                    &Synced::default(),
                    &CashflowDir::new("unused")
                )
                .unwrap()
                .name(),
                *name
            );
        }
        assert!(
            importer(
                "frob",
                &Config::default(),
                &Synced::default(),
                &CashflowDir::new("unused")
            )
            .is_none()
        );
    }

    /// Import `text` as a ledger file from source `bank` into `layer`,
    /// stamped `imported`.
    fn import_text(store: &JournalStore, text: &str, layer: &str, imported: u64) -> Written {
        let file = store.dir().with_extension(format!("{imported}.ledger"));
        std::fs::write(&file, text).unwrap();
        let ledger = importer(
            "ledger",
            &Config::default(),
            &Synced::default(),
            &CashflowDir::new("unused"),
        )
        .unwrap();
        write(
            store,
            &Default::default(),
            &*ledger,
            &input("bank", vec![file]),
            Some(layer),
            Some(imported),
        )
        .unwrap()
    }

    /// A one-posting card charge with record id `test:<n>`.
    fn charge(n: u32, mark: &str, qty: &str) -> String {
        format!(
            "2024-01-0{n} {mark}Shop {n}\n  ; id: test:{n}\n  liabilities:card  -{qty} CAD\n  expenses:unknown\n\n"
        )
    }

    #[test]
    fn ledger_import_writes_one_segment_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let text = format!("{}{}", charge(1, "* ", "4.50"), charge(2, "* ", "1.00"));

        let first = import_text(&store, &text, "ledger", 7);
        assert!(first.new);
        assert_eq!((first.txns, first.prices), (2, 0));
        assert_eq!(
            first.txn_rows,
            Rows {
                new: 2,
                changed: 0,
                unchanged: 0
            }
        );
        assert_eq!(first.header.layer, "ledger");
        assert_eq!(first.header.source, "bank");
        assert_eq!(store.list().unwrap(), [first.hash.unwrap()]);

        // A later pull that the journal already fully holds writes nothing.
        let again = import_text(&store, &text, "ledger", 8);
        assert_eq!(again.hash, None);
        assert!(!again.new);
        assert_eq!(again.txns, 0);
        assert_eq!(
            again.txn_rows,
            Rows {
                new: 0,
                changed: 0,
                unchanged: 2
            }
        );
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn only_new_and_changed_rows_are_written() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let held = format!("{}{}", charge(1, "! ", "4.50"), charge(2, "* ", "1.00"));
        import_text(&store, &held, "ledger", 7);

        // test:1 has posted, test:2 is as it was, test:3 is new.
        let pull = [
            charge(1, "* ", "4.50"),
            charge(2, "* ", "1.00"),
            charge(3, "", "2.00"),
        ];
        let written = import_text(&store, &pull.concat(), "ledger", 8);
        assert!(written.new);
        assert_eq!(
            written.txn_rows,
            Rows {
                new: 1,
                changed: 1,
                unchanged: 1
            }
        );
        assert_eq!(written.txns, 2);
        let segment = store.read(written.hash.unwrap()).unwrap();
        let ids: Vec<_> = segment
            .journal
            .txns
            .iter()
            .map(|t| segment.journal.strings.get(t.id.unwrap()))
            .collect();
        assert_eq!(ids, ["test:1", "test:3"]);

        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        let posted = ledger.journal().txn(ledger.find("test:1").unwrap());
        assert_eq!(posted.flag, ledgerdb::TxnFlag::Cleared);
        assert_eq!(ledger.txns().len(), 3);
    }

    #[test]
    fn a_precision_difference_alone_is_not_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        // The journal holds CAD at three places, so test:1 is -4.500 there.
        let finer = format!("{}{}", charge(1, "* ", "4.50"), charge(2, "* ", "0.001"));
        import_text(&store, &finer, "ledger", 7);

        let again = import_text(&store, &charge(1, "* ", "4.50"), "ledger", 8);
        assert_eq!(
            again.txn_rows,
            Rows {
                new: 0,
                changed: 0,
                unchanged: 1
            }
        );
        assert_eq!(again.hash, None);
    }

    #[test]
    fn a_row_in_another_layer_still_counts_as_held() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        import_text(&store, &charge(1, "* ", "4.50"), "cards", 7);
        // A reader that switches the `cards` layer off doesn't see it, but
        // its record id still dedups, so it is in the journal.
        let filtered = LayerFilter::Except(vec!["cards".to_owned()]);
        assert!(
            Ledger::load(&store, filtered)
                .unwrap()
                .ledger
                .find("test:1")
                .is_none()
        );

        let again = import_text(&store, &charge(1, "* ", "4.50"), "bank", 8);
        assert_eq!(
            again.txn_rows,
            Rows {
                new: 0,
                changed: 0,
                unchanged: 1
            }
        );
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn a_price_is_written_until_the_journal_uses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        import_text(&store, &charge(1, "* ", "4.50"), "ledger", 7);

        let text = format!(
            "P 2024-01-01 BTC 61234.56789 CAD\n\n{}{}",
            charge(1, "* ", "4.50"),
            charge(2, "* ", "1.00")
        );
        let written = import_text(&store, &text, "ledger", 8);
        assert_eq!((written.txns, written.prices), (1, 1));
        let segment = store.read(written.hash.unwrap()).unwrap();
        assert!(segment.errors.is_empty());
        assert_eq!(segment.journal.prices.len(), 1);
        assert_eq!(segment.journal.prices[0].value.scale, 5);
        assert_eq!(
            written.price_rows,
            Rows {
                new: 1,
                changed: 0,
                unchanged: 0
            }
        );

        // Now the journal uses that rate, so a re-import has nothing to add.
        let again = import_text(&store, &text, "ledger", 9);
        assert_eq!(again.hash, None);
        assert_eq!(
            again.price_rows,
            Rows {
                new: 0,
                changed: 0,
                unchanged: 1
            }
        );
    }

    #[test]
    fn ledger_import_refuses_what_it_cannot_take() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let ledger = importer(
            "ledger",
            &Config::default(),
            &Synced::default(),
            &CashflowDir::new("unused"),
        )
        .unwrap();
        let empty = tmp.path().join("c");
        std::fs::write(&empty, "; nothing\nnot a transaction\n").unwrap();
        let run = |paths| {
            write(
                &store,
                &Default::default(),
                &*ledger,
                &input("x", paths),
                None,
                None,
            )
        };

        assert!(matches!(run(vec![]), Err(ImportError::Usage(_))));
        assert!(matches!(
            run(vec![tmp.path().join("missing")]),
            Err(ImportError::Read { .. })
        ));
        let err = run(vec![empty]).unwrap_err();
        assert!(matches!(&err, ImportError::Empty { skipped } if skipped.len() == 1));
        assert_eq!(
            err.to_string(),
            "nothing to import: 1 skipped, first 2:1: unsupported directive"
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn ledger_import_reports_lines_it_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("in.ledger");
        std::fs::write(
            &file,
            "raw\tline\n\n2024-01-02 Shop\n  a  1 CAD\n  b  2 CAD\n\n2024-01-03 Ok\n  a  1 CAD\n  b\n",
        )
        .unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let ledger = importer(
            "ledger",
            &Config::default(),
            &Synced::default(),
            &CashflowDir::new("unused"),
        )
        .unwrap();
        let written = write(
            &store,
            &Default::default(),
            &*ledger,
            &input("x", vec![file]),
            None,
            None,
        )
        .unwrap();

        assert_eq!(written.txns, 1);
        let lines: Vec<_> = written.skipped.iter().map(|e| e.line).collect();
        assert_eq!(lines, [1, 3], "{:?}", written.skipped);
        let segment = store.read(written.hash.unwrap()).unwrap();
        assert!(segment.errors.is_empty(), "the segment itself is clean");
    }
}
