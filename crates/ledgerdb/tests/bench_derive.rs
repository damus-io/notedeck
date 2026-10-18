//! Derive benchmark. Ignored by default; run in release with
//!
//! ```text
//! cargo test --release -p ledgerdb --test bench_derive -- --ignored --nocapture
//! ```
//!
//! It prints counts and timings only.

use std::fmt::Write;
use std::time::Instant;

use ledgerdb::{
    Cond, Edits, Field, Journal, JournalStore, LayerFilter, Ledger, Override, Parsed, Rule,
    SegmentHeader, Why, derive, rederive,
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

/// How many distinct synthetic merchants purchases are spread over. Rules
/// name the first [`RULED`] of them, so the rest fall through every rule,
/// which is the slow path.
const MERCHANTS: u64 = 400;
const RULED: u64 = 190;

/// Transactions `first..first + n` of a synthetic card-statement journal:
/// one uncategorized purchase each, with the merchant as `; raw-merchant:`.
fn synthetic(rng: &mut Rng, first: usize, n: usize, total: usize) -> String {
    let mut out = String::with_capacity(n * 160);
    let start = ledgerdb::Date::parse("2022-01-01").unwrap().days();
    for i in first..first + n {
        let date = ledgerdb::Date::from_days(start + (i * 1461 / total) as u32);
        let cents = rng.next() % 50_000 + 1;
        let merchant = rng.next() % MERCHANTS;
        let store = rng.next() % 90;
        writeln!(
            out,
            "{date} * Merchant {merchant:04} #{store}\n\t; id: card:{i:08x}\n\t; raw-merchant: MERCHANT {merchant:04} STORE #{store} TORONTO ON"
        )
        .unwrap();
        writeln!(
            out,
            "\tliabilities:card\t-{}.{:02} CAD\n\texpenses:unknown\n",
            cents / 100,
            cents % 100
        )
        .unwrap();
    }
    out
}

/// About 200 rules: one per ruled merchant, in amex.toml's style, plus a
/// few amount buckets and payee rules, and a thousand overrides.
fn edits() -> Edits {
    let mut rules: Vec<Rule> = (0..RULED)
        .map(|m| Rule {
            account: format!("expenses:merchant:{m}"),
            conditions: vec![Cond::Contains {
                field: Field::Meta("raw-merchant".to_owned()),
                needle: format!("merchant {m:04} store"),
            }],
            layer: None,
        })
        .collect();
    for bucket in 0..5 {
        rules.push(Rule {
            account: format!("expenses:bucket:{bucket}"),
            conditions: vec![
                Cond::Source("card".to_owned()),
                Cond::Amount {
                    commodity: Some("CAD".to_owned()),
                    min: bucket * 1000,
                    max: bucket * 1000 + 3,
                },
            ],
            layer: None,
        });
    }
    for store in 0..5 {
        rules.push(Rule {
            account: format!("expenses:store:{store}"),
            conditions: vec![Cond::Contains {
                field: Field::Payee,
                needle: format!("#{store}0"),
            }],
            layer: Some("cards".to_owned()),
        });
    }
    let overrides = (0..1000)
        .map(|i| Override {
            record_id: format!("card:{:08x}", i * 331),
            account: "expenses:override".to_owned(),
        })
        .collect();
    Edits {
        defaults: vec!["expenses:unknown".to_owned()],
        rules,
        overrides,
        ..Edits::default()
    }
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn bench_derive() {
    const TXNS: usize = 350_000;
    const SEGMENTS: usize = 50;
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let per = TXNS / SEGMENTS;
    for s in 0..SEGMENTS {
        let Parsed { journal, errors } = Journal::parse(&synthetic(&mut rng, s * per, per, TXNS));
        assert!(errors.is_empty());
        let header = SegmentHeader {
            source: "card".to_owned(),
            imported: 1_700_000_000 + s as u64,
            layer: "cards".to_owned(),
        };
        store.write_segment(&header, &journal).unwrap();
    }
    let raw = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
    assert_eq!(raw.txns().len(), TXNS);

    let edits = edits();
    let mut best = f64::MAX;
    let mut derived = None;
    for _ in 0..5 {
        let started = Instant::now();
        let d = derive(&raw, &edits);
        best = best.min(started.elapsed().as_secs_f64());
        derived = Some(d);
    }
    let derived = derived.unwrap();
    assert!(derived.report().rejected.is_empty());

    let mut by_rule = 0;
    let mut by_override = 0;
    let mut defaulted = 0;
    for at in 0..raw.txns().len() {
        match derived.why(ledgerdb::TxnId(at as u32)) {
            Why::Rule(_) => by_rule += 1,
            Why::Override(_) => by_override += 1,
            Why::Default => defaulted += 1,
            _ => {}
        }
    }
    println!(
        "derive: {} txns, {} postings, {} rules, {} overrides in {:.1} ms ({by_rule} by rule, {by_override} by override, {defaulted} left at the default)",
        raw.txns().len(),
        raw.postings().len(),
        edits.rules.len(),
        edits.overrides.len(),
        best * 1e3,
    );

    // One more override: the edit a triage click makes.
    let mut edited = edits.clone();
    edited.overrides.push(Override {
        record_id: "card:00000007".to_owned(),
        account: "expenses:triaged".to_owned(),
    });
    let mut best = f64::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        let again = rederive(&raw, &edited, &derived);
        best = best.min(started.elapsed().as_secs_f64());
        assert!(again.reused_rule_matches());
    }
    println!("rederive after one more override: {:.1} ms", best * 1e3);
}
