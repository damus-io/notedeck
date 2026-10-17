//! Daily exchange rates fetched from public sources and filed as
//! `prices`-layer segments of `P` directives.
//!
//! Three sources, none needing a key:
//!
//! - [`boc`]: the Bank of Canada's daily USD/CAD rate, `P <day> USD <rate>
//!   CAD`, business days only (a lookup takes the nearest earlier rate, which
//!   covers weekends and holidays).
//! - [`bitstamp`]: Bitstamp's daily BTC/USD close, `P <day> btc <close>
//!   USD`, every day.
//! - [`UNITS`]: the exact relations between bitcoin's units, `msat → sat →
//!   btc`, dated 1970-01-01 so they cover every transaction.
//!
//! With all three a msat amount converts to CAD by chaining msat → sat → btc
//! → USD → CAD, which [`ledgerdb::PriceDb`] does on its own.
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

use std::fmt;

use ledgerdb::{Date, JournalStore, Ledger, PutSegment, SegmentHash, SegmentHeader, StoreError};

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
pub const SOURCES: &[Source] = &[Source::Units, Source::Boc, Source::Bitstamp];

/// Seconds in a day.
const DAY: u64 = 86_400;

/// One place rates come from. Its [`name`](Source::name) is the source
/// header of its segments.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The fixed unit relations, [`UNITS`].
    Units,
    /// The Bank of Canada's USD/CAD, [`boc`].
    Boc,
    /// Bitstamp's BTC/USD, [`bitstamp`].
    Bitstamp,
}

impl Source {
    /// The source name segments are filed under, and `--source` takes.
    pub fn name(self) -> &'static str {
        match self {
            Source::Units => "units",
            Source::Boc => "boc",
            Source::Bitstamp => "bitstamp",
        }
    }

    /// The source named `name`.
    pub fn from_name(name: &str) -> Option<Source> {
        SOURCES.iter().copied().find(|s| s.name() == name)
    }

    /// The rates on days `since` up to, not including, `today`, plus
    /// `today`'s when the source publishes a final one during the day.
    /// [`Source::Units`] ignores the range: its relations are fixed.
    fn fetch(self, http: &dyn Http, since: Date, now: u64) -> Result<Vec<RawPrice>, FetchError> {
        match self {
            Source::Units => Ok(units()),
            Source::Boc => boc::fetch(http, since, day_of(now)),
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

/// Where a fetch of `source` with no `--since` starts: the day after the
/// newest rate `ledger` already has from it, else the first day of the
/// ledger's oldest transaction. `None` for an empty ledger.
pub fn resume_from(ledger: &Ledger, source: Source) -> Option<Date> {
    let newest = ledger
        .prices()
        .iter()
        .enumerate()
        .rev()
        .find(|(at, _)| {
            let origin = ledger.price_origin(*at);
            ledger.segments()[origin.segment as usize].header.source == source.name()
        })
        .map(|(_, price)| price.date);
    match newest {
        Some(day) => Some(Date::from_days(day.days() + 1)),
        None => ledger.txns().first().map(|txn| txn.date),
    }
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
        // units; boc 2023, 2024; bitstamp 2023, 2024.
        assert_eq!(store.list().unwrap().len(), 5);
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
    fn resume_starts_after_the_newest_rate_from_that_source() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path().join("journal"));
        let http = canned();
        let load = || Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        assert_eq!(resume_from(&load(), Source::Boc), None);

        let txn = "2022-03-04 * Shop\n  ; id: t:1\n  a  1 CAD\n  b\n";
        let journal = ledgerdb::Journal::parse(txn).journal;
        let header = SegmentHeader {
            source: "bank".to_owned(),
            imported: 1,
            layer: "bank".to_owned(),
        };
        store.write_segment(&header, &journal).unwrap();
        assert_eq!(resume_from(&load(), Source::Boc), Some(date("2022-03-04")));

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
            Some(date("2022-03-04"))
        );
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
