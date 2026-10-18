//! The `ledger` importer: a ledger-cli file, already in ledgerdb's subset,
//! taken as one batch.
//!
//! This is how existing hand-kept or script-made ledger files come in before
//! a proper importer for their source exists. A transaction that already
//! carries `; id:` keeps it. One that doesn't is given a **legacy id**,
//! `<source>:<hash>`, where the hash is the first 16 hex digits of the
//! sha256 of the transaction's canonical text (as [`ledgerdb`]'s writer
//! prints it). Identical transactions in one file, such as two equal routing
//! fees on one day, take `:2`, `:3`, … after the hash in file order.
//!
//! A legacy id names a transaction by its contents, so it is only as stable
//! as the file: edit a transaction, or add an amount that is written more
//! precisely than any before it (which reprints every amount of that
//! commodity), and its id changes, leaving any edit keyed on the old one
//! behind. That is fine for a one-time import whose segment a real importer
//! later replaces (retire it with `cashflow journal rm`).
//!
//! Lines that don't parse don't stop the import: the transaction they are in
//! is left out and the error is kept in [`Batch::skipped`] for the caller to
//! report.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use ledgerdb::{Journal, Parsed, WriteOptions};
use sha2::{Digest, Sha256};

use super::{Batch, ImportError, ImportInput, Importer, Skipped};

/// How many hex digits of the sha256 a legacy id keeps: 64 bits, so a file
/// of a million transactions has about a one in thirty million chance of any
/// two different ones sharing a hash.
const LEGACY_HASH_DIGITS: usize = 16;

/// Reads one ledger-cli file, giving legacy ids to transactions without one.
pub struct LedgerImporter;

impl Importer for LedgerImporter {
    fn name(&self) -> &'static str {
        "ledger"
    }

    fn layer(&self) -> &'static str {
        "ledger"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        let [path] = input.paths.as_slice() else {
            return Err(ImportError::Usage(
                "`import ledger` takes exactly one <file>".to_owned(),
            ));
        };
        import_file(path, &input.source)
    }
}

#[profiling::function]
fn import_file(path: &Path, source: &str) -> Result<Batch, ImportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ImportError::Read {
        path: path.to_owned(),
        source,
    })?;
    let Parsed {
        mut journal,
        errors,
    } = Journal::parse(&text);
    give_legacy_ids(&mut journal, source);
    Ok(Batch {
        skipped: errors.into_iter().map(Skipped::from).collect(),
        ..Batch::new(journal)
    })
}

/// Give each transaction without an id its legacy id under `source` (see
/// the module docs) and return how many were given one.
#[profiling::function]
fn give_legacy_ids(journal: &mut Journal, source: &str) -> usize {
    let mut text = String::new();
    let mut seen: HashMap<[u8; 32], u32> = HashMap::new();
    journal.fill_missing_ids(|journal, txn| {
        text.clear();
        journal
            .write_txns([txn], &mut text, WriteOptions::default())
            .expect("writing to a String cannot fail");
        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        let count = seen.entry(hash).or_default();
        *count += 1;
        legacy_id(source, &hash, *count)
    })
}

/// `<source>:<hash>`, then `:<nth>` from the second identical transaction.
fn legacy_id(source: &str, hash: &[u8; 32], nth: u32) -> String {
    let mut id = String::with_capacity(source.len() + LEGACY_HASH_DIGITS + 8);
    id.push_str(source);
    id.push(':');
    for byte in &hash[..LEGACY_HASH_DIGITS / 2] {
        let _ = write!(id, "{byte:02x}");
    }
    if nth > 1 {
        let _ = write!(id, ":{nth}");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(text: &str, source: &str) -> Vec<String> {
        let mut journal = Journal::parse(text).journal;
        give_legacy_ids(&mut journal, source);
        journal
            .txns
            .iter()
            .map(|txn| journal.strings.get(txn.id.unwrap()).to_owned())
            .collect()
    }

    const FEES: &str = "\
2022-07-26\tundefined
\trouted:out\t36000 msat
\tassets:cln\t-36000 msat

2022-07-26\tundefined
\trouted:out\t36000 msat
\tassets:cln\t-36000 msat

2022-07-26 * Kept
    ; id: bank:9
    a  1 msat
    b

2022-07-26\tundefined
\trouted:out\t36000 msat
\tassets:cln\t-36000 msat
";

    #[test]
    fn identical_txns_are_numbered_and_given_ids_are_kept() {
        let got = ids(FEES, "cln");
        let first = &got[0];
        assert!(first.starts_with("cln:"), "{first}");
        assert_eq!(first.len(), "cln:".len() + LEGACY_HASH_DIGITS);
        assert!(first[4..].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(got[1], format!("{first}:2"));
        assert_eq!(got[2], "bank:9");
        assert_eq!(got[3], format!("{first}:3"));
    }

    #[test]
    fn a_legacy_id_names_the_txn_not_its_layout() {
        // The same transaction, spaced and indented differently, in another
        // file: same id. A different amount: a different one.
        let spaced =
            "2022-07-26   undefined\n  routed:out    36000 msat\n  assets:cln  -36000 msat\n";
        assert_eq!(ids(spaced, "cln")[0], ids(FEES, "cln")[0]);
        assert_ne!(ids(spaced, "other")[0], ids(FEES, "cln")[0]);
        let other = spaced.replace("36000", "36001");
        assert_ne!(ids(&other, "cln")[0], ids(FEES, "cln")[0]);
    }

    #[test]
    fn a_legacy_journal_encodes_as_a_segment() {
        let mut journal = Journal::parse(FEES).journal;
        assert_eq!(give_legacy_ids(&mut journal, "cln"), 3);
        let header = ledgerdb::SegmentHeader {
            source: "cln".to_owned(),
            imported: 1,
            layer: "ledger".to_owned(),
        };
        let text = ledgerdb::Segment::encode(&header, &journal).unwrap();
        let hash = ledgerdb::SegmentHash::of(text.as_bytes());
        let segment = ledgerdb::Segment::parse(hash, &text).unwrap();
        assert!(segment.errors.is_empty());
        let reread: Vec<_> = segment
            .journal
            .txns
            .iter()
            .map(|txn| segment.journal.strings.get(txn.id.unwrap()))
            .collect();
        assert_eq!(reread, ids(FEES, "cln"));
    }
}
