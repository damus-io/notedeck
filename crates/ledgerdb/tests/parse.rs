//! The ledger-cli subset parser, against synthetic fixtures.

use ledgerdb::{CostKind, Decimal, Journal, ParseErrorKind, Parsed, PostingFlags, Txn, TxnFlag};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}");
    journal
}

/// Each posting of `txn` as `account amount`, written in its commodity's
/// style, so tests can compare against text.
fn postings(journal: &Journal, txn: &Txn) -> Vec<String> {
    journal
        .postings_of(txn)
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                journal.accounts.full_name(p.account),
                p.amount().display(&journal.commodities)
            )
        })
        .collect()
}

/// The `key=value` metadata of `txn`, with the posting offset when it has
/// one.
fn meta(journal: &Journal, txn: &Txn) -> Vec<String> {
    journal
        .meta_of(txn)
        .iter()
        .map(|m| {
            let at = m.posting.map_or(String::new(), |p| format!("@{p} "));
            format!(
                "{at}{}={}",
                journal.strings.get(m.key),
                journal.strings.get(m.value)
            )
        })
        .collect()
}

fn comments(journal: &Journal, txn: &Txn) -> Vec<String> {
    journal
        .comments_of(txn)
        .iter()
        .map(|c| {
            let at = c.posting.map_or(String::new(), |p| format!("@{p} "));
            format!("{at}{}", journal.strings.get(c.text))
        })
        .collect()
}

#[test]
fn header_fields_and_both_separators() {
    let journal = parse_ok(
        "2024-01-02 * (1042) Coffee Shop\n\
         \texpenses:food:coffee\t4.50 CAD\n\
         \tassets:bank:checking  -4.50 CAD\n\
         \n\
         2024/1/3 ! Second\n  \
           assets:my wallet  $-1.00\n  \
           expenses:misc  $1.00\n",
    );
    assert_eq!(journal.txns.len(), 2);

    let first = &journal.txns[0];
    assert_eq!(first.date.to_string(), "2024-01-02");
    assert_eq!(first.flag, TxnFlag::Cleared);
    assert_eq!(journal.strings.get(first.code.unwrap()), "1042");
    assert_eq!(journal.strings.get(first.payee), "Coffee Shop");
    assert_eq!(
        postings(&journal, first),
        [
            "expenses:food:coffee 4.50 CAD",
            "assets:bank:checking -4.50 CAD"
        ]
    );

    let second = &journal.txns[1];
    assert_eq!(second.flag, TxnFlag::Pending);
    assert_eq!(second.code, None);
    assert_eq!(
        postings(&journal, second),
        ["assets:my wallet $-1.00", "expenses:misc $1.00"]
    );
    assert_eq!(journal.postings[2].txn.index(), 1);
}

#[test]
fn payee_keeps_semicolons_at_and_equals() {
    let journal = parse_ok(
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
    );
    let payees: Vec<&str> = journal
        .txns
        .iter()
        .map(|t| journal.strings.get(t.payee))
        .collect();
    assert_eq!(
        payees,
        ["A;B @ C = D ; still payee", "Tabbed", "Spaced", ""]
    );
    assert_eq!(comments(&journal, &journal.txns[1]), ["note after a tab"]);
    assert_eq!(meta(&journal, &journal.txns[2]), ["key=value"]);
}

#[test]
fn posting_comment_needs_whitespace_before_semicolon() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \tassets:a;b\t1 X ; trailing note\n\
         \tassets:c ; just a note\t-1 X\n\
         \tassets:c\t-0 X\n",
    );
    let txn = &journal.txns[0];
    // The second line's amount sits after its comment, so it is elided and
    // the third line makes it unambiguous which commodity to fill.
    assert_eq!(
        postings(&journal, txn),
        ["assets:a;b 1 X", "assets:c -1 X", "assets:c 0 X"]
    );
    assert_eq!(
        comments(&journal, txn),
        ["@0 trailing note", "@1 just a note\t-1 X"]
    );
    assert!(journal.postings[1].flags.contains(PostingFlags::ELIDED));
}

