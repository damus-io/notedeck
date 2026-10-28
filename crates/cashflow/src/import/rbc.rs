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
//!
//! **Statements.** A download only reaches back a few months, so the
//! importer also reads the monthly e-statement PDFs (see
//! [`super::rbc_statement`], which checks each one's rows against its
//! opening and closing balance). A `.pdf` path is a statement, and a
//! directory reads its `*.pdf` files beside its `*.csv` ones. A statement
//! row is filed the way a download's is, under the same account mapping,
//! with `; raw-statement:` the statement's date, a card row's posting date
//! in `; raw-posted:` and a foreign charge in `; raw-foreign:`. Its id is
//! `<source>:stmt:<last4>:<statement date>:<date>:<cents>:<description>:<n>`,
//! numbered within its statement, so re-reading a statement keeps each row
//! once and two statements never share an id.
//!
//! **Where statements and downloads overlap**, both list the same
//! transactions, and their ids can't match: a statement describes a row in
//! its own words (`e-Transfer - Autodeposit Shop C1A2B3C4D5E6`, where the
//! download has `E-TRF AUTODEPOSIT`). Their dates and amounts do match, so
//! the two are counted together by account, date and amount: for each, the
//! journal keeps as many rows as the source listing more of them has, never
//! the sum. Rows the journal already holds stay; the room left goes to a
//! download's new rows first, then a statement's, and the rest are left
//! out. Whichever source reaches the journal first keeps a day's rows, so
//! an override made on one is never stranded by the other, whichever order
//! they're imported in.

use std::collections::BTreeMap;

use async_trait::async_trait;
use ledgerdb::Date;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use ledgerdb::Ledger;

use super::csv_rows::{
    Header, Identified, RowError, files_with, id_key, number, parse_cents, parse_mdy, read,
    read_bytes, squeeze,
};
use super::rbc_statement::{self, Statement};
use super::{
    Batch, ImportError, ImportInput, Importer, LeftOut, RawAmount, RawMeta, RawPosting, RawTxn,
    pdf_text,
};
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

#[async_trait(?Send)]
impl Importer for RbcImporter {
    fn name(&self) -> &'static str {
        "rbc"
    }

    fn layer(&self) -> &'static str {
        "rbc"
    }

    async fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        self.read_all(input, None)
    }

    async fn import_held(&self, input: &ImportInput, held: &Ledger) -> Result<Batch, ImportError> {
        self.read_all(input, Some(held))
    }
}

impl RbcImporter {
    /// Every row of `input`'s downloads and statements as one batch, less
    /// the rows a source `held` in the journal already records (see the
    /// module docs on overlap).
    fn read_all(&self, input: &ImportInput, held: Option<&Ledger>) -> Result<Batch, ImportError> {
        if input.paths.is_empty() {
            return Err(ImportError::Usage(
                "`import rbc` takes one or more CSV or statement PDF files or directories"
                    .to_owned(),
            ));
        }
        let mut downloaded: BTreeMap<String, Row> = BTreeMap::new();
        let mut stated: BTreeMap<String, Row> = BTreeMap::new();
        let mut statements: BTreeSet<StatementKey> = BTreeSet::new();
        for path in files_with(&input.paths, &["csv", "pdf"])? {
            if !is_pdf(&path) {
                let file = self
                    .read_export(&read(&path)?)
                    .map_err(|error| error.in_file(&path))?;
                for Identified { id, row } in give_ids(&input.source, file) {
                    downloaded.entry(id).or_insert(row);
                }
                continue;
            }
            let statement = self.read_statement(&path)?;
            let key = StatementKey {
                last4: statement.last4.clone(),
                to: statement.to,
            };
            if !statements.insert(key) {
                continue;
            }
            for Identified { id, row } in give_statement_ids(&input.source, statement) {
                stated.insert(id, row);
            }
        }

        let mut batch_rows = Vec::new();
        let mut left_out = Vec::new();
        let held = Held::of(held, &input.source);
        for Overlap { rows, left } in overlap(downloaded, stated, &held) {
            batch_rows.extend(rows);
            left_out.extend(left);
        }
        let txns: Vec<RawTxn> = batch_rows
            .into_iter()
            .map(|Identified { id, row }| self.raw_txn(id, row))
            .collect();
        let mut batch = Batch::from_raw(&txns, &[])?;
        batch.left_out = left_out;
        if !statements.is_empty() {
            batch.notes.push(format!(
                "read {} statement{}, each balancing from its opening to its closing balance",
                statements.len(),
                if statements.len() == 1 { "" } else { "s" }
            ));
        }
        Ok(batch)
    }

