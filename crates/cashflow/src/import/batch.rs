//! One import's worth of transactions, on its way to becoming a segment.
//!
//! An importer reading a structured source (a CSV, a JSON feed) builds
//! [`RawTxn`]s and [`RawPrice`]s and turns them into a [`Batch`] with
//! [`Batch::from_raw`]. The batch is a ledgerdb [`Journal`] of its own, the
//! shape [`ledgerdb::JournalStore::write_segment`] takes.
//!
//! `from_raw` renders the records as ledger-cli text and reads that back with
//! ledgerdb's parser rather than filling the journal's tables by hand, so the
//! parser stays the one place that decides precision, styles and balance: a
//! raw transaction that doesn't balance is an error naming its record id, not
//! a segment that loads short.

use std::fmt::Write;

use ledgerdb::{Date, Journal, ParseError, Parsed};

/// One transaction as an importer read it: the leg it is sure of plus a
/// counter leg to a structural default (see [`crate::rules`]), and what it
/// read as `raw-*` metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTxn {
    pub date: Date,
    /// Written with ledger-cli's `*` mark when set.
    pub cleared: bool,
    /// The payee or description line. Line breaks and runs of whitespace
    /// become single spaces.
    pub payee: String,
    /// The record id, `<source>:<native-id>`, written as `; id:`. It must
    /// stay the same when the source re-exports the record, so edits keyed
    /// on it survive.
    pub id: String,
    /// `; key: value` lines on the transaction, in order.
    pub meta: Vec<RawMeta>,
    /// At least two; at most one may leave its amount out.
    pub postings: Vec<RawPosting>,
}

/// One `; key: value` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMeta {
    /// No whitespace or `:`.
    pub key: String,
    /// Line breaks become spaces.
    pub value: String,
}

/// One posting of a [`RawTxn`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawPosting {
    pub account: String,
    /// `None` leaves it for ledger-cli to infer from the others.
    pub amount: Option<RawAmount>,
}

/// An exact amount: `qty / 10^precision` of `commodity`, so `-1250, 2, CAD`
/// is `-12.50 CAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawAmount {
    pub qty: i64,
    /// Decimal places, at most [`ledgerdb::MAX_PRECISION`].
    pub precision: u8,
    pub commodity: String,
}

/// A `P` directive: one `commodity` cost `price` on `date`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawPrice {
    pub date: Date,
    pub commodity: String,
    pub price: RawAmount,
}

/// A batch of transactions and prices that becomes one segment.
#[derive(Debug)]
pub struct Batch {
    /// The batch, in a journal of its own: amounts are written at its own
    /// commodity table's precision, which a segment's hash depends on.
    pub journal: Journal,
}

impl Batch {
    /// A batch holding `journal` as it is.
    pub fn new(journal: Journal) -> Self {
        Self { journal }
    }

    /// The transactions in the batch.
    pub fn txn_count(&self) -> usize {
        self.journal.txns.len()
    }

    /// The `P` directives in the batch.
    pub fn price_count(&self) -> usize {
        self.journal.prices.len()
    }

    /// Whether the batch holds nothing to write.
    pub fn is_empty(&self) -> bool {
        self.txn_count() == 0 && self.price_count() == 0
    }

    /// Build a batch from raw records. Each transaction must parse whole,
    /// balance and carry an id.
    pub fn from_raw(txns: &[RawTxn], prices: &[RawPrice]) -> Result<Batch, BatchError> {
        let mut text = String::new();
        for price in prices {
            write_price(&mut text, price);
        }
        let price_lines = prices.len() as u32;
        // The line each transaction starts on, to name it in an error: each
        // is a blank line, then its header, id, metadata and postings.
        let mut starts = Vec::with_capacity(txns.len());
        let mut line = price_lines;
        for txn in txns {
            check_txn(txn)?;
            text.push('\n');
            starts.push(line + 2);
            line += 3 + (txn.meta.len() + txn.postings.len()) as u32;
            write_txn(&mut text, txn);
        }

        let Parsed { journal, errors } = Journal::parse(&text);
        if let Some(error) = errors.into_iter().next() {
            let id = if error.line <= price_lines {
                None
            } else {
                let at = starts.partition_point(|&start| start <= error.line);
                Some(txns[at.saturating_sub(1)].id.clone())
            };
            return Err(BatchError::Invalid { id, error });
        }
        Ok(Batch { journal })
    }
}

/// A raw record that can't be part of a batch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// A transaction with an empty id, which a later import couldn't
    /// replace.
    #[error("a transaction dated {date} ({payee:?}) has no record id")]
    MissingId { date: Date, payee: String },
    /// A metadata key that would not read back as one.
    #[error("{id}: metadata key {key:?} must be non-empty with no whitespace or ':'")]
    BadMetaKey { id: String, key: String },
    /// The records did not parse back, such as a transaction that doesn't
    /// balance or an invalid account name. `id` is `None` for a price.
    #[error("{}: {error}", id.as_deref().unwrap_or("price"))]
    Invalid {
        id: Option<String>,
        error: ParseError,
    },
}

/// The checks the parser can't make, because the text would read back as
/// something else rather than fail.
fn check_txn(txn: &RawTxn) -> Result<(), BatchError> {
    if one_line(&txn.id).is_empty() {
        return Err(BatchError::MissingId {
            date: txn.date,
            payee: txn.payee.clone(),
        });
    }
    let bad_key =
        |key: &str| key.is_empty() || key.contains(|c: char| c.is_whitespace() || c == ':');
    if let Some(meta) = txn.meta.iter().find(|m| bad_key(&m.key)) {
        return Err(BatchError::BadMetaKey {
            id: txn.id.clone(),
            key: meta.key.clone(),
        });
    }
    Ok(())
}

