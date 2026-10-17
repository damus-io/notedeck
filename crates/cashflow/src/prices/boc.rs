//! The Bank of Canada's daily USD/CAD rate, from its Valet API.
//!
//! Valet's `FXUSDCAD` series starts on 2017-01-03. Days before that come from
//! the legacy noon-rate series `IEXE0101`, which ran until 2017, so a fetch
//! from earlier asks for each series over its own part of the range. Both
//! publish business days only.
//!
//! A response is
//! `{"observations": [{"d": "2024-01-02", "FXUSDCAD": {"v": "1.3316"}}, …]}`;
//! an observation with no value is skipped.

use ledgerdb::{Date, Ymd};
use serde_json::Value;

use super::{CAD, FetchError, Http, USD, decimal, get_json};
use crate::import::RawPrice;

/// The Valet observations endpoint; a series name and query follow.
pub const BASE: &str = "https://www.bankofcanada.ca/valet/observations/";

/// The daily average rate, from [`FX_START`] on.
pub const FX_SERIES: &str = "FXUSDCAD";

/// The legacy noon rate, for the days before [`FX_START`].
pub const LEGACY_SERIES: &str = "IEXE0101";

/// The first day of [`FX_SERIES`].
fn fx_start() -> Date {
    Date::from_ymd(Ymd {
        year: 2017,
        month: 1,
        day: 3,
    })
    .expect("a calendar date")
}

/// The rates on business days from `since` through `today`.
pub fn fetch(http: &dyn Http, since: Date, today: Date) -> Result<Vec<RawPrice>, FetchError> {
    let mut prices = Vec::new();
    let fx_start = fx_start();
    if since < fx_start {
        let end = today.min(Date::from_days(fx_start.days() - 1));
        fetch_series(http, LEGACY_SERIES, since, end, &mut prices)?;
    }
    if today >= fx_start {
        fetch_series(http, FX_SERIES, since.max(fx_start), today, &mut prices)?;
    }
    Ok(prices)
}

/// `series`' rates from `start` through `end` onto `prices`.
fn fetch_series(
    http: &dyn Http,
    series: &str,
    start: Date,
    end: Date,
    prices: &mut Vec<RawPrice>,
) -> Result<(), FetchError> {
    if start > end {
        return Ok(());
    }
    let url = format!("{BASE}{series}/json?start_date={start}&end_date={end}");
    let body: Value = get_json(http, &url)?;
    let mut read = Vec::new();
    parse(&body, series, &url, &mut read)?;
    // Valet keeps to the range asked; this only guards against a server
    // that doesn't.
    prices.extend(
        read.into_iter()
            .filter(|p| p.date >= start && p.date <= end),
    );
    Ok(())
}

