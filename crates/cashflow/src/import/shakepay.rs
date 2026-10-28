//! The `shakepay` importer: Shakepay's transaction history CSV for the
//! Spending account, the CAD balance behind the Shakepay card.
//!
//! An export looks like this, with the columns found by their header names:
//!
//! ```text
//! "Date","Account","Type","Description","Debit","Credit","Spot Rate","Buy / Sell Rate","Balance"
//! "2026-03-01 09:15:00","Spending","Interac e-Transfer","me@example.com",,500,,,500
//! "2026-03-01 09:20:00","Spending","Buy","Bought 0.001 BTC @ CA$100,000.00",100,,,100000.00,400
//! "2026-03-02 18:40:12","Spending","Card purchase","CORNER CAFE",4.50,,,,395.5
//! ```
//!
//! **One account.** Every row seen so far is the `Spending` account's, in
//! CAD; a row of any other account is an error rather than a guess at what
//! its amounts are in. Each row is a transaction with the account's leg
//! ([`ShakepayConfig::account`], `assets:shakepay:cad` by default), its
//! `Credit` less its `Debit`, and a counter leg taking the other side.
//!
//! **Counter legs.** A card purchase or bill payment posts against
//! `expenses:unknown` either way, so a refund lowers spending, as `amex`
//! does. Every other type posts by sign, to `expenses:unknown` when money
//! left and `income:unknown` when it came in. That covers Interac
//! e-Transfers to and from the owner's bank, which `transfer suggest` pairs
//! by amount, peer-to-peer `Send`s and `Receive`s, rewards, and `Buy`s and
//! `Sell`s of bitcoin: the export has only their CAD side, the bitcoin
//! moving in a wallet it doesn't list, so the CAD goes to the defaults and
//! the trade's rate to `; raw-rate:`.
//!
//! **Dates.** `Date` is a wall-clock time in Eastern time
//! ([`EXPORT_ZONE`], daylight saving included), Shakepay's own zone,
//! whatever zone the export was made in. The record is dated on the day
//! that instant falls on in the synced `timezone` setting, the way `cln`
//! dates its events, so a purchase at 22:00 in Vancouver isn't filed under
//! the next day.
//!
//! **Record ids.** The export has no reference column, so a row's id is the
//! occurrence id `<source>:<time>:<cents>:<type>:<n>` (see
//! [`super::csv_rows`]), `<time>` being the export's own stamp as
//! `2026-03-02T184012`. The time is to the second, so the description adds
//! nothing to tell rows apart and is left out of the id: a merchant
//! descriptor Shakepay rewords between exports keeps its record.
//!
//! Every row moves the balance, `UBER * PENDING`-style descriptors
//! included: those are the merchant's own wording, and a later export lists
//! them unchanged. So every row is cleared. The `Balance` column goes in
//! `; raw-balance:`, and a run notes where an export's balances don't
//! follow from its rows, which would mean a row is missing.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use ledgerdb::Date;

use super::csv_rows::{Header, Identified, RowError, csv_files, id_key, number, parse_cents, read};
use super::{
    Batch, ImportError, ImportInput, Importer, RawAmount, RawMeta, RawPosting, RawTxn, Synced,
};
use crate::config::ShakepayConfig;
use crate::csv;
use crate::zone::{Dating, Zone};

/// The Spending account's ledger account when `[shakepay] account` doesn't
/// name one.
pub const DEFAULT_ACCOUNT: &str = "assets:shakepay:cad";

/// The zone the export's `Date` column is written in. Shakepay withdrawals
/// to a wallet land on chain minutes after the `Buy` they follow once the
/// `Date` is read as Eastern time, four hours behind UTC in summer and five
/// in winter, and hours before it read as UTC or Pacific.
pub const EXPORT_ZONE: Tz = chrono_tz::America::Toronto;

/// The one account an export is known to hold.
const SPENDING: &str = "Spending";

/// The account's commodity.
const CAD: &str = "CAD";

/// The counter leg when money leaves the account, and for every card
/// purchase.
const SPENT: &str = "expenses:unknown";

/// The counter leg when money comes in.
const RECEIVED: &str = "income:unknown";

/// The types that are spending on the card, refunds included.
const CARD_TYPES: &[&str] = &["Card purchase", "Bill payment"];

