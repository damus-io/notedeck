//! The Bank of Canada's daily rates into CAD, from its Valet API.
//!
//! Valet quotes each of [`CURRENCIES`] in CAD in two series:
//!
//! - a daily average, `FX<code>CAD`, from 2017-01-03 on;
//! - a legacy noon rate, which ran from 2007-05-01 through 2017-04-28.
//!
//! Each currency's days before 2017-01-03 come from its legacy series, and
//! the rest from its daily one. A currency with no daily series (DKK) keeps
//! its legacy rates to their end. Nothing in Valet goes back before
//! 2007-05-01. All of them publish business days only.
//!
//! Valet answers several series in one request, so a fetch is at most two:
//! one for the legacy series and one for the daily ones. A response is
//! `{"observations": [{"d": "2024-01-02", "FXUSDCAD": {"v": "1.3316"},
//! "FXEURCAD": {"v": "1.4584"}, …}, …]}`, where a series with no rate that
//! day is missing or has no value; a day none of them have is left out.

use ledgerdb::{Date, Ymd};
use serde_json::Value;

use super::{CAD, FetchError, Http, decimal, get_json};
use crate::import::RawPrice;

/// The Valet observations endpoint; the series names and query follow.
pub const BASE: &str = "https://www.bankofcanada.ca/valet/observations/";

/// One currency Valet quotes in CAD, and the series that quote it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Currency {
    /// The commodity, as the journal writes it.
    pub code: &'static str,
    /// The daily average series, from [`fx_start`] on, if there is one.
    pub fx: Option<&'static str>,
    /// The legacy noon series, through [`legacy_end`].
    pub legacy: &'static str,
}

/// The currencies this source prices in CAD.
///
/// Valet's legacy series also quote the UAE dirham, but AED is left out: a
/// lookup takes a pair's nearest earlier rate however old it is, so an
/// AED/CAD rate that ends in 2017 would go on pricing AED for every day
/// after.
pub const CURRENCIES: &[Currency] = &[
    Currency {
        code: "AUD",
        fx: Some("FXAUDCAD"),
        legacy: "IEXE1601",
    },
    Currency {
        code: "BRL",
        fx: Some("FXBRLCAD"),
        legacy: "IEXE2801",
    },
    Currency {
        code: "CHF",
        fx: Some("FXCHFCAD"),
        legacy: "IEXE1101",
    },
    Currency {
        code: "DKK",
        fx: None,
        legacy: "IEXE0301",
    },
    Currency {
        code: "EUR",
        fx: Some("FXEURCAD"),
        legacy: "EUROCAE01",
    },
    Currency {
        code: "GBP",
        fx: Some("FXGBPCAD"),
        legacy: "IEXE1201",
    },
    Currency {
        code: "HKD",
        fx: Some("FXHKDCAD"),
        legacy: "IEXE1401",
    },
    Currency {
        code: "JPY",
        fx: Some("FXJPYCAD"),
        legacy: "IEXE0701",
    },
    Currency {
        code: "MXN",
        fx: Some("FXMXNCAD"),
        legacy: "IEXE2001",
    },
    Currency {
        code: "NOK",
        fx: Some("FXNOKCAD"),
        legacy: "IEXE0901",
    },
    Currency {
        code: "RUB",
        fx: Some("FXRUBCAD"),
        legacy: "IEXE2101",
    },
    Currency {
        code: "THB",
        fx: Some("FXTHBCAD"),
        legacy: "IEXE3601",
    },
    Currency {
        code: "TRY",
        fx: Some("FXTRYCAD"),
        legacy: "IEXE5802",
    },
    Currency {
        code: "USD",
        fx: Some("FXUSDCAD"),
        legacy: "IEXE0101",
    },
];

/// The first day of the legacy noon series, and of this source.
pub fn first_day() -> Date {
    ymd(2007, 5, 1)
}

/// The first day of the daily `FX…CAD` series.
pub fn fx_start() -> Date {
    ymd(2017, 1, 3)
}

/// The last day of the legacy noon series.
pub fn legacy_end() -> Date {
    ymd(2017, 4, 28)
}

fn ymd(year: u16, month: u8, day: u8) -> Date {
    Date::from_ymd(Ymd { year, month, day }).expect("a calendar date")
}

