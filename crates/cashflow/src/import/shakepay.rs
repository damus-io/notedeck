//! The `shakepay` importer: Shakepay's transaction history CSV for the
//! Spending account, the CAD balance behind the Shakepay card, and its
//! crypto export, the bitcoin side (see [`super::shakepay_crypto`]). Each
//! file is read as whichever of the two its header says it is.
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
//! does. Every other type but a trade posts by sign, to `expenses:unknown`
//! when money left and `income:unknown` when it came in. That covers
//! Interac e-Transfers to and from the owner's bank, which `transfer
//! suggest` pairs by amount, peer-to-peer `Send`s and `Receive`s, and
//! rewards.
//!
//! **Trades.** A `Buy` or `Sell` swaps CAD for bitcoin, so it is neither
//! spending nor income. Its description (`Sold 0.002 BTC @ CA$95,000.00`)
//! gives the bitcoin, which posts in `msat` to
//! [`ShakepayConfig::btc_account`] (`assets:shakepay:btc`), the commodity
//! the `btc` importer writes. An elided [`CONVERSION`] leg balances the two
//! commodities, as ledger-cli's trading accounts do, so the record holds no
//! uncategorized leg. The export doesn't list the bitcoin's deposits and
//! withdrawals: the crypto export does, and with both imported the BTC
//! account nets to what Shakepay holds.
//!
//! **The crypto export** holds the same trades, as one bitcoin leg each.
//! Those are left out: the Spending export's swap already holds the trade's
//! bitcoin, so writing the row too would count it twice. A trade is matched
//! to a swap moving the same msat stamped at most [`TRADE_SLACK_SECS`]
//! apart (the two exports differ by a second or so on some), among the
//! swaps the journal holds and those of the Spending exports read in the
//! same run, so either file may be imported first. A trade no swap holds is
//! left out too, and said so: it waits for the Spending export.
//!
//! Every other crypto row moves bitcoin on [`ShakepayConfig::btc_account`]
//! against `income:unknown` (a reward or a deposit) or `expenses:unknown`
//! (a withdrawal or a send to another user), so `transfer suggest` pairs a
//! deposit with the wallet send that funded it, and a withdrawal with the
//! wallet receive at the address it paid. Each reward is its own record,
//! its description the payee, so a rule sorts cashback from referrals.
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

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use ledgerdb::{Date, Ledger};

use super::csv_rows::{Header, Identified, RowError, csv_files, id_key, number, parse_cents, read};
use super::shakepay_crypto::{CryptoRow, is_crypto, read_crypto};
use super::{
    Batch, ImportError, ImportInput, Importer, LeftOut, RawAmount, RawMeta, RawPosting, RawTxn,
    Synced,
};
use crate::config::ShakepayConfig;
use crate::csv;
use crate::prices::MSAT;
use crate::zone::{Dating, Zone};

/// The Spending account's ledger account when `[shakepay] account` doesn't
/// name one.
pub const DEFAULT_ACCOUNT: &str = "assets:shakepay:cad";

/// The bitcoin a `Buy` or `Sell` trades goes to or comes from this account
/// when `[shakepay] btc_account` doesn't name one.
pub const DEFAULT_BTC_ACCOUNT: &str = "assets:shakepay:btc";

/// The elided leg that balances a trade's two commodities.
pub const CONVERSION: &str = "equity:conversion";

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

/// Millisatoshis in a satoshi.
const MSAT_PER_SAT: i64 = 1000;

/// Decimal places of a satoshi in bitcoin.
const SAT_DIGITS: i64 = 8;

/// The types that are spending on the card, refunds included.
const CARD_TYPES: &[&str] = &["Card purchase", "Bill payment"];

/// Reads Shakepay's Spending account and crypto CSV exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShakepayImporter {
    /// The Spending account's ledger account.
    pub account: String,
    /// The account a trade's bitcoin posts to.
    pub btc_account: String,
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
            btc_account: config
                .btc_account
                .unwrap_or_else(|| DEFAULT_BTC_ACCOUNT.to_owned()),
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
        self.read(input, Vec::new())
    }

    /// [`import`](Importer::import), with the swaps the journal already
    /// `held` to leave the crypto export's trades out against.
    async fn import_held(&self, input: &ImportInput, held: &Ledger) -> Result<Batch, ImportError> {
        self.read(input, self.held_swaps(held))
    }
}