/// Reads Shakepay Spending account CSV exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShakepayImporter {
    /// The Spending account's ledger account.
    pub account: String,
    /// `[shakepay] timezone`, the day rows fall on here, if set.
    pub timezone: Option<String>,
    /// The synced timezone setting, when the run read the vault.
    pub synced_timezone: Option<String>,
}

impl ShakepayImporter {
    /// The importer as `config.toml`'s `[shakepay]` table and the `synced`
    /// settings set it up, `None` being all defaults.
    pub fn new(config: Option<&ShakepayConfig>, synced: &Synced) -> Self {
        let config = config.cloned().unwrap_or_default();
        Self {
            account: config.account.unwrap_or_else(|| DEFAULT_ACCOUNT.to_owned()),
            timezone: config.timezone,
            synced_timezone: synced.timezone.clone(),
        }
    }
}

#[async_trait(?Send)]
impl Importer for ShakepayImporter {
    fn name(&self) -> &'static str {
        "shakepay"
    }

    fn layer(&self) -> &'static str {
        "shakepay"
    }

    async fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        if input.paths.is_empty() {
            return Err(ImportError::Usage(
                "`import shakepay` takes one or more CSV files or directories".to_owned(),
            ));
        }
        let Dating { zone, note } = Dating::pick(
            "shakepay",
            self.timezone.as_deref(),
            self.synced_timezone.as_deref(),
        )?;
        let mut notes: Vec<String> = note.into_iter().collect();
        let mut rows: BTreeMap<String, Row> = BTreeMap::new();
        for path in csv_files(&input.paths)? {
            let file = read_export(&read(&path)?, zone).map_err(|error| error.in_file(&path))?;
            if let Some(gap) = balance_gaps(&file) {
                notes.push(format!(
                    "{}: the Balance column doesn't follow from the rows {} time(s), first at \
                     row {}; the export may be missing rows",
                    path.display(),
                    gap.count,
                    gap.first_row
                ));
            }
            for Identified { id, row } in give_ids(&input.source, file) {
                rows.entry(id).or_insert(row);
            }
        }
        let txns: Vec<RawTxn> = rows
            .into_iter()
            .map(|(id, row)| self.raw_txn(id, row))
            .collect();
        let mut batch = Batch::from_raw(&txns, &[])?;
        batch.notes.extend(notes);
        Ok(batch)
    }
}

/// One export row, as read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    /// The `Date` stamp as the id writes it, `2026-03-02T184012`.
    stamp: String,
    /// Signed cents: the change to the account.
    cents: i64,
    /// [`id_key`] of the type, the id's last part. These three fields come
    /// first so rows sharing an id stem sort together.
    kind_key: String,
    /// The description, trimmed; may be empty.
    description: String,
    /// `Type`, as written.
    kind: String,
    /// The balance after the row, in cents.
    balance: i64,
    /// The day the row is filed under.
    date: Date,
    /// `Date`, as written.
    time: String,
    /// `Buy / Sell Rate`, empty when there is none.
    rate: String,
    /// `Spot Rate`, empty when there is none.
    spot: String,
    /// The export's row number, from 2.
    line: usize,
}

/// Where each column the importer reads sits in a row.
struct Columns {
    date: usize,
    account: usize,
    kind: usize,
    description: usize,
    debit: usize,
    credit: usize,
    spot: usize,
    rate: usize,
    balance: usize,
}

impl Columns {
    /// Find the columns by name in the `header` row. All nine are required:
    /// this is the one Shakepay header known.
    fn find(header: &Header) -> Result<Self, RowError> {
        Ok(Self {
            date: header.need("Date")?,
            account: header.need("Account")?,
            kind: header.need("Type")?,
            description: header.need("Description")?,
            debit: header.need("Debit")?,
            credit: header.need("Credit")?,
            spot: header.need("Spot Rate")?,
            rate: header.need("Buy / Sell Rate")?,
            balance: header.need("Balance")?,
        })
    }
}