#[test]
fn elided_amount_balances_one_commodity() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \texpenses:a\t1.25 CAD\n\
         \texpenses:b\t2 CAD\n\
         \tassets:bank\n",
    );
    let txn = &journal.txns[0];
    assert_eq!(
        postings(&journal, txn),
        [
            "expenses:a 1.25 CAD",
            "expenses:b 2.00 CAD",
            "assets:bank -3.25 CAD"
        ]
    );
    let last = journal.postings_of(txn)[2];
    assert!(last.flags.contains(PostingFlags::ELIDED));
    assert!(
        !journal.postings_of(txn)[0]
            .flags
            .contains(PostingFlags::ELIDED)
    );
}

#[test]
fn elided_amount_expands_per_commodity_in_place() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \texpenses:a\t5 CAD\n\
         \tassets:wallet ; key: on-elided\n\
         \texpenses:b\t3 USD ; key: on-b\n\
         \t; own: line-after-b\n",
    );
    let txn = &journal.txns[0];
    assert_eq!(
        postings(&journal, txn),
        [
            "expenses:a 5 CAD",
            "assets:wallet -5 CAD",
            "assets:wallet -3 USD",
            "expenses:b 3 USD",
        ]
    );
    let flags: Vec<bool> = journal
        .postings_of(txn)
        .iter()
        .map(|p| p.flags.contains(PostingFlags::ELIDED))
        .collect();
    assert_eq!(flags, [false, true, true, false]);
    // Metadata after the expansion points at the same postings it was
    // written on.
    assert_eq!(
        meta(&journal, txn),
        ["@1 key=on-elided", "@3 key=on-b", "@3 own=line-after-b"]
    );
}

#[test]
fn unbalanced_transaction_is_reported_and_left_out() {
    let Parsed { journal, errors } = Journal::parse(
        "2024-01-01 good\n\
         \ta\t1 X\n\
         \tb\t-1 X\n\
         \n\
         2024-01-02 bad\n\
         \ta\t1 X\n\
         \tb\t-2 X\n\
         \t; id: lost\n\
         \n\
         2024-01-03 good again\n\
         \ta\t3 X\n\
         \tb\n",
    );
    assert_eq!(errors.len(), 1);
    assert_eq!((errors[0].line, errors[0].col), (5, 1));
    assert_eq!(
        errors[0].kind,
        ParseErrorKind::Unbalanced {
            residual: "-1 X".into()
        }
    );
    assert_eq!(journal.txns.len(), 2);
    assert_eq!(journal.postings.len(), 4);
    assert!(journal.meta.is_empty());
    assert_eq!(journal.strings.get(journal.txns[1].payee), "good again");
    assert_eq!(journal.postings[2].txn.index(), 1);
    assert_eq!(journal.txns[1].postings.start, 2);
}

#[test]
fn metadata_attaches_to_transaction_or_posting() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \t; id: src:123\n\
         \t; a plain comment\n\
         \t; raw-merchant: Shop: Main St\n\
         \tassets:a\t1 X ; posted: 2024-01-03\n\
         \t; note on a\n\
         \tassets:b\t-1 X\n\
         \t; tag:\n",
    );
    let txn = &journal.txns[0];
    assert_eq!(journal.strings.get(txn.id.unwrap()), "src:123");
    assert_eq!(
        meta(&journal, txn),
        [
            "id=src:123",
            "raw-merchant=Shop: Main St",
            "@0 posted=2024-01-03",
            "@1 tag=",
        ]
    );
    assert_eq!(comments(&journal, txn), ["a plain comment", "@0 note on a"]);
    assert_eq!(journal.txn_meta(txn, "raw-merchant"), Some("Shop: Main St"));
    assert_eq!(journal.txn_meta(txn, "posted"), None);
}

#[test]
fn precision_is_learned_before_any_amount_is_converted() {
    // A one-pass parser would fix CAD at 0 decimals on the first line.
    let journal = parse_ok(
        "2024-01-01 x\n\
         \ta\t5 CAD\n\
         \tb\t-5 CAD\n\
         2024-01-02 y\n\
         \ta\t5.25 CAD\n\
         \tb\t-5.25 CAD\n",
    );
    let qtys: Vec<i64> = journal.postings.iter().map(|p| p.qty).collect();
    assert_eq!(qtys, [500, -500, 525, -525]);
    let cad = journal.commodities.get("CAD").unwrap();
    assert_eq!(journal.commodities.precision(cad), 2);
}