impl ShakepayImporter {
    /// Read `input`'s exports, of either format, into one batch, leaving out
    /// each crypto export trade that one of the `held` swaps or a swap of
    /// this run's Spending exports already holds.
    fn read(&self, input: &ImportInput, held: Vec<Swap>) -> Result<Batch, ImportError> {
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
        let mut crypto: BTreeMap<String, CryptoRow> = BTreeMap::new();
        for path in csv_files(&input.paths)? {
            let text = read(&path)?;
            if is_crypto(&header_of(&text)) {
                let file = read_crypto(&text, zone).map_err(|error| error.in_file(&path))?;
                for Identified { id, row } in give_crypto_ids(&input.source, file) {
                    crypto.entry(id).or_insert(row);
                }
                continue;
            }
            let file = read_export(&text, zone).map_err(|error| error.in_file(&path))?;
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

        let mut swaps = Swaps::new(held, &rows);
        let mut held_trades = 0;
        let mut left_out = Vec::new();
        let mut txns: Vec<RawTxn> = rows
            .into_iter()
            .map(|(id, row)| self.raw_txn(id, row))
            .collect();
        for (id, row) in crypto {
            if !TRADE_TYPES.contains(&row.kind.as_str()) {
                txns.push(self.crypto_txn(id, row));
            } else if swaps.take(row.msat, row.when) {
                held_trades += 1;
            } else {
                left_out.push(LeftOut {
                    id,
                    why: "a trade no Spending export swap holds; import the Spending export",
                });
            }
        }
        if held_trades > 0 {
            notes.push(format!(
                "{held_trades} Buy/Sell row(s) of the crypto export left out: the Spending \
                 export's swaps already hold their bitcoin"
            ));
        }
        let mut batch = Batch::from_raw(&txns, &[])?;
        batch.notes.extend(notes);
        batch.left_out = left_out;
        Ok(batch)
    }

    /// The Spending export's swaps the journal `held`: every record with a
    /// `Buy` or `Sell` `raw-type`, a `raw-time` and a leg in msat on the
    /// bitcoin account.
    fn held_swaps(&self, held: &Ledger) -> Vec<Swap> {
        let journal = held.journal();
        let (Some(btc), Some(msat)) = (
            journal.accounts.lookup(&self.btc_account),
            journal.commodities.get(MSAT),
        ) else {
            return Vec::new();
        };
        held.txns()
            .iter()
            .filter_map(|txn| {
                let kind = journal.txn_meta(txn, "raw-type")?;
                if !TRADE_TYPES.contains(&kind) {
                    return None;
                }
                let when = parse_time(journal.txn_meta(txn, "raw-time")?)?;
                let leg = journal
                    .postings_of(txn)
                    .iter()
                    .find(|p| p.account == btc && p.commodity == msat)?;
                Some(Swap {
                    id: journal.strings.get(txn.id?).to_owned(),
                    msat: leg.qty,
                    when,
                })
            })
            .collect()
    }
}

/// The trade types, which the Spending export writes as swaps.
const TRADE_TYPES: &[&str] = &["Buy", "Sell"];

/// How many seconds apart the two exports may stamp one trade. They agree
/// to the second on most trades and differ by a few on the rest.
const TRADE_SLACK_SECS: i64 = 60;

/// A Spending export swap, as the crypto export's trades are matched
/// against it.
struct Swap {
    /// Its record id, so a swap both held and re-read counts once.
    id: String,
    /// The bitcoin it moves, signed, in msat.
    msat: i64,
    when: NaiveDateTime,
}

/// The swaps a crypto export's trades can be matched to, each at most once.
struct Swaps {
    /// Each swap's time and whether a trade took it, by its signed msat.
    by_msat: HashMap<i64, Vec<Slot>>,
}

/// One swap a trade can take.
struct Slot {
    when: NaiveDateTime,
    taken: bool,
}

impl Swaps {
    /// The `held` swaps and those of this run's Spending export `rows`.
    fn new(held: Vec<Swap>, rows: &BTreeMap<String, Row>) -> Self {
        let read = rows.iter().filter_map(|(id, row)| {
            Some(Swap {
                id: id.clone(),
                msat: row.msat?,
                when: parse_time(&row.time)?,
            })
        });
        let mut by_id: BTreeMap<String, Swap> = BTreeMap::new();
        for swap in held.into_iter().chain(read) {
            by_id.insert(swap.id.clone(), swap);
        }
        let mut by_msat: HashMap<i64, Vec<Slot>> = HashMap::new();
        for swap in by_id.into_values() {
            by_msat.entry(swap.msat).or_default().push(Slot {
                when: swap.when,
                taken: false,
            });
        }
        Self { by_msat }
    }