/// Every row of one export, in its order, dated in `zone`. The first record
/// is the header; a row that doesn't read is an error, so nothing is
/// silently dropped.
fn read_export(text: &str, zone: Zone) -> Result<Vec<Row>, RowError> {
    let mut records = csv::Reader::new(text);
    let Some(header) = records.next() else {
        return Ok(Vec::new());
    };
    let header = Header::new(
        header.map(|field| field.into_owned()).collect(),
        "Date, Account, Type, Description, Debit, Credit, Spot Rate, Buy / Sell Rate, Balance",
    );
    let columns = Columns::find(&header)?;

    let mut rows = Vec::new();
    for (index, record) in records.enumerate() {
        let line = index + 2;
        let fields: Vec<_> = record.collect();
        let field = |at: usize| fields.get(at).map_or("", |f| f.as_ref()).trim();
        let error = |message: String| RowError { row: line, message };

        let account = field(columns.account);
        if account != SPENDING {
            return Err(error(format!(
                "a row of the {account:?} account; only the {SPENDING} (CAD) account's export \
                 is known, so its amounts aren't guessed at"
            )));
        }
        let time = field(columns.date);
        let when = NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S")
            .map_err(|_| error(format!("bad date {time:?}")))?;
        let kind = field(columns.kind);
        if kind.is_empty() {
            return Err(error("no Type".to_owned()));
        }
        let cents = amount(field(columns.debit), field(columns.credit)).map_err(error)?;
        let balance = field(columns.balance);
        let balance =
            parse_cents(balance).ok_or_else(|| error(format!("bad balance {balance:?}")))?;
        rows.push(Row {
            stamp: when.format("%Y-%m-%dT%H%M%S").to_string(),
            cents,
            kind_key: id_key(kind),
            description: field(columns.description).to_owned(),
            kind: kind.to_owned(),
            balance,
            date: zone.date(unix_time(when)),
            time: time.to_owned(),
            rate: field(columns.rate).to_owned(),
            spot: field(columns.spot).to_owned(),
            line,
        });
    }
    Ok(rows)
}

/// The change to the account from `debit` and `credit`, exactly one of
/// which must be filled, and neither negative.
fn amount(debit: &str, credit: &str) -> Result<i64, String> {
    let (text, sign) = match (debit.is_empty(), credit.is_empty()) {
        (false, true) => (debit, -1),
        (true, false) => (credit, 1),
        (true, true) => return Err("no amount in Debit or Credit".to_owned()),
        (false, false) => {
            return Err(format!(
                "amounts in both Debit ({debit}) and Credit ({credit})"
            ));
        }
    };
    match parse_cents(text) {
        Some(cents) if cents >= 0 => Ok(sign * cents),
        _ => Err(format!("bad amount {text:?}")),
    }
}

/// The unix time of `when`, a wall-clock time in [`EXPORT_ZONE`]. A time
/// the clocks went back over is its first occurrence; one they skipped
/// forward over is read an hour later, as the clock showed it.
fn unix_time(when: NaiveDateTime) -> u64 {
    let instant = EXPORT_ZONE
        .from_local_datetime(&when)
        .earliest()
        .or_else(|| {
            EXPORT_ZONE
                .from_local_datetime(&(when + chrono::Duration::hours(1)))
                .earliest()
        });
    let secs = instant.map_or_else(|| when.and_utc().timestamp(), |t| t.timestamp());
    secs.max(0) as u64
}

/// Where an export's balances stop following from its rows.
#[derive(Debug, PartialEq, Eq)]
struct BalanceGaps {
    /// How many rows have a balance other than the one before plus the row.
    count: usize,
    /// The export row number of the first.
    first_row: usize,
}

/// The rows of one export, in its order, whose balance isn't the one before
/// plus the row's amount, or `None` when every one follows.
fn balance_gaps(rows: &[Row]) -> Option<BalanceGaps> {
    let mut gaps: Option<BalanceGaps> = None;
    for pair in rows.windows(2) {
        let [before, row] = pair else { continue };
        if before.balance + row.cents == row.balance {
            continue;
        }
        match &mut gaps {
            Some(gaps) => gaps.count += 1,
            None => {
                gaps = Some(BalanceGaps {
                    count: 1,
                    first_row: row.line,
                })
            }
        }
    }
    gaps
}

/// Give each row of one export its occurrence id under `source`, the stem
/// being (stamp, cents, type), which [`Row`] sorts by first.
fn give_ids(source: &str, rows: Vec<Row>) -> Vec<Identified<Row>> {
    number(rows, |row| {
        format!("{source}:{}:{}:{}", row.stamp, row.cents, row.kind_key)
    })
}

