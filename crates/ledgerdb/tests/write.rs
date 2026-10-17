//! The ledger-cli writer: parse → write → parse gives the same journal, and
//! writing is a fixed point on its own output.

use std::fmt::Write;

use ledgerdb::{CostKind, Journal, Parsed, PostingFlags, TxnFlag, TxnId, WriteOptions};

const ELIDED: WriteOptions = WriteOptions {
    explicit_amounts: false,
};
const EXPLICIT: WriteOptions = WriteOptions {
    explicit_amounts: true,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// A journal with every id resolved to the text it names, so two journals
/// that interned in a different order still compare equal.
#[derive(Debug, PartialEq)]
struct Resolved {
    commodities: Vec<String>,
    txns: Vec<String>,
    prices: Vec<String>,
    file_comments: Vec<String>,
}

/// Resolve `journal`; with `drop_elided`, ignore which postings were elided,
/// for comparing against a journal written with explicit amounts.
fn resolve(journal: &Journal, drop_elided: bool) -> Resolved {
    let strings = &journal.strings;
    let commodities = &journal.commodities;
    let mut names: Vec<String> = commodities
        .ids()
        .map(|id| {
            format!(
                "{:?} precision={} {:?}",
                commodities.name(id),
                commodities.precision(id),
                commodities.style(id)
            )
        })
        .collect();
    names.sort();

    let txns = journal
        .txns
        .iter()
        .map(|txn| {
            let mut out = format!(
                "{} {:?} code={:?} payee={:?} id={:?}\n",
                txn.date,
                txn.flag,
                txn.code.map(|c| strings.get(c)),
                strings.get(txn.payee),
                txn.id.map(|id| strings.get(id)),
            );
            for (offset, posting) in journal.postings_of(txn).iter().enumerate() {
                let index = txn.postings.start + offset as u32;
                let mut flags = posting.flags;
                if drop_elided {
                    flags.0 &= !PostingFlags::ELIDED.0;
                }
                let cost = journal.cost_of(index).map(|cost| {
                    format!(
                        "{:?} {:?} {}",
                        cost.kind,
                        cost.value,
                        commodities.name(cost.commodity)
                    )
                });
                writeln!(
                    out,
                    "  {} {:?} {} {} cost={cost:?}",
                    journal.accounts.full_name(posting.account),
                    flags,
                    posting.qty,
                    commodities.name(posting.commodity),
                )
                .unwrap();
            }
            for meta in journal.meta_of(txn) {
                writeln!(
                    out,
                    "  meta@{:?} {:?}={:?}",
                    meta.posting,
                    strings.get(meta.key),
                    strings.get(meta.value)
                )
                .unwrap();
            }
            for comment in journal.comments_of(txn) {
                writeln!(
                    out,
                    "  comment@{:?} {:?}",
                    comment.posting,
                    strings.get(comment.text)
                )
                .unwrap();
            }
            out
        })
        .collect();

    let prices = journal
        .prices
        .iter()
        .map(|p| {
            format!(
                "{} {} {:?} {}",
                p.date,
                commodities.name(p.commodity),
                p.value,
                commodities.name(p.quote)
            )
        })
        .collect();
    let file_comments = journal
        .file_comments
        .iter()
        .map(|c| format!("{} {:?}", c.txns_before, strings.get(c.text)))
        .collect();
    Resolved {
        commodities: names,
        txns,
        prices,
        file_comments,
    }
}

/// The writer's two properties on one journal text:
/// - parse ∘ write ∘ parse = parse, with and without explicit amounts;
/// - write ∘ parse is idempotent: its output is its own fixed point.
fn assert_round_trips(text: &str) {
    let first = parse_ok(text);
    let written = first.to_ledger(ELIDED);
    let second = parse_ok(&written);
    assert_eq!(
        resolve(&first, false),
        resolve(&second, false),
        "round trip changed the journal\n--- source\n{text}\n--- written\n{written}"
    );
    assert_eq!(
        second.to_ledger(ELIDED),
        written,
        "writing is not idempotent"
    );

    let explicit = first.to_ledger(EXPLICIT);
    let third = parse_ok(&explicit);
    assert_eq!(
        resolve(&first, true),
        resolve(&third, true),
        "explicit round trip changed the journal\n--- source\n{text}\n--- written\n{explicit}"
    );
    assert_eq!(third.to_ledger(EXPLICIT), explicit);
}

/// Hand-written fixtures covering every part of the subset, each laid out
/// the way people write ledger files rather than the canonical way.
const FIXTURES: &[&str] = &[
    // Both separators, codes, marks.
    "2024-01-02 * (1042) Coffee Shop\n\
     \texpenses:food:coffee\t4.50 CAD\n\
     \tassets:bank:checking  -4.50 CAD\n\
     \n\
     2024/1/3 ! Second\n  \
       assets:my wallet  $-1.00\n  \
       expenses:misc  $1.00\n",
    // Semicolons in payees and header comments after a tab or two spaces.
    "2024-01-02 A;B @ C = D ; still payee\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     2024-01-02 Tabbed\t; note after a tab\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     2024-01-02 Spaced  ; key: value\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     2024-01-02\t\n\
     \ta\t1 X\n\
     \tb\t-1 X\n",
    // Trailing posting comments, and a comment that swallowed an amount.
    "2024-01-02 x\n\
     \tassets:a;b\t1 X ; trailing note\n\
     \tassets:c ; just a note\t-1 X\n\
     \tassets:c\t-0 X\n",
    // An elided posting filled with two commodities, with notes around it.
    "2024-01-02 x\n\
     \texpenses:a\t5 CAD\n\
     \tassets:wallet ; key: on-elided\n\
     \texpenses:b\t3 USD ; key: on-b\n\
     \t; own: line-after-b\n",
    // Prices at their own scale, and a quoted commodity.
    "P 2024-01-01 BTC 61234.56789 CAD\n\
     P 2024/01/02 msat 0.00000000001 BTC\n\
     P 2024-01-02 \"Gift Card\" 25 CAD\n\
     2024-01-03 x\n\
     \ta\t1.00 CAD\n\
     \tb\t-1.00 CAD\n\
     \tc\t2 \"Gift Card\"\n\
     \td\t-2 \"Gift Card\"\n",
    // Costs, per unit and total.
    "2024-01-02 buy\n\
     \tassets:btc\t0.5 BTC @ 40000.005 CAD\n\
     \tassets:bank\t-20000.00 CAD\n\
     2024-01-03 buy total\n\
     \tassets:btc\t-2 BTC @@ 1000 CAD\n\
     \tassets:bank\n",
    // Virtual postings, and an elided bracketed one.
    "2024-01-02 x\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     \t(budget:food)\t-50 X\n\
     \t[fund:a]\t10 X\n\
     \t[fund:b]\n",
    // Posting marks.
    "2024-01-02 x\n\
     \t* a\t1 X\n\
     \t! b\t-1 X\n",
    // File comments in three places, with three comment characters.
    "; ledgerdb-segment: 1\n\
     # source: synthetic\n\
     ;\n\
     2024-01-02 x\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     ; between\n\
     % after a percent\n\
     2024-01-03 y\n\
     \ta\t1 X\n\
     \tb\t-1 X\n\
     ; at the end\n",
    // CRLF line endings, a zero elided posting, and a bare tag.
    "2024-01-02 x\r\n\
     \t; flagged:\r\n\
     \t;\r\n\
     \ta\t1 X\r\n\
     \tb\t-1 X\r\n\
     \tc\r\n",
    // A prefix commodity with a space, and an id that becomes Txn::id.
    "2024-01-02 * paid\n\
     \t; id: src:0001\n\
     \t; id: second-is-just-meta\n\
     \texpenses:x\tUSD 12.5\n\
     \tassets:y\n",
];

#[test]
fn fixtures_round_trip() {
    for fixture in FIXTURES {
        assert_round_trips(fixture);
    }
}

/// Canonical text is a fixed point: parsing it and writing it back gives
/// exactly the same bytes.
#[test]
fn canonical_text_is_a_fixed_point() {
    let canonical = "\
; ledgerdb-segment: 1
; source: synthetic

P 2024-01-01 BTC 61234.56789 CAD
P 2024-01-01 \"Gift Card\" 25.00 CAD

2024-01-02 * (1042) Coffee Shop ; not a comment
\t; id: synthetic:1
\t; flagged:
\t; a plain comment
\t* expenses:food:coffee\t4.50 CAD
\t; merchant: Coffee Shop
\t; on the posting
\tassets:bank:checking

; between

2024-01-03 ! buy
\tassets:btc\t0.50000000 BTC @ 40000.005 CAD
\t[fund:btc]\t-1.00 CAD
\t[fund:cash]\t1.00 CAD
\t(budget:btc)\t$-5
\tassets:bank\t-20000.00 CAD

2024-01-04
\tassets:a\t2 \"Gift Card\"
\tassets:b
";
    let journal = parse_ok(canonical);
    assert_eq!(journal.to_ledger(ELIDED), canonical);
}

#[test]
fn layout_is_normalized() {
    let journal = parse_ok(
        "2024/1/2    *   (7)   Payee  ; note\n  \
           a    1.5 CAD   ; key:   v  \n  \
           b\n\
         \n\n\n\
         P 2024/01/01 BTC 1 CAD\n",
    );
    assert_eq!(
        journal.to_ledger(ELIDED),
        "P 2024-01-01 BTC 1 CAD\n\
         \n\
         2024-01-02 * (7) Payee\n\
         \t; note\n\
         \ta\t1.5 CAD\n\
         \t; key: v\n\
         \tb\n"
    );
}

#[test]
fn elided_multi_commodity_posting_is_one_line_unless_explicit() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \texpenses:a\t5 CAD\n\
         \tassets:wallet\n\
         \texpenses:b\t3 USD\n",
    );
    assert_eq!(
        journal.to_ledger(ELIDED),
        "2024-01-02 x\n\
         \texpenses:a\t5 CAD\n\
         \tassets:wallet\n\
         \texpenses:b\t3 USD\n"
    );
    assert_eq!(
        journal.to_ledger(EXPLICIT),
        "2024-01-02 x\n\
         \texpenses:a\t5 CAD\n\
         \tassets:wallet\t-5 CAD\n\
         \tassets:wallet\t-3 USD\n\
         \texpenses:b\t3 USD\n"
    );
}