    /// The statement PDF at `path`, read and balanced, its account mapped.
    fn read_statement(&self, path: &Path) -> Result<Statement, ImportError> {
        let error = |message: String| ImportError::Statement {
            path: path.to_owned(),
            message,
        };
        let lines = pdf_text::lines(&read_bytes(path)?).map_err(error)?;
        let statement = rbc_statement::parse(&lines).map_err(error)?;
        if !self.accounts.contains_key(&statement.last4) {
            return Err(error(unmapped(&statement.last4)));
        }
        Ok(statement)
    }
}

/// Which statement a PDF is: two copies of one are read once.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct StatementKey {
    last4: String,
    to: Date,
}

/// Whether `path` names a PDF, by its extension in any case.
fn is_pdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// The error for an account ending `last4` that `[rbc.accounts]` doesn't
/// map.
fn unmapped(last4: &str) -> String {
    format!(
        "the account ending {last4} has no ledger account; run `cashflow config \
         set rbc.accounts.{last4} assets:rbc:chequing` (or wherever it belongs) to map \
         it under [rbc.accounts]"
    )
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
    /// `Account Type`, as written, such as `Chequing` or `Visa`; empty on
    /// a statement row.
    account_type: String,
    /// The date of the statement a row came from; `None` for a download's.
    statement: Option<Date>,
    /// A statement card row's posting date.
    posted: Option<Date>,
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
                return Err(error(unmapped(&last4)));
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
                statement: None,
                posted: None,
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
        if let Some(posted) = row.posted {
            meta.push(RawMeta {
                key: "raw-posted".to_owned(),
                value: posted.to_string(),
            });
        }
        if let Some(statement) = row.statement {
            meta.push(RawMeta {
                key: "raw-statement".to_owned(),
                value: statement.to_string(),
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

/// Give each row of `statement` its occurrence id under `source`, the stem
/// being (last4, statement date, date, cents, description key), numbered
/// within the statement.
fn give_statement_ids(source: &str, statement: Statement) -> Vec<Identified<Row>> {
    let Statement {
        last4, to, rows, ..
    } = statement;
    let rows = rows
        .into_iter()
        .map(|row| Row {
            last4: last4.clone(),
            date: row.date,
            cents: row.cents,
            key: id_key(&row.description),
            description: row.description,
            foreign: row.foreign,
            commodity: "CAD",
            cheque: String::new(),
            account_type: String::new(),
            statement: Some(to),
            posted: row.posted,
        })
        .collect();
    number(rows, |row| {
        format!(
            "{source}:stmt:{}:{to}:{}:{}:{}",
            row.last4, row.date, row.cents, row.key
        )
    })
}

/// What both kinds of row are matched on where downloads and statements
/// overlap: a transaction's account, date and amount.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Moment {
    last4: String,
    date: Date,
    cents: i64,
}

impl Moment {
    /// The moment a record id of `source` names, and whether it is a
    /// statement's; `None` for any other id.
    fn of_id(source: &str, id: &str) -> Option<Named> {
        let rest = id.strip_prefix(source)?.strip_prefix(':')?;
        let (stated, rest) = match rest.strip_prefix("stmt:") {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        let mut parts = rest.split(':');
        let last4 = parts.next()?;
        if stated {
            parts.next()?;
        }
        let date = Date::parse(parts.next()?).ok()?;
        let cents = parts.next()?.parse().ok()?;
        Some(Named {
            moment: Moment {
                last4: last4.to_owned(),
                date,
                cents,
            },
            stated,
        })
    }
}

/// A record id's [`Moment`] and kind, from [`Moment::of_id`].
struct Named {
    moment: Moment,
    stated: bool,
}

/// The ids of `source` the journal holds, by moment and kind.
#[derive(Default)]
struct Held {
    downloaded: HashMap<Moment, BTreeSet<String>>,
    stated: HashMap<Moment, BTreeSet<String>>,
}

impl Held {
    /// The ids of `source` in `ledger`; none without one.
    fn of(ledger: Option<&Ledger>, source: &str) -> Self {
        let mut held = Self::default();
        let Some(ledger) = ledger else {
            return held;
        };
        let journal = ledger.journal();
        for txn in ledger.txns() {
            let Some(id) = txn.id.map(|id| journal.strings.get(id)) else {
                continue;
            };
            let Some(Named { moment, stated }) = Moment::of_id(source, id) else {
                continue;
            };
            let ids = if stated {
                &mut held.stated
            } else {
                &mut held.downloaded
            };
            ids.entry(moment).or_default().insert(id.to_owned());
        }
        held
    }
}

/// One moment's rows: those to import, and those left out because the
/// other source already records them.
struct Overlap {
    rows: Vec<Identified<Row>>,
    left: Vec<LeftOut>,
}

/// The `downloaded` and `stated` rows of a run, by id, against what the
/// journal `held`, sorted into what to import and what to leave out (see
/// the module docs).
fn overlap(
    downloaded: BTreeMap<String, Row>,
    stated: BTreeMap<String, Row>,
    held: &Held,
) -> Vec<Overlap> {
    /// A moment's rows of this run, downloads' and statements' apart.
    #[derive(Default)]
    struct Run {
        downloaded: Vec<Identified<Row>>,
        stated: Vec<Identified<Row>>,
    }
    let mut runs: BTreeMap<Moment, Run> = BTreeMap::new();
    let moment = |row: &Row| Moment {
        last4: row.last4.clone(),
        date: row.date,
        cents: row.cents,
    };
    for (id, row) in downloaded {
        let run = runs.entry(moment(&row)).or_default();
        run.downloaded.push(Identified { id, row });
    }
    for (id, row) in stated {
        let run = runs.entry(moment(&row)).or_default();
        run.stated.push(Identified { id, row });
    }

    let none = BTreeSet::new();
    let mut out = Vec::with_capacity(runs.len());
    for (moment, run) in runs {
        let held_downloaded = held.downloaded.get(&moment).unwrap_or(&none);
        let held_stated = held.stated.get(&moment).unwrap_or(&none);
        let count = |held: &BTreeSet<String>, run: &[Identified<Row>]| {
            held.len() + run.iter().filter(|r| !held.contains(&r.id)).count()
        };
        let want = count(held_downloaded, &run.downloaded).max(count(held_stated, &run.stated));
        let mut room = want.saturating_sub(held_downloaded.len() + held_stated.len());
        let mut rows = Vec::new();
        let mut left = Vec::new();
        let kinds = [
            (
                run.downloaded,
                held_downloaded,
                "a statement in the journal records it",
            ),
            (run.stated, held_stated, "a CSV download records it"),
        ];
        for (run_rows, held_ids, why) in kinds {
            for row in run_rows {
                if held_ids.contains(&row.id) {
                    rows.push(row);
                } else if room > 0 {
                    room -= 1;
                    rows.push(row);
                } else {
                    left.push(LeftOut { id: row.id, why });
                }
            }
        }
        out.push(Overlap { rows, left });
    }
    out
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
    #[tokio::test]
    async fn rows_become_raw_transactions() {
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
            .await
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

    #[tokio::test]
    async fn an_unmapped_account_is_an_error_without_its_number() {
        let tmp = tempfile::tempdir().unwrap();
        let path = file(
            tmp.path(),
            "a.csv",
            &format!("{HEADER}Visa,4500000000004444,4/3/2026,,SHOP,,-5.00,,\n"),
        );
        let err = rbc().import(&input(vec![path.clone()])).await.unwrap_err();
        let message = err.to_string();
        assert!(
            message.starts_with(&format!(
                "{}: row 2: the account ending 4444",
                path.display()
            )),
            "{message}"
        );
        assert!(message.contains("[rbc.accounts]"), "{message}");
        assert!(
            message.contains("cashflow config set rbc.accounts.4444 "),
            "{message}"
        );
        assert!(!message.contains("4500000000004444"), "{message}");

        // With no [rbc] table at all, the same.
        let none = RbcImporter::new(None)
            .import(&input(vec![path]))
            .await
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
    #[tokio::test]
    async fn overlapping_downloads_import_as_their_union() {
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
        let run = async |name: &str, text: &str, imported| {
            let path = file(tmp.path(), name, text);
            write(
                &store,
                &Default::default(),
                &rbc(),
                &input(vec![path]),
                None,
                Some(imported),
            )
            .await
            .unwrap()
        };
        assert_eq!(run("1.csv", &first, 1).await.txns, 4);
        let later = run("2.csv", &second, 2).await;
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

    /// The statement tests' synthetic two-page bank statement for `…1234`
    /// over a new year, and its Visa statement for `…4321`, as PDFs.
    mod statements {
        use super::*;
        use crate::import::rbc_statement::tests::{
            BAL, IN, OUT, card, cell, figure, front, header, line, pdf_of, row,
        };

        /// A bank statement whose table opens at 1,000.00: an e-transfer
        /// of 3,000.00 on Dec 30 over two lines, then on Jan 2 a fee of
        /// 12.95.
        fn bank() -> Vec<u8> {
            let mut lines = front(
                "December 6, 2025",
                "January 7, 2026",
                "1,000.00",
                "3,987.05",
            );
            lines.extend([
                header(1),
                row(1, "", "Opening Balance", vec![figure(BAL, "1,000.00")]),
                row(1, "30 Dec", "e-Transfer - Autodeposit Shop", vec![]),
                row(
                    1,
                    "",
                    "  C1A2B3C4D5E6",
                    vec![figure(IN, "3,000.00"), figure(BAL, "4,000.00")],
                ),
                line(1, vec![cell(570.0, "1 of 2")]),
                header(2),
                row(
                    2,
                    "2 Jan",
                    "Monthly fee",
                    vec![figure(OUT, "12.95"), figure(BAL, "3,987.05")],
                ),
                row(2, "", "Closing Balance", vec![figure(BAL, "$3,987.05")]),
            ]);
            pdf_of(&lines)
        }

        fn ledger_of(batch: &Batch) -> String {
            batch.journal.to_ledger(WriteOptions::default())
        }

        #[tokio::test]
        async fn statement_pdfs_become_raw_transactions() {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("statements");
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("Chequing 2026-01-07.pdf"), bank()).unwrap();
            std::fs::write(dir.join("Visa 2019-01-25.PDF"), pdf_of(&card("$58.00"))).unwrap();
            // A second copy of a statement is read once.
            std::fs::write(dir.join("copy.pdf"), bank()).unwrap();
            let batch = rbc().import(&input(vec![dir])).await.unwrap();
            assert_eq!(
                ledger_of(&batch),
                "2025-12-30 * e-Transfer - Autodeposit Shop C1A2B3C4D5E6\n\
                 \t; id: rbc:stmt:1234:2026-01-07:2025-12-30:300000:e-transfer-autodeposit-shop-c1a2b3c4d5e6:1\n\
                 \t; raw-description: e-Transfer - Autodeposit Shop C1A2B3C4D5E6\n\
                 \t; raw-statement: 2026-01-07\n\
                 \tassets:rbc:chequing\t3000.00 CAD\n\
                 \tincome:unknown\t-3000.00 CAD\n\n\
                 2026-01-02 * Monthly fee\n\
                 \t; id: rbc:stmt:1234:2026-01-07:2026-01-02:-1295:monthly-fee:1\n\
                 \t; raw-description: Monthly fee\n\
                 \t; raw-statement: 2026-01-07\n\
                 \tassets:rbc:chequing\t-12.95 CAD\n\
                 \texpenses:unknown\t12.95 CAD\n\n\
                 2018-12-23 * SOFTWARE CO\n\
                 \t; id: rbc:stmt:4321:2019-01-25:2018-12-23:-5800:software-co:1\n\
                 \t; raw-description: SOFTWARE CO\n\
                 \t; raw-foreign: 40.00 USD @ 1.450000\n\
                 \t; raw-posted: 2018-12-27\n\
                 \t; raw-statement: 2019-01-25\n\
                 \tliabilities:rbc:visa\t-58.00 CAD\n\
                 \texpenses:unknown\t58.00 CAD\n\n\
                 2019-01-14 * PAYMENT - THANK YOU\n\
                 \t; id: rbc:stmt:4321:2019-01-25:2019-01-14:1000:payment-thank-you:1\n\
                 \t; raw-description: PAYMENT - THANK YOU\n\
                 \t; raw-posted: 2019-01-14\n\
                 \t; raw-statement: 2019-01-25\n\
                 \tliabilities:rbc:visa\t10.00 CAD\n\
                 \tincome:unknown\t-10.00 CAD\n"
            );
            assert_eq!(
                batch.notes,
                ["read 2 statements, each balancing from its opening to its closing balance"]
            );
        }

        #[tokio::test]
        async fn a_statement_that_does_not_balance_names_its_file() {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("visa.pdf");
            std::fs::write(&path, pdf_of(&card("$68.00"))).unwrap();
            let err = rbc().import(&input(vec![path.clone()])).await.unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "{}: the rows take the previous balance 10.00 to 58.00, but the new \
                     balance is 68.00",
                    path.display()
                )
            );
        }

        #[tokio::test]
        async fn a_statement_of_an_unmapped_account_is_an_error() {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("bank.pdf");
            std::fs::write(&path, bank()).unwrap();
            let importer = RbcImporter::new(Some(&RbcConfig {
                accounts: BTreeMap::from([("4321".to_owned(), "liabilities:rbc:visa".to_owned())]),
            }));
            let message = importer
                .import(&input(vec![path]))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("the account ending 1234 has no ledger account"),
                "{message}"
            );
            assert!(message.contains("rbc.accounts.1234"), "{message}");
            assert!(!message.contains("00000-0001234"), "{message}");
        }

        #[test]
        fn a_file_that_is_not_a_pdf_is_an_error() {
            let tmp = tempfile::tempdir().unwrap();
            let path = file(tmp.path(), "a.pdf", "Date,Amount\n");
            let err = rbc().read_statement(&path).unwrap_err().to_string();
            assert!(err.contains("not a PDF"), "{err}");
        }

        /// A download covering the statement's Jan 2: its fee in the
        /// download's own words, and a deposit the statement doesn't reach.
        fn download() -> String {
            format!(
                "{HEADER}\
                 Chequing,00000-0001234,1/2/2026,,\"MONTHLY FEE\",,-12.95,,\n\
                 Chequing,00000-0001234,1/6/2026,,\"DEPOSIT\",,5.00,,\n"
            )
        }

        /// The ids the journal at `store` holds, in order.
        fn ids(store: &JournalStore) -> Vec<String> {
            let ledger = ledgerdb::Ledger::load(store, ledgerdb::LayerFilter::All)
                .unwrap()
                .ledger;
            let journal = ledger.journal();
            let mut ids: Vec<String> = ledger
                .txns()
                .iter()
                .filter_map(|t| t.id.map(|id| journal.strings.get(id).to_owned()))
                .collect();
            ids.sort();
            ids
        }

        /// Downloads and statements overlap on Jan 2's fee, which the
        /// journal keeps once whichever is imported first, or when both are
        /// imported in one run, where the download's row wins.
        #[tokio::test]
        async fn where_downloads_and_statements_overlap_each_row_is_kept_once() {
            let tmp = tempfile::tempdir().unwrap();
            let csv = file(tmp.path(), "a.csv", &download());
            let pdf = tmp.path().join("bank.pdf");
            std::fs::write(&pdf, bank()).unwrap();
            let run = async |store: &JournalStore, paths: Vec<PathBuf>, imported| {
                write(
                    store,
                    &Default::default(),
                    &rbc(),
                    &input(paths),
                    None,
                    Some(imported),
                )
                .await
                .unwrap()
            };
            let statement_fee = "rbc:stmt:1234:2026-01-07:2026-01-02:-1295:monthly-fee:1";
            let download_fee = "rbc:1234:2026-01-02:-1295:monthly-fee:1";
            let rest = [
                "rbc:1234:2026-01-06:500:deposit:1",
                "rbc:stmt:1234:2026-01-07:2025-12-30:300000:e-transfer-autodeposit-shop-c1a2b3c4d5e6:1",
            ];
            let with = |fee: &str| {
                let mut all: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
                all.push(fee.to_owned());
                all.sort();
                all
            };

            // The download first: the statement's fee is left out.
            let store = JournalStore::new(tmp.path().join("csv-first"));
            run(&store, vec![csv.clone()], 1).await;
            let later = run(&store, vec![pdf.clone()], 2).await;
            assert_eq!(
                later.left_out,
                [LeftOut {
                    id: statement_fee.to_owned(),
                    why: "a CSV download records it",
                }]
            );
            assert_eq!(ids(&store), with(download_fee));
            // Reading both again adds nothing.
            let again = run(&store, vec![csv.clone(), pdf.clone()], 3).await;
            assert_eq!(again.txn_rows.new, 0);
            assert_eq!(ids(&store), with(download_fee));

            // The statement first: the download's fee is left out, on
            // every later download too.
            let store = JournalStore::new(tmp.path().join("pdf-first"));
            run(&store, vec![pdf.clone()], 1).await;
            for imported in [2, 3] {
                let later = run(&store, vec![csv.clone()], imported).await;
                assert_eq!(
                    later.left_out,
                    [LeftOut {
                        id: download_fee.to_owned(),
                        why: "a statement in the journal records it",
                    }]
                );
            }
            assert_eq!(ids(&store), with(statement_fee));

            // Both in one run: the download's row.
            let store = JournalStore::new(tmp.path().join("one-run"));
            run(&store, vec![pdf, csv], 1).await;
            assert_eq!(ids(&store), with(download_fee));
        }

        /// Two identical fees on one day: a download listing one of them
        /// beside a statement listing both leaves the journal with two.
        #[test]
        fn overlap_keeps_the_most_rows_either_source_has() {
            let fee = |n: u32, statement: Option<Date>| Identified {
                id: format!("{}{n}", if statement.is_some() { "s" } else { "d" }),
                row: Row {
                    last4: "1234".to_owned(),
                    date: date("2026-01-02"),
                    cents: -1295,
                    key: "monthly-fee".to_owned(),
                    description: "Monthly fee".to_owned(),
                    foreign: String::new(),
                    commodity: "CAD",
                    cheque: String::new(),
                    account_type: String::new(),
                    statement,
                    posted: None,
                },
            };
            let by_id = |rows: Vec<Identified<Row>>| -> BTreeMap<String, Row> {
                rows.into_iter().map(|r| (r.id, r.row)).collect()
            };
            let on = Some(date("2026-01-07"));
            let overlaps = overlap(
                by_id(vec![fee(1, None)]),
                by_id(vec![fee(1, on), fee(2, on)]),
                &Held::default(),
            );
            let kept: Vec<&str> = overlaps
                .iter()
                .flat_map(|o| o.rows.iter().map(|r| r.id.as_str()))
                .collect();
            let left: Vec<&str> = overlaps
                .iter()
                .flat_map(|o| o.left.iter().map(|l| l.id.as_str()))
                .collect();
            assert_eq!(kept, ["d1", "s1"]);
            assert_eq!(left, ["s2"]);
        }

        #[test]
        fn record_ids_name_their_moment() {
            let named =
                Moment::of_id("rbc", "rbc:stmt:1234:2026-01-07:2026-01-02:-1295:fee:1").unwrap();
            assert!(named.stated);
            assert_eq!(
                named.moment,
                Moment {
                    last4: "1234".to_owned(),
                    date: date("2026-01-02"),
                    cents: -1295
                }
            );
            let named = Moment::of_id("rbc", "rbc:1234:2026-01-02:-1295:fee:1").unwrap();
            assert!(!named.stated);
            assert_eq!(named.moment.date, date("2026-01-02"));
            assert!(Moment::of_id("rbc", "amex:1234:2026-01-02:-1295:fee:1").is_none());
            assert!(Moment::of_id("rbc", "rbcx:1234:2026-01-02:-1295:fee:1").is_none());
        }
    }
}