/// One series asked for: whose rates it holds, and the last day they are
/// taken from it.
struct Quoted {
    series: &'static str,
    code: &'static str,
    last: Date,
}

/// The rates on business days from `since` through `today`.
pub fn fetch(http: &dyn Http, since: Date, today: Date) -> Result<Vec<RawPrice>, FetchError> {
    let mut prices = Vec::new();
    let fx_start = fx_start();
    let before_fx = Date::from_days(fx_start.days() - 1);
    let legacy: Vec<Quoted> = CURRENCIES
        .iter()
        .map(|c| Quoted {
            series: c.legacy,
            code: c.code,
            last: if c.fx.is_some() {
                before_fx
            } else {
                legacy_end()
            },
        })
        .collect();
    let legacy_until = legacy.iter().map(|q| q.last).max().unwrap_or(before_fx);
    fetch_series(http, &legacy, since, today.min(legacy_until), &mut prices)?;

    let fx: Vec<Quoted> = CURRENCIES
        .iter()
        .filter_map(|c| {
            Some(Quoted {
                series: c.fx?,
                code: c.code,
                last: today,
            })
        })
        .collect();
    fetch_series(http, &fx, since.max(fx_start), today, &mut prices)?;
    Ok(prices)
}

/// The rates of `quoted` from `start` through `end` onto `prices`, each
/// series' only through its own `last` day.
fn fetch_series(
    http: &dyn Http,
    quoted: &[Quoted],
    start: Date,
    end: Date,
    prices: &mut Vec<RawPrice>,
) -> Result<(), FetchError> {
    if start > end || quoted.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = quoted.iter().map(|q| q.series).collect();
    let url = format!(
        "{BASE}{}/json?start_date={start}&end_date={end}",
        names.join(",")
    );
    let body: Value = get_json(http, &url)?;
    let mut read = Vec::new();
    parse(&body, quoted, &url, &mut read)?;
    // Valet keeps to the range asked; the `start`/`end` check only guards
    // against a server that doesn't.
    prices.extend(
        read.into_iter()
            .filter(|p| p.date >= start && p.date <= end),
    );
    Ok(())
}

