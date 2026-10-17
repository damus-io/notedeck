//! `Ledger::novelty`: which of a batch's rows a ledger already holds.

use ledgerdb::{
    Journal, LayerFilter, Ledger, Novelty, Parsed, Segment, SegmentHash, SegmentHeader, TxnId,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// A ledger holding `text` as one segment.
fn ledger_of(text: &str) -> Ledger {
    ledger_of_imports(&[text])
}

/// A ledger holding each of `texts` as a segment, imported in the order
/// given.
fn ledger_of_imports(texts: &[&str]) -> Ledger {
    let mut ledger = Ledger::new(LayerFilter::All);
    for (at, text) in texts.iter().enumerate() {
        let header = SegmentHeader {
            source: "test".to_owned(),
            imported: at as u64 + 1,
            layer: "base".to_owned(),
        };
        let bytes = Segment::encode(&header, &parse_ok(text)).unwrap();
        let segment = Segment::parse(SegmentHash::of(bytes.as_bytes()), &bytes).unwrap();
        ledger.add_segment(segment).unwrap();
    }
    ledger
}

/// The novelty of each of `batch`'s transactions against `ledger`.
fn novelties(ledger: &Ledger, batch: &str) -> Vec<Novelty> {
    let batch = parse_ok(batch);
    (0..batch.txns.len() as u32)
        .map(|at| ledger.novelty(&batch, TxnId(at)))
        .collect()
}

const HELD: &str = "\
2024-01-02 ! (42) Coffee
    ; id: card:1
    ; raw-desc: COFFEE SHOP
    ; a note
    liabilities:card  -4.50 CAD
    expenses:unknown

2024-01-03 * Trade
    ; id: card:2
    assets:btc  0.001 BTC @ 61234.5 CAD
    assets:bank  -61.23 CAD
    expenses:fees
";

#[test]
fn the_same_rows_are_unchanged_and_others_are_new() {
    let ledger = ledger_of(HELD);
    assert_eq!(
        novelties(&ledger, HELD),
        [Novelty::Unchanged, Novelty::Unchanged]
    );
    let other = "2024-01-02 Other\n    ; id: card:3\n    a  1 CAD\n    b\n";
    assert_eq!(novelties(&ledger, other), [Novelty::New]);
    let no_id = "2024-01-02 ! (42) Coffee\n    liabilities:card  -4.50 CAD\n    expenses:unknown\n";
    assert_eq!(novelties(&ledger, no_id), [Novelty::New]);
}

#[test]
fn a_precision_difference_alone_is_not_a_change() {
    // The ledger has seen CAD at three places, so it holds 4.500 and
    // 61.230; the batch writes them at two, and the cost at another scale.
    let finer = format!("{HELD}\n2024-01-04 Fine\n    ; id: card:9\n    a  0.001 CAD\n    b\n");
    let ledger = ledger_of(&finer);
    let coarser = HELD.replace("61234.5 CAD", "61234.500 CAD");
    assert_eq!(
        novelties(&ledger, &coarser),
        [Novelty::Unchanged, Novelty::Unchanged]
    );
}

#[test]
fn any_field_that_differs_is_a_change() {
    let ledger = ledger_of(HELD);
    let edits = [
        ("2024-01-02 !", "2024-01-05 !"),
        ("! (42)", "* (42)"),
        ("(42) ", "(43) "),
        ("(42) ", ""),
        ("Coffee", "Tea"),
        ("COFFEE SHOP", "TEA SHOP"),
        ("    ; a note\n", ""),
        ("-4.50 CAD", "-4.51 CAD"),
        ("-4.50 CAD", "-4.50 USD"),
        ("liabilities:card", "liabilities:other"),
        ("    liabilities:card", "    * liabilities:card"),
        ("@ 61234.5 CAD", "@ 61234.6 CAD"),
        ("@ 61234.5 CAD", "@@ 61.2345 CAD"),
        ("    expenses:fees\n", ""),
        ("expenses:fees", "expenses:other"),
        ("expenses:fees", "expenses:fees  -0.0045 CAD"),
    ];
    for (from, to) in edits {
        let edited = HELD.replace(from, to);
        assert_ne!(edited, HELD, "{from:?} is in the fixture");
        let got = novelties(&ledger, &edited);
        assert!(
            got.contains(&Novelty::Changed),
            "{from:?} -> {to:?} went unnoticed: {got:?}"
        );
    }
}

#[test]
fn a_price_is_compared_with_the_rate_in_use_for_its_date_and_pair() {
    let ledger = ledger_of(
        "P 2024-01-02 BTC 61234.5 CAD\nP 2024-01-02 BTC 85000 USD\nP 2024-01-03 BTC 62000 CAD\n",
    );
    let batch = parse_ok(
        "P 2024-01-02 BTC 61234.50 CAD\n\
         P 2024-01-02 BTC 61000 CAD\n\
         P 2024-01-04 BTC 61234.5 CAD\n\
         P 2024-01-02 BTC 61234.5 EUR\n\
         P 2024-01-02 ETH 61234.5 CAD\n",
    );
    let got: Vec<_> = (0..batch.prices.len())
        .map(|at| ledger.price_novelty(&batch, at))
        .collect();
    assert_eq!(
        got,
        [
            Novelty::Unchanged,
            Novelty::Changed,
            Novelty::New,
            Novelty::New,
            Novelty::New
        ]
    );
}

#[test]
fn a_rate_a_later_import_revised_is_compared_with_the_revision() {
    let ledger = ledger_of_imports(&[
        "P 2024-01-02 BTC 61000 CAD\n",
        "P 2024-01-02 BTC 62000 CAD\n",
    ]);
    let stale = parse_ok("P 2024-01-02 BTC 61000 CAD\n");
    assert_eq!(ledger.price_novelty(&stale, 0), Novelty::Changed);
    let current = parse_ok("P 2024-01-02 BTC 62000 CAD\n");
    assert_eq!(ledger.price_novelty(&current, 0), Novelty::Unchanged);
}
