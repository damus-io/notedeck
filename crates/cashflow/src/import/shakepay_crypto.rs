//! The `shakepay` importer's second format: the crypto export,
//! `crypto_transactions_summary.csv`, Shakepay's bitcoin history.
//!
//! An export looks like this, with the columns found by their header names:
//!
//! ```text
//! "Date","Account","Amount Debited","Asset Debited","Amount Credited","Asset Credited","Market Value","Market Value Currency","Book Cost","Book Cost Currency","Type","Spot Rate","Buy / Sell Rate","Description"
//! "2026-03-01 12:20:00","Spending",,,0.001,"BTC",100,"CAD",100,"CAD","Buy",100000,100000.00,"Bought @ CA$100,000.00"
//! "2026-03-02 08:00:00","Spending",,,1.5e-5,"BTC",1.5,"CAD",1.5,"CAD","Reward",100000,,"Bitcoin cashback"
//! "2026-03-03 10:00:00","Spending",0.0005,"BTC",,,50,"CAD",50,"CAD","Send",100000,,"Bitcoin address bc1qexample"
//! ```
//!
//! [`is_crypto`] tells it from the Spending export by its header: the
//! `Account` column says `Spending` on every row of both.
//!
//! **One leg.** Each row moves bitcoin in or out, in `Amount Credited` or
//! `Amount Debited`, never both; a row in any asset but `BTC` is an error
//! rather than a guess at its commodity. An amount may be in scientific
//! notation (`1.5e-5`) or carry a float's noise, and is read exactly from
//! its text, rounded to the satoshi, never through an `f64`.
//!
//! What the row is worth in CAD (`Market Value`) and what Shakepay counts as
//! its cost (`Book Cost`) are kept as metadata, as are the spot rate and,
//! for a `Send` to an address, the address, which is the `raw-address` the
//! `btc` importer keeps on the wallet's receive.
//!
//! A row's id is `<source>:btc:<time>:<sats>:<type>:<n>`, the Spending
//! export's occurrence id with `btc:` ahead of it and the signed satoshis
//! in place of the cents, so the two exports' ids never meet.

use chrono::NaiveDateTime;
use ledgerdb::Date;

use super::csv_rows::{Header, RowError, id_key};
use super::shakepay::{btc_msat, parse_time, unix_time};
use crate::csv;
use crate::zone::Zone;

/// The column only the crypto export has.
const MARK: &str = "Asset Credited";

/// The one asset an export is known to hold.
const BTC: &str = "BTC";

/// What a `Send` to an on-chain address writes ahead of the address.
const ADDRESS_PREFIX: &str = "Bitcoin address ";

/// Whether `header`, an export's header row, is the crypto export's.
pub(super) fn is_crypto(header: &[String]) -> bool {
    header
        .iter()
        .any(|name| name.trim().eq_ignore_ascii_case(MARK))
}

/// One crypto export row, as read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CryptoRow {
    /// The `Date` stamp as the id writes it, `2026-03-02T184012`.
    pub stamp: String,
    /// Signed msat: the change to the bitcoin account, a whole number of
    /// satoshis.
    pub msat: i64,
    /// [`id_key`] of the type. These three fields come first so rows
    /// sharing an id stem sort together.
    pub kind_key: String,
    /// `Type`, as written.
    pub kind: String,
    /// The description, trimmed; may be empty.
    pub description: String,
    /// The day the row is filed under.
    pub date: Date,
    /// `Date`, as written.
    pub time: String,
    /// `Date`, read.
    pub when: NaiveDateTime,
    /// `Market Value` and its currency, `12.34 CAD`.
    pub market_value: String,
    /// `Book Cost` and its currency.
    pub book_cost: String,
    /// `Spot Rate`, empty when there is none.
    pub spot: String,
    /// `Buy / Sell Rate`, empty when there is none.
    pub rate: String,
}

impl CryptoRow {
    /// The address a `Send` paid, from its description, or `None` for any
    /// other row, such as a send to another Shakepay user.
    pub fn address(&self) -> Option<&str> {
        if self.kind != "Send" {
            return None;
        }
        let address = self.description.strip_prefix(ADDRESS_PREFIX)?.trim();
        (!address.is_empty() && !address.contains(',')).then_some(address)
    }

    /// What the ledger shows as the payee: the description, or the type
    /// where the description is only addresses or an invoice, so rows of one
    /// kind read alike.
    pub fn payee(&self) -> &str {
        let listing = self.description.is_empty()
            || self.description.starts_with(ADDRESS_PREFIX)
            || self.description.starts_with("Lightning invoice ");
        if listing {
            &self.kind
        } else {
            &self.description
        }
    }
}