#[test]
fn a_subset_is_written_in_the_order_given() {
    let journal = parse_ok(
        "; header\n\
         P 2024-01-01 BTC 1 CAD\n\
         2024-01-01 one\n\ta\t1 X\n\tb\n\
         2024-01-02 two\n\ta\t2 X\n\tb\n\
         2024-01-03 three\n\ta\t3 X\n\tb\n",
    );
    let mut out = String::new();
    journal
        .write_txns([TxnId(2), TxnId(0)], &mut out, ELIDED)
        .unwrap();
    assert_eq!(
        out,
        "2024-01-03 three\n\ta\t3 X\n\tb\n\
         \n\
         2024-01-01 one\n\ta\t1 X\n\tb\n"
    );
}

/// `write_prices` writes the `P` lines dated before its end, canonically.
#[test]
fn write_prices_stops_at_the_end_date() {
    let journal = parse_ok(
        "P 2024-01-01 BTC 61234.5 CAD\n\
         P 2024-02-01 \"AB 1\" 2 CAD\n\
         P 2024-03-01 BTC 70000 CAD\n",
    );
    let mut all = String::new();
    journal.write_prices(None, &mut all).unwrap();
    assert_eq!(
        all,
        "P 2024-01-01 BTC 61234.5 CAD\n\
         P 2024-02-01 \"AB 1\" 2 CAD\n\
         P 2024-03-01 BTC 70000 CAD\n"
    );
    let mut before = String::new();
    let end = ledgerdb::Date::parse("2024-03-01").unwrap();
    journal.write_prices(Some(end), &mut before).unwrap();
    assert_eq!(
        before,
        "P 2024-01-01 BTC 61234.5 CAD\nP 2024-02-01 \"AB 1\" 2 CAD\n"
    );
}