impl ShakepayImporter {
    /// `row` as a raw transaction with record id `id`.
    fn raw_txn(&self, id: String, row: Row) -> RawTxn {
        let amount = |qty| {
            Some(RawAmount {
                qty,
                precision: 2,
                commodity: CAD.to_owned(),
            })
        };
        let card = CARD_TYPES.contains(&row.kind.as_str());
        let counter = if card || row.cents < 0 {
            SPENT
        } else {
            RECEIVED
        };
        let meta = |key: &str, value: String| RawMeta {
            key: key.to_owned(),
            value,
        };
        let mut metas = vec![
            meta("raw-type", row.kind.clone()),
            meta("raw-time", row.time),
            meta("raw-balance", cents_text(row.balance)),
        ];
        if !row.description.is_empty() {
            metas.insert(0, meta("raw-description", row.description.clone()));
        }
        if !row.rate.is_empty() {
            metas.push(meta("raw-rate", row.rate));
        }
        if !row.spot.is_empty() {
            metas.push(meta("raw-spot-rate", row.spot));
        }
        let payee = if row.description.is_empty() {
            row.kind
        } else {
            row.description
        };
        RawTxn {
            date: row.date,
            cleared: true,
            payee,
            id,
            meta: metas,
            postings: vec![
                RawPosting {
                    account: self.account.clone(),
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

/// `cents` as a decimal, `-1234` as `-12.34`.
fn cents_text(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Rows, write};
    use ledgerdb::{JournalStore, WriteOptions};
    use std::path::{Path, PathBuf};

    /// The header as Shakepay writes it, every name quoted.
    const HEADER: &str = "\"Date\",\"Account\",\"Type\",\"Description\",\"Debit\",\"Credit\",\
                          \"Spot Rate\",\"Buy / Sell Rate\",\"Balance\"\n";

    /// An importer dating rows in Vancouver, as the synced setting would.
    fn shakepay() -> ShakepayImporter {
        ShakepayImporter::new(
            None,
            &Synced {
                timezone: Some("America/Vancouver".to_owned()),
            },
        )
    }

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn input(paths: Vec<PathBuf>) -> ImportInput {
        ImportInput {
            source: "shakepay".to_owned(),
            paths,
        }
    }

    /// One row in Shakepay's shape, every field quoted but the empty ones
    /// and the numbers, up to its balance, which the caller appends.
    fn row(time: &str, kind: &str, description: &str, debit: &str, credit: &str) -> String {
        format!("\"{time}\",\"Spending\",\"{kind}\",\"{description}\",{debit},{credit},,")
    }

    /// Every type the export has, each in the shape it comes in: a buy and
    /// a sell with their rate, a refund on the card, a referral reward, a
    /// peer-to-peer send and receive, a description left empty, and
    /// balances that follow from the rows.
    #[tokio::test]
    async fn every_type_becomes_a_raw_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = [
            format!("{},500", row("2026-03-01 12:15:00", "Interac e-Transfer", "me@example.com", "", "500")),
            "\"2026-03-01 12:20:00\",\"Spending\",\"Buy\",\"Bought 0.001 BTC @ CA$100,000.00\",100,,,100000.00,400".to_owned(),
            format!("{},395.5", row("2026-03-02 18:40:12", "Card purchase", "CORNER CAFE", "4.50", "")),
            format!("{},397", row("2026-03-02 19:00:00", "Card purchase", "CORNER CAFE", "", "1.50")),
            format!("{},427", row("2026-03-03 10:00:00", "Reward", "Referral reward", "", "30")),
            format!("{},417", row("2026-03-03 10:05:00", "Send", "@friend", "10", "")),
            format!("{},422", row("2026-03-03 10:06:00", "Receive", "@friend", "", "5")),
            format!("{},372", row("2026-03-04 11:00:00", "Bill payment", "PHONE CO", "50", "")),
            format!("{},360", row("2026-03-04 11:30:00", "Card purchase", "", "12", "")),
            "\"2026-03-05 09:00:00\",\"Spending\",\"Sell\",\"Sold 0.002 BTC @ CA$95,000.00\",,190,,95000.00,550".to_owned(),
            format!("{},0", row("2026-03-05 09:30:00", "Interac e-Transfer", "me@example.com", "550", "")),
        ];
        let text = format!("{HEADER}{}\n", rows.join("\n"));
        let batch = shakepay()
            .import(&input(vec![file(tmp.path(), "a.csv", &text)]))
            .await
            .unwrap();
        assert_eq!(batch.notes, Vec::<String>::new());
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            "2026-03-01 * me@example.com\n\
             \t; id: shakepay:2026-03-01T121500:50000:interac-e-transfer:1\n\
             \t; raw-description: me@example.com\n\
             \t; raw-type: Interac e-Transfer\n\
             \t; raw-time: 2026-03-01 12:15:00\n\
             \t; raw-balance: 500.00\n\
             \tassets:shakepay:cad\t500.00 CAD\n\
             \tincome:unknown\t-500.00 CAD\n\n\
             2026-03-01 * Bought 0.001 BTC @ CA$100,000.00\n\
             \t; id: shakepay:2026-03-01T122000:-10000:buy:1\n\
             \t; raw-description: Bought 0.001 BTC @ CA$100,000.00\n\
             \t; raw-type: Buy\n\
             \t; raw-time: 2026-03-01 12:20:00\n\
             \t; raw-balance: 400.00\n\
             \t; raw-rate: 100000.00\n\
             \tassets:shakepay:cad\t-100.00 CAD\n\
             \texpenses:unknown\t100.00 CAD\n\n\
             2026-03-02 * CORNER CAFE\n\
             \t; id: shakepay:2026-03-02T184012:-450:card-purchase:1\n\
             \t; raw-description: CORNER CAFE\n\
             \t; raw-type: Card purchase\n\
             \t; raw-time: 2026-03-02 18:40:12\n\
             \t; raw-balance: 395.50\n\
             \tassets:shakepay:cad\t-4.50 CAD\n\
             \texpenses:unknown\t4.50 CAD\n\n\
             2026-03-02 * CORNER CAFE\n\
             \t; id: shakepay:2026-03-02T190000:150:card-purchase:1\n\
             \t; raw-description: CORNER CAFE\n\
             \t; raw-type: Card purchase\n\
             \t; raw-time: 2026-03-02 19:00:00\n\
             \t; raw-balance: 397.00\n\
             \tassets:shakepay:cad\t1.50 CAD\n\
             \texpenses:unknown\t-1.50 CAD\n\n\
             2026-03-03 * Referral reward\n\
             \t; id: shakepay:2026-03-03T100000:3000:reward:1\n\
             \t; raw-description: Referral reward\n\
             \t; raw-type: Reward\n\
             \t; raw-time: 2026-03-03 10:00:00\n\
             \t; raw-balance: 427.00\n\
             \tassets:shakepay:cad\t30.00 CAD\n\
             \tincome:unknown\t-30.00 CAD\n\n\
             2026-03-03 * @friend\n\
             \t; id: shakepay:2026-03-03T100500:-1000:send:1\n\
             \t; raw-description: @friend\n\
             \t; raw-type: Send\n\
             \t; raw-time: 2026-03-03 10:05:00\n\
             \t; raw-balance: 417.00\n\
             \tassets:shakepay:cad\t-10.00 CAD\n\
             \texpenses:unknown\t10.00 CAD\n\n\
             2026-03-03 * @friend\n\
             \t; id: shakepay:2026-03-03T100600:500:receive:1\n\
             \t; raw-description: @friend\n\
             \t; raw-type: Receive\n\
             \t; raw-time: 2026-03-03 10:06:00\n\
             \t; raw-balance: 422.00\n\
             \tassets:shakepay:cad\t5.00 CAD\n\
             \tincome:unknown\t-5.00 CAD\n\n\
             2026-03-04 * PHONE CO\n\
             \t; id: shakepay:2026-03-04T110000:-5000:bill-payment:1\n\
             \t; raw-description: PHONE CO\n\
             \t; raw-type: Bill payment\n\
             \t; raw-time: 2026-03-04 11:00:00\n\
             \t; raw-balance: 372.00\n\
             \tassets:shakepay:cad\t-50.00 CAD\n\
             \texpenses:unknown\t50.00 CAD\n\n\
             2026-03-04 * Card purchase\n\
             \t; id: shakepay:2026-03-04T113000:-1200:card-purchase:1\n\
             \t; raw-type: Card purchase\n\
             \t; raw-time: 2026-03-04 11:30:00\n\
             \t; raw-balance: 360.00\n\
             \tassets:shakepay:cad\t-12.00 CAD\n\
             \texpenses:unknown\t12.00 CAD\n\n\
             2026-03-05 * Sold 0.002 BTC @ CA$95,000.00\n\
             \t; id: shakepay:2026-03-05T090000:19000:sell:1\n\
             \t; raw-description: Sold 0.002 BTC @ CA$95,000.00\n\
             \t; raw-type: Sell\n\
             \t; raw-time: 2026-03-05 09:00:00\n\
             \t; raw-balance: 550.00\n\
             \t; raw-rate: 95000.00\n\
             \tassets:shakepay:cad\t190.00 CAD\n\
             \tincome:unknown\t-190.00 CAD\n\n\
             2026-03-05 * me@example.com\n\
             \t; id: shakepay:2026-03-05T093000:-55000:interac-e-transfer:1\n\
             \t; raw-description: me@example.com\n\
             \t; raw-type: Interac e-Transfer\n\
             \t; raw-time: 2026-03-05 09:30:00\n\
             \t; raw-balance: 0.00\n\
             \tassets:shakepay:cad\t-550.00 CAD\n\
             \texpenses:unknown\t550.00 CAD\n"
        );
    }

    /// The stamp is Eastern time: just after midnight there in winter is
    /// still the evening before in Vancouver, and in summer too, an hour
    /// later on the clock; the stamp in the id is left as written.
    #[test]
    fn rows_are_dated_in_the_synced_zone_from_eastern_time() {
        let vancouver = Zone::parse("America/Vancouver").unwrap();
        let date = |stamp: &str| {
            let when = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").unwrap();
            vancouver.date(unix_time(when)).to_string()
        };
        assert_eq!(date("2026-01-15 02:30:00"), "2026-01-14");
        assert_eq!(date("2026-01-15 03:00:00"), "2026-01-15");
        assert_eq!(date("2026-07-15 02:30:00"), "2026-07-14");
        assert_eq!(date("2026-07-15 03:00:00"), "2026-07-15");
        // 2026-03-08 02:30 never happened in Toronto: read as 03:30 EDT.
        let skipped = NaiveDateTime::parse_from_str("2026-03-08 02:30:00", "%Y-%m-%d %H:%M:%S");
        assert_eq!(unix_time(skipped.unwrap()), 1_772_955_000);
        // 2026-11-01 01:30 happened twice: the first, EDT, is 05:30 UTC.
        let twice = NaiveDateTime::parse_from_str("2026-11-01 01:30:00", "%Y-%m-%d %H:%M:%S");
        assert_eq!(unix_time(twice.unwrap()), 1_793_511_000);

        let utc = ShakepayImporter::new(
            Some(&ShakepayConfig {
                account: Some("assets:shakepay".to_owned()),
                timezone: Some("utc".to_owned()),
            }),
            &Synced::default(),
        );
        assert_eq!(utc.account, "assets:shakepay");
        let rows = read_export(
            &format!(
                "{HEADER}{},5\n",
                row("2026-01-15 20:30:00", "Card purchase", "X", "", "5")
            ),
            Zone::parse(utc.timezone.as_deref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(rows[0].date.to_string(), "2026-01-16");
        assert_eq!(rows[0].stamp, "2026-01-15T203000");
    }

    /// With no synced zone and no `[shakepay] timezone`, the run says it
    /// dated rows in this machine's zone.
    #[tokio::test]
    async fn the_machines_own_zone_is_noted() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{HEADER}{},5\n",
            row("2026-01-15 20:30:00", "Card purchase", "X", "", "5")
        );
        let batch = ShakepayImporter::new(None, &Synced::default())
            .import(&input(vec![file(tmp.path(), "a.csv", &text)]))
            .await
            .unwrap();
        assert_eq!(batch.notes.len(), 1);
        assert!(
            batch.notes[0].contains("settings set timezone"),
            "{:?}",
            batch.notes
        );
    }

    /// A balance that doesn't follow from the rows before it is noted, with
    /// where it first happens; the rows still import.
    #[tokio::test]
    async fn a_balance_gap_is_noted() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{HEADER}{},100\n{},90\n{},70\n{},60\n",
            row(
                "2026-01-01 12:00:00",
                "Interac e-Transfer",
                "me@example.com",
                "",
                "100"
            ),
            row("2026-01-02 12:00:00", "Card purchase", "SHOP", "10", ""),
            row("2026-01-04 12:00:00", "Card purchase", "SHOP", "10", ""),
            row("2026-01-05 12:00:00", "Card purchase", "SHOP", "10", ""),
        );
        let path = file(tmp.path(), "a.csv", &text);
        let batch = shakepay().import(&input(vec![path.clone()])).await.unwrap();
        assert_eq!(batch.journal.txns.len(), 4);
        assert_eq!(
            batch.notes,
            [format!(
                "{}: the Balance column doesn't follow from the rows 1 time(s), first at row 4; \
                 the export may be missing rows",
                path.display()
            )]
        );
    }

