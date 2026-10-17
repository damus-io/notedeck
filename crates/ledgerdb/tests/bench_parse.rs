//! Parse-speed benchmarks. Ignored by default; run in release with
//!
//! ```text
//! cargo test --release -p ledgerdb --test bench_parse -- --ignored --nocapture
//! ```
//!
//! Set `LEDGERDB_BENCH_FILE` to a local journal to time a real file too. It
//! prints counts and timings only, never the file's contents.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::Instant;

use ledgerdb::{Journal, ParseErrorKind};

/// A deterministic xorshift generator, so every run parses the same text.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// A synthetic journal shaped like a lightning node's bookkeeping export:
/// `txns` two-posting transactions in msat, tab-separated, about 85% of
/// them against one asset account, over four years. With `ids`, each also
/// carries a `; id:` line, as journal segments will.
fn synthetic(txns: usize, ids: bool) -> String {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = String::with_capacity(txns * 110);
    let start = ledgerdb::Date::parse("2022-01-01").unwrap().days();
    for i in 0..txns {
        let day = start + (i * 1461 / txns) as u32;
        let date = ledgerdb::Date::from_days(day);
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
        writeln!(out, "{date}\t{payee}").unwrap();
        if ids {
            writeln!(out, "\t; id: cln:{i:08x}").unwrap();
        }
        writeln!(out, "\t{to}\t{msat} msat").unwrap();
        writeln!(out, "\t{from}\t-{msat} msat").unwrap();
        out.push('\n');
    }
    out
}

/// Parse `text` a few times and print the best run.
fn time_parse(label: &str, text: &str) {
    let mut best = f64::MAX;
    let mut counts = None;
    for _ in 0..5 {
        let started = Instant::now();
        let parsed = Journal::parse(text);
        best = best.min(started.elapsed().as_secs_f64());
        counts = Some(Counts {
            txns: parsed.journal.txns.len(),
            postings: parsed.journal.postings.len(),
            accounts: parsed.journal.accounts.len(),
            errors: error_kinds(&parsed.errors),
        });
    }
    let counts = counts.expect("parsed at least once");
    let mb = text.len() as f64 / 1e6;
    println!(
        "{label}: {mb:.1} MB, {} txns, {} postings, {} accounts in {:.1} ms = {:.0} MB/s; errors {:?}",
        counts.txns,
        counts.postings,
        counts.accounts,
        best * 1e3,
        mb / best,
        counts.errors,
    );
}

/// What one parse produced, by count.
struct Counts {
    txns: usize,
    postings: usize,
    accounts: usize,
    errors: BTreeMap<&'static str, usize>,
}

/// Errors counted by kind, without their text, which can quote amounts.
fn error_kinds(errors: &[ledgerdb::ParseError]) -> BTreeMap<&'static str, usize> {
    let mut kinds = BTreeMap::new();
    for err in errors {
        let kind = match err.kind {
            ParseErrorKind::Date(_) => "date",
            ParseErrorKind::Amount(_) => "amount",
            ParseErrorKind::Account(_) => "account",
            ParseErrorKind::OrphanPosting => "orphan",
            ParseErrorKind::UnsupportedDirective => "directive",
            ParseErrorKind::UnclosedCode => "code",
            ParseErrorKind::UnclosedVirtual => "virtual",
            ParseErrorKind::CostWithoutAmount => "cost",
            ParseErrorKind::SecondElided => "second-elided",
            ParseErrorKind::ElidedVirtual => "elided-virtual",
            ParseErrorKind::NoPostings => "no-postings",
            ParseErrorKind::Unbalanced { .. } => "unbalanced",
            ParseErrorKind::BadPrice => "price",
        };
        *kinds.entry(kind).or_default() += 1;
    }
    kinds
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn bench_parse_synthetic_cln_shape() {
    let plain = synthetic(330_000, false);
    let parsed = Journal::parse(&plain);
    assert!(parsed.errors.is_empty());
    assert_eq!(parsed.journal.postings.len(), 660_000);
    time_parse("synthetic", &plain);
    time_parse("synthetic + ids", &synthetic(330_000, true));
}

#[test]
#[ignore = "benchmark: set LEDGERDB_BENCH_FILE to a local journal"]
fn bench_parse_local_file() {
    let Ok(path) = std::env::var("LEDGERDB_BENCH_FILE") else {
        println!("LEDGERDB_BENCH_FILE is not set; skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read LEDGERDB_BENCH_FILE");
    time_parse("local file", &text);
}