#[test]
fn an_empty_journal_writes_nothing() {
    assert_eq!(Journal::new().to_ledger(ELIDED), "");
}

/// Strings the format cannot hold, as an importer might put them in a
/// journal it built, come back normalized rather than corrupting the file.
#[test]
fn unwritable_strings_are_normalized() {
    let mut journal = parse_ok(
        "2024-01-02 x\n\
         \t; note: n\n\
         \t; c\n\
         \ta\t1 X\n\
         \tb\n",
    );
    let payee = journal.strings.intern("  Shop\t; Main St\nUnit 4  ");
    let value = journal.strings.intern("line one\nline two");
    let text = journal.strings.intern("first\r\nsecond");
    journal.txns[0].payee = payee;
    journal.meta[0].value = value;
    journal.comments[0].text = text;

    let written = journal.to_ledger(ELIDED);
    assert_eq!(
        written,
        "2024-01-02 Shop ; Main St Unit 4\n\
         \t; note: line one line two\n\
         \t; first  second\n\
         \ta\t1 X\n\
         \tb\n"
    );
    let back = parse_ok(&written);
    assert_eq!(
        back.strings.get(back.txns[0].payee),
        "Shop ; Main St Unit 4"
    );
    assert_eq!(back.to_ledger(ELIDED), written);
}

