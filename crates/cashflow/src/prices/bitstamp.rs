//! Bitstamp's daily BTC/USD close, from its OHLC API.
//!
//! One request returns at most [`LIMIT`] daily candles from a start time, so
//! a backfill pages forward from the day after each page's last candle.
//! (Kraken's OHLC endpoint stops at 720 candles back, too short to reach
//! years-old transactions.) A response is
//! `{"data": {"pair": "BTC/USD", "ohlc": [{"timestamp": "1704067200",
//! "close": "42283.00", …}, …]}}`, each candle opening at midnight UTC.
//!
//! The candle of the day in progress is left out: its close is not final
//! until the day ends.

use ledgerdb::Date;
use serde::Deserialize;

use super::{BTC, DAY, FetchError, Http, USD, decimal, get_json};
use crate::import::RawPrice;

/// The daily BTC/USD candles endpoint; the query follows.
pub const BASE: &str = "https://www.bitstamp.net/api/v2/ohlc/btcusd/";

/// The most candles one request returns.
pub const LIMIT: usize = 1000;

#[derive(Deserialize)]
struct Response {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    ohlc: Vec<Candle>,
}

#[derive(Deserialize)]
struct Candle {
    timestamp: String,
    close: String,
}

/// The closes of every finished day from `since` on, at unix time `now`.
pub fn fetch(http: &dyn Http, since: Date, now: u64) -> Result<Vec<RawPrice>, FetchError> {
    fetch_pages(http, since, now, LIMIT)
}

