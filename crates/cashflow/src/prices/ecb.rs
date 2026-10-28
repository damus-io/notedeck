//! The European Central Bank's daily reference rates, crossed into CAD, for
//! the days the Bank of Canada ([`super::boc`]) has no rate.
//!
//! The ECB quotes every currency per euro, CAD among them, every TARGET
//! business day from 1999-01-04, so one currency in CAD on a day is CAD per
//! euro over that currency per euro, rounded to [`SIGNIFICANT`] digits.
//! EUR in CAD is the CAD quote itself.
//!
//! The ECB rate is only used where Valet has nothing, in the stretches
//! [`FILLS`] lists. The rates are a different fixing, so mixing them with
//! the Bank of Canada's on the same days would make a currency's rate jump
//! between the two from one day to the next. A lookup takes a pair's nearest
//! earlier rate however old it is. So without these fills, DKK would be
//! priced at its last legacy rate of 2017 forever after, and THB at its
//! 2019 rate until 2026.
//!
//! The data API answers a series key such as `D.CAD+DKK.EUR.SP00.A` with
//! CSV, and with `detail=dataonly` its columns are `KEY, FREQ, CURRENCY,
//! CURRENCY_DENOM, EXR_TYPE, EXR_SUFFIX, TIME_PERIOD, OBS_VALUE`. A holiday
//! is a row with an empty `OBS_VALUE`, and a range with no rates at all is
//! an empty body.

use std::collections::HashMap;

use ledgerdb::{Date, Ymd};

use super::{CAD, FetchError, Http, decimal};
use crate::import::{RawAmount, RawPrice};

/// The daily exchange rate series endpoint; a series key and query follow.
pub const BASE: &str = "https://data-api.ecb.europa.eu/service/data/EXR/";

/// The significant digits a crossed rate keeps: the ECB publishes five or
/// six.
pub const SIGNIFICANT: u32 = 6;

/// The euro, which the ECB quotes everything in.
const EUR: &str = "EUR";

/// A stretch of days on which this source prices `codes` in CAD because
/// the Bank of Canada does not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub codes: &'static [&'static str],
    pub first: Ymd,
    /// The last day, or `None` while it lasts.
    pub last: Option<Ymd>,
}

impl Fill {
    fn first(&self) -> Date {
        Date::from_ymd(self.first).expect("a calendar date")
    }

    fn last(&self) -> Option<Date> {
        self.last
            .map(|l| Date::from_ymd(l).expect("a calendar date"))
    }
}

/// Where the Bank of Canada has no rate, as Valet answered on 2026-10-27.
pub const FILLS: &[Fill] = &[
    // Before Valet's legacy series start on 2007-05-01: every currency boc
    // prices.
    Fill {
        codes: &[
            "AUD", "BRL", "CHF", "DKK", "EUR", "GBP", "HKD", "JPY", "MXN", "NOK", "RUB", "THB",
            "TRY", "USD",
        ],
        first: Ymd {
            year: 1999,
            month: 1,
            day: 4,
        },
        last: Some(Ymd {
            year: 2007,
            month: 4,
            day: 30,
        }),
    },
    // DKK has no daily series, and its legacy one ends on 2017-04-28.
    Fill {
        codes: &["DKK"],
        first: Ymd {
            year: 2017,
            month: 4,
            day: 29,
        },
        last: None,
    },
    // FXTHBCAD has no rates from 2020 until 2026-05-01.
    Fill {
        codes: &["THB"],
        first: Ymd {
            year: 2020,
            month: 1,
            day: 1,
        },
        last: Some(Ymd {
            year: 2026,
            month: 4,
            day: 30,
        }),
    },
];

/// The first day of the ECB's reference rates.
pub fn first_day() -> Date {
    FILLS[0].first()
}

/// The first day on or after `day` this source has rates for, or `None`
/// when every fill ends before it.
pub fn reach(day: Date) -> Option<Date> {
    FILLS
        .iter()
        .filter(|f| f.last().is_none_or(|last| last >= day))
        .map(|f| f.first().max(day))
        .min()
}

