//! Ledger load benchmark. Ignored by default; run in release with
//!
//! ```text
//! cargo test --release -p ledgerdb --test bench_ledger -- --ignored --nocapture
//! ```
//!
//! It prints counts, timings and sizes only.

use std::fmt::Write;
use std::mem::size_of;
use std::time::Instant;

use ledgerdb::{
    Comment, Cost, Journal, JournalStore, LayerFilter, Ledger, Meta, Origin, Posting, Price,
    SegmentHeader, Txn,
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

/// Transactions `first..first + n` of a synthetic lightning-node-shaped
/// journal (the shape `bench_parse` uses) over four years, two postings each
/// in msat, every one with a `; id:`.
fn synthetic(rng: &mut Rng, first: usize, n: usize, total: usize) -> String {
    let mut out = String::with_capacity(n * 130);
    let start = ledgerdb::Date::parse("2022-01-01").unwrap().days();
    for i in first..first + n {
        let date = ledgerdb::Date::from_days(start + (i * 1461 / total) as u32);
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

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn bench_ledger_load() {
    const TXNS: usize = 350_000;
    const SEGMENTS: usize = 50;
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let per = TXNS / SEGMENTS;
    for s in 0..SEGMENTS {
        // Each segment re-imports the last 1% of the previous one's ids, as a
        // re-export of pending rows would.
        let first = (s * per).saturating_sub(per / 100);
        let n = per + (s * per - first);
        let Ledgered { journal } = Ledgered::parse(&synthetic(&mut rng, first, n, TXNS));
        let header = SegmentHeader {
            source: "cln".to_owned(),
            imported: 1_700_000_000 + s as u64,
            layer: "lightning".to_owned(),
        };
        store.write_segment(&header, &journal).unwrap();
    }
    let disk: u64 = store
        .list()
        .unwrap()
        .iter()
        .map(|h| std::fs::metadata(store.path_of(*h)).unwrap().len())
        .sum();

    let mut read = f64::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        for hash in store.list().unwrap() {
            store.read(hash).unwrap();
        }
        read = read.min(started.elapsed().as_secs_f64());
    }

    let mut best = f64::MAX;
    let mut ledger = None;
    for _ in 0..3 {
        let started = Instant::now();
        let loaded = Ledger::load(&store, LayerFilter::All).unwrap();
        best = best.min(started.elapsed().as_secs_f64());
        assert_eq!(loaded.report.loaded.len(), SEGMENTS);
        ledger = Some(loaded.ledger);
    }
    let ledger = ledger.unwrap();
    assert_eq!(ledger.txns().len(), TXNS);

    let journal = ledger.journal();
    let arrays = [
        ("postings", journal.postings.len() * size_of::<Posting>()),
        ("txns", journal.txns.len() * size_of::<Txn>()),
        ("origins", journal.txns.len() * size_of::<Origin>()),
        ("meta", journal.meta.len() * size_of::<Meta>()),
        ("comments", journal.comments.len() * size_of::<Comment>()),
        ("costs", journal.costs.len() * size_of::<Cost>()),
        ("prices", journal.prices.len() * size_of::<Price>()),
        (
            "account index",
            (journal.postings.len() + journal.accounts.len() + 1) * size_of::<u32>(),
        ),
        ("string text", journal.strings.text_bytes()),
    ];
    let total: usize = arrays.iter().map(|(_, bytes)| bytes).sum();
    println!(
        "ledger load: {SEGMENTS} segments, {:.1} MB on disk, {} txns, {} postings, {} accounts, {} strings in {:.1} ms (read + verify + parse + merge + sort)",
        disk as f64 / 1e6,
        journal.txns.len(),
        journal.postings.len(),
        journal.accounts.len(),
        journal.strings.len(),
        best * 1e3,
    );
    println!(
        "  of which read + verify + parse alone: {:.1} ms; merge + sort + index: {:.1} ms",
        read * 1e3,
        (best - read) * 1e3
    );
    for (name, bytes) in arrays {
        println!("  {name:>14}: {:>6.1} MB", bytes as f64 / 1e6);
    }
    println!("  {:>14}: {:>6.1} MB", "total", total as f64 / 1e6);

    // One more segment merged into the loaded ledger.
    let mut ledger = ledger;
    let Ledgered { journal } = Ledgered::parse(&synthetic(&mut rng, TXNS, per, TXNS));
    let header = SegmentHeader {
        source: "cln".to_owned(),
        imported: 1_800_000_000,
        layer: "lightning".to_owned(),
    };
    store.write_segment(&header, &journal).unwrap();
    let started = Instant::now();
    let report = ledger.refresh(&store).unwrap();
    println!(
        "refresh with 1 new segment: {:.1} ms",
        started.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(report.loaded.len(), 1);
}

/// A journal parsed with no errors.
struct Ledgered {
    journal: Journal,
}

impl Ledgered {
    fn parse(text: &str) -> Self {
        let parsed = Journal::parse(text);
        assert!(parsed.errors.is_empty());
        Self {
            journal: parsed.journal,
        }
    }
}
