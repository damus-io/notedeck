//! Daily exchange rates fetched from public sources and filed as
//! `prices`-layer segments of `P` directives.
//!
//! Four sources, none needing a key:
//!
//! - [`boc`]: the Bank of Canada's daily rates into CAD for the currencies
//!   in [`boc::CURRENCIES`], `P <day> USD <rate> CAD`, business days only (a
//!   lookup takes the nearest earlier rate, which covers weekends and
//!   holidays).
//! - [`ecb`]: the European Central Bank's reference rates, crossed through
//!   the euro into CAD, only for the days and currencies the Bank of Canada
//!   has no rate for ([`ecb::FILLS`]).
//! - [`bitstamp`]: Bitstamp's daily BTC/USD close, `P <day> btc <close>
//!   USD`, every day.
//! - [`UNITS`]: the exact relations between bitcoin's units, `msat → sat →
//!   btc`, dated 1970-01-01 so they cover every transaction; and [`PEGS`],
//!   currencies held at a fixed rate to another, dated from the peg. Both
//!   are filed under the `units` source.
//!
//! With these a msat amount converts to CAD by chaining msat → sat → btc
//! → USD → CAD, and AED by AED → USD → CAD, which [`ledgerdb::PriceDb`]
//! does on its own.
//!
//! **One segment per source per calendar year.** A segment's `imported`
//! header is midnight UTC of the day after its last price, not the clock, so
//! its bytes depend only on the rates in it: fetching a closed year again
//! writes the file that is already there, and fetching again on a day with
//! no new rate writes nothing. A fetch that does bring a new day stamps a
//! later `imported`, and because the ledger orders same-day prices by import
//! (see [`ledgerdb::Ledger::prices`]), a rate it revises replaces the old
//! one. A `P` directive carries no record id: its pair and day are its
//! identity.
//!
//! The network is behind [`Http`], so tests feed recorded responses and never
//! make a request; [`Ehttp`] is the real client.

pub mod bitstamp;
pub mod boc;
pub mod ecb;

use std::fmt;

use ledgerdb::{
    Date, JournalStore, Ledger, PutSegment, SegmentHash, SegmentHeader, StoreError, Ymd,
};

use crate::import::{Batch, BatchError, RawAmount, RawPrice};

/// The layer every fetched price segment is filed in, so a reader can leave
/// them all out.
pub const LAYER: &str = "prices";

/// US dollars, as the sources quote them.
pub const USD: &str = "USD";
/// Canadian dollars, the home currency.
pub const CAD: &str = "CAD";
/// Bitcoin, in whole coins.
pub const BTC: &str = "btc";
/// Satoshis, 10⁻⁸ btc.
pub const SAT: &str = "sat";
/// Millisatoshis, 10⁻³ sat, which lightning amounts are in.
pub const MSAT: &str = "msat";

/// Every source `cashflow prices fetch` knows, by the segment source name
/// it files them under.
pub const SOURCES: &[Source] = &[Source::Units, Source::Boc, Source::Ecb, Source::Bitstamp];

/// Seconds in a day.
const DAY: u64 = 86_400;

/// One place rates come from. Its [`name`](Source::name) is the source
/// header of its segments.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The fixed relations, [`UNITS`] and [`PEGS`].
    Units,
    /// The Bank of Canada's rates into CAD, [`boc`].
    Boc,
    /// The ECB's rates crossed into CAD where boc has none, [`ecb`].
    Ecb,
    /// Bitstamp's BTC/USD, [`bitstamp`].
    Bitstamp,
}