/// [`fetch`], `limit` candles a request, so tests can page with a few.
fn fetch_pages(
    http: &dyn Http,
    since: Date,
    now: u64,
    limit: usize,
) -> Result<Vec<RawPrice>, FetchError> {
    let since_ts = u64::from(since.days()) * DAY;
    let mut start = since_ts;
    let mut prices: Vec<RawPrice> = Vec::new();
    while start + DAY <= now {
        let url = format!("{BASE}?step={DAY}&limit={limit}&start={start}");
        let response: Response = get_json(http, &url)?;
        let candles = response.data.ohlc;
        let mut last = None;
        for candle in &candles {
            let ts: u64 = candle.timestamp.parse().map_err(|_| {
                FetchError::response(&url, format!("bad timestamp {:?}", candle.timestamp))
            })?;
            last = Some(ts);
            let day = Date::from_days((ts / DAY) as u32);
            // Before the range (a page starts at the candle holding `start`),
            // already read on an earlier page, or not over yet.
            let seen = prices.last().is_some_and(|p| p.date >= day);
            if ts < since_ts || seen || ts + DAY > now {
                continue;
            }
            let price = decimal(&candle.close, USD).ok_or_else(|| {
                FetchError::response(&url, format!("{day}: bad close {:?}", candle.close))
            })?;
            prices.push(RawPrice {
                date: day,
                commodity: BTC.to_owned(),
                price,
            });
        }
        let Some(last) = last else {
            break;
        };
        if candles.len() < limit {
            break;
        }
        let next = last + DAY;
        if next <= start {
            return Err(FetchError::response(&url, "paging did not move forward"));
        }
        start = next;
    }
    Ok(prices)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::test_http::Canned;
    use super::*;

    /// Unix time at midnight UTC starting `day`.
    fn at(day: &str) -> u64 {
        u64::from(Date::parse(day).unwrap().days()) * DAY
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// Daily candles 2023-12-27 through 2024-01-04, with made-up closes.
    pub const FIXTURE: &str = r#"{"data": {"pair": "BTC/USD", "ohlc": [
        {"timestamp": "1703635200", "open": "1", "close": "43000.00", "volume": "1"},
        {"timestamp": "1703721600", "open": "1", "close": "42500.00", "volume": "1"},
        {"timestamp": "1703808000", "open": "1", "close": "41900.25", "volume": "1"},
        {"timestamp": "1703894400", "open": "1", "close": "42100.00", "volume": "1"},
        {"timestamp": "1703980800", "open": "1", "close": "42000.50", "volume": "1"},
        {"timestamp": "1704067200", "open": "1", "close": "44183.00", "volume": "1"},
        {"timestamp": "1704153600", "open": "1", "close": "45000.00", "volume": "1"},
        {"timestamp": "1704240000", "open": "1", "close": "42800.00", "volume": "1"},
        {"timestamp": "1704326400", "open": "1", "close": "43100.00", "volume": "1"}
    ]}}"#;

    fn closes(prices: &[RawPrice]) -> Vec<String> {
        prices
            .iter()
            .map(|p| format!("{} {}/{}", p.date, p.price.qty, p.price.precision))
            .collect()
    }

    #[test]
    fn closes_read_as_btc_in_usd_without_the_day_in_progress() {
        assert_eq!(at("2023-12-27"), 1_703_635_200, "the fixture's first day");
        let http = Canned::default().route(BASE, FIXTURE);
        // An hour into 2024-01-04, whose candle is not finished.
        let prices = fetch(&http, date("2023-12-30"), at("2024-01-04") + 3600).unwrap();
        assert_eq!(
            closes(&prices),
            [
                "2023-12-30 4210000/2",
                "2023-12-31 4200050/2",
                "2024-01-01 4418300/2",
                "2024-01-02 4500000/2",
                "2024-01-03 4280000/2",
            ]
        );
        assert!(prices.iter().all(|p| p.commodity == BTC));
        assert!(prices.iter().all(|p| p.price.commodity == USD));
    }

    /// A page of the fixture: `limit` candles from the one opening at `from`.
    fn page(from: u64, limit: usize) -> String {
        let all: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let candles: Vec<_> = all["data"]["ohlc"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["timestamp"].as_str().unwrap().parse::<u64>().unwrap() >= from)
            .take(limit)
            .cloned()
            .collect();
        serde_json::json!({"data": {"pair": "BTC/USD", "ohlc": candles}}).to_string()
    }

    #[test]
    fn a_backfill_pages_forward_until_a_short_page() {
        let url = |start: u64| format!("{BASE}?step=86400&limit=3&start={start}");
        let mut http = Canned::default();
        for day in ["2023-12-27", "2023-12-30", "2024-01-02", "2024-01-05"] {
            http = http.route(&url(at(day)), &page(at(day), 3));
        }
        let prices = fetch_pages(&http, date("2023-12-27"), at("2024-01-10"), 3).unwrap();
        assert_eq!(prices.len(), 9, "{:?}", closes(&prices));
        assert_eq!(prices[0].date, date("2023-12-27"));
        assert_eq!(prices[8].date, date("2024-01-04"));
        assert_eq!(
            *http.asked.borrow(),
            [
                url(at("2023-12-27")),
                url(at("2023-12-30")),
                url(at("2024-01-02")),
                url(at("2024-01-05")),
            ]
        );
    }

    #[test]
    fn a_page_holding_the_start_day_skips_what_is_before_it() {
        // Bitstamp answers a mid-day start with the candle holding it.
        let http = Canned::default().route(BASE, FIXTURE);
        let prices = fetch(&http, date("2024-01-03"), at("2024-01-05")).unwrap();
        assert_eq!(
            closes(&prices),
            ["2024-01-03 4280000/2", "2024-01-04 4310000/2"]
        );
    }

    #[test]
    fn nothing_is_asked_before_a_day_has_finished() {
        let http = Canned::default();
        let prices = fetch(&http, date("2024-01-04"), at("2024-01-04") + 3600).unwrap();
        assert!(prices.is_empty());
        assert!(http.asked.borrow().is_empty());
    }

    #[test]
    fn a_bad_response_is_an_error() {
        for body in [
            r#"{"errors": [{"message": "nope"}]}"#,
            r#"{"data": {"ohlc": [{"timestamp": "x", "close": "1"}]}}"#,
            r#"{"data": {"ohlc": [{"timestamp": "1703635200", "close": "n/a"}]}}"#,
        ] {
            let http = Canned::default().route(BASE, body);
            let err = fetch(&http, date("2023-12-27"), at("2024-01-01")).unwrap_err();
            assert!(matches!(err, FetchError::Response { .. }), "{body}: {err}");
        }
    }
}
