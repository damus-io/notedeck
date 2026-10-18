//! The `rbc` importer: RBC (Royal Bank of Canada) online banking's
//! "Download Transactions" CSV, for chequing, savings and credit card
//! accounts alike.
//!
//! An export looks like this, with the columns found by their header names.
//! The first header name is unquoted and every row carries a ninth, empty
//! field; the reader takes both in its stride:
//!
//! ```text
//! Account Type,"Account Number","Transaction Date","Cheque Number","Description 1","Description 2","CAD$","USD$"
//! Chequing,00000-0001234,8/3/2026,,"WWW TRF DDA - 1111",,-500.00,,
//! Savings,00000-0005678,8/3/2026,,"Transfer","WWW TRANSFER - 1111 ",500.00,,
//! Visa,4500000000009876,9/3/2026,,"SOFTWARE CO","5.00 USD @ 1.400000000000",-7.00,,
//! ```
//!
//! The amount is the change to the account, from its owner's side: money out
//! of chequing and a purchase on a card are both negative. Each row becomes
//! a transaction with the account's leg, the account [`RbcConfig::accounts`]
//! maps the account number's last four digits to, and a counter leg taking
//! the other side, to `expenses:unknown` when money left and
//! `income:unknown` when it came in. A full account number is never
//! written anywhere; a number with no mapping is an error, so a new account
//! is never filed under a guess.
//!
//! A row with an amount in `USD$` rather than `CAD$` is from a US-dollar
//! account and is written in USD. A card's foreign charge is not: it is
//! billed in `CAD$`, and its `Description 2` is the original amount and the
//! rate (`5.00 USD @ 1.400000000000`), which goes in `; raw-foreign:` rather
//! than the description.
//!
//! Otherwise Description 1 and 2 are joined into the payee and
//! `; raw-description:`, and a cheque's number goes in `; raw-cheque:`. The
//! record id is the occurrence id
//! `<source>:<last4>:<date>:<cents>:<description>:<n>` (see
//! [`super::csv_rows`]), so overlapping downloads keep each row once. An RBC
//! download only lists posted transactions, so every row is cleared.

use std::collections::BTreeMap;

use ledgerdb::Date;

use super::csv_rows::{
    Header, Identified, RowError, csv_files, id_key, number, parse_cents, parse_mdy, read, squeeze,
};
use super::{Batch, ImportError, ImportInput, Importer, RawAmount, RawMeta, RawPosting, RawTxn};
use crate::config::RbcConfig;
use crate::csv;

/// The counter leg when money leaves the account.
const SPENT: &str = "expenses:unknown";

/// The counter leg when money comes in.
const RECEIVED: &str = "income:unknown";

/// Reads RBC "Download Transactions" CSV exports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RbcImporter {
    /// Ledger accounts by the last four digits of the RBC account number.
    pub accounts: BTreeMap<String, String>,
}

impl RbcImporter {
    /// The importer as `config.toml`'s `[rbc]` table sets it up. With no
    /// table it maps no accounts, so every row is an error that says how to
    /// add one.
    pub fn new(config: Option<&RbcConfig>) -> Self {
        Self {
            accounts: config.map(|c| c.accounts.clone()).unwrap_or_default(),
        }
    }
}

impl Importer for RbcImporter {
    fn name(&self) -> &'static str {
        "rbc"
    }

    fn layer(&self) -> &'static str {
        "rbc"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        if input.paths.is_empty() {
            return Err(ImportError::Usage(
                "`import rbc` takes one or more CSV files or directories".to_owned(),
            ));
        }
        let mut rows: BTreeMap<String, Row> = BTreeMap::new();
        for path in csv_files(&input.paths)? {
            let file = self
                .read_export(&read(&path)?)
                .map_err(|error| error.in_file(&path))?;
            for Identified { id, row } in give_ids(&input.source, file) {
                rows.entry(id).or_insert(row);
            }
        }
        let txns: Vec<RawTxn> = rows
            .into_iter()
            .map(|(id, row)| self.raw_txn(id, row))
            .collect();
        Ok(Batch::from_raw(&txns, &[])?)
    }
}