/// Read a Valet response for `series` onto `prices`.
fn parse(
    body: &Value,
    series: &str,
    url: &str,
    prices: &mut Vec<RawPrice>,
) -> Result<(), FetchError> {
    let observations = body
        .get("observations")
        .and_then(Value::as_array)
        .ok_or_else(|| FetchError::response(url, "no `observations` array"))?;
    for observation in observations {
        let day = observation.get("d").and_then(Value::as_str);
        let day = day
            .and_then(|d| Date::parse(d).ok())
            .ok_or_else(|| FetchError::response(url, format!("bad day in {observation}")))?;
        let value = observation
            .get(series)
            .and_then(|s| s.get("v"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let Some(value) = value else {
            continue;
        };
        let price = decimal(value, CAD)
            .ok_or_else(|| FetchError::response(url, format!("{day}: bad rate {value:?}")))?;
        prices.push(RawPrice {
            date: day,
            commodity: USD.to_owned(),
            price,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::test_http::Canned;
    use super::*;

    /// A trimmed `FXUSDCAD` response around a new year, with a holiday that
    /// has no value. The rates are made up.
    pub const FIXTURE: &str = r#"{
        "seriesDetail": {"FXUSDCAD": {"label": "USD/CAD"}},
        "observations": [
            {"d": "2023-12-27", "FXUSDCAD": {"v": "1.3200"}},
            {"d": "2023-12-28", "FXUSDCAD": {"v": "1.3210"}},
            {"d": "2023-12-29", "FXUSDCAD": {"v": "1.3226"}},
            {"d": "2024-01-01", "FXUSDCAD": {}},
            {"d": "2024-01-02", "FXUSDCAD": {"v": "1.3316"}},
            {"d": "2024-01-03", "FXUSDCAD": {"v": "1.3350"}}
        ]
    }"#;

    /// A trimmed legacy `IEXE0101` response.
    const LEGACY: &str = r#"{"observations": [
        {"d": "2016-12-29", "IEXE0101": {"v": "1.3488"}},
        {"d": "2016-12-30", "IEXE0101": {"v": "1.3427"}}
    ]}"#;

    /// A `FXUSDCAD` response for the series' first days.
    const FIRST_DAYS: &str = r#"{"observations": [
        {"d": "2017-01-03", "FXUSDCAD": {"v": "1.3435"}}
    ]}"#;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn lines(prices: &[RawPrice]) -> Vec<String> {
        prices
            .iter()
            .map(|p| {
                format!(
                    "{} {} {}/{} {}",
                    p.date, p.commodity, p.price.qty, p.price.precision, p.price.commodity
                )
            })
            .collect()
    }

    #[test]
    fn a_response_reads_as_usd_in_cad_skipping_empty_days() {
        let http = Canned::default().route(BASE, FIXTURE);
        let prices = fetch(&http, date("2023-12-28"), date("2024-01-02")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                "2023-12-28 USD 13210/4 CAD",
                "2023-12-29 USD 13226/4 CAD",
                "2024-01-02 USD 13316/4 CAD",
            ]
        );
        assert_eq!(
            *http.asked.borrow(),
            [format!(
                "{BASE}FXUSDCAD/json?start_date=2023-12-28&end_date=2024-01-02"
            )]
        );
    }

    #[test]
    fn days_before_2017_come_from_the_legacy_series() {
        let http = Canned::default()
            .route(&format!("{BASE}{LEGACY_SERIES}/"), LEGACY)
            .route(&format!("{BASE}{FX_SERIES}/"), FIRST_DAYS);
        let prices = fetch(&http, date("2016-12-29"), date("2017-01-03")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                "2016-12-29 USD 13488/4 CAD",
                "2016-12-30 USD 13427/4 CAD",
                "2017-01-03 USD 13435/4 CAD",
            ]
        );
        assert_eq!(
            *http.asked.borrow(),
            [
                format!("{BASE}IEXE0101/json?start_date=2016-12-29&end_date=2017-01-02"),
                format!("{BASE}FXUSDCAD/json?start_date=2017-01-03&end_date=2017-01-03"),
            ]
        );

        // Entirely before the switch: only the legacy series is asked.
        let legacy_only = Canned::default().route(BASE, LEGACY);
        fetch(&legacy_only, date("2016-12-29"), date("2016-12-30")).unwrap();
        assert_eq!(legacy_only.asked.borrow().len(), 1);
    }

    #[test]
    fn a_bad_response_is_an_error_naming_the_url() {
        for body in [
            "{}",
            "not json",
            r#"{"observations": [{"d": "2024-13-01", "FXUSDCAD": {"v": "1"}}]}"#,
            r#"{"observations": [{"d": "2024-01-02", "FXUSDCAD": {"v": "-1.3"}}]}"#,
        ] {
            let http = Canned::default().route(BASE, body);
            let err = fetch(&http, date("2024-01-02"), date("2024-01-02")).unwrap_err();
            assert!(matches!(err, FetchError::Response { .. }), "{body}: {err}");
            assert!(err.to_string().contains("FXUSDCAD"), "{err}");
        }
        let unreachable = Canned::default();
        let err = fetch(&unreachable, date("2024-01-02"), date("2024-01-02")).unwrap_err();
        assert!(matches!(err, FetchError::Http { .. }), "{err}");
    }

    #[test]
    fn nothing_is_asked_for_an_empty_range() {
        let http = Canned::default();
        let prices = fetch(&http, date("2024-01-03"), date("2024-01-02")).unwrap();
        assert!(prices.is_empty());
        assert!(http.asked.borrow().is_empty());
    }
}