impl Source {
    /// The source name segments are filed under, and `--source` takes.
    pub fn name(self) -> &'static str {
        match self {
            Source::Units => "units",
            Source::Boc => "boc",
            Source::Ecb => "ecb",
            Source::Bitstamp => "bitstamp",
        }
    }

    /// The source named `name`.
    pub fn from_name(name: &str) -> Option<Source> {
        SOURCES.iter().copied().find(|s| s.name() == name)
    }

    /// The first day the source has a rate for. A fetch never asks for
    /// earlier days: there are none, and Bitstamp answers a start before its
    /// first candle with no candles at all.
    pub fn first_day(self) -> Date {
        match self {
            Source::Units => Date::EPOCH,
            Source::Boc => boc::first_day(),
            Source::Ecb => ecb::first_day(),
            Source::Bitstamp => bitstamp::first_day(),
        }
    }

    /// The first day on or after `day` the source has rates for, or `None`
    /// when it has none from `day` on.
    pub fn reach(self, day: Date) -> Option<Date> {
        match self {
            Source::Ecb => ecb::reach(day),
            _ => Some(day.max(self.first_day())),
        }
    }

    /// The rates on days `since` up to, not including, `today`, plus
    /// `today`'s when the source publishes a final one during the day.
    /// [`Source::Units`] ignores the range: its relations are fixed.
    fn fetch(self, http: &dyn Http, since: Date, now: u64) -> Result<Vec<RawPrice>, FetchError> {
        let since = since.max(self.first_day());
        match self {
            Source::Units => Ok(units().into_iter().chain(pegs()).collect()),
            Source::Boc => boc::fetch(http, since, day_of(now)),
            Source::Ecb => ecb::fetch(http, since, day_of(now)),
            Source::Bitstamp => bitstamp::fetch(http, since, now),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The exact relations between bitcoin's units: one msat is 0.001 sat and
/// one sat is 0.00000001 btc.
pub const UNITS: &[UnitRelation] = &[
    UnitRelation {
        commodity: MSAT,
        qty: 1,
        precision: 3,
        quote: SAT,
    },
    UnitRelation {
        commodity: SAT,
        qty: 1,
        precision: 8,
        quote: BTC,
    },
];

/// One unit of `commodity` is `qty / 10^precision` of `quote`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UnitRelation {
    pub commodity: &'static str,
    pub qty: i64,
    pub precision: u8,
    pub quote: &'static str,
}

/// [`UNITS`] as prices on 1970-01-01.
fn units() -> Vec<RawPrice> {
    UNITS
        .iter()
        .map(|u| RawPrice {
            date: Date::EPOCH,
            commodity: u.commodity.to_owned(),
            price: RawAmount {
                qty: u.qty,
                precision: u.precision,
                commodity: u.quote.to_owned(),
            },
        })
        .collect()
}

/// Currencies their central bank holds at a fixed rate to another, so one
/// rate prices them on every day since.
///
/// The UAE dirham has been pegged at 3.6725 to the US dollar since November
/// 1997. Neither the Bank of Canada (after 2017) nor the ECB quotes it, and
/// through the peg every AED amount takes the day's USD rate into CAD.
pub const PEGS: &[Peg] = &[Peg {
    relation: UnitRelation {
        commodity: USD,
        qty: 36725,
        precision: 4,
        quote: "AED",
    },
    since: Ymd {
        year: 1997,
        month: 11,
        day: 1,
    },
}];

/// A [`UnitRelation`] that holds from `since` on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Peg {
    pub relation: UnitRelation,
    pub since: Ymd,
}

/// [`PEGS`] as prices on the day each began.
fn pegs() -> impl Iterator<Item = RawPrice> {
    PEGS.iter().map(|peg| {
        let u = peg.relation;
        RawPrice {
            date: Date::from_ymd(peg.since).expect("a calendar date"),
            commodity: u.commodity.to_owned(),
            price: RawAmount {
                qty: u.qty,
                precision: u.precision,
                commodity: u.quote.to_owned(),
            },
        }
    })
}

/// A blocking HTTP GET, so the fetchers can be tested without a network.
pub trait Http {
    /// The body of a successful GET of `url`, or why there isn't one.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// [`Http`] over `ehttp`, the client the rest of notedeck uses.
pub struct Ehttp;

impl Http for Ehttp {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let mut request = ehttp::Request::get(url);
        request.headers.insert("User-Agent", "cashflow");
        let response = ehttp::fetch_blocking(&request)?;
        if !response.ok {
            return Err(format!("{} {}", response.status, response.status_text));
        }
        Ok(response.bytes)
    }
}

/// Why fetching a source failed. Nothing of that source is written.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The request failed or the server refused it.
    #[error("GET {url}: {error}")]
    Http { url: String, error: String },
    /// The response is not what the source documents.
    #[error("{url}: {error}")]
    Response { url: String, error: String },
    /// The rates didn't make a batch.
    #[error(transparent)]
    Batch(#[from] BatchError),
    /// A segment could not be written.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl FetchError {
    /// A response from `url` that can't be read.
    fn response(url: &str, error: impl fmt::Display) -> FetchError {
        FetchError::Response {
            url: url.to_owned(),
            error: error.to_string(),
        }
    }
}