/// One export row, as read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    /// The account number's last four digits.
    last4: String,
    date: Date,
    /// Signed cents: the change to the account.
    cents: i64,
    /// [`id_key`] of the description, the id's description part. These
    /// four fields come first so rows sharing an id stem sort together.
    key: String,
    /// Description 1 and 2, each squeezed, joined by a space; Description 1
    /// alone when 2 is a [`foreign`] charge.
    description: String,
    /// Description 2 when it is a foreign charge's amount and rate,
    /// squeezed; empty otherwise.
    foreign: String,
    /// `"USD"` for a `USD$` amount, otherwise `"CAD"`.
    commodity: &'static str,
    /// `Cheque Number`, empty when there is none.
    cheque: String,
    /// `Account Type`, as written, such as `Chequing` or `Visa`.
    account_type: String,
}

/// Where each column the importer reads sits in a row.
struct Columns {
    account_type: usize,
    account: usize,
    date: usize,
    cheque: usize,
    description1: usize,
    description2: usize,
    cad: usize,
    usd: usize,
}

impl Columns {
    /// Find the columns by name in the `header` row. All eight are
    /// required: this is the one RBC header known, and a header missing any
    /// of them is a format to look at, not to guess about.
    fn find(header: &Header) -> Result<Self, RowError> {
        Ok(Self {
            account_type: header.need("Account Type")?,
            account: header.need("Account Number")?,
            date: header.need("Transaction Date")?,
            cheque: header.need("Cheque Number")?,
            description1: header.need("Description 1")?,
            description2: header.need("Description 2")?,
            cad: header.need("CAD$")?,
            usd: header.need("USD$")?,
        })
    }
}

impl RbcImporter {
    /// Every row of one export, in its order. The first record is the
    /// header; a row that doesn't read is an error, so nothing is silently
    /// dropped.
    fn read_export(&self, text: &str) -> Result<Vec<Row>, RowError> {
        let mut records = csv::Reader::new(text);
        let Some(header) = records.next() else {
            return Ok(Vec::new());
        };
        let header = Header::new(
            header.map(|field| field.into_owned()).collect(),
            "Account Type, Account Number, Transaction Date, Cheque Number, \
             Description 1, Description 2, CAD$, USD$",
        );
        let columns = Columns::find(&header)?;

        let mut rows = Vec::new();
        for (index, record) in records.enumerate() {
            let row = index + 2;
            let fields: Vec<_> = record.collect();
            let field = |at: usize| fields.get(at).map_or("", |f| f.as_ref()).trim();
            let error = |message: String| RowError { row, message };

            let last4 = last4(field(columns.account))
                .ok_or_else(|| error("an account number with fewer than 4 digits".to_owned()))?;
            if !self.accounts.contains_key(&last4) {
                return Err(error(format!(
                    "the account ending {last4} has no ledger account; add \
                     `\"{last4}\" = \"assets:rbc:chequing\"` (or wherever it belongs) \
                     under [rbc.accounts] in config.toml"
                )));
            }
            let date = parse_mdy(field(columns.date))
                .ok_or_else(|| error(format!("bad date {:?}", field(columns.date))))?;
            let Amount { cents, commodity } =
                amount(field(columns.cad), field(columns.usd)).map_err(error)?;
            let second = squeeze(field(columns.description2));
            let (second, foreign) = if foreign(&second) {
                (String::new(), second)
            } else {
                (second, String::new())
            };
            let description = [squeeze(field(columns.description1)), second]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if description.is_empty() {
                return Err(error("empty description".to_owned()));
            }
            rows.push(Row {
                last4,
                date,
                cents,
                key: id_key(&description),
                description,
                foreign,
                commodity,
                cheque: field(columns.cheque).to_owned(),
                account_type: squeeze(field(columns.account_type)),
            });
        }
        Ok(rows)
    }