fn write_txn(out: &mut String, txn: &RawTxn) {
    let mark = if txn.cleared { " *" } else { "" };
    let _ = writeln!(out, "{}{mark} {}", txn.date, one_line(&txn.payee));
    let _ = writeln!(out, "    ; id: {}", one_line(&txn.id));
    for meta in &txn.meta {
        let _ = writeln!(out, "    ; {}: {}", meta.key, one_line(&meta.value));
    }
    for posting in &txn.postings {
        let _ = write!(out, "    {}", one_line(&posting.account));
        if let Some(amount) = &posting.amount {
            out.push('\t');
            write_amount(out, amount);
        }
        out.push('\n');
    }
}

fn write_price(out: &mut String, price: &RawPrice) {
    let _ = write!(out, "P {} {} ", price.date, quoted(&price.commodity));
    write_amount(out, &price.price);
    out.push('\n');
}

/// `-12.50 CAD`: the commodity after the number, so it parses in any style.
fn write_amount(out: &mut String, amount: &RawAmount) {
    let sign = if amount.qty < 0 { "-" } else { "" };
    let digits = amount.qty.unsigned_abs().to_string();
    let precision = amount.precision as usize;
    let _ = if precision == 0 {
        write!(out, "{sign}{digits}")
    } else {
        let padded = format!("{digits:0>width$}", width = precision + 1);
        let (whole, frac) = padded.split_at(padded.len() - precision);
        write!(out, "{sign}{whole}.{frac}")
    };
    let _ = write!(out, " {}", quoted(&amount.commodity));
}

/// A commodity name, quoted unless it is all letters.
fn quoted(name: &str) -> String {
    if !name.is_empty() && name.chars().all(char::is_alphabetic) {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    }
}

/// `text` on one line with single spaces: what a payee, an id or a metadata
/// value must be to read back as written.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledgerdb::WriteOptions;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn cad(qty: i64) -> Option<RawAmount> {
        Some(RawAmount {
            qty,
            precision: 2,
            commodity: "CAD".to_owned(),
        })
    }

    fn purchase(id: &str, qty: i64) -> RawTxn {
        RawTxn {
            date: date("2024-03-05"),
            cleared: true,
            payee: "Corner\n  Shop".to_owned(),
            id: id.to_owned(),
            meta: vec![RawMeta {
                key: "raw-merchant".to_owned(),
                value: "corner shop".to_owned(),
            }],
            postings: vec![
                RawPosting {
                    account: "liabilities:card".to_owned(),
                    amount: cad(-qty),
                },
                RawPosting {
                    account: "expenses:unknown".to_owned(),
                    amount: None,
                },
            ],
        }
    }

    #[test]
    fn raw_records_become_a_journal() {
        let price = RawPrice {
            date: date("2024-01-01"),
            commodity: "BTC".to_owned(),
            price: RawAmount {
                qty: 6_123_456,
                precision: 2,
                commodity: "CAD".to_owned(),
            },
        };
        let batch =
            Batch::from_raw(&[purchase("card:1", 5), purchase("card:2", 1250)], &[price]).unwrap();
        assert_eq!(batch.txn_count(), 2);
        assert_eq!(batch.price_count(), 1);
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            "P 2024-01-01 BTC 61234.56 CAD\n\n\
             2024-03-05 * Corner Shop\n\t; id: card:1\n\t; raw-merchant: corner shop\n\
             \tliabilities:card\t-0.05 CAD\n\texpenses:unknown\n\n\
             2024-03-05 * Corner Shop\n\t; id: card:2\n\t; raw-merchant: corner shop\n\
             \tliabilities:card\t-12.50 CAD\n\texpenses:unknown\n"
        );
    }

    #[test]
    fn a_bad_txn_is_named_by_its_id() {
        let mut unbalanced = purchase("card:7", 100);
        unbalanced.postings[1].amount = cad(99);
        let err = Batch::from_raw(&[purchase("card:6", 1), unbalanced], &[]).unwrap_err();
        assert!(
            matches!(&err, BatchError::Invalid { id: Some(id), .. } if id == "card:7"),
            "{err:?}"
        );

        let mut no_id = purchase("  ", 1);
        no_id.id = " ".to_owned();
        assert!(matches!(
            Batch::from_raw(&[no_id], &[]),
            Err(BatchError::MissingId { .. })
        ));

        let mut bad_key = purchase("card:8", 1);
        bad_key.meta[0].key = "raw merchant".to_owned();
        assert!(matches!(
            Batch::from_raw(&[bad_key], &[]),
            Err(BatchError::BadMetaKey { .. })
        ));
    }

    #[test]
    fn amounts_render_exactly() {
        let render = |qty, precision, commodity: &str| {
            let mut out = String::new();
            write_amount(
                &mut out,
                &RawAmount {
                    qty,
                    precision,
                    commodity: commodity.to_owned(),
                },
            );
            out
        };
        assert_eq!(render(-5, 2, "CAD"), "-0.05 CAD");
        assert_eq!(render(123_000, 0, "msat"), "123000 msat");
        assert_eq!(render(1, 8, "BTC"), "0.00000001 BTC");
        assert_eq!(render(i64::MIN, 0, "x"), "-9223372036854775808 x");
        assert_eq!(render(42, 0, "AAPL 2"), "42 \"AAPL 2\"");
    }
}