/// A deterministic xorshift generator, so every run checks the same
/// journals.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// A commodity the generator writes amounts in.
struct Unit {
    /// The commodity as written, quoted if it has to be.
    name: &'static str,
    prefix: bool,
    spaced: bool,
    /// The most decimal places an amount is written with.
    decimals: u32,
}

const UNITS: &[Unit] = &[
    Unit {
        name: "CAD",
        prefix: false,
        spaced: true,
        decimals: 2,
    },
    Unit {
        name: "$",
        prefix: true,
        spaced: false,
        decimals: 2,
    },
    Unit {
        name: "msat",
        prefix: false,
        spaced: true,
        decimals: 0,
    },
    Unit {
        name: "BTC",
        prefix: false,
        spaced: true,
        decimals: 8,
    },
    Unit {
        name: "\"Gift Card\"",
        prefix: false,
        spaced: true,
        decimals: 0,
    },
];

/// `qty / 10^decimals` written in `unit`'s style.
fn amount_text(qty: i64, decimals: u32, unit: &Unit) -> String {
    let sign = if qty < 0 { "-" } else { "" };
    let magnitude = qty.unsigned_abs();
    let scale = 10u64.pow(decimals);
    let number = if decimals == 0 {
        format!("{sign}{magnitude}")
    } else {
        format!(
            "{sign}{}.{:0width$}",
            magnitude / scale,
            magnitude % scale,
            width = decimals as usize
        )
    };
    let sep = if unit.spaced { " " } else { "" };
    if unit.prefix {
        format!("{}{sep}{number}", unit.name)
    } else {
        format!("{number}{sep}{}", unit.name)
    }
}

const ACCOUNTS: &[&str] = &[
    "assets:bank:checking",
    "assets:cln",
    "assets:my wallet",
    "expenses:food",
    "expenses:unknown",
    "income:zap",
    "liabilities:card",
];
const PAYEES: &[&str] = &["Coffee", "invoice", "A;B", "x ; y", "Two  spaces", ""];
const SEPARATORS: &[&str] = &["\t", "  ", "\t\t", "    "];
const INDENTS: &[&str] = &["\t", "  ", "    "];