/// The fills' rates in CAD on business days from `since` through `today`.
pub fn fetch(http: &dyn Http, since: Date, today: Date) -> Result<Vec<RawPrice>, FetchError> {
    let mut prices = Vec::new();
    for fill in FILLS {
        let start = since.max(fill.first());
        let end = fill.last().map_or(today, |last| last.min(today));
        if start > end {
            continue;
        }
        fetch_fill(http, fill, start, end, &mut prices)?;
    }
    Ok(prices)
}

/// `fill`'s rates from `start` through `end` onto `prices`.
fn fetch_fill(
    http: &dyn Http,
    fill: &Fill,
    start: Date,
    end: Date,
    prices: &mut Vec<RawPrice>,
) -> Result<(), FetchError> {
    let mut key = CAD.to_owned();
    for code in fill.codes.iter().filter(|&&c| c != EUR) {
        key.push('+');
        key.push_str(code);
    }
    let url = format!(
        "{BASE}D.{key}.EUR.SP00.A?startPeriod={start}&endPeriod={end}&format=csvdata&detail=dataonly"
    );
    let body = http.get(&url).map_err(|error| FetchError::Http {
        url: url.clone(),
        error,
    })?;
    let body = std::str::from_utf8(&body).map_err(|e| FetchError::response(&url, e))?;
    let quotes = parse(body, &url)?;

    let cad: HashMap<Date, &RawAmount> = quotes
        .iter()
        .filter(|q| q.code == CAD)
        .map(|q| (q.day, &q.per_euro))
        .collect();
    let in_range = |day: Date| day >= start && day <= end;
    if fill.codes.contains(&EUR) {
        for (&day, &per_euro) in cad.iter().filter(|(day, _)| in_range(**day)) {
            prices.push(RawPrice {
                date: day,
                commodity: EUR.to_owned(),
                price: per_euro.clone(),
            });
        }
    }
    for quote in &quotes {
        if quote.code == CAD || !in_range(quote.day) || !fill.codes.contains(&quote.code) {
            continue;
        }
        let Some(cad) = cad.get(&quote.day) else {
            continue;
        };
        let price = cross(cad, &quote.per_euro).ok_or_else(|| {
            FetchError::response(
                &url,
                format!("{} {}: no cross into CAD", quote.day, quote.code),
            )
        })?;
        prices.push(RawPrice {
            date: quote.day,
            commodity: quote.code.to_owned(),
            price,
        });
    }
    Ok(())
}

/// One currency's rate per euro on one day.
struct Quote<'a> {
    day: Date,
    code: &'a str,
    per_euro: RawAmount,
}

/// The quotes in a `csvdata` response, skipping days with no value.
fn parse<'a>(body: &'a str, url: &str) -> Result<Vec<Quote<'a>>, FetchError> {
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());
    let Some(header) = lines.next() else {
        return Ok(Vec::new());
    };
    let columns: Vec<&str> = header.split(',').collect();
    let column = |name: &str| {
        columns
            .iter()
            .position(|c| *c == name)
            .ok_or_else(|| FetchError::response(url, format!("no {name} column in {header:?}")))
    };
    let (code_at, denom_at) = (column("CURRENCY")?, column("CURRENCY_DENOM")?);
    let (day_at, value_at) = (column("TIME_PERIOD")?, column("OBS_VALUE")?);

    let mut quotes = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let field = |at: usize| {
            fields
                .get(at)
                .copied()
                .ok_or_else(|| FetchError::response(url, format!("short row {line:?}")))
        };
        if field(denom_at)? != EUR {
            return Err(FetchError::response(url, format!("not per euro: {line:?}")));
        }
        let value = field(value_at)?;
        if value.is_empty() {
            continue;
        }
        let day = Date::parse(field(day_at)?)
            .map_err(|_| FetchError::response(url, format!("bad day in {line:?}")))?;
        let code = field(code_at)?;
        let per_euro = decimal(value, code)
            .ok_or_else(|| FetchError::response(url, format!("{day}: bad rate {value:?}")))?;
        quotes.push(Quote {
            day,
            code,
            per_euro,
        });
    }
    Ok(quotes)
}