/// GET `url` and read its body as JSON of type `T`.
fn get_json<T: serde::de::DeserializeOwned>(http: &dyn Http, url: &str) -> Result<T, FetchError> {
    let body = http.get(url).map_err(|error| FetchError::Http {
        url: url.to_owned(),
        error,
    })?;
    serde_json::from_slice(&body).map_err(|e| FetchError::response(url, e))
}

/// `text`, a plain decimal such as `1.3456` or `44183`, as an amount of
/// `commodity`, or `None` unless it is positive digits with at most one
/// point and at most [`ledgerdb::MAX_PRECISION`] places.
fn decimal(text: &str, commodity: &str) -> Option<RawAmount> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) {
        return None;
    }
    let precision = u8::try_from(frac.len()).ok()?;
    if precision > ledgerdb::MAX_PRECISION {
        return None;
    }
    let qty: i64 = format!("{whole}{frac}").parse().ok()?;
    (qty > 0).then(|| RawAmount {
        qty,
        precision,
        commodity: commodity.to_owned(),
    })
}

/// The UTC day of unix time `secs`.
fn day_of(secs: u64) -> Date {
    Date::from_days((secs / DAY).min(u64::from(u32::MAX)) as u32)
}

/// One calendar year's rates from one source, as it becomes a segment.
#[derive(Debug)]
pub struct YearSegment {
    pub header: SegmentHeader,
    pub batch: Batch,
    /// The first and last day with a rate.
    pub first: Date,
    pub last: Date,
}

/// Split `prices` from `source` into one segment per calendar year, each
/// sorted by day then commodity, stamped as the module docs describe.
/// [`Source::Units`]' segment is stamped 0, so it never changes either.
pub fn year_segments(
    source: Source,
    mut prices: Vec<RawPrice>,
) -> Result<Vec<YearSegment>, BatchError> {
    prices.sort_by(|a, b| {
        (a.date, &a.commodity, &a.price.commodity).cmp(&(b.date, &b.commodity, &b.price.commodity))
    });
    let mut segments = Vec::new();
    for year in prices.chunk_by(|a, b| a.date.ymd().year == b.date.ymd().year) {
        let first = year[0].date;
        let last = year[year.len() - 1].date;
        let imported = match source {
            Source::Units => 0,
            _ => u64::from(last.days() + 1) * DAY,
        };
        segments.push(YearSegment {
            header: SegmentHeader {
                source: source.name().to_owned(),
                imported,
                layer: LAYER.to_owned(),
            },
            batch: Batch::from_raw(&[], year)?,
            first,
            last,
        });
    }
    Ok(segments)
}

/// One segment a fetch wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    pub hash: SegmentHash,
    /// False when the journal already had these exact bytes.
    pub new: bool,
    pub header: SegmentHeader,
    pub prices: usize,
    pub first: Date,
    pub last: Date,
}

/// What fetching one source did.
#[derive(Debug)]
pub struct Fetched {
    pub source: Source,
    /// The first day asked for.
    pub since: Date,
    /// The segments written, oldest year first; empty when the source had
    /// nothing from `since` on.
    pub filed: Vec<Filed>,
}

/// Fetch `source`'s rates from `since` on, at unix time `now`, and write
/// them to `store` as [`year_segments`].
#[profiling::function]
pub fn fetch_into(
    store: &JournalStore,
    http: &dyn Http,
    source: Source,
    since: Date,
    now: u64,
) -> Result<Fetched, FetchError> {
    let prices = source.fetch(http, since, now)?;
    let mut filed = Vec::new();
    for segment in year_segments(source, prices)? {
        let PutSegment { hash, new } =
            store.put_segment(&segment.header, &segment.batch.journal)?;
        filed.push(Filed {
            hash,
            new,
            prices: segment.batch.price_count(),
            header: segment.header,
            first: segment.first,
            last: segment.last,
        });
    }
    Ok(Fetched {
        source,
        since,
        filed,
    })
}

/// How far before the oldest transaction a backfill starts, so a first
/// transaction on a weekend or holiday still has the business day's rate
/// before it.
const LOOKBACK_DAYS: u32 = 7;