/// Where each column the importer reads sits in a row.
struct Columns {
    date: usize,
    debited: usize,
    debited_asset: usize,
    credited: usize,
    credited_asset: usize,
    market_value: usize,
    market_currency: usize,
    book_cost: usize,
    book_currency: usize,
    kind: usize,
    spot: usize,
    rate: usize,
    description: usize,
}

impl Columns {
    /// Find the columns by name in the `header` row. Every one is required:
    /// this is the one crypto export header known.
    fn find(header: &Header) -> Result<Self, RowError> {
        Ok(Self {
            date: header.need("Date")?,
            debited: header.need("Amount Debited")?,
            debited_asset: header.need("Asset Debited")?,
            credited: header.need("Amount Credited")?,
            credited_asset: header.need("Asset Credited")?,
            market_value: header.need("Market Value")?,
            market_currency: header.need("Market Value Currency")?,
            book_cost: header.need("Book Cost")?,
            book_currency: header.need("Book Cost Currency")?,
            kind: header.need("Type")?,
            spot: header.need("Spot Rate")?,
            rate: header.need("Buy / Sell Rate")?,
            description: header.need("Description")?,
        })
    }
}

/// Every row of one crypto export, in its order, dated in `zone`. The first
/// record is the header; a row that doesn't read is an error, so nothing is
/// silently dropped.
pub(super) fn read_crypto(text: &str, zone: Zone) -> Result<Vec<CryptoRow>, RowError> {
    let mut records = csv::Reader::new(text);
    let Some(header) = records.next() else {
        return Ok(Vec::new());
    };
    let header = Header::new(
        header.map(|field| field.into_owned()).collect(),
        "Date, Amount Debited, Asset Debited, Amount Credited, Asset Credited, Market Value, \
         Market Value Currency, Book Cost, Book Cost Currency, Type, Spot Rate, \
         Buy / Sell Rate, Description",
    );
    let columns = Columns::find(&header)?;

    let mut rows = Vec::new();
    for (index, record) in records.enumerate() {
        let line = index + 2;
        let fields: Vec<_> = record.collect();
        let field = |at: usize| fields.get(at).map_or("", |f| f.as_ref()).trim();
        let error = |message: String| RowError { row: line, message };

        let time = field(columns.date);
        let when = parse_time(time).ok_or_else(|| error(format!("bad date {time:?}")))?;
        let kind = field(columns.kind);
        if kind.is_empty() {
            return Err(error("no Type".to_owned()));
        }
        let msat = leg(
            Side {
                amount: field(columns.debited),
                asset: field(columns.debited_asset),
            },
            Side {
                amount: field(columns.credited),
                asset: field(columns.credited_asset),
            },
        )
        .map_err(error)?;
        let wrong_way = match kind {
            "Buy" => msat < 0,
            "Sell" => msat > 0,
            _ => false,
        };
        if wrong_way {
            return Err(error(format!(
                "a {kind} that {} bitcoin",
                if msat < 0 { "debits" } else { "credits" }
            )));
        }
        let money = |value: usize, currency: usize| {
            let value = field(value);
            if value.is_empty() {
                String::new()
            } else {
                format!("{value} {}", field(currency))
            }
        };
        rows.push(CryptoRow {
            stamp: when.format("%Y-%m-%dT%H%M%S").to_string(),
            msat,
            kind_key: id_key(kind),
            kind: kind.to_owned(),
            description: field(columns.description).to_owned(),
            date: zone.date(unix_time(when)),
            time: time.to_owned(),
            when,
            market_value: money(columns.market_value, columns.market_currency),
            book_cost: money(columns.book_cost, columns.book_currency),
            spot: field(columns.spot).to_owned(),
            rate: field(columns.rate).to_owned(),
        });
    }
    Ok(rows)
}

/// One side of a row: an amount and the asset it is in.
struct Side<'a> {
    amount: &'a str,
    asset: &'a str,
}

/// The change to the bitcoin account, in msat, from a row's `debited` and
/// `credited` sides, exactly one of which must be filled, in bitcoin.
fn leg(debited: Side, credited: Side) -> Result<i64, String> {
    let (side, sign) = match (debited.amount.is_empty(), credited.amount.is_empty()) {
        (false, true) => (debited, -1),
        (true, false) => (credited, 1),
        (true, true) => return Err("no amount debited or credited".to_owned()),
        (false, false) => {
            return Err(format!(
                "amounts both debited ({}) and credited ({})",
                debited.amount, credited.amount
            ));
        }
    };
    if side.asset != BTC {
        return Err(format!(
            "an amount in {:?}; only {BTC} is known, so its commodity isn't guessed at",
            side.asset
        ));
    }
    btc_msat(side.amount)
        .map(|msat| sign * msat)
        .ok_or_else(|| format!("bad amount {:?}", side.amount))
}
