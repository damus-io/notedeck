//! Times [`cashflow::Books::load`] stage by stage, and a few folds, over a
//! real local `$CASHFLOW_DIR`.
//!
//! The journal is personal data, so there is no fixture: point
//! `CASHFLOW_BENCH_DIR` at a data dir (the one `cashflow --dir` takes) and
//! run
//!
//! ```text
//! CASHFLOW_BENCH_DIR=<dir> cargo test --release -p cashflow --test bench_books -- --ignored --nocapture
//! ```
//!
//! It prints counts and milliseconds only, never an account, payee or
//! amount, so its output can go in a card comment.

use std::time::Instant;

use cashflow::CashflowDir;
use ledgerdb::{
    Bucket, Date, DateRange, Edits, Filter, LayerFilter, Ledger, PriceDb, Rollups, derive,
};

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

fn report(name: &str, ms: f64, what: String) {
    println!("  {name:<40} {ms:>8.2} ms  {what}");
}

#[test]
#[ignore = "benchmark: set CASHFLOW_BENCH_DIR to a local data dir"]
fn bench_books_load_and_folds() {
    let Ok(dir) = std::env::var("CASHFLOW_BENCH_DIR") else {
        println!("CASHFLOW_BENCH_DIR is not set; skipping");
        return;
    };
    let store = cashflow::journal::store(&CashflowDir::new(dir));

    println!("load stages (best of 3):");
    let loaded = time(3, || Ledger::load(&store, LayerFilter::All).unwrap());
    let ledger = &loaded.value.ledger;
    report(
        "Ledger::load (read, hash, parse, merge)",
        loaded.ms,
        format!(
            "{} segments, {} txns, {} postings, {} accounts",
            loaded.value.report.loaded.len(),
            ledger.txns().len(),
            ledger.postings().len(),
            ledger.journal().accounts.len(),
        ),
    );
    let derived = time(3, || derive(ledger, &Edits::default()));
    report("derive (no edits)", derived.ms, String::new());
    let books = derived.value.ledger();
    let prices = time(3, || PriceDb::from_ledger(books));
    report(
        "PriceDb::from_ledger",
        prices.ms,
        format!("{} P directives", books.prices().len()),
    );
    let home = books.journal().commodities.get("CAD");
    let rollups = time(3, || Rollups::new(books, &prices.value, home));
    report(
        "Rollups::new (home CAD)",
        rollups.ms,
        format!("{} cells", rollups.value.cell_count()),
    );
    println!(
        "  {:<40} {:>8.2} ms",
        "total",
        loaded.ms + derived.ms + prices.ms + rollups.ms
    );

    let q = rollups.value.query(books, &prices.value);
    let Some(all) = q.span() else {
        println!("the journal is empty");
        return;
    };
    let last = Date::from_days(all.end.days() - 1);
    let accounts = &books.journal().accounts;
    println!("folds (best of 20):");
    let roots = accounts.roots();
    let b = time(20, || {
        roots
            .iter()
            .map(|&root| q.balance(root, last, home).unconverted.count)
            .sum::<u64>()
    });
    report(
        "balance(each root, last day, CAD)",
        b.ms,
        format!("{} roots, {} unconverted", roots.len(), b.value),
    );
    let b = time(20, || {
        roots
            .iter()
            .map(|&root| q.balance(root, last, None).totals.len())
            .sum::<usize>()
    });
    report(
        "balance(each root, last day, native)",
        b.ms,
        format!("{} root-commodity totals", b.value),
    );
    let Some(cad) = home else {
        println!("no CAD in the journal; skipping converted folds");
        return;
    };
    if let Some(expenses) = accounts.lookup("expenses") {
        let t = time(20, || q.top_n(expenses, 2, all, 10, cad));
        report(
            "top_n(expenses, depth 2, all time, 10)",
            t.ms,
            format!("{} ranked", t.value.ranked.len()),
        );
    }
    if let Some(assets) = accounts.lookup("assets") {
        let f = time(20, || q.flows(assets, all, Bucket::Month, cad));
        report(
            "flows(assets, all time, monthly)",
            f.ms,
            format!("{} points", f.value.points.len()),
        );
        let f = time(5, || q.flows(assets, all, Bucket::Day, cad));
        report(
            "flows(assets, all time, daily)",
            f.ms,
            format!("{} points", f.value.points.len()),
        );
    }
    let year = DateRange::new(Date::from_days(all.end.days().saturating_sub(365)), all.end);
    let r = time(5, || {
        q.register(Filter {
            range: Some(year),
            ..Filter::default()
        })
        .count()
    });
    report(
        "register(last 365 days).count()",
        r.ms,
        format!("{} rows", r.value),
    );
}