/// Where a fetch of `source` with no `--since` starts. `None` for an empty
/// ledger.
///
/// Usually the day after the newest rate `ledger` already has from it. But
/// when its oldest rate is later than the oldest transaction, as far back
/// as the source reaches, it backfills: from [`LOOKBACK_DAYS`] before that
/// transaction. That happens when an import brings older transactions than
/// the last fetch saw, when a source learns a currency, or when the source
/// has no rates in the ledger yet.
pub fn resume_from(ledger: &Ledger, source: Source) -> Option<Date> {
    let from_source = |at: &usize| {
        let origin = ledger.price_origin(*at);
        ledger.segments()[origin.segment as usize].header.source == source.name()
    };
    let prices = ledger.prices();
    let oldest = (0..prices.len())
        .find(from_source)
        .map(|at| prices[at].date);
    let newest = (0..prices.len())
        .rev()
        .find(from_source)
        .map(|at| prices[at].date);
    let backfill = ledger.txns().first().and_then(|txn| {
        let from = Date::from_days(txn.date.days().saturating_sub(LOOKBACK_DAYS));
        Some(Backfill {
            reach: source.reach(txn.date)?,
            from: from.max(source.first_day()),
        })
    });
    let Some(newest) = newest else {
        return backfill.map(|b| b.from);
    };
    match backfill {
        Some(b) if oldest.is_some_and(|oldest| oldest > b.reach) => Some(b.from),
        _ => Some(Date::from_days(newest.days() + 1)),
    }
}

/// A backfill [`resume_from`] may start.
struct Backfill {
    /// The oldest day the ledger needs a rate for that the source can
    /// have: the oldest transaction's, or the source's first after it.
    reach: Date,
    /// Where fetching it starts.
    from: Date,
}

#[cfg(test)]
pub(crate) mod test_http {
    //! A canned [`Http`] for the fetchers' tests.

    use std::cell::RefCell;

    use super::Http;

    /// Answers each GET with the body of the first route whose URL prefix
    /// matches, and records every URL asked for. An unrouted URL fails, so a
    /// test can't reach the network.
    #[derive(Default)]
    pub struct Canned {
        pub routes: Vec<(String, String)>,
        pub asked: RefCell<Vec<String>>,
    }

    impl Canned {
        pub fn route(mut self, prefix: &str, body: &str) -> Self {
            self.routes.push((prefix.to_owned(), body.to_owned()));
            self
        }
    }