/// One unit of a currency in CAD, from CAD per euro and that currency per
/// euro: `cad / other`, rounded half up to [`SIGNIFICANT`] digits (whole
/// units at least), or `None` if it doesn't fit.
fn cross(cad: &RawAmount, other: &RawAmount) -> Option<RawAmount> {
    // cad.qty / 10^cad.precision ÷ (other.qty / 10^other.precision).
    let num = i128::from(cad.qty).checked_mul(10i128.checked_pow(other.precision.into())?)?;
    let den = i128::from(other.qty).checked_mul(10i128.checked_pow(cad.precision.into())?)?;
    let floor = 10i128.pow(SIGNIFICANT - 1);
    let mut precision = 0u8;
    while num.checked_mul(10i128.checked_pow(precision.into())?)? / den < floor {
        if precision == ledgerdb::MAX_PRECISION {
            return None;
        }
        precision += 1;
    }
    let scaled = num.checked_mul(10i128.pow(precision.into()))?;
    let qty = (scaled.checked_mul(2)? + den) / (den * 2);
    Some(RawAmount {
        qty: i64::try_from(qty).ok()?,
        precision,
        commodity: CAD.to_owned(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::test_http::Canned;
    use super::*;

    /// A real response for days before Valet begins, trimmed to two
    /// currencies; 2007-01-01 is a holiday with no values.
    pub const BEFORE_VALET: &str = "\
KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE
EXR.D.CAD.EUR.SP00.A,D,CAD,EUR,SP00,A,2006-12-29,1.5281
EXR.D.CAD.EUR.SP00.A,D,CAD,EUR,SP00,A,2007-01-01,
EXR.D.CAD.EUR.SP00.A,D,CAD,EUR,SP00,A,2007-01-02,1.5452
EXR.D.JPY.EUR.SP00.A,D,JPY,EUR,SP00,A,2006-12-29,156.93
EXR.D.JPY.EUR.SP00.A,D,JPY,EUR,SP00,A,2007-01-01,
EXR.D.JPY.EUR.SP00.A,D,JPY,EUR,SP00,A,2007-01-02,157.76
EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2006-12-29,1.317
EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2007-01-01,
EXR.D.USD.EUR.SP00.A,D,USD,EUR,SP00,A,2007-01-02,1.327
";

    /// A real response for days Valet has no DKK or THB rate.
    pub const GAPS: &str = "\
KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE
EXR.D.CAD.EUR.SP00.A,D,CAD,EUR,SP00,A,2023-12-29,1.4642
EXR.D.CAD.EUR.SP00.A,D,CAD,EUR,SP00,A,2024-01-02,1.4565
EXR.D.DKK.EUR.SP00.A,D,DKK,EUR,SP00,A,2023-12-29,7.4529
EXR.D.DKK.EUR.SP00.A,D,DKK,EUR,SP00,A,2024-01-02,7.4551
EXR.D.THB.EUR.SP00.A,D,THB,EUR,SP00,A,2023-12-29,37.973
EXR.D.THB.EUR.SP00.A,D,THB,EUR,SP00,A,2024-01-02,37.563
";

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

    #[test]
    fn rates_cross_through_the_euro_into_cad() {
        let http = Canned::default().route(BASE, BEFORE_VALET);
        let prices = fetch(&http, date("2006-12-29"), date("2007-01-02")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                // 1.5281 / 1.317 = 1.160288…, 1.5281 / 156.93 = 0.009737462…
                "2006-12-29 EUR 15281/4 CAD",
                "2006-12-29 JPY 973746/8 CAD",
                "2006-12-29 USD 116029/5 CAD",
                "2007-01-02 EUR 15452/4 CAD",
                "2007-01-02 JPY 979462/8 CAD",
                "2007-01-02 USD 116443/5 CAD",
            ]
        );
        let asked = http.asked.borrow();
        assert_eq!(asked.len(), 1, "only the stretch before Valet: {asked:?}");
        assert!(asked[0].starts_with(&format!(
            "{BASE}D.CAD+AUD+BRL+CHF+DKK+GBP+HKD+JPY+MXN+NOK+RUB+THB+TRY+USD.EUR.SP00.A?\
             startPeriod=2006-12-29&endPeriod=2007-01-02&"
        )));
    }

    #[test]
    fn only_the_gaps_in_valet_are_filled() {
        let http = Canned::default().route(BASE, GAPS);
        let prices = fetch(&http, date("2023-12-29"), date("2024-01-02")).unwrap();
        assert_eq!(
            lines(&prices),
            [
                // 1.4642 / 7.4529 = 0.196460…, 1.4642 / 37.973 = 0.0385590…
                "2023-12-29 DKK 196460/6 CAD",
                "2023-12-29 THB 385590/7 CAD",
                "2024-01-02 DKK 195370/6 CAD",
                "2024-01-02 THB 387749/7 CAD",
            ]
        );
        let asked = http.asked.borrow();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[0].starts_with(&format!("{BASE}D.CAD+DKK.EUR.SP00.A?")));
        assert!(asked[1].starts_with(&format!("{BASE}D.CAD+THB.EUR.SP00.A?")));
    }

    #[test]
    fn nothing_is_asked_where_valet_has_every_rate() {
        let http = Canned::default();
        // The legacy series run 2007-05-01 through 2017-04-28 for DKK, and
        // THB's daily series has rates through 2019.
        let prices = fetch(&http, date("2007-05-01"), date("2017-04-28")).unwrap();
        assert!(prices.is_empty());
        assert!(http.asked.borrow().is_empty());
    }

    #[test]
    fn an_empty_body_is_no_rates() {
        let http = Canned::default().route(BASE, "");
        let prices = fetch(&http, date("2024-01-06"), date("2024-01-07")).unwrap();
        assert!(prices.is_empty());
    }

    #[test]
    fn a_bad_response_is_an_error_naming_the_url() {
        for body in [
            "KEY,CURRENCY,OBS_VALUE\nx,CAD,1\n",
            "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE\n\
             EXR.D.DKK.USD.SP00.A,D,DKK,USD,SP00,A,2024-01-02,6.7\n",
            "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE\n\
             EXR.D.DKK.EUR.SP00.A,D,DKK,EUR,SP00,A,2024-13-02,7.4\n",
            "KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE\n\
             EXR.D.DKK.EUR.SP00.A,D,DKK,EUR,SP00,A,2024-01-02,NaN\n",
        ] {
            let http = Canned::default().route(BASE, body);
            let err = fetch(&http, date("2024-01-02"), date("2024-01-02")).unwrap_err();
            assert!(matches!(err, FetchError::Response { .. }), "{body}: {err}");
            assert!(err.to_string().contains(BASE), "{err}");
        }
    }

    #[test]
    fn the_first_fill_covers_every_currency_boc_prices() {
        let boc: Vec<_> = super::super::boc::CURRENCIES
            .iter()
            .map(|c| c.code)
            .collect();
        assert_eq!(FILLS[0].codes, boc.as_slice());
        assert_eq!(
            FILLS[0].last().map(|l| l.days() + 1),
            Some(super::super::boc::first_day().days())
        );
    }

    #[test]
    fn reach_is_the_first_day_with_a_fill() {
        assert_eq!(reach(date("1990-01-01")), Some(date("1999-01-04")));
        assert_eq!(reach(date("2010-06-01")), Some(date("2017-04-29")));
        assert_eq!(reach(date("2022-07-26")), Some(date("2022-07-26")));
    }

    #[test]
    fn crosses_round_half_up_to_six_digits() {
        let amount = |t| decimal(t, "X").unwrap();
        let cross = |a, b| cross(&amount(a), &amount(b)).map(|c| (c.qty, c.precision));
        assert_eq!(cross("1.5", "1"), Some((150000, 5)));
        // 1 / 8 = 0.125 exactly; 1 / 3 = 0.333333…; 2 / 3 = 0.6666666… up.
        assert_eq!(cross("1", "8"), Some((125000, 6)));
        assert_eq!(cross("1", "3"), Some((333333, 6)));
        assert_eq!(cross("2", "3"), Some((666667, 6)));
        // Large rates keep whole units.
        assert_eq!(cross("1234567", "1"), Some((1234567, 0)));
    }
}