    /// `row` as a raw transaction with record id `id`.
    fn raw_txn(&self, id: String, row: Row) -> RawTxn {
        let amount = |qty| {
            Some(RawAmount {
                qty,
                precision: 2,
                commodity: row.commodity.to_owned(),
            })
        };
        let counter = if row.cents < 0 { SPENT } else { RECEIVED };
        let mut meta = vec![RawMeta {
            key: "raw-description".to_owned(),
            value: row.description.clone(),
        }];
        if !row.foreign.is_empty() {
            meta.push(RawMeta {
                key: "raw-foreign".to_owned(),
                value: row.foreign,
            });
        }
        if !row.cheque.is_empty() {
            meta.push(RawMeta {
                key: "raw-cheque".to_owned(),
                value: row.cheque,
            });
        }
        if !row.account_type.is_empty() {
            meta.push(RawMeta {
                key: "raw-account-type".to_owned(),
                value: row.account_type,
            });
        }
        RawTxn {
            date: row.date,
            cleared: true,
            payee: row.description,
            id,
            meta,
            postings: vec![
                RawPosting {
                    account: self.accounts[&row.last4].clone(),
                    amount: amount(row.cents),
                },
                RawPosting {
                    account: counter.to_owned(),
                    amount: amount(-row.cents),
                },
            ],
        }
    }
}

/// Give each row of one export its occurrence id under `source`, the stem
/// being (last4, date, cents, description key), which [`Row`] sorts by
/// first.
fn give_ids(source: &str, rows: Vec<Row>) -> Vec<Identified<Row>> {
    number(rows, |row| {
        format!(
            "{source}:{}:{}:{}:{}",
            row.last4, row.date, row.cents, row.key
        )
    })
}

/// A row's amount, from whichever of its two amount columns is filled.
struct Amount {
    cents: i64,
    commodity: &'static str,
}

/// The amount in `cad` or `usd`, exactly one of which must be filled.
fn amount(cad: &str, usd: &str) -> Result<Amount, String> {
    let (text, commodity) = match (cad.is_empty(), usd.is_empty()) {
        (false, true) => (cad, "CAD"),
        (true, false) => (usd, "USD"),
        (true, true) => return Err("no amount in CAD$ or USD$".to_owned()),
        (false, false) => return Err(format!("amounts in both CAD$ ({cad}) and USD$ ({usd})")),
    };
    let cents = parse_cents(text).ok_or_else(|| format!("bad amount {text:?}"))?;
    Ok(Amount { cents, commodity })
}

/// Whether a card row's Description 2 is a foreign charge's original amount
/// and rate, `<qty> <currency> @ <rate>` such as `5.00 USD @ 1.400000000000`.
fn foreign(text: &str) -> bool {
    let is_number = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b == b'.' || b == b',')
    };
    let mut parts = text.split(' ');
    let (Some(qty), Some(currency), Some("@"), Some(rate), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    is_number(qty)
        && is_number(rate)
        && currency.len() == 3
        && currency.bytes().all(|b| b.is_ascii_uppercase())
}