    impl Http for Canned {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.asked.borrow_mut().push(url.to_owned());
            self.routes
                .iter()
                .find(|(prefix, _)| url.starts_with(prefix.as_str()))
                .map(|(_, body)| body.clone().into_bytes())
                .ok_or_else(|| format!("no canned response for {url}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_http::Canned;
    use super::*;
    use ledgerdb::{Amount, LayerFilter, PriceDb};

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn price(day: &str, commodity: &str, value: &str, quote: &str) -> RawPrice {
        RawPrice {
            date: date(day),
            commodity: commodity.to_owned(),
            price: decimal(value, quote).unwrap(),
        }
    }

    #[test]
    fn decimals_read_exactly_or_not_at_all() {
        let amount = |t| decimal(t, "CAD").map(|a| (a.qty, a.precision));
        assert_eq!(amount("1.3456"), Some((13456, 4)));
        assert_eq!(amount("44183"), Some((44183, 0)));
        assert_eq!(amount("0.10"), Some((10, 2)));
        for bad in ["", ".5", "1.2.3", "-1", "1e5", "0", "0.00"] {
            assert_eq!(amount(bad), None, "{bad:?}");
        }
        let too_fine = format!("1.{}", "1".repeat(usize::from(ledgerdb::MAX_PRECISION) + 1));
        assert_eq!(amount(&too_fine), None);
    }

    #[test]
    fn years_split_and_stamp_the_day_after_their_last_rate() {
        let prices = vec![
            price("2024-01-02", USD, "1.33", CAD),
            price("2023-12-29", USD, "1.32", CAD),
            price("2023-06-01", USD, "1.35", CAD),
        ];
        let segments = year_segments(Source::Boc, prices).unwrap();
        let spans: Vec<_> = segments
            .iter()
            .map(|s| (s.first, s.last, s.header.imported, s.batch.price_count()))
            .collect();
        let stamp = |day: &str| u64::from(date(day).days()) * DAY;
        assert_eq!(
            spans,
            [
                (
                    date("2023-06-01"),
                    date("2023-12-29"),
                    stamp("2023-12-30"),
                    2
                ),
                (
                    date("2024-01-02"),
                    date("2024-01-02"),
                    stamp("2024-01-03"),
                    1
                ),
            ]
        );
        assert!(segments.iter().all(|s| s.header.layer == LAYER));
        assert!(segments.iter().all(|s| s.header.source == "boc"));
    }

    /// A 2023 USD/CAD and BTC/USD history, enough to price msat in CAD.
    fn canned() -> Canned {
        Canned::default()
            .route(boc::BASE, boc::tests::FIXTURE)
            .route(bitstamp::BASE, bitstamp::tests::FIXTURE)
            .route(ecb::BASE, ecb::tests::GAPS)
    }

    /// Unix time at midnight UTC starting `day`.
    fn at(day: &str) -> u64 {
        u64::from(date(day).days()) * DAY
    }

    #[test]
    fn a_refetch_writes_nothing_new() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        let now = at("2024-01-04") + 3600;
        let run = || -> Vec<Filed> {
            SOURCES
                .iter()
                .flat_map(|&s| {
                    fetch_into(&store, &http, s, date("2023-12-27"), now)
                        .unwrap()
                        .filed
                })
                .collect()
        };
        let first = run();
        assert!(first.iter().all(|f| f.new), "{first:#?}");
        let again = run();
        assert!(again.iter().all(|f| !f.new), "{again:#?}");
        assert_eq!(
            first.iter().map(|f| f.hash).collect::<Vec<_>>(),
            again.iter().map(|f| f.hash).collect::<Vec<_>>()
        );
        // units 1970 and 1997 (the AED peg); boc, ecb and bitstamp 2023
        // and 2024.
        assert_eq!(store.list().unwrap().len(), 8);
    }

    #[test]
    fn fetched_rates_convert_msat_to_cad_in_four_hops() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        for &source in SOURCES {
            fetch_into(&store, &http, source, date("2023-12-27"), at("2024-01-04")).unwrap();
        }
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        let commodities = &ledger.journal().commodities;
        let (msat, cad) = (
            commodities.get(MSAT).unwrap(),
            commodities.get(CAD).unwrap(),
        );
        let db = PriceDb::from_ledger(&ledger);
        // Sunday 2023-12-31: Bitstamp's close that day (42000.50 USD) and the
        // BoC rate of Friday the 29th (1.3226): 1e11 msat = 1 btc.
        let one_btc = Amount {
            qty: 100_000_000_000,
            commodity: msat,
        };
        let in_cad = db.convert(one_btc, cad, date("2023-12-31")).unwrap().qty;
        // 42000.50 × 1.3226 = 55549.86, at CAD's precision of 0 (no CAD
        // amount was ever written, only rates): 55550.
        assert_eq!(commodities.precision(cad), 0);
        assert_eq!(in_cad, 55_550);
    }

    #[test]
    fn a_currency_boc_lacks_converts_through_the_ecb_fill() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        // A CAD amount, so CAD has cents.
        put(
            &store,
            "bank",
            1,
            "2024-01-02 * Shop\n  ; id: t:1\n  a  1.00 CAD\n  b\n",
        );
        for &source in SOURCES {
            fetch_into(&store, &http, source, date("2023-12-27"), at("2024-01-04")).unwrap();
        }
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        let commodities = &ledger.journal().commodities;
        let (dkk, cad) = (
            commodities.get("DKK").unwrap(),
            commodities.get(CAD).unwrap(),
        );
        let db = PriceDb::from_ledger(&ledger);
        let hundred = Amount {
            qty: 100,
            commodity: dkk,
        };
        // 1.4565 / 7.4551 = 0.195370 CAD a krone on 2024-01-02, and on the
        // Sunday before, Friday's 0.196460.
        let in_cad = |day| db.convert(hundred, cad, date(day)).unwrap().qty;
        assert_eq!(in_cad("2024-01-02"), 1954);
        assert_eq!(in_cad("2023-12-31"), 1965);
    }

    /// Write `text` to `store` as a segment from `source`.
    fn put(store: &JournalStore, source: &str, imported: u64, text: &str) {
        let journal = ledgerdb::Journal::parse(text).journal;
        let header = SegmentHeader {
            source: source.to_owned(),
            imported,
            layer: source.to_owned(),
        };
        store.write_segment(&header, &journal).unwrap();
    }

