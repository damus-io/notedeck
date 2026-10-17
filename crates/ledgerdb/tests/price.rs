//! The price db: lookups at a date, chained and inverted rates, exact
//! conversion and its rounding, and misses.

use ledgerdb::{
    Added, Amount, CommodityId, Date, Journal, LayerFilter, Ledger, Parsed, PriceDb, PriceGap,
    Rate, Segment, SegmentHash, SegmentHeader, Unpriced,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// A journal holding `prices` plus one balanced transaction per amount in
/// `precisions` (such as `1.00 CAD`), so each commodity gets that precision.
fn journal(precisions: &[&str], prices: &str) -> Journal {
    let mut text = String::new();
    for amount in precisions {
        text.push_str(&format!(
            "2000-01-01 * precision\n  assets:x  {amount}\n  equity:x  -{amount}\n\n"
        ));
    }
    text.push_str(prices);
    parse_ok(&text)
}

/// A price db over `journal`'s prices.
fn db(journal: &Journal) -> PriceDb {
    PriceDb::new(&journal.prices, &journal.commodities)
}

fn id(journal: &Journal, name: &str) -> CommodityId {
    journal.commodities.get(name).expect("commodity in journal")
}

fn date(text: &str) -> Date {
    Date::parse(text).unwrap()
}

fn rate(num: i128, den: i128) -> Option<Rate> {
    Some(Rate::new(num, den).unwrap())
}

#[test]
fn a_lookup_takes_the_nearest_previous_rate() {
    let j = journal(
        &[],
        "P 2024-01-01 USD 1.30 CAD\n\
         P 2024-01-10 USD 1.40 CAD\n",
    );
    let db = db(&j);
    let (usd, cad) = (id(&j, "USD"), id(&j, "CAD"));
    assert_eq!(db.rate(usd, cad, date("2024-01-01")), rate(13, 10));
    assert_eq!(db.rate(usd, cad, date("2024-01-09")), rate(13, 10));
    assert_eq!(db.rate(usd, cad, date("2024-01-10")), rate(7, 5));
    assert_eq!(db.rate(usd, cad, date("2030-06-01")), rate(7, 5));
}

#[test]
fn no_rate_leaks_backwards_from_the_future() {
    let j = journal(
        &[],
        "P 2020-01-01 BTC 7000 USD\n\
         P 2020-01-01 USD 1.30 CAD\n\
         P 2024-01-01 BTC 60000 CAD\n\
         P 2024-01-01 USD 1.35 CAD\n\
         P 2024-01-01 BTC 44000 USD\n",
    );
    let db = db(&j);
    let (btc, usd, cad) = (id(&j, "BTC"), id(&j, "USD"), id(&j, "CAD"));

    assert_eq!(db.rate(usd, cad, date("2019-12-31")), None);
    // Before BTC/CAD existed the lookup chains through USD, at 2020's rates,
    // and once it exists the direct edge is shorter. Asked in either order,
    // since paths are memoized.
    let later = db.rate(btc, cad, date("2024-03-01"));
    let earlier = db.rate(btc, cad, date("2023-12-31"));
    assert_eq!(earlier, rate(7000 * 13, 10));
    assert_eq!(later, rate(60000, 1));
    let fresh = self::db(&j);
    assert_eq!(fresh.rate(btc, cad, date("2023-12-31")), earlier);
    assert_eq!(fresh.rate(btc, cad, date("2024-03-01")), later);
}

/// msat → sat → BTC → USD → CAD, the chain the importers' price feed emits.
fn four_hops() -> Journal {
    journal(
        &["1 msat", "1 sat", "1.00000000 BTC", "1.00 USD", "1.00 CAD"],
        "P 2024-01-01 msat 0.001 sat\n\
         P 2024-01-01 sat 0.00000001 BTC\n\
         P 2024-03-01 BTC 60000 USD\n\
         P 2024-03-01 USD 1.35 CAD\n",
    )
}

#[test]
fn a_four_hop_chain_converts_exactly() {
    let j = four_hops();
    let db = db(&j);
    let (msat, cad) = (id(&j, "msat"), id(&j, "CAD"));
    let on = date("2024-03-02");

    // 0.001 × 10^-8 × 60000 × 1.35 = 81 / 10^8.
    assert_eq!(db.rate(msat, cad, on), rate(81, 100_000_000));
    // 0.0015 BTC is 90 USD is 121.50 CAD.
    let amount = Amount {
        qty: 150_000_000,
        commodity: msat,
    };
    assert_eq!(
        db.convert(amount, cad, on),
        Some(Amount {
            qty: 12150,
            commodity: cad,
        })
    );
    // The chain did not exist before its last hop did.
    assert_eq!(db.rate(msat, cad, date("2024-02-29")), None);
}

#[test]
fn every_price_also_prices_the_other_way() {
    let j = journal(&["1.00 USD", "1.00 CAD"], "P 2024-01-01 USD 1.25 CAD\n");
    let db = db(&j);
    let (usd, cad) = (id(&j, "USD"), id(&j, "CAD"));
    let on = date("2024-01-01");
    assert_eq!(db.rate(cad, usd, on), rate(4, 5));
    let hundred = Amount {
        qty: 10000,
        commodity: cad,
    };
    assert_eq!(db.convert(hundred, usd, on).map(|a| a.qty), Some(8000));

    // A chain walked against both prices stays exact: 21 C is 3 B is 1 A.
    let j = journal(
        &["1.00 A", "1.00 C"],
        "P 2024-01-01 A 3 B\n\
         P 2024-01-01 B 7 C\n",
    );
    let db = self::db(&j);
    let (a, c) = (id(&j, "A"), id(&j, "C"));
    assert_eq!(db.rate(c, a, on), rate(1, 21));
    let amount = Amount {
        qty: 2100,
        commodity: c,
    };
    assert_eq!(db.convert(amount, a, on).map(|a| a.qty), Some(100));
}

#[test]
fn conversion_rounds_once_to_nearest_with_halves_away_from_zero() {
    let j = journal(
        &["1.00 USD", "1.00 CAD", "1 JPY"],
        "P 2024-01-01 USD 1.5 CAD\n\
         P 2024-01-01 USD 3 EUR\n\
         P 2024-01-01 USD 150.123456789 JPY\n",
    );
    let db = db(&j);
    let (usd, cad, jpy) = (id(&j, "USD"), id(&j, "CAD"), id(&j, "JPY"));
    let on = date("2024-01-01");
    let to_cad = |qty| {
        db.convert(
            Amount {
                qty,
                commodity: usd,
            },
            cad,
            on,
        )
        .unwrap()
        .qty
    };
    assert_eq!(to_cad(1), 2, "0.015 rounds up");
    assert_eq!(to_cad(-1), -2, "-0.015 rounds down, symmetric");
    assert_eq!(to_cad(3), 5, "0.045 rounds up");
    assert_eq!(to_cad(2), 3, "0.03 is exact");
    assert_eq!(to_cad(0), 0);

    // Through USD, against the CAD price: 1.00 CAD is 2/3 USD is 2 EUR.
    // EUR has no postings, so its precision is 0 and 2 EUR is qty 2.
    let eur = id(&j, "EUR");
    let cad_amount = Amount {
        qty: 100,
        commodity: cad,
    };
    assert_eq!(db.convert(cad_amount, eur, on).map(|a| a.qty), Some(2));

    // A rate finer than either commodity's precision is applied in full
    // before the one rounding: 1000.00 USD × 150.123456789 = 150123.456789.
    let thousand = Amount {
        qty: 100_000,
        commodity: usd,
    };
    assert_eq!(db.convert(thousand, jpy, on).map(|a| a.qty), Some(150_123));
    // And back: 150123 JPY is 999.996... USD, which rounds to 1000.00.
    let back = Amount {
        qty: 150_123,
        commodity: jpy,
    };
    assert_eq!(db.convert(back, usd, on).map(|a| a.qty), Some(100_000));
}

#[test]
fn a_missing_rate_is_none_and_counted() {
    let j = journal(
        &["1.00 USD", "1.00 CAD", "1.00 EUR"],
        "P 2024-01-01 USD 1.35 CAD\n",
    );
    let db = db(&j);
    let (usd, cad, eur) = (id(&j, "USD"), id(&j, "CAD"), id(&j, "EUR"));
    let one = |commodity| Amount {
        qty: 100,
        commodity,
    };

    let mut unpriced = Unpriced::default();
    assert!(unpriced.is_empty());
    assert_eq!(db.rate(eur, cad, date("2024-02-01")), None);
    for day in ["2024-02-01", "2023-06-01", "2024-01-15"] {
        assert_eq!(
            db.convert_counting(one(eur), cad, date(day), &mut unpriced),
            None
        );
    }
    // Too early for the one rate there is.
    assert_eq!(
        db.convert_counting(one(usd), cad, date("2023-01-01"), &mut unpriced),
        None
    );
    // A hit is not counted.
    assert!(
        db.convert_counting(one(usd), cad, date("2024-01-01"), &mut unpriced)
            .is_some()
    );
    // A commodity converts to itself without any price.
    assert_eq!(
        db.convert(one(eur), eur, date("1999-01-01")),
        Some(one(eur))
    );

    assert_eq!(unpriced.count, 4);
    assert_eq!(
        unpriced.gaps,
        vec![
            PriceGap {
                from: eur,
                to: cad,
                count: 3,
                first: date("2023-06-01"),
                last: date("2024-02-01"),
            },
            PriceGap {
                from: usd,
                to: cad,
                count: 1,
                first: date("2023-01-01"),
                last: date("2023-01-01"),
            },
        ]
    );
}

#[test]
fn zero_prices_are_ignored_and_same_day_duplicates_take_the_last() {
    let j = journal(
        &[],
        "P 2024-01-01 USD 0 CAD\n\
         P 2024-01-02 USD 1.30 CAD\n\
         P 2024-01-02 CAD 0.8 USD\n",
    );
    let db = db(&j);
    let (usd, cad) = (id(&j, "USD"), id(&j, "CAD"));
    assert_eq!(db.rate(usd, cad, date("2024-01-01")), None);
    // The second line, written the other way round, is the later one.
    assert_eq!(db.rate(usd, cad, date("2024-01-02")), rate(5, 4));
}

#[test]
fn from_ledger_settles_duplicates_by_segment_not_load_order() {
    let segment = |imported: u64, value: &str| {
        let header = SegmentHeader {
            source: "prices".to_owned(),
            imported,
            layer: "prices".to_owned(),
        };
        let text = format!("P 2024-01-01 USD {value} CAD\n");
        let bytes = Segment::encode(&header, &parse_ok(&text)).unwrap();
        Segment::parse(SegmentHash::of(bytes.as_bytes()), &bytes).unwrap()
    };
    let ledger_of = |order: [(u64, &str); 2]| {
        let mut ledger = Ledger::new(LayerFilter::All);
        for (imported, value) in order {
            assert_eq!(
                ledger.add_segment(segment(imported, value)),
                Ok(Added::Loaded)
            );
        }
        ledger
    };
    let (x, y) = ((1, "1.30"), (2, "1.40"));
    let winner = if segment(x.0, x.1).hash > segment(y.0, y.1).hash {
        x.1
    } else {
        y.1
    };
    let expected =
        Rate::from_decimal(parse_ok(&format!("P 2024-01-01 USD {winner} CAD\n")).prices[0].value);

    for order in [[x, y], [y, x]] {
        let ledger = ledger_of(order);
        let commodities = &ledger.journal().commodities;
        let (usd, cad) = (
            commodities.get("USD").unwrap(),
            commodities.get("CAD").unwrap(),
        );
        let db = PriceDb::from_ledger(&ledger);
        assert_eq!(db.rate(usd, cad, date("2024-01-01")), expected);
    }
}