/// The last four digits of an account number such as `00000-0001234` or a
/// card number, ignoring dashes, spaces and masking, or `None` when it has
/// fewer than four.
fn last4(number: &str) -> Option<String> {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    let start = digits.len().checked_sub(4)?;
    Some(digits[start..].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Rows, write};
    use ledgerdb::{JournalStore, WriteOptions};
    use std::path::{Path, PathBuf};

    /// The header as RBC writes it, its first name unquoted.
    const HEADER: &str = "Account Type,\"Account Number\",\"Transaction Date\",\"Cheque Number\",\
                          \"Description 1\",\"Description 2\",\"CAD$\",\"USD$\"\n";

    /// An importer mapping `…1234` to chequing, `…4321` to a Visa card and
    /// `…9876` to a USD account.
    fn rbc() -> RbcImporter {
        RbcImporter::new(Some(&RbcConfig {
            accounts: BTreeMap::from([
                ("1234".to_owned(), "assets:rbc:chequing".to_owned()),
                ("4321".to_owned(), "liabilities:rbc:visa".to_owned()),
                ("9876".to_owned(), "assets:rbc:usd".to_owned()),
            ]),
        }))
    }

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn input(paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: "rbc".to_owned(),
            paths,
        }
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    #[test]
    fn dates_and_account_numbers() {
        assert_eq!(parse_mdy("4/3/2026"), Some(date("2026-04-03")));
        assert_eq!(parse_mdy("12/31/2025"), Some(date("2025-12-31")));
        assert_eq!(parse_mdy("13/1/2026"), None);
        assert_eq!(parse_mdy("2026-04-03"), None);
        assert_eq!(parse_mdy("4/3/2026/1"), None);
        assert_eq!(last4("00000-0001234").as_deref(), Some("1234"));
        assert_eq!(last4("4500 **** **** 9876").as_deref(), Some("9876"));
        assert_eq!(last4("12-3"), None);
    }

    /// Rows in the shape of a real download: a trailing empty field on each,
    /// blanks after Description 2, a card's foreign charge, a cheque and a
    /// USD account.
    #[test]
    fn rows_become_raw_transactions() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{HEADER}\
             Chequing,00000-0001234,4/3/2026,,\"BILL PAYMENT\",\"Phone Co  \",-50.00,,\n\
             Chequing,00000-0001234,4/1/2026,,\"PAYROLL DEPOSIT\",\"Example Corp \",\"2,000.00\",,\n\
             Chequing,00000-0001234,4/2/2026,123,\"Cheque\",,-80.00,,\n\
             Visa,4500000000004321,4/2/2026,,\"SOFTWARE CO\",\"5.00 USD @ 1.400000000000\",-7.00,,\n\
             USD Savings,00000-0009876,4/2/2026,,\"DEPOSIT INTEREST\",,,0.12,\n"
        );
        let batch = rbc()
            .import(&input(vec![file(tmp.path(), "a.csv", &text)]))
            .unwrap();
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            "2026-04-01 * PAYROLL DEPOSIT Example Corp\n\
             \t; id: rbc:1234:2026-04-01:200000:payroll-deposit-example-corp:1\n\
             \t; raw-description: PAYROLL DEPOSIT Example Corp\n\
             \t; raw-account-type: Chequing\n\
             \tassets:rbc:chequing\t2000.00 CAD\n\
             \tincome:unknown\t-2000.00 CAD\n\n\
             2026-04-02 * Cheque\n\
             \t; id: rbc:1234:2026-04-02:-8000:cheque:1\n\
             \t; raw-description: Cheque\n\
             \t; raw-cheque: 123\n\
             \t; raw-account-type: Chequing\n\
             \tassets:rbc:chequing\t-80.00 CAD\n\
             \texpenses:unknown\t80.00 CAD\n\n\
             2026-04-03 * BILL PAYMENT Phone Co\n\
             \t; id: rbc:1234:2026-04-03:-5000:bill-payment-phone-co:1\n\
             \t; raw-description: BILL PAYMENT Phone Co\n\
             \t; raw-account-type: Chequing\n\
             \tassets:rbc:chequing\t-50.00 CAD\n\
             \texpenses:unknown\t50.00 CAD\n\n\
             2026-04-02 * SOFTWARE CO\n\
             \t; id: rbc:4321:2026-04-02:-700:software-co:1\n\
             \t; raw-description: SOFTWARE CO\n\
             \t; raw-foreign: 5.00 USD @ 1.400000000000\n\
             \t; raw-account-type: Visa\n\
             \tliabilities:rbc:visa\t-7.00 CAD\n\
             \texpenses:unknown\t7.00 CAD\n\n\
             2026-04-02 * DEPOSIT INTEREST\n\
             \t; id: rbc:9876:2026-04-02:12:deposit-interest:1\n\
             \t; raw-description: DEPOSIT INTEREST\n\
             \t; raw-account-type: USD Savings\n\
             \tassets:rbc:usd\t0.12 USD\n\
             \tincome:unknown\t-0.12 USD\n"
        );
    }

    #[test]
    fn foreign_charges() {
        assert!(foreign("5.00 USD @ 1.400000000000"));
        assert!(foreign("1,200.00 EUR @ 1.5"));
        assert!(!foreign("WWW TRANSFER - 1111"));
        assert!(!foreign("5.00 usd @ 1.4"));
        assert!(!foreign("5.00 USD 1.4"));
        assert!(!foreign("5.00 USD @ 1.4 x"));
        assert!(!foreign(""));
    }

    #[test]
    fn an_unmapped_account_is_an_error_without_its_number() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(
            tmp.path(),
            "a.csv",
            &format!("{HEADER}Visa,4500000000004444,4/3/2026,,SHOP,,-5.00,,\n"),
        );
        let err = rbc().import(&input(vec![path.clone()])).unwrap_err();
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "{}: row 2: the account ending 4444",
                path.display()
            )),
            "{message}"
        );
        assert!(message.contains("[rbc.accounts]"), "{message}");
        assert!(!message.contains("4500000000004444"), "{message}");

        // With no [rbc] table at all, the same.
        let none = RbcImporter::new(None)
            .import(&input(vec![path]))
            .unwrap_err();
        assert!(none.to_string().contains("[rbc.accounts]"), "{none}");
    }

    #[test]
    fn a_header_without_a_column_fails_quoting_it() {
        let err = rbc()
            .read_export("Date,Description,Amount\n1/1/2026,X,1.00\n")
            .unwrap_err();
        assert_eq!(err.row, 1);
        assert!(
            err.message.starts_with(
                "no \"Account Type\" column; the header is \"Date,Description,Amount\""
            ),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_row_that_does_not_read_is_an_error() {
        let rows = |row: &str| {
            rbc()
                .read_export(&format!("{HEADER}{row}\n"))
                .unwrap_err()
                .message
        };
        assert!(rows("Chequing,1234,2026-01-01,,X,,1.00,").contains("bad date"));
        assert!(rows("Chequing,1234,1/1/2026,,X,,lots,").contains("bad amount"));
        assert!(rows("Chequing,1234,1/1/2026,,X,,,").contains("no amount"));
        assert!(rows("Chequing,1234,1/1/2026,,X,,1.00,1.00").contains("both"));
        assert!(rows("Chequing,1234,1/1/2026,, , ,1.00,").contains("empty"));
        assert!(rows("Chequing,12,1/1/2026,,X,,1.00,").contains("fewer than 4"));
    }

    /// Two downloads that overlap, the second also listing a day of three
    /// identical coffees as two.
    #[test]
    fn overlapping_downloads_import_as_their_union() {
        let tmp = tempfile::tempdir().unwrap();
        let coffee = "Chequing,00000-0001234,1/2/2026,,\"Purchase\",\"CAFE\",-4.00,,\n";
        let first = format!(
            "{HEADER}Chequing,00000-0001234,1/1/2026,,\"Purchase\",\"GROCER\",-40.00,,\n{}",
            coffee.repeat(3)
        );
        let second = format!(
            "{HEADER}{}Chequing,00000-0001234,1/3/2026,,\"Deposit\",,100.00,,\n",
            coffee.repeat(2)
        );
        let store = JournalStore::new(tmp.path().join("journal"));
        let run = |name: &str, text: &str, imported| {
            let path = file(tmp.path(), name, text);
            write(&store, &rbc(), &input(vec![path]), None, Some(imported)).unwrap()
        };
        assert_eq!(run("1.csv", &first, 1).txns, 4);
        let later = run("2.csv", &second, 2);
        assert_eq!(
            later.txn_rows,
            Rows {
                new: 1,
                changed: 0,
                unchanged: 2
            }
        );
        let ledger = ledgerdb::Ledger::load(&store, ledgerdb::LayerFilter::All)
            .unwrap()
            .ledger;
        assert_eq!(ledger.txns().len(), 5);
        assert!(
            ledger
                .find("rbc:1234:2026-01-02:-400:purchase-cafe:3")
                .is_some()
        );
    }
}