    #[test]
    fn a_header_without_a_column_fails_quoting_it() {
        let err = read_export("\"Date\",\"Amount\"\n", Zone::Fixed(0)).unwrap_err();
        assert_eq!(err.row, 1);
        assert!(
            err.message
                .starts_with("no \"Account\" column; the header is \"Date,Amount\""),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_row_that_does_not_read_is_an_error() {
        let rows = |row: &str| {
            read_export(&format!("{HEADER}{row}\n"), Zone::Fixed(0))
                .unwrap_err()
                .message
        };
        let ok = "\"2026-01-01 12:00:00\",\"Spending\",\"Send\",\"@a\",5,,,,0";
        assert!(read_export(&format!("{HEADER}{ok}\n"), Zone::Fixed(0)).is_ok());
        assert!(rows(&ok.replace("Spending", "Bitcoin")).contains("\"Bitcoin\" account"));
        assert!(rows(&ok.replace("2026-01-01 12:00:00", "2026-01-01")).contains("bad date"));
        assert!(rows(&ok.replace("\"Send\"", "\"\"")).contains("no Type"));
        assert!(rows(&ok.replace(",5,,", ",,,")).contains("no amount"));
        assert!(rows(&ok.replace(",5,,", ",5,5,")).contains("both"));
        assert!(rows(&ok.replace(",5,,", ",-5,,")).contains("bad amount"));
        assert!(rows(&ok.replace(",5,,", ",lots,,")).contains("bad amount"));
        assert!(rows(&ok.replace(",,0", ",,")).contains("bad balance"));
    }

    /// Two exports that overlap, the second listing one of two identical
    /// coffees in one second: the union is imported, each row once. A third
    /// rewording a merchant keeps its record and rewrites it.
    #[tokio::test]
    async fn overlapping_exports_import_as_their_union() {
        let tmp = tempfile::tempdir().unwrap();
        let coffee = |balance: &str| {
            format!(
                "{},{balance}\n",
                row("2026-01-02 12:00:00", "Card purchase", "CAFE", "4", "")
            )
        };
        let first = format!(
            "{HEADER}{},100\n{}{}",
            row(
                "2026-01-01 12:00:00",
                "Interac e-Transfer",
                "me@example.com",
                "",
                "100"
            ),
            coffee("96"),
            coffee("92")
        );
        let second = format!(
            "{HEADER}{}{},82\n",
            coffee("92"),
            // Reworded from one export to the next: still the same record.
            row(
                "2026-01-03 12:00:00",
                "Card purchase",
                "RIDE * PENDING",
                "10",
                ""
            )
        );
        let third = format!(
            "{HEADER}{},82\n",
            row("2026-01-03 12:00:00", "Card purchase", "RIDE CO", "10", "")
        );
        let store = JournalStore::new(tmp.path().join("journal"));
        let run = async |name: &str, text: &str, imported| {
            let path = file(tmp.path(), name, text);
            write(
                &store,
                &Default::default(),
                &shakepay(),
                &input(vec![path]),
                None,
                Some(imported),
            )
            .await
            .unwrap()
        };
        assert_eq!(run("1.csv", &first, 1).await.txns, 3);
        let later = run("2.csv", &second, 2).await;
        assert_eq!(
            later.txn_rows,
            Rows {
                new: 1,
                changed: 0,
                unchanged: 1
            }
        );
        let reworded = run("3.csv", &third, 3).await;
        assert_eq!(
            reworded.txn_rows,
            Rows {
                new: 0,
                changed: 1,
                unchanged: 0
            }
        );
        let ledger = ledgerdb::Ledger::load(&store, ledgerdb::LayerFilter::All)
            .unwrap()
            .ledger;
        assert_eq!(ledger.txns().len(), 4);
        assert!(
            ledger
                .find("shakepay:2026-01-02T120000:-400:card-purchase:2")
                .is_some()
        );
        assert!(
            ledger
                .find("shakepay:2026-01-03T120000:-1000:card-purchase:1")
                .is_some()
        );
    }

    #[test]
    fn cents_as_text() {
        assert_eq!(cents_text(0), "0.00");
        assert_eq!(cents_text(39550), "395.50");
        assert_eq!(cents_text(-5), "-0.05");
    }
}