#[test]
fn price_directives_keep_their_own_scale() {
    let journal = parse_ok(
        "P 2024-01-01 BTC 61234.56789 CAD\n\
         P 2024/01/02 msat 0.00000000001 BTC\n\
         2024-01-03 x\n\
         \ta\t1.00 CAD\n\
         \tb\t-1.00 CAD\n",
    );
    assert_eq!(journal.prices.len(), 2);
    let cad = journal.commodities.get("CAD").unwrap();
    let btc = journal.commodities.get("BTC").unwrap();
    let first = journal.prices[0];
    assert_eq!(first.date.to_string(), "2024-01-01");
    assert_eq!(first.commodity, btc);
    assert_eq!(first.quote, cad);
    assert_eq!(
        first.value,
        Decimal {
            mantissa: 6_123_456_789,
            scale: 5
        }
    );
    // The price's five decimals did not leak into CAD's precision.
    assert_eq!(journal.commodities.precision(cad), 2);
    assert_eq!(journal.prices[1].value.scale, 11);
}

#[test]
fn costs_balance_and_are_kept() {
    let journal = parse_ok(
        "2024-01-02 buy\n\
         \tassets:btc\t0.5 BTC @ 40000.005 CAD\n\
         \tassets:bank\t-20000.00 CAD\n\
         2024-01-03 buy total\n\
         \tassets:btc\t-2 BTC @@ 1000 CAD\n\
         \tassets:bank\n",
    );
    assert_eq!(journal.costs.len(), 2);
    let first = journal.costs[0];
    assert_eq!(first.posting, 0);
    assert_eq!(first.kind, CostKind::PerUnit);
    assert_eq!(
        first.value,
        Decimal {
            mantissa: 40_000_005,
            scale: 3
        }
    );
    let second = journal.cost_of(2).unwrap();
    assert_eq!(second.kind, CostKind::Total);
    assert!(journal.cost_of(1).is_none());
    // `@@` follows the sign of the quantity: selling 2 BTC brings in CAD.
    assert_eq!(
        postings(&journal, &journal.txns[1]),
        ["assets:btc -2.0 BTC", "assets:bank 1000.00 CAD"]
    );
}

#[test]
fn virtual_postings() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \ta\t1 X\n\
         \tb\t-1 X\n\
         \t(budget:food)\t-50 X\n\
         \t[fund:a]\t10 X\n\
         \t[fund:b]\n",
    );
    let txn = &journal.txns[0];
    assert_eq!(
        postings(&journal, txn),
        [
            "a 1 X",
            "b -1 X",
            "budget:food -50 X",
            "fund:a 10 X",
            "fund:b -10 X"
        ]
    );
    let flags: Vec<PostingFlags> = journal.postings_of(txn).iter().map(|p| p.flags).collect();
    assert!(flags[2].contains(PostingFlags::VIRTUAL));
    assert!(flags[3].contains(PostingFlags::BALANCED_VIRTUAL));
    assert!(flags[4].contains(PostingFlags::BALANCED_VIRTUAL.with(PostingFlags::ELIDED)));
}

#[test]
fn posting_marks() {
    let journal = parse_ok(
        "2024-01-02 x\n\
         \t* a\t1 X\n\
         \t! b\t-1 X\n",
    );
    assert!(journal.postings[0].flags.contains(PostingFlags::CLEARED));
    assert!(journal.postings[1].flags.contains(PostingFlags::PENDING));
    assert_eq!(journal.accounts.full_name(journal.postings[1].account), "b");
}

