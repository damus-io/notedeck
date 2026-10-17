//! Query benchmark. Ignored by default; run in release with
//!
//! ```text
//! cargo test --release -p ledgerdb --test bench_query -- --ignored --nocapture
//! ```
//!
//! It prints counts and timings only.

use std::fmt::Write;
use std::time::Instant;

use ledgerdb::{
    Bucket, Date, DateRange, Filter, Journal, JournalStore, LayerFilter, Ledger, PriceDb, Rollups,
    SegmentHeader,
};

/// A deterministic xorshift generator, so every run builds the same journal.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const TXNS: usize = 350_000;
const DAYS: usize = 1461;

/// Transactions `first..first + n` of the lightning-node-shaped journal
/// `bench_ledger` uses: four years, two msat postings each, 4000 zap peers.
fn synthetic(rng: &mut Rng, first: usize, n: usize) -> String {
    let mut out = String::with_capacity(n * 130);
    let start = Date::parse("2022-01-01").unwrap().days();
    for i in first..first + n {
        let date = Date::from_days(start + (i * DAYS / TXNS) as u32);
        let msat = rng.next() % 50_000_000 + 1;
        let roll = rng.next() % 100;
        let (payee, from, to) = if roll < 85 {
            let peer = rng.next() % 4000;
            (
                "invoice",
                format!("income:zap:{peer:016x}"),
                "assets:cln".to_owned(),
            )
        } else if roll < 95 {
            (
                "routed",
                "assets:cln".to_owned(),
                "income:routing".to_owned(),
            )
        } else {
            let channel = rng.next() % 300;
            (
                "onchain_fee",
                "assets:cln".to_owned(),
                format!("expenses:fees:{channel}x{}", channel * 7 % 13),
            )
        };
        writeln!(out, "{date}\t{payee}\n\t; id: cln:{i:08x}").unwrap();
        writeln!(out, "\t{to}\t{msat} msat\n\t{from}\t-{msat} msat\n").unwrap();
    }
    out
}

/// A daily msat/CAD rate over the four years, and one CAD posting so CAD
/// has cents.
fn prices() -> String {
    let mut out = String::new();
    let start = Date::parse("2022-01-01").unwrap().days();
    for day in 0..DAYS as u32 {
        let date = Date::from_days(start + day);
        writeln!(out, "P {date} msat 0.000000{} CAD", 500 + day % 700).unwrap();
    }
    writeln!(
        out,
        "\n2022-01-01 * open\n\t; id: open:1\n\tassets:cad\t1.00 CAD\n\tequity:open\n"
    )
    .unwrap();
    out
}

/// The best of `runs` timings of `f`, in milliseconds, and its last result.
struct Timed<T> {
    ms: f64,
    value: T,
}

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> Timed<T> {
    let mut ms = f64::MAX;
    let mut value = None;
    for _ in 0..runs {
        let started = Instant::now();
        value = Some(f());
        ms = ms.min(started.elapsed().as_secs_f64() * 1e3);
    }
    Timed {
        ms,
        value: value.unwrap(),
    }
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn bench_query() {
    const SEGMENTS: usize = 50;
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let per = TXNS / SEGMENTS;
    for s in 0..SEGMENTS {
        let mut text = synthetic(&mut rng, s * per, per);
        if s == 0 {
            text.push_str(&prices());
        }
        let parsed = Journal::parse(&text);
        assert!(parsed.errors.is_empty());
        let header = SegmentHeader {
            source: "cln".to_owned(),
            imported: 1_700_000_000 + s as u64,
            layer: "lightning".to_owned(),
        };
        store.write_segment(&header, &parsed.journal).unwrap();
    }
    let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
    let journal = ledger.journal();
    let accounts = &journal.accounts;
    let cad = journal.commodities.get("CAD").unwrap();
    let msat = journal.commodities.get("msat").unwrap();
    let prices = PriceDb::from_ledger(&ledger);

    let built = time(3, || Rollups::new(&ledger, &prices, Some(cad)));
    let rollups = built.value;
    println!(
        "rollups: {} postings, {} accounts, {} prices -> {} cells in {:.1} ms (converting every posting to CAD)",
        ledger.postings().len(),
        accounts.len(),
        ledger.prices().len(),
        rollups.cell_count(),
        built.ms,
    );

    let q = rollups.query(&ledger, &prices);
    let all = q.span().unwrap();
    let last = Date::from_days(all.end.days() - 1);
    let mid = DateRange::new(
        Date::parse("2022-03-17").unwrap(),
        Date::parse("2025-08-09").unwrap(),
    );
    let assets = accounts.lookup("assets").unwrap();
    let income = accounts.lookup("income").unwrap();
    let expenses = accounts.lookup("expenses").unwrap();
    let root = |name| accounts.lookup(name).unwrap();

    let report = |name: &str, ms: f64, what: String| println!("  {name:<44} {ms:>7.2} ms  {what}");
    let b = time(20, || q.balance(assets, last, Some(cad)));
    report(
        "balance(assets, last day, CAD)",
        b.ms,
        format!("{} unconverted", b.value.unconverted.count),
    );
    let b = time(20, || q.balance(income, last, None));
    report(
        "balance(income, last day, native)",
        b.ms,
        format!("{} commodities", b.value.totals.len()),
    );
    let b = time(20, || {
        q.balance(income, Date::parse("2024-06-15").unwrap(), Some(cad))
    });
    report("balance(income, mid-month, CAD)", b.ms, String::new());
    let b = time(5, || q.balance(root("assets:cln"), last, Some(msat)));
    report(
        "balance(assets:cln, last day, msat) [scan]",
        b.ms,
        String::new(),
    );

    let f = time(20, || q.flows(income, all, Bucket::Month, cad));
    report(
        "flows(income, all time, month, CAD)",
        f.ms,
        format!("{} points", f.value.points.len()),
    );
    let f = time(20, || q.flows(income, mid, Bucket::Month, cad));
    report(
        "flows(income, unaligned range, month, CAD)",
        f.ms,
        format!("{} points", f.value.points.len()),
    );
    let f = time(20, || q.flows(expenses, all, Bucket::Year, cad));
    report(
        "flows(expenses, all time, year, CAD)",
        f.ms,
        format!("{} points", f.value.points.len()),
    );
    let f = time(5, || q.flows(income, all, Bucket::Day, cad));
    report(
        "flows(income, all time, day, CAD) [scan]",
        f.ms,
        format!("{} points", f.value.points.len()),
    );

    let t = time(20, || q.top_n(income, 3, all, 10, cad));
    report(
        "top_n(income, depth 3, all time, 10, CAD)",
        t.ms,
        format!("of {} peers", accounts.children(root("income:zap")).len()),
    );
    let t = time(20, || q.top_n(expenses, 3, mid, 10, cad));
    report(
        "top_n(expenses, depth 3, unaligned, 10, CAD)",
        t.ms,
        String::new(),
    );

    let r = time(5, || {
        q.register(Filter {
            account: Some(root("income:routing")),
            range: Some(mid),
            ..Filter::default()
        })
        .count()
    });
    report(
        "register(income:routing, range).count()",
        r.ms,
        format!("{} txns", r.value),
    );
    let r = time(5, || {
        q.register(Filter {
            payee: Some("FEE"),
            ..Filter::default()
        })
        .count()
    });
    report(
        "register(payee ~ \"FEE\").count()",
        r.ms,
        format!("{} txns", r.value),
    );
}