/// One random journal, in a deliberately untidy layout, that parses without
/// errors: every transaction balances, by an elided posting or by explicit
/// balancing postings.
fn random_journal(rng: &mut Rng) -> String {
    let mut out = String::new();
    if rng.chance(50) {
        out.push_str("; ledgerdb-segment: 1\n");
    }
    for _ in 0..rng.below(3) {
        let day = rng.below(28) + 1;
        writeln!(
            out,
            "P 2024-01-{day:02} BTC {}.{} CAD",
            rng.below(90_000),
            rng.below(100_000)
        )
        .unwrap();
    }
    for t in 0..rng.below(8) + 1 {
        let day = rng.below(28) + 1;
        let flag = rng.pick(&["", " *", " !"]);
        let code = if rng.chance(20) { " (42)" } else { "" };
        let payee = rng.pick(PAYEES);
        write!(out, "2024-02-{day:02}{flag}{code} {payee}").unwrap();
        if rng.chance(20) {
            out.push_str("  ; header: note");
        }
        out.push('\n');
        let indent = rng.pick(INDENTS);
        if rng.chance(50) {
            writeln!(out, "{indent}; id: synthetic:{t}").unwrap();
        }
        if rng.chance(20) {
            writeln!(out, "{indent}; a transaction comment").unwrap();
        }

        let elide = rng.chance(60);
        let mut sums = [0i64; UNITS.len()];
        let mut decimals_used = [0u32; UNITS.len()];
        for _ in 0..rng.below(3) + 1 {
            let mark = rng.pick(&["", "* ", "! "]);
            let account = rng.pick(ACCOUNTS);
            let unit_index = rng.below(UNITS.len() as u64) as usize;
            let unit = &UNITS[unit_index];
            let decimals = rng.below(u64::from(unit.decimals) + 1) as u32;
            let qty = rng.below(2_000_000) as i64 - 1_000_000;
            let sep = rng.pick(SEPARATORS);
            write!(
                out,
                "{indent}{mark}{account}{sep}{}",
                amount_text(qty, decimals, unit)
            )
            .unwrap();
            // Costs only where an elided posting will absorb their weight.
            if elide && unit.name == "BTC" && rng.chance(50) {
                let at = rng.pick(&["@", "@@"]);
                write!(out, " {at} {}.{} CAD", rng.below(90_000), rng.below(1000)).unwrap();
            } else {
                sums[unit_index] += qty * 10i64.pow(unit.decimals - decimals);
                decimals_used[unit_index] = unit.decimals;
            }
            if rng.chance(25) {
                out.push_str(" ; raw-merchant: Somewhere");
            }
            out.push('\n');
            if rng.chance(15) {
                writeln!(out, "{indent}; posting note").unwrap();
            }
        }
        if rng.chance(15) {
            writeln!(out, "{indent}(budget:food){}-5 CAD", rng.pick(SEPARATORS)).unwrap();
        }
        if rng.chance(15) {
            writeln!(out, "{indent}[fund:a]\t3 msat\n{indent}[fund:b]\t-3 msat").unwrap();
        }
        if elide {
            writeln!(out, "{indent}{}", rng.pick(ACCOUNTS)).unwrap();
        } else {
            for (index, unit) in UNITS.iter().enumerate() {
                if sums[index] == 0 && decimals_used[index] == 0 {
                    continue;
                }
                writeln!(
                    out,
                    "{indent}equity:balance\t{}",
                    amount_text(-sums[index], decimals_used[index], unit)
                )
                .unwrap();
            }
        }
        if rng.chance(30) {
            out.push_str("\n; between transactions\n");
        }
        out.push('\n');
    }
    out
}

#[test]
fn random_journals_round_trip() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..500 {
        let text = random_journal(&mut rng);
        assert_round_trips(&text);
    }
}

#[test]
fn costs_are_written_with_their_kind() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \ta\t1 BTC @@ 10.5 CAD\n\
         \tb\n",
    );
    assert_eq!(journal.costs[0].kind, CostKind::Total);
    assert!(
        journal
            .to_ledger(ELIDED)
            .contains("\ta\t1 BTC @@ 10.5 CAD\n")
    );
    assert_eq!(journal.txns[0].flag, TxnFlag::Uncleared);
}

/// Round-trip a local journal named by `LEDGERDB_BENCH_FILE`, printing
/// counts and timings only, never the file's contents. Ignored by default:
///
/// ```text
/// LEDGERDB_BENCH_FILE=books.ledger \
///   cargo test --release -p ledgerdb --test write -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn local_file_round_trips() {
    let Ok(path) = std::env::var("LEDGERDB_BENCH_FILE") else {
        println!("LEDGERDB_BENCH_FILE not set; skipping");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read LEDGERDB_BENCH_FILE");
    let first = Journal::parse(&text);
    let start = std::time::Instant::now();
    let written = first.journal.to_ledger(ELIDED);
    let elapsed = start.elapsed();
    let second = Journal::parse(&written);
    println!(
        "{} txns, {} postings, {} parse errors; wrote {} bytes in {:.1} ms",
        first.journal.txns.len(),
        first.journal.postings.len(),
        first.errors.len(),
        written.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        second.errors.is_empty(),
        "the written text has parse errors"
    );
    assert!(
        resolve(&first.journal, false) == resolve(&second.journal, false),
        "round trip changed the journal"
    );
    assert!(
        second.journal.to_ledger(ELIDED) == written,
        "not idempotent"
    );
}