#[test]
fn file_comments_record_their_place() {
    let journal = parse_ok(
        "; ledgerdb-segment: 1\n\
         # source: synthetic\n\
         2024-01-02 x\n\
         \ta\t1 X\n\
         \tb\t-1 X\n\
         ; between\n",
    );
    let found: Vec<(&str, u32)> = journal
        .file_comments
        .iter()
        .map(|c| (journal.strings.get(c.text), c.txns_before))
        .collect();
    assert_eq!(
        found,
        [
            ("ledgerdb-segment: 1", 0),
            ("source: synthetic", 0),
            ("between", 1)
        ]
    );
}

#[test]
fn crlf_line_endings() {
    let journal = parse_ok("2024-01-02 x\r\n\ta\t1 X\r\n\tb\r\n");
    assert_eq!(journal.strings.get(journal.txns[0].payee), "x");
    assert_eq!(postings(&journal, &journal.txns[0]), ["a 1 X", "b -1 X"]);
}

#[test]
fn repeated_strings_are_interned_once() {
    let journal = parse_ok(
        "2024-01-02 same\n\ta\t1 X\n\tb\t-1 X\n\
         2024-01-03 same\n\ta\t2 X\n\tb\t-2 X\n",
    );
    assert_eq!(journal.txns[0].payee, journal.txns[1].payee);
    assert_eq!(journal.accounts.len(), 2);
    assert_eq!(journal.strings.len(), 1);
}

#[test]
fn errors_recover_and_report_line_and_column() {
    let Parsed { journal, errors } = Journal::parse(
        "\tstray\t1 X\n\
         2024-13-01 bad date\n\
         \ta\t1 X\n\
         \tb\t-1 X\n\
         2024-01-02 bad amount\n\
         \ta\t1 X\n\
         \tb\t-1..0 X\n\
         \tc\t5 X\n\
         include other.ledger\n\
         \tindented under a directive\n\
         2024-01-03 two elided\n\
         \ta\n\
         \tb\n\
         2024-01-04 no postings\n\
         2024-01-05 (unclosed code\n\
         \ta\t1 X\n\
         2024-01-06 unclosed virtual\n\
         \t(a\t1 X\n\
         2024-01-07 elided virtual\n\
         \ta\t1 X\n\
         \t(b)\n\
         2024-01-08 cost without amount\n\
         \ta\t@ 1 X\n\
         P 2024-01-09 BTC\n\
         P 2024-01-09 BTC 1..0 X\n\
         2024-01-10 survivor\n\
         \ta\t7 X\n\
         \tb\n",
    );
    let found: Vec<(u32, u32, &ParseErrorKind)> =
        errors.iter().map(|e| (e.line, e.col, &e.kind)).collect();
    use ParseErrorKind::*;
    assert_eq!(found.len(), 12, "{errors:#?}");
    assert_eq!(found[0], (1, 1, &OrphanPosting));
    assert!(matches!(found[1], (2, 1, Date(_))));
    assert!(matches!(found[2], (7, 4, Amount(_))));
    assert_eq!(found[3], (9, 1, &UnsupportedDirective));
    assert_eq!(found[4], (13, 2, &SecondElided));
    assert_eq!(found[5], (14, 1, &NoPostings));
    assert_eq!(found[6], (15, 12, &UnclosedCode));
    assert_eq!(found[7], (18, 2, &UnclosedVirtual));
    assert_eq!(found[8], (21, 2, &ElidedVirtual));
    assert_eq!(found[9], (23, 4, &CostWithoutAmount));
    assert_eq!(found[10], (24, 17, &BadPrice));
    assert!(matches!(found[11], (25, 18, Amount(_))));

    // Only the survivor made it, intact and pointing at its own postings.
    assert_eq!(journal.txns.len(), 1);
    let txn = &journal.txns[0];
    assert_eq!(journal.strings.get(txn.payee), "survivor");
    assert_eq!(postings(&journal, txn), ["a 7 X", "b -7 X"]);
    assert!(journal.postings.iter().all(|p| p.txn.index() == 0));
    assert!(journal.prices.is_empty());
    assert_eq!(journal.costs.len(), 0);
    // Amount still parses through the same table.
    let x = journal.commodities.get("X").unwrap();
    assert_eq!(
        journal.postings[0].amount(),
        ledgerdb::Amount {
            qty: 7,
            commodity: x
        }
    );
}