    /// Take the untaken swap moving `msat` stamped nearest `when`, at most
    /// [`TRADE_SLACK_SECS`] away, and return whether there was one.
    fn take(&mut self, msat: i64, when: NaiveDateTime) -> bool {
        let Some(slots) = self.by_msat.get_mut(&msat) else {
            return false;
        };
        let nearest = slots
            .iter_mut()
            .filter(|slot| !slot.taken)
            .map(|slot| ((slot.when - when).num_seconds().abs(), slot))
            .filter(|(secs, _)| *secs <= TRADE_SLACK_SECS)
            .min_by_key(|(secs, _)| *secs);
        let Some((_, slot)) = nearest else {
            return false;
        };
        slot.taken = true;
        true
    }
}

/// The header row of an export, empty when it has none.
fn header_of(text: &str) -> Vec<String> {
    csv::Reader::new(text)
        .next()
        .map(|header| header.map(|field| field.into_owned()).collect())
        .unwrap_or_default()
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
    /// The bitcoin a `Buy` (positive) or `Sell` (negative) trades, in msat.
    msat: Option<i64>,
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
        let when = parse_time(time).ok_or_else(|| error(format!("bad date {time:?}")))?;
        let kind = field(columns.kind);
        if kind.is_empty() {
            return Err(error("no Type".to_owned()));
        }
        let cents = amount(field(columns.debit), field(columns.credit)).map_err(error)?;
        let description = field(columns.description);
        let msat = trade(kind, description, cents).map_err(error)?;
        let balance = field(columns.balance);
        let balance =
            parse_cents(balance).ok_or_else(|| error(format!("bad balance {balance:?}")))?;
        rows.push(Row {
            stamp: when.format("%Y-%m-%dT%H%M%S").to_string(),
            cents,
            kind_key: id_key(kind),
            description: description.to_owned(),
            kind: kind.to_owned(),
            msat,
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

/// The bitcoin a row of type `kind` trades, in msat, signed as it moves the
/// bitcoin account: `Some` for a `Buy` or `Sell`, `None` for any other type.
/// A trade's `description` must read `Bought|Sold <qty> BTC @ CA$<price>`,
/// matching its type, and its CAD must move the other way: a buy spends
/// (`cents` negative) and a sell receives.
fn trade(kind: &str, description: &str, cents: i64) -> Result<Option<i64>, String> {
    let (verb, sign) = match kind {
        "Buy" => ("Bought", 1),
        "Sell" => ("Sold", -1),
        _ => return Ok(None),
    };
    let bad = || {
        format!("a {kind} whose description {description:?} isn't `{verb} <qty> BTC @ CA$<price>`")
    };
    let mut parts = description.split(' ');
    let (Some(said), Some(qty), Some("BTC"), Some("@"), Some(price), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(bad());
    };
    if said != verb || !price.starts_with("CA$") {
        return Err(bad());
    }
    let msat = btc_msat(qty).ok_or_else(bad)?;
    if cents.signum() != -sign {
        return Err(format!(
            "a {kind} that {} CAD",
            if cents < 0 { "spends" } else { "receives" }
        ));
    }
    Ok(Some(sign * msat))
}

/// A bitcoin quantity such as `0.0141287` or `1.5e-5` in msat, rounded to
/// the satoshi, half up: Shakepay moves whole satoshis, but some exports
/// write the quantity with a float's noise (`0.009899979999999999`) or in
/// scientific notation. The text is read exactly, never through an `f64`.
pub(super) fn btc_msat(qty: &str) -> Option<i64> {
    let (mantissa, exponent) = match qty.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (qty, 0),
    };
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) {
        return None;
    }
    let digits = format!("{whole}{frac}");
    let digits = digits.trim_start_matches('0');
    let value: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    // The quantity is `value` × 10^(exponent − decimals) bitcoin, so this
    // many powers of ten more satoshis.
    let shift = i64::from(exponent) + SAT_DIGITS - frac.len() as i64;
    let sats = if shift >= 0 {
        value.checked_mul(10i128.checked_pow(u32::try_from(shift).ok()?)?)?
    } else {
        let unit = 10i128.checked_pow(u32::try_from(-shift).ok()?)?;
        (value + unit / 2) / unit
    };
    i64::try_from(sats.checked_mul(i128::from(MSAT_PER_SAT))?).ok()
}

/// A `Date` column's text, `2026-03-02 18:40:12`, read.
pub(super) fn parse_time(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()
}

/// The unix time of `when`, a wall-clock time in [`EXPORT_ZONE`]. A time
/// the clocks went back over is its first occurrence; one they skipped
/// forward over is read an hour later, as the clock showed it.
pub(super) fn unix_time(when: NaiveDateTime) -> u64 {
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

/// Give each row of one crypto export its occurrence id under `source`,
/// the stem being (stamp, sats, type), which [`CryptoRow`] sorts by first.
fn give_crypto_ids(source: &str, rows: Vec<CryptoRow>) -> Vec<Identified<CryptoRow>> {
    number(rows, |row| {
        format!(
            "{source}:btc:{}:{}:{}",
            row.stamp,
            row.msat / MSAT_PER_SAT,
            row.kind_key
        )
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
        let cad = RawPosting {
            account: self.account.clone(),
            amount: amount(row.cents),
        };
        let postings = match row.msat {
            Some(msat) => vec![
                cad,
                RawPosting {
                    account: self.btc_account.clone(),
                    amount: Some(RawAmount {
                        qty: msat,
                        precision: 0,
                        commodity: MSAT.to_owned(),
                    }),
                },
                RawPosting {
                    account: CONVERSION.to_owned(),
                    amount: None,
                },
            ],
            None => vec![
                cad,
                RawPosting {
                    account: counter.to_owned(),
                    amount: amount(-row.cents),
                },
            ],
        };
        RawTxn {
            date: row.date,
            cleared: true,
            payee,
            id,
            meta: metas,
            postings,
        }
    }
}

impl ShakepayImporter {
    /// A crypto export `row` that isn't a trade as a raw transaction with
    /// record id `id`: its bitcoin on the bitcoin account, against
    /// `income:unknown` when it came in and `expenses:unknown` when it left.
    fn crypto_txn(&self, id: String, row: CryptoRow) -> RawTxn {
        let amount = |qty| {
            Some(RawAmount {
                qty,
                precision: 0,
                commodity: MSAT.to_owned(),
            })
        };
        let counter = if row.msat < 0 { SPENT } else { RECEIVED };
        let meta = |key: &str, value: &str| RawMeta {
            key: key.to_owned(),
            value: value.to_owned(),
        };
        let mut metas = Vec::new();
        if !row.description.is_empty() {
            metas.push(meta("raw-description", &row.description));
        }
        metas.push(meta("raw-type", &row.kind));
        metas.push(meta("raw-time", &row.time));
        if let Some(address) = row.address() {
            metas.push(meta("raw-address", address));
        }
        let optional = [
            ("raw-market-value", &row.market_value),
            ("raw-book-cost", &row.book_cost),
            ("raw-spot-rate", &row.spot),
            ("raw-rate", &row.rate),
        ];
        for (key, value) in optional {
            if !value.is_empty() {
                metas.push(meta(key, value));
            }
        }
        RawTxn {
            date: row.date,
            cleared: true,
            payee: row.payee().to_owned(),
            id,
            meta: metas,
            postings: vec![
                RawPosting {
                    account: self.btc_account.clone(),
                    amount: amount(row.msat),
                },
                RawPosting {
                    account: counter.to_owned(),
                    amount: amount(-row.msat),
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
    use crate::import::shakepay_crypto::read_crypto;
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
    /// a sell swapping CAD for bitcoin, a refund on the card, a referral reward, a
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
             \tassets:shakepay:btc\t100000000 msat\n\
             \tequity:conversion\n\n\
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
             \tassets:shakepay:btc\t-200000000 msat\n\
             \tequity:conversion\n\n\
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
                btc_account: None,
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
    fn btc_quantities_round_to_the_satoshi() {
        assert_eq!(btc_msat("0.0141287"), Some(1_412_870_000));
        assert_eq!(btc_msat("1"), Some(100_000_000_000));
        assert_eq!(btc_msat("0.00000001"), Some(1_000));
        // A float's noise rounds to the satoshi it meant.
        assert_eq!(btc_msat("0.009899979999999999"), Some(989_998_000));
        assert_eq!(btc_msat("0.012300000000000001"), Some(1_230_000_000));
        // Scientific notation, read from the text: no float in between.
        assert_eq!(btc_msat("1.5e-5"), Some(1_500_000));
        assert_eq!(btc_msat("1e-3"), Some(100_000_000));
        assert_eq!(btc_msat("8E-8"), Some(8_000));
        assert_eq!(btc_msat("1.2340000000000001e-4"), Some(12_340_000));
        assert_eq!(btc_msat("2.5e+1"), Some(2_500_000_000_000));
        // Half a satoshi rounds up, less rounds down.
        assert_eq!(btc_msat("5e-9"), Some(1_000));
        assert_eq!(btc_msat("4.9e-9"), Some(0));
        for bad in ["", ".5", "1.2.3", "-1", "0,5", "1e", "e-5", "1e-x", "1e-99"] {
            assert_eq!(btc_msat(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_trade_reads_its_bitcoin_or_fails() {
        let buy = |description: &str, cents| trade("Buy", description, cents);
        assert_eq!(
            buy("Bought 0.001 BTC @ CA$100,000.00", -10_000),
            Ok(Some(100_000_000))
        );
        assert_eq!(
            trade("Sell", "Sold 0.002 BTC @ CA$95,000.00", 19_000),
            Ok(Some(-200_000_000))
        );
        assert_eq!(
            trade("Card purchase", "Bought 1 BTC @ CA$1", -100),
            Ok(None)
        );
        for bad in [
            "Sold 0.001 BTC @ CA$100,000.00",
            "Bought 0.001 ETH @ CA$100,000.00",
            "Bought 0.001 BTC @ US$100,000.00",
            "Bought 0.001 BTC",
            "Bought lots BTC @ CA$1",
        ] {
            assert!(
                buy(bad, -10_000).unwrap_err().contains("isn't `Bought"),
                "{bad}"
            );
        }
        assert!(
            buy("Bought 0.001 BTC @ CA$100,000.00", 10_000)
                .unwrap_err()
                .contains("a Buy that receives CAD")
        );
    }

    /// The crypto export's header as Shakepay writes it, every name quoted.
    const CRYPTO_HEADER: &str = "\"Date\",\"Account\",\"Amount Debited\",\"Asset Debited\",\
                                 \"Amount Credited\",\"Asset Credited\",\"Market Value\",\
                                 \"Market Value Currency\",\"Book Cost\",\"Book Cost Currency\",\
                                 \"Type\",\"Spot Rate\",\"Buy / Sell Rate\",\"Description\"\n";

    /// One crypto export row in Shakepay's shape: text quoted, numbers and
    /// empty fields bare. `btc` is credited, or debited when it starts with
    /// `-`; `cad` is both its market value and its book cost.
    fn crypto_row(time: &str, kind: &str, btc: &str, cad: &str, description: &str) -> String {
        let legs = match btc.strip_prefix('-') {
            Some(debited) => format!("{debited},\"BTC\",,"),
            None => format!(",,{btc},\"BTC\""),
        };
        let rate = if TRADE_TYPES.contains(&kind) {
            "100000.00"
        } else {
            ""
        };
        format!(
            "\"{time}\",\"Spending\",{legs},{cad},\"CAD\",{cad},\"CAD\",\"{kind}\",100000,{rate},\
             \"{description}\""
        )
    }

    /// A crypto export of `rows`.
    fn crypto_export(rows: &[String]) -> String {
        format!("{CRYPTO_HEADER}{}", rows.join("\n"))
    }

    /// Every type the crypto export has, each in the shape it comes in:
    /// rewards in scientific notation and with a float's noise, a deposit
    /// listing its sending addresses (the last cut off), one by Lightning,
    /// a withdrawal to an address and a send to another user. A trade no
    /// Spending swap holds is left out, never written as a second swap.
    #[tokio::test]
    async fn every_crypto_type_becomes_a_raw_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let text = crypto_export(&[
            crypto_row(
                "2026-03-01 12:00:00",
                "Reward",
                "1.5e-5",
                "1.5",
                "Bitcoin cashback",
            ),
            crypto_row(
                "2026-03-01 13:00:00",
                "Reward",
                "0.000012340000000001",
                "1.23",
                "ShakeSquad",
            ),
            crypto_row(
                "2026-03-02 09:00:00",
                "Receive",
                "0.002",
                "200",
                "Bitcoin address bc1qsenderone,bc1qsendertwo,bc",
            ),
            crypto_row(
                "2026-03-02 10:00:00",
                "Receive",
                "1e-4",
                "10",
                "Lightning invoice lnbc1example",
            ),
            crypto_row(
                "2026-03-03 11:00:00",
                "Send",
                "-0.0015",
                "150",
                "Bitcoin address bc1qmywallet",
            ),
            crypto_row("2026-03-03 12:00:00", "Send", "-5e-5", "5", "@friend"),
            crypto_row(
                "2026-03-04 12:00:00",
                "Buy",
                "0.001",
                "100",
                "Bought @ CA$100,000.00",
            ),
        ]);
        let batch = shakepay()
            .import(&input(vec![file(tmp.path(), "crypto.csv", &text)]))
            .await
            .unwrap();
        assert_eq!(batch.notes, Vec::<String>::new());
        assert_eq!(
            batch.left_out,
            [LeftOut {
                id: "shakepay:btc:2026-03-04T120000:100000:buy:1".to_owned(),
                why: "a trade no Spending export swap holds; import the Spending export",
            }]
        );
        assert_eq!(
            batch.journal.to_ledger(WriteOptions::default()),
            "2026-03-01 * Bitcoin cashback\n\
             \t; id: shakepay:btc:2026-03-01T120000:1500:reward:1\n\
             \t; raw-description: Bitcoin cashback\n\
             \t; raw-type: Reward\n\
             \t; raw-time: 2026-03-01 12:00:00\n\
             \t; raw-market-value: 1.5 CAD\n\
             \t; raw-book-cost: 1.5 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t1500000 msat\n\
             \tincome:unknown\t-1500000 msat\n\
             \n\
             2026-03-01 * ShakeSquad\n\
             \t; id: shakepay:btc:2026-03-01T130000:1234:reward:1\n\
             \t; raw-description: ShakeSquad\n\
             \t; raw-type: Reward\n\
             \t; raw-time: 2026-03-01 13:00:00\n\
             \t; raw-market-value: 1.23 CAD\n\
             \t; raw-book-cost: 1.23 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t1234000 msat\n\
             \tincome:unknown\t-1234000 msat\n\
             \n\
             2026-03-02 * Receive\n\
             \t; id: shakepay:btc:2026-03-02T090000:200000:receive:1\n\
             \t; raw-description: Bitcoin address bc1qsenderone,bc1qsendertwo,bc\n\
             \t; raw-type: Receive\n\
             \t; raw-time: 2026-03-02 09:00:00\n\
             \t; raw-market-value: 200 CAD\n\
             \t; raw-book-cost: 200 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t200000000 msat\n\
             \tincome:unknown\t-200000000 msat\n\
             \n\
             2026-03-02 * Receive\n\
             \t; id: shakepay:btc:2026-03-02T100000:10000:receive:1\n\
             \t; raw-description: Lightning invoice lnbc1example\n\
             \t; raw-type: Receive\n\
             \t; raw-time: 2026-03-02 10:00:00\n\
             \t; raw-market-value: 10 CAD\n\
             \t; raw-book-cost: 10 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t10000000 msat\n\
             \tincome:unknown\t-10000000 msat\n\
             \n\
             2026-03-03 * Send\n\
             \t; id: shakepay:btc:2026-03-03T110000:-150000:send:1\n\
             \t; raw-description: Bitcoin address bc1qmywallet\n\
             \t; raw-type: Send\n\
             \t; raw-time: 2026-03-03 11:00:00\n\
             \t; raw-address: bc1qmywallet\n\
             \t; raw-market-value: 150 CAD\n\
             \t; raw-book-cost: 150 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t-150000000 msat\n\
             \texpenses:unknown\t150000000 msat\n\
             \n\
             2026-03-03 * @friend\n\
             \t; id: shakepay:btc:2026-03-03T120000:-5000:send:1\n\
             \t; raw-description: @friend\n\
             \t; raw-type: Send\n\
             \t; raw-time: 2026-03-03 12:00:00\n\
             \t; raw-market-value: 5 CAD\n\
             \t; raw-book-cost: 5 CAD\n\
             \t; raw-spot-rate: 100000\n\
             \tassets:shakepay:btc\t-5000000 msat\n\
             \texpenses:unknown\t5000000 msat\n"
        );
    }

    /// The crypto export's trades are the Spending export's swaps, stamped
    /// the same or a second or so apart: read together, or in either order,
    /// each trade's bitcoin is written once, by the swap.
    #[tokio::test]
    async fn trades_the_spending_export_holds_are_not_written_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let spending = file(
            tmp.path(),
            "spending.csv",
            &format!(
                "{HEADER}{}\n{}\n",
                "\"2026-03-01 12:20:00\",\"Spending\",\"Buy\",\"Bought 0.001 BTC @ CA$100,000.00\",100,,,100000.00,0",
                "\"2026-03-05 09:00:00\",\"Spending\",\"Sell\",\"Sold 0.0005 BTC @ CA$100,000.00\",,50,,100000.00,50",
            ),
        );
        let crypto = file(
            tmp.path(),
            "crypto.csv",
            &crypto_export(&[
                crypto_row(
                    "2026-03-01 12:20:01",
                    "Buy",
                    "0.001",
                    "100",
                    "Bought @ CA$100,000.00",
                ),
                crypto_row(
                    "2026-03-02 08:00:00",
                    "Reward",
                    "2e-5",
                    "2",
                    "Bitcoin cashback",
                ),
                crypto_row(
                    "2026-03-05 09:00:00",
                    "Sell",
                    "-0.0005",
                    "50",
                    "Sold @ CA$100,000.00",
                ),
            ]),
        );
        let held = |n: usize| {
            format!(
                "{n} Buy/Sell row(s) of the crypto export left out: the Spending export's swaps \
                 already hold their bitcoin"
            )
        };

        let together = shakepay()
            .import(&input(vec![spending.clone(), crypto.clone()]))
            .await
            .unwrap();
        assert_eq!(together.notes, [held(2)]);
        assert!(together.left_out.is_empty());
        assert_eq!(together.journal.txns.len(), 3);

        // Crypto first: its trades wait for the Spending export, which then
        // writes them, and a second crypto import finds them held.
        let store = JournalStore::new(tmp.path().join("journal"));
        let run = async |path: &PathBuf, imported| {
            write(
                &store,
                &Default::default(),
                &shakepay(),
                &input(vec![path.clone()]),
                None,
                Some(imported),
            )
            .await
            .unwrap()
        };
        let first = run(&crypto, 1).await;
        assert_eq!((first.txns, first.left_out.len()), (1, 2));
        assert_eq!(run(&spending, 2).await.txns, 2);
        let again = run(&crypto, 3).await;
        assert_eq!(again.notes, [held(2)]);
        assert!(again.left_out.is_empty());
        assert_eq!(again.txns, 0);

        let ledger = ledgerdb::Ledger::load(&store, ledgerdb::LayerFilter::All)
            .unwrap()
            .ledger;
        let journal = ledger.journal();
        let btc = journal.accounts.lookup(DEFAULT_BTC_ACCOUNT).unwrap();
        let msat: i64 = ledger.postings_to(btc).map(|p| p.qty).sum();
        assert_eq!(msat, (100_000 + 2_000 - 50_000) * MSAT_PER_SAT);
    }

    /// Two crypto exports that overlap, the first with two identical
    /// rewards in one second and the second with one of them: the union is
    /// imported, each row once.
    #[tokio::test]
    async fn overlapping_crypto_exports_import_as_their_union() {
        let tmp = tempfile::tempdir().unwrap();
        let reward = || {
            crypto_row(
                "2026-01-02 12:00:00",
                "Reward",
                "1e-5",
                "1",
                "Bitcoin cashback",
            )
        };
        let first = crypto_export(&[reward(), reward()]);
        let second = crypto_export(&[
            reward(),
            crypto_row("2026-01-03 12:00:00", "Reward", "3e-5", "3", "ShakingSats"),
        ]);
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
        assert_eq!(run("1.csv", &first, 1).await.txns, 2);
        assert_eq!(
            run("2.csv", &second, 2).await.txn_rows,
            Rows {
                new: 1,
                changed: 0,
                unchanged: 1
            }
        );
        let ledger = ledgerdb::Ledger::load(&store, ledgerdb::LayerFilter::All)
            .unwrap()
            .ledger;
        assert_eq!(ledger.txns().len(), 3);
        assert!(
            ledger
                .find("shakepay:btc:2026-01-02T120000:1000:reward:2")
                .is_some()
        );
    }

    #[test]
    fn a_crypto_row_that_does_not_read_is_an_error() {
        let rows = |row: &str| {
            read_crypto(&format!("{CRYPTO_HEADER}{row}\n"), Zone::Fixed(0))
                .unwrap_err()
                .message
        };
        let ok = crypto_row("2026-01-01 12:00:00", "Send", "-0.001", "100", "@a");
        assert!(read_crypto(&format!("{CRYPTO_HEADER}{ok}\n"), Zone::Fixed(0)).is_ok());
        assert!(rows(&ok.replace("2026-01-01 12:00:00", "2026-01-01")).contains("bad date"));
        assert!(rows(&ok.replace("\"Send\"", "\"\"")).contains("no Type"));
        assert!(rows(&ok.replace("0.001,\"BTC\",,", ",,,")).contains("no amount"));
        assert!(
            rows(
                &ok.replace(",,100", ",\"BTC\",100")
                    .replace("0.001,\"BTC\",", "0.001,\"BTC\",0.001")
            )
            .contains("both")
        );
        assert!(rows(&ok.replace("\"BTC\"", "\"ETH\"")).contains("\"ETH\""));
        assert!(rows(&ok.replace("0.001", "lots")).contains("bad amount"));
        let buy = crypto_row(
            "2026-01-01 12:00:00",
            "Buy",
            "-0.001",
            "100",
            "Bought @ CA$1",
        );
        assert!(rows(&buy).contains("a Buy that debits bitcoin"));
        let header = read_crypto("\"Date\",\"Asset Credited\"\n", Zone::Fixed(0)).unwrap_err();
        assert!(
            header.message.starts_with("no \"Amount Debited\" column"),
            "{}",
            header.message
        );
    }

    #[test]
    fn cents_as_text() {
        assert_eq!(cents_text(0), "0.00");
        assert_eq!(cents_text(39550), "395.50");
        assert_eq!(cents_text(-5), "-0.05");
    }
}
