//! Folds and the query API: rollups agree with a naive fold over every
//! posting, conversion happens at each posting's date, top-N ordering and
//! shares, and register filters.

use std::fmt::Write;

use ledgerdb::{
    AccountId, Added, Bucket, CommodityId, Date, DateRange, Filter, Journal, LayerFilter, Ledger,
    Parsed, PriceDb, Rollups, Segment, SegmentHash, SegmentHeader, Total,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// A ledger of one segment holding `text`, giving each transaction that
/// has no `; id:` one, as segments require.
fn ledger(text: &str) -> Ledger {
    let mut with_ids = String::new();
    let mut lines = text.lines().peekable();
    let mut n = 0;
    while let Some(line) = lines.next() {
        with_ids.push_str(line);
        with_ids.push('\n');
        let header = line.starts_with(|c: char| c.is_ascii_digit());
        if header && !lines.peek().is_some_and(|next| next.contains("; id:")) {
            n += 1;
            with_ids.push_str(&format!("  ; id: auto:{n}\n"));
        }
    }
    let text = &with_ids;
    let header = SegmentHeader {
        source: "test".to_owned(),
        imported: 1,
        layer: "test".to_owned(),
    };
    let bytes = Segment::encode(&header, &parse_ok(text)).unwrap();
    let segment = Segment::parse(SegmentHash::of(bytes.as_bytes()), &bytes).unwrap();
    let mut ledger = Ledger::new(LayerFilter::All);
    assert_eq!(ledger.add_segment(segment), Ok(Added::Loaded));
    ledger
}

fn date(text: &str) -> Date {
    Date::parse(text).unwrap()
}

fn range(start: &str, end: &str) -> DateRange {
    DateRange::new(date(start), date(end))
}

fn account(ledger: &Ledger, name: &str) -> AccountId {
    ledger.journal().accounts.lookup(name).expect("account")
}

fn commodity(ledger: &Ledger, name: &str) -> CommodityId {
    ledger.journal().commodities.get(name).expect("commodity")
}

/// A deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Two years of random spending and income in CAD, USD and an unpriced
/// commodity, over accounts two to four deep, with a USD/CAD rate that
/// changes twice a month and starts after the first postings.
fn random_journal() -> String {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut out = String::new();
    let start = date("2023-01-01").days();
    for day in (0..730).step_by(15) {
        let d = Date::from_days(start + 20 + day);
        writeln!(out, "P {d} USD {}.{:02} CAD", 1 + day % 2, 10 + day % 80).unwrap();
    }
    out.push('\n');
    for i in 0..3000u32 {
        let d = Date::from_days(start + (i * 730 / 3000));
        let qty = rng.next() % 100_000 + 1;
        let commodity = match rng.next() % 10 {
            0..=6 => "CAD",
            7 | 8 => "USD",
            _ => "XYZ",
        };
        let leaf = rng.next() % 6;
        let account = match rng.next() % 5 {
            0 => format!("expenses:food:place{leaf}"),
            1 => "expenses:rent".to_owned(),
            2 => format!("expenses:travel:air:carrier{leaf}"),
            3 => "income:salary".to_owned(),
            _ => format!("income:side:client{leaf}"),
        };
        writeln!(
            out,
            "{d} * txn {i}\n  ; id: t:{i}\n  {account}  {}.{:02} {commodity}\n  assets:bank\n",
            qty / 100,
            qty % 100
        )
        .unwrap();
    }
    out
}

/// The sum of every posting to `root`'s subtree in `range`, natively per
/// commodity or converted at each posting's date, the slow obvious way.
struct Naive {
    totals: Vec<Total>,
    converted: i128,
    unconverted: u64,
}

fn naive(
    ledger: &Ledger,
    prices: &PriceDb,
    root: AccountId,
    range: DateRange,
    to: Option<CommodityId>,
) -> Naive {
    let accounts = &ledger.journal().accounts;
    let mut out = Naive {
        totals: Vec::new(),
        converted: 0,
        unconverted: 0,
    };
    for p in ledger.postings() {
        if !range.contains(p.date) || !(p.account == root || accounts.is_ancestor(root, p.account))
        {
            continue;
        }
        match out.totals.iter_mut().find(|t| t.commodity == p.commodity) {
            Some(t) => t.qty += i128::from(p.qty),
            None => out.totals.push(Total {
                commodity: p.commodity,
                qty: i128::from(p.qty),
            }),
        }
        let Some(to) = to else { continue };
        match prices.convert(p.amount(), to, p.date) {
            Some(c) => out.converted += i128::from(c.qty),
            None => out.unconverted += 1,
        }
    }
    out.totals.retain(|t| t.qty != 0);
    out.totals.sort_by_key(|t| t.commodity);
    out
}

/// Dates to check balances at: before, at and after month edges and the
/// ledger's ends.
const PROBES: &[&str] = &[
    "2022-12-31",
    "2023-01-01",
    "2023-01-20",
    "2023-01-31",
    "2023-02-01",
    "2023-06-15",
    "2023-12-31",
    "2024-02-29",
    "2024-03-01",
    "2024-12-31",
    "2030-01-01",
];

#[test]
fn balances_match_a_naive_fold() {
    let ledger = ledger(&random_journal());
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let usd = commodity(&ledger, "USD");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    for name in [
        "expenses",
        "expenses:food",
        "expenses:travel:air",
        "income",
        "income:side:client3",
        "assets:bank",
    ] {
        let root = account(&ledger, name);
        for at in PROBES {
            let through = DateRange::through(date(at));
            let native = q.balance(root, date(at), None);
            let want = naive(&ledger, &prices, root, through, Some(cad));
            assert_eq!(native.totals, want.totals, "{name} native at {at}");
            assert!(native.unconverted.is_empty());

            // In the home commodity: from the rollups.
            let home = q.balance(root, date(at), Some(cad));
            assert_eq!(home.get(cad), want.converted, "{name} in CAD at {at}");
            assert_eq!(home.unconverted.count, want.unconverted, "{name} at {at}");

            // In another commodity: a scan, same answer as naive.
            let other = q.balance(root, date(at), Some(usd));
            let want = naive(&ledger, &prices, root, through, Some(usd));
            assert_eq!(other.get(usd), want.converted, "{name} in USD at {at}");
            assert_eq!(other.unconverted.count, want.unconverted);
        }
    }
}

#[test]
fn flows_match_a_naive_fold_per_bucket() {
    let ledger = ledger(&random_journal());
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    let expenses = account(&ledger, "expenses");
    let all = q.span().unwrap();
    for bucket in [Bucket::Day, Bucket::Week, Bucket::Month, Bucket::Year] {
        for r in [
            all,
            range("2023-01-17", "2024-11-03"),
            range("2023-03-01", "2023-05-01"),
            range("2023-03-05", "2023-03-09"),
        ] {
            let series = q.flows(expenses, r, bucket, cad);
            assert_eq!(series.points.first().unwrap().start, r.start);
            assert_eq!(series.points.last().unwrap().end, r.end);
            for pair in series.points.windows(2) {
                assert_eq!(
                    pair[0].end, pair[1].start,
                    "{bucket:?} points are contiguous"
                );
            }
            let mut unconverted = 0;
            for point in &series.points {
                let want = naive(
                    &ledger,
                    &prices,
                    expenses,
                    DateRange::new(point.start, point.end),
                    Some(cad),
                );
                assert_eq!(point.total, want.converted, "{bucket:?} {r:?} {point:?}");
                assert_eq!(point.unconverted, want.unconverted);
                unconverted += point.unconverted;
            }
            assert_eq!(series.unconverted.count, unconverted);
            assert_eq!(
                series.total(),
                naive(&ledger, &prices, expenses, r, Some(cad)).converted
            );
        }
    }
}

#[test]
fn bucket_edges_fall_on_calendar_boundaries() {
    let ledger = ledger(&random_journal());
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    let root = account(&ledger, "expenses");
    let r = range("2023-01-04", "2023-03-02");
    let starts = |bucket| -> Vec<String> {
        q.flows(root, r, bucket, cad)
            .points
            .iter()
            .map(|p| p.start.to_string())
            .collect()
    };
    assert_eq!(
        starts(Bucket::Month),
        ["2023-01-04", "2023-02-01", "2023-03-01"]
    );
    assert_eq!(starts(Bucket::Year), ["2023-01-04"]);
    // 2023-01-09 is a Monday.
    assert_eq!(
        &starts(Bucket::Week)[..3],
        ["2023-01-04", "2023-01-09", "2023-01-16"]
    );
    assert_eq!(starts(Bucket::Day).len(), 57);
}

#[test]
fn conversion_happens_at_each_postings_date() {
    let ledger = ledger(
        "P 2024-01-01 USD 1.30 CAD\n\
         P 2024-02-01 USD 1.40 CAD\n\
         \n\
         2024-01-05 * a\n  expenses:x  100.00 USD\n  assets:usd\n\n\
         2024-02-05 * b\n  expenses:x  100.00 USD\n  assets:usd\n\n\
         2024-02-06 * c\n  expenses:x  5.00 CAD\n  assets:cad\n\n",
    );
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let usd = commodity(&ledger, "USD");
    let x = account(&ledger, "expenses:x");
    for home in [Some(cad), None] {
        let rollups = Rollups::new(&ledger, &prices, home);
        let q = rollups.query(&ledger, &prices);
        // 130 + 140 + 5, not 200 USD at the final rate (280) + 5.
        let b = q.balance(x, date("2024-02-28"), Some(cad));
        assert_eq!(
            b.totals,
            [Total {
                commodity: cad,
                qty: 27_500
            }]
        );
        let native = q.balance(x, date("2024-02-28"), None);
        assert_eq!(native.get(usd), 20_000);
        assert_eq!(native.get(cad), 500);
        let months = q.flows(x, range("2024-01-01", "2024-03-01"), Bucket::Month, cad);
        let totals: Vec<i128> = months.points.iter().map(|p| p.total).collect();
        assert_eq!(totals, [13_000, 14_500]);
    }
}

#[test]
fn unpriced_postings_are_reported_not_dropped() {
    let ledger = ledger(
        "P 2024-02-01 USD 1.40 CAD\n\
         \n\
         2024-01-01 * sets CAD's precision\n  assets:cad  1.00 CAD\n  equity:open\n\n\
         2024-01-05 * before any price\n  expenses:x  100.00 USD\n  assets:usd\n\n\
         2024-02-05 * priced\n  expenses:x  100.00 USD\n  assets:usd\n\n\
         2024-02-06 * never priced\n  expenses:x  7 ZZZ\n  assets:zzz\n\n",
    );
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let usd = commodity(&ledger, "USD");
    let zzz = commodity(&ledger, "ZZZ");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    let x = account(&ledger, "expenses:x");
    let b = q.balance(x, date("2024-12-31"), Some(cad));
    assert_eq!(b.get(cad), 14_000);
    assert_eq!(b.unconverted.count, 2);
    let mut want = vec![
        Total {
            commodity: usd,
            qty: 10_000,
        },
        Total {
            commodity: zzz,
            qty: 7,
        },
    ];
    want.sort_by_key(|t| t.commodity);
    assert_eq!(b.unconverted.native, want);
    let year = q.flows(x, range("2024-01-01", "2025-01-01"), Bucket::Year, cad);
    assert_eq!(year.points[0].total, 14_000);
    assert_eq!(year.points[0].unconverted, 2);
    assert_eq!(year.unconverted, b.unconverted);
}

#[test]
fn top_n_ranks_by_magnitude_with_shares() {
    let ledger = ledger(
        "2024-01-02 * a\n  expenses:food:coffee  30.00 CAD\n  assets:bank\n\n\
         2024-01-03 * b\n  expenses:food:grocer  50.00 CAD\n  assets:bank\n\n\
         2024-01-04 * c\n  expenses:rent  100.00 CAD\n  assets:bank\n\n\
         2024-01-05 * d\n  expenses:travel  20.00 CAD\n  assets:bank\n\n\
         2024-01-06 * refund\n  expenses:gifts  -20.00 CAD\n  assets:bank\n\n\
         2024-01-07 * untouched\n  expenses  1.00 CAD\n  assets:bank\n\n\
         2024-03-01 * later\n  expenses:travel  500.00 CAD\n  assets:bank\n\n",
    );
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    let expenses = account(&ledger, "expenses");
    let jan = range("2024-01-01", "2024-02-01");
    let name = |a| ledger.journal().accounts.full_name(a).to_owned();

    let top = q.top_n(expenses, 2, jan, 3, cad);
    let ranked: Vec<(String, i128)> = top
        .ranked
        .iter()
        .map(|r| (name(r.account), r.amount))
        .collect();
    assert_eq!(
        ranked,
        [
            ("expenses:rent".to_owned(), 10_000),
            ("expenses:food".to_owned(), 8_000),
            // A 20.00 tie, broken by name.
            ("expenses:gifts".to_owned(), -2_000),
        ]
    );
    // The total covers every depth-2 account, ranked or not, but not the
    // posting to `expenses` itself.
    assert_eq!(top.total, 10_000 + 8_000 + 2_000 - 2_000);
    assert!((top.ranked[0].share - 10_000.0 / 18_000.0).abs() < 1e-12);

    let deeper = q.top_n(expenses, 3, jan, 10, cad);
    let names: Vec<String> = deeper.ranked.iter().map(|r| name(r.account)).collect();
    assert_eq!(names, ["expenses:food:grocer", "expenses:food:coffee"]);
    let shares: f64 = deeper.ranked.iter().map(|r| r.share).sum();
    assert!((shares - 1.0).abs() < 1e-12);

    assert!(q.top_n(expenses, 2, jan, 0, cad).ranked.is_empty());
    let all = q.top_n(expenses, 2, q.span().unwrap(), 1, cad);
    assert_eq!(name(all.ranked[0].account), "expenses:travel");
}

#[test]
fn top_n_matches_a_naive_fold() {
    let ledger = ledger(&random_journal());
    let prices = PriceDb::from_ledger(&ledger);
    let cad = commodity(&ledger, "CAD");
    let rollups = Rollups::new(&ledger, &prices, Some(cad));
    let q = rollups.query(&ledger, &prices);
    let accounts = &ledger.journal().accounts;
    let expenses = account(&ledger, "expenses");
    let r = range("2023-02-11", "2024-07-19");
    let top = q.top_n(expenses, 3, r, 100, cad);
    let mut want: Vec<(String, i128)> = accounts
        .ids()
        .filter(|&a| accounts.depth(a) == 3 && accounts.is_ancestor(expenses, a))
        .map(|a| {
            (
                accounts.full_name(a).to_owned(),
                naive(&ledger, &prices, a, r, Some(cad)).converted,
            )
        })
        .filter(|(_, amount)| *amount != 0)
        .collect();
    want.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
    let got: Vec<(String, i128)> = top
        .ranked
        .iter()
        .map(|r| (accounts.full_name(r.account).to_owned(), r.amount))
        .collect();
    assert_eq!(got, want);
}

#[test]
fn register_filters() {
    let ledger = ledger(
        "2024-01-02 * Bean There Cafe\n  ; id: card:1\n  expenses:unknown  4.50 CAD\n  liabilities:card\n\n\
         2024-01-09 * CORNER GROCER\n  ; id: card:2\n  expenses:food:grocer  30.00 CAD\n  liabilities:card\n  ; raw-city: Toronto\n\n\
         2024-02-01 * Payroll\n  ; id: bank:1\n  ; raw-memo: pay\n  assets:bank  1000.00 CAD\n  income:unknown\n\n\
         2024-03-01 * bean there cafe\n  ; id: card:3\n  expenses:food:coffee  6.25 CAD\n  liabilities:card\n\n",
    );
    let prices = PriceDb::from_ledger(&ledger);
    let rollups = Rollups::new(&ledger, &prices, None);
    let q = rollups.query(&ledger, &prices);
    let ids = |filter: Filter| -> Vec<String> {
        q.register(filter)
            .map(|t| {
                let journal = ledger.journal();
                journal.strings.get(t.txn.id.unwrap()).to_owned()
            })
            .collect()
    };

    assert_eq!(
        ids(Filter::default()),
        ["card:1", "card:2", "bank:1", "card:3"]
    );
    let food = account(&ledger, "expenses:food");
    assert_eq!(
        ids(Filter {
            account: Some(food),
            ..Filter::default()
        }),
        ["card:2", "card:3"]
    );
    assert_eq!(
        ids(Filter {
            range: Some(range("2024-01-09", "2024-03-01")),
            ..Filter::default()
        }),
        ["card:2", "bank:1"]
    );
    assert_eq!(
        ids(Filter {
            payee: Some("BEAN there"),
            ..Filter::default()
        }),
        ["card:1", "card:3"]
    );
    // Posting-level metadata counts.
    assert_eq!(
        ids(Filter {
            meta_key: Some("raw-city"),
            ..Filter::default()
        }),
        ["card:2"]
    );
    assert!(
        ids(Filter {
            meta_key: Some("no-such-key"),
            ..Filter::default()
        })
        .is_empty()
    );
    assert_eq!(
        ids(Filter {
            record_id: Some("bank:1"),
            ..Filter::default()
        }),
        ["bank:1"]
    );
    assert!(
        ids(Filter {
            record_id: Some("card:9"),
            ..Filter::default()
        })
        .is_empty()
    );
    let unknown = [
        account(&ledger, "expenses:unknown"),
        account(&ledger, "income:unknown"),
    ];
    assert_eq!(
        ids(Filter {
            unknown: Some(&unknown),
            ..Filter::default()
        }),
        ["card:1", "bank:1"]
    );
    // Every field at once.
    let card = account(&ledger, "liabilities:card");
    assert_eq!(
        ids(Filter {
            account: Some(card),
            range: Some(range("2024-01-01", "2024-02-01")),
            payee: Some("cafe"),
            unknown: Some(&unknown),
            ..Filter::default()
        }),
        ["card:1"]
    );
    let t = q
        .register(Filter {
            record_id: Some("card:3"),
            ..Filter::default()
        })
        .next()
        .unwrap();
    assert_eq!(Some(t.id), ledger.find("card:3"));
}

#[test]
#[should_panic(expected = "rollups were built from a different ledger")]
fn rollups_refuse_another_ledger() {
    let small = ledger("2024-01-02 * a\n  expenses:x  1.00 CAD\n  assets:bank\n\n");
    let big = ledger(&random_journal());
    let prices = PriceDb::from_ledger(&small);
    let rollups = Rollups::new(&small, &prices, None);
    rollups.query(&big, &prices);
}