    #[test]
    fn resume_starts_after_the_newest_rate_from_that_source() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        let load = || Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        assert_eq!(resume_from(&load(), Source::Boc), None);

        // A Saturday: the backfill starts a week before, so Friday's rate
        // is in it.
        put(
            &store,
            "bank",
            1,
            "2023-12-30 * Shop\n  ; id: t:1\n  a  1 CAD\n  b\n",
        );
        assert_eq!(resume_from(&load(), Source::Boc), Some(date("2023-12-23")));

        fetch_into(
            &store,
            &http,
            Source::Boc,
            date("2023-12-27"),
            at("2024-01-04"),
        )
        .unwrap();
        let ledger = load();
        // The fixture's last BoC day is 2024-01-03.
        assert_eq!(resume_from(&ledger, Source::Boc), Some(date("2024-01-04")));
        assert_eq!(
            resume_from(&ledger, Source::Bitstamp),
            Some(date("2023-12-23"))
        );
    }

    #[test]
    fn resume_backfills_transactions_older_than_the_oldest_rate() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        let load = || Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        put(
            &store,
            "bank",
            1,
            "2023-12-30 * Shop\n  ; id: t:1\n  a  1 CAD\n  b\n",
        );
        fetch_into(
            &store,
            &http,
            Source::Boc,
            date("2023-12-23"),
            at("2024-01-04"),
        )
        .unwrap();
        assert_eq!(resume_from(&load(), Source::Boc), Some(date("2024-01-04")));

        // An import brings a transaction older than every rate.
        put(
            &store,
            "bank",
            2,
            "2023-12-20 * Shop\n  ; id: t:2\n  a  1 CAD\n  b\n",
        );
        assert_eq!(resume_from(&load(), Source::Boc), Some(date("2023-12-13")));

        // One older than the source reaches backfills from its first day,
        // and once that day's rate is in, never again.
        put(
            &store,
            "bank",
            3,
            "2005-03-01 * Shop\n  ; id: t:3\n  a  1 CAD\n  b\n",
        );
        assert_eq!(resume_from(&load(), Source::Boc), Some(boc::first_day()));
        put(&store, "boc", 4, "P 2007-05-01 USD 1.1089 CAD\n");
        assert_eq!(resume_from(&load(), Source::Boc), Some(date("2024-01-04")));
    }

    #[test]
    fn a_fetch_never_asks_before_the_source_begins() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        let since = date("2009-01-01");
        fetch_into(&store, &http, Source::Bitstamp, since, at("2011-08-20")).unwrap();
        let first = u64::from(bitstamp::first_day().days()) * DAY;
        assert!(
            http.asked.borrow()[0].ends_with(&format!("&start={first}")),
            "{:?}",
            http.asked.borrow()
        );
    }

    #[test]
    fn aed_converts_through_its_peg_to_the_dollar() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        put(
            &store,
            "bank",
            1,
            "2024-01-02 * Shop\n  ; id: t:1\n  a  1.00 CAD\n  b\n",
        );
        for &source in SOURCES {
            fetch_into(&store, &http, source, date("2023-12-27"), at("2024-01-04")).unwrap();
        }
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        let commodities = &ledger.journal().commodities;
        let (aed, cad) = (
            commodities.get("AED").unwrap(),
            commodities.get(CAD).unwrap(),
        );
        let db = PriceDb::from_ledger(&ledger);
        let hundred = Amount {
            qty: 100,
            commodity: aed,
        };
        // 100 / 3.6725 × 1.3316 = 36.2587 CAD on 2024-01-02.
        let in_cad = db.convert(hundred, cad, date("2024-01-02")).unwrap();
        assert_eq!(in_cad.qty, 3626);
    }

    #[test]
    fn units_are_exact_and_dated_at_the_epoch() {
        let segments = year_segments(Source::Units, units()).unwrap();
        let [segment] = segments.as_slice() else {
            panic!("{segments:?}")
        };
        assert_eq!(segment.header.imported, 0);
        assert_eq!(
            segment
                .batch
                .journal
                .to_ledger(ledgerdb::WriteOptions::default()),
            "P 1970-01-01 msat 0.001 sat\nP 1970-01-01 sat 0.00000001 btc\n"
        );
    }
}