/// Read a Valet response holding the series of `quoted` onto `prices`.
fn parse(
    body: &Value,
    quoted: &[Quoted],
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
        for q in quoted.iter().filter(|q| day <= q.last) {
            let value = observation
                .get(q.series)
                .and_then(|s| s.get("v"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty());
            let Some(value) = value else {
                continue;
            };
            let price = decimal(value, CAD).ok_or_else(|| {
                FetchError::response(url, format!("{day} {}: bad rate {value:?}", q.series))
            })?;
            prices.push(RawPrice {
                date: day,
                commodity: q.code.to_owned(),
                price,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::test_http::Canned;
    use super::*;

    /// A real daily-series response around a new year, trimmed to three
    /// series. Valet leaves out the 2024-01-01 holiday, and `FXTHBCAD` has
    /// no rates from 2020 into 2026, so it is missing from every day.
    pub const FIXTURE: &str = r#"{
        "seriesDetail": {"FXUSDCAD": {"label": "USD/CAD"}},
        "observations": [
            {"d": "2023-12-27", "FXEURCAD": {"v": "1.4656"}, "FXUSDCAD": {"v": "1.3205"}, "FXJPYCAD": {"v": "0.009290"}},
            {"d": "2023-12-28", "FXUSDCAD": {"v": "1.3212"}, "FXJPYCAD": {"v": "0.009360"}, "FXEURCAD": {"v": "1.4640"}},
            {"d": "2023-12-29", "FXJPYCAD": {"v": "0.009350"}, "FXUSDCAD": {"v": "1.3226"}, "FXEURCAD": {"v": "1.4626"}},
            {"d": "2024-01-02", "FXUSDCAD": {"v": "1.3316"}, "FXJPYCAD": {"v": "0.009380"}, "FXEURCAD": {"v": "1.4584"}},
            {"d": "2024-01-03", "FXUSDCAD": {"v": "1.3356"}, "FXJPYCAD": {"v": "0.009320"}, "FXEURCAD": {"v": "1.4577"}}
        ]
    }"#;

    /// A real legacy response across the switch, trimmed to three series:
    /// on 2017-01-03 the legacy USD rate is still published but the daily
    /// one takes over, while DKK, which has no daily series, stays legacy.
    const LEGACY: &str = r#"{"observations": [
        {"d": "2016-12-29", "IEXE0101": {"v": "1.3488"}, "IEXE0301": {"v": "0.1902"}, "EUROCAE01": {"v": "1.4140"}},
        {"d": "2016-12-30", "IEXE0101": {"v": "1.3427"}, "EUROCAE01": {"v": "1.4169"}, "IEXE0301": {"v": "0.1906"}},
        {"d": "2017-01-03", "IEXE0301": {"v": "0.1883"}, "IEXE0101": {"v": "1.3438"}, "EUROCAE01": {"v": "1.3996"}}
    ]}"#;

    /// A real daily-series response for the series' first day, trimmed.
    const FIRST_DAYS: &str = r#"{"observations": [
        {"d": "2017-01-03", "FXUSDCAD": {"v": "1.3435"}, "FXEURCAD": {"v": "1.3995"}}
    ]}"#;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn lines(prices: &[RawPrice]) -> Vec<String> {
        let mut lines: Vec<String> = prices
            .iter()
            .map(|p| {
                format!(
                    "{} {} {}/{} {}",
                    p.date, p.commodity, p.price.qty, p.price.precision, p.price.commodity
                )
            })
            .collect();
        lines.sort();
        lines
    }

    fn all(series: impl Fn(&Currency) -> Option<&'static str>) -> String {
        let names: Vec<_> = CURRENCIES.iter().filter_map(series).collect();
        names.join(",")
    }

    #[test]
    fn one_request_reads_every_currency_in_cad() {
        let http = Canned::default().route(BASE, FIXTURE);
        let prices = fetch(&http, date("2023-12-29"), date("2024-01-02")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                "2023-12-29 EUR 14626/4 CAD",
                "2023-12-29 JPY 9350/6 CAD",
                "2023-12-29 USD 13226/4 CAD",
                "2024-01-02 EUR 14584/4 CAD",
                "2024-01-02 JPY 9380/6 CAD",
                "2024-01-02 USD 13316/4 CAD",
            ]
        );
        assert_eq!(
            *http.asked.borrow(),
            [format!(
                "{BASE}{}/json?start_date=2023-12-29&end_date=2024-01-02",
                all(|c| c.fx)
            )]
        );
    }

    #[test]
    fn days_before_2017_come_from_the_legacy_series() {
        let http = Canned::default()
            .route(&format!("{BASE}IEXE"), LEGACY)
            .route(&format!("{BASE}FX"), FIRST_DAYS);
        let prices = fetch(&http, date("2016-12-29"), date("2017-01-03")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                "2016-12-29 DKK 1902/4 CAD",
                "2016-12-29 EUR 14140/4 CAD",
                "2016-12-29 USD 13488/4 CAD",
                "2016-12-30 DKK 1906/4 CAD",
                "2016-12-30 EUR 14169/4 CAD",
                "2016-12-30 USD 13427/4 CAD",
                // DKK stays on its legacy series; the rest switch.
                "2017-01-03 DKK 1883/4 CAD",
                "2017-01-03 EUR 13995/4 CAD",
                "2017-01-03 USD 13435/4 CAD",
            ]
        );
        assert_eq!(
            *http.asked.borrow(),
            [
                format!(
                    "{BASE}{}/json?start_date=2016-12-29&end_date=2017-01-03",
                    all(|c| Some(c.legacy))
                ),
                format!(
                    "{BASE}{}/json?start_date=2017-01-03&end_date=2017-01-03",
                    all(|c| c.fx)
                ),
            ]
        );

        // Entirely before the switch: only the legacy series are asked.
        let legacy_only = Canned::default().route(BASE, LEGACY);
        fetch(&legacy_only, date("2016-12-29"), date("2016-12-30")).unwrap();
        assert_eq!(legacy_only.asked.borrow().len(), 1);
    }

    #[test]
    fn the_legacy_series_are_not_asked_after_they_end() {
        let http = Canned::default().route(BASE, FIXTURE);
        fetch(&http, date("2017-04-29"), date("2017-05-01")).unwrap();
        let asked = http.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].contains("FXUSDCAD"), "{asked:?}");
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
