//! The in-memory ledger: merging segments, record-id dedup, layers and
//! incremental loads.

use std::fs;

use ledgerdb::{
    Added, Journal, JournalStore, LayerFilter, Ledger, Parsed, Segment, SegmentHash, SegmentHeader,
    WriteOptions,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// A segment's header and journal, which can be encoded any number of times.
struct Batch {
    header: SegmentHeader,
    journal: Journal,
}

impl Batch {
    fn new(source: &str, imported: u64, layer: &str, text: &str) -> Self {
        Self {
            header: SegmentHeader {
                source: source.to_owned(),
                imported,
                layer: layer.to_owned(),
            },
            journal: parse_ok(text),
        }
    }

    /// The segment as read back from its encoded bytes.
    fn segment(&self) -> Segment {
        let bytes = Segment::encode(&self.header, &self.journal).unwrap();
        Segment::parse(SegmentHash::of(bytes.as_bytes()), &bytes).unwrap()
    }

    fn hash(&self) -> SegmentHash {
        self.segment().hash
    }
}

/// One transaction moving `qty` out of `from` on `date`, with record id `id`.
fn txn(date: &str, id: &str, from: &str, qty: &str) -> String {
    format!("{date} * Payee {id}\n  ; id: {id}\n  {from}  -{qty}\n  expenses:unknown\n\n")
}

/// The ledger exported as canonical text, which is equal for two ledgers
/// exactly when they hold the same transactions in the same order.
fn export(ledger: &Ledger) -> String {
    ledger.journal().to_ledger(WriteOptions::default())
}

/// A ledger built by adding `batches` one at a time, in the order given.
fn added(batches: &[&Batch]) -> Ledger {
    let mut ledger = Ledger::new(LayerFilter::All);
    for batch in batches {
        assert_eq!(ledger.add_segment(batch.segment()), Ok(Added::Loaded));
    }
    ledger
}

/// The quantity of the posting to `account` in the transaction with record
/// id `id`.
fn qty_of(ledger: &Ledger, id: &str, account: &str) -> i64 {
    let txn = ledger.find(id).expect("record id loaded");
    let account = ledger.journal().accounts.lookup(account).unwrap();
    let journal = ledger.journal();
    journal
        .postings_of(journal.txn(txn))
        .iter()
        .find(|p| p.account == account)
        .expect("posting to account")
        .qty
}

#[test]
fn the_later_import_wins_in_either_order() {
    let pending = Batch::new(
        "card",
        100,
        "cards",
        &(txn("2024-01-02", "card:1", "liabilities:card", "4.50 CAD")
            + &txn("2024-01-03", "card:2", "liabilities:card", "7.00 CAD")),
    );
    let posted = Batch::new(
        "card",
        200,
        "cards",
        &txn("2024-01-04", "card:1", "liabilities:card", "4.75 CAD"),
    );

    let one = added(&[&pending, &posted]);
    let two = added(&[&posted, &pending]);
    for ledger in [&one, &two] {
        assert_eq!(ledger.txns().len(), 2, "card:1 appears once");
        assert_eq!(qty_of(ledger, "card:1", "liabilities:card"), -475);
        assert_eq!(qty_of(ledger, "card:2", "liabilities:card"), -700);
        let winner = ledger.find("card:1").unwrap();
        assert_eq!(ledger.segment_of(winner).hash, posted.hash());
        assert_eq!(ledger.segment_of(winner).header.imported, 200);
    }
    assert_eq!(export(&one), export(&two));
}

#[test]
fn equal_imports_fall_back_to_the_larger_hash() {
    let a = Batch::new(
        "card",
        100,
        "cards",
        &txn("2024-01-02", "card:1", "liabilities:card", "1.00 CAD"),
    );
    let b = Batch::new(
        "card",
        100,
        "cards",
        &txn("2024-01-02", "card:1", "liabilities:card", "2.00 CAD"),
    );
    let (winner, want) = if a.hash() > b.hash() {
        (&a, -100)
    } else {
        (&b, -200)
    };

    for ledger in [added(&[&a, &b]), added(&[&b, &a])] {
        assert_eq!(ledger.txns().len(), 1);
        assert_eq!(qty_of(&ledger, "card:1", "liabilities:card"), want);
        let txn = ledger.find("card:1").unwrap();
        assert_eq!(ledger.segment_of(txn).hash, winner.hash());
    }
}

#[test]
fn a_segment_is_only_merged_once() {
    let batch = Batch::new(
        "card",
        1,
        "cards",
        &txn("2024-01-02", "card:1", "liabilities:card", "1.00 CAD"),
    );
    let mut ledger = added(&[&batch]);
    let before = export(&ledger);
    assert_eq!(ledger.add_segment(batch.segment()), Ok(Added::AlreadySeen));
    assert_eq!(export(&ledger), before);
    assert!(ledger.has_seen(batch.hash()));
}

/// Six batches across three layers whose record ids overlap and whose dates
/// interleave.
fn mixed_batches() -> Vec<Batch> {
    let mut batches = Vec::new();
    for (n, layer) in ["cards", "bank", "chain", "cards", "bank", "chain"]
        .iter()
        .enumerate()
    {
        let mut text = String::new();
        for i in 0..20 {
            let day = (i * 7 + n * 3) % 28 + 1;
            // Ids 10.. repeat in the next batch of the same layer.
            let id = format!("{layer}:{}", i + n / 3 * 10);
            let from = match *layer {
                "chain" => "assets:btc",
                "bank" => "assets:bank",
                _ => "liabilities:card",
            };
            let qty = match *layer {
                "chain" => format!("{}{:03} sat", i + 1, n),
                _ => format!("{}.{:02} CAD", i + 1, n * 11),
            };
            text.push_str(&txn(&format!("2024-02-{day:02}"), &id, from, &qty));
        }
        if n == 2 {
            text.push_str("P 2024-02-01 sat 0.00085 CAD\n\n");
        }
        batches.push(Batch::new(layer, 1000 + n as u64, layer, &text));
    }
    batches
}

#[test]
fn incremental_adds_equal_a_full_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let batches = mixed_batches();
    for batch in &batches {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }
    let full = Ledger::load(&store, LayerFilter::All).unwrap();
    assert_eq!(full.report.loaded.len(), batches.len());
    assert!(full.report.unreadable.is_empty() && full.report.unmergeable.is_empty());
    let full = full.ledger;

    let forward: Vec<&Batch> = batches.iter().collect();
    let backward: Vec<&Batch> = batches.iter().rev().collect();
    let shuffled: Vec<&Batch> = [3, 0, 5, 1, 4, 2].iter().map(|&i| &batches[i]).collect();
    for order in [forward, backward, shuffled] {
        let ledger = added(&order);
        assert_eq!(export(&ledger), export(&full));
        assert_eq!(ledger.segments(), full.segments());
        assert_eq!(ledger.prices().len(), 1);
    }

    // 3 layers x (20 + 20 - 10 overlapping ids) records.
    assert_eq!(full.txns().len(), 90);
    for id in ["cards:0", "cards:29", "bank:15", "chain:5"] {
        assert!(full.find(id).is_some(), "{id}");
    }
    assert_eq!(full.find("cards:30"), None);
}

#[test]
fn arrays_are_sorted_and_indexed() {
    let batches = mixed_batches();
    let ledger = added(&batches.iter().collect::<Vec<_>>());
    let postings = ledger.postings();
    assert!(
        postings
            .windows(2)
            .all(|w| (w[0].date, w[0].txn) <= (w[1].date, w[1].txn))
    );
    for (at, txn) in ledger.txns().iter().enumerate() {
        for posting in &postings[txn.postings.range()] {
            assert_eq!(posting.txn.index(), at);
            assert_eq!(posting.date, txn.date);
        }
        assert_eq!(
            ledger
                .find(ledger.journal().strings.get(txn.id.unwrap()))
                .unwrap()
                .index(),
            at
        );
    }

    let mut indexed = 0;
    for account in ledger.journal().accounts.ids() {
        let at = ledger.account_postings(account);
        assert!(at.windows(2).all(|w| w[0] < w[1]), "in posting order");
        assert!(ledger.postings_to(account).all(|p| p.account == account));
        indexed += at.len();
    }
    assert_eq!(indexed, postings.len(), "every posting indexed once");
}

#[test]
fn layers_are_filtered() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    for batch in mixed_batches() {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }
    let layers_of = |ledger: &Ledger| {
        let mut layers: Vec<String> = ledger
            .segments()
            .iter()
            .map(|s| s.header.layer.clone())
            .collect();
        layers.dedup();
        layers.sort();
        layers.dedup();
        layers
    };

    let only = Ledger::load(&store, LayerFilter::Only(vec!["chain".into()])).unwrap();
    assert_eq!(layers_of(&only.ledger), ["chain"]);
    assert_eq!(only.report.loaded.len(), 2);
    assert_eq!(only.report.excluded.len(), 4);
    assert_eq!(only.ledger.excluded().len(), 4);
    assert!(only.ledger.find("cards:0").is_none());
    assert!(only.ledger.find("chain:0").is_some());

    let except = Ledger::load(&store, LayerFilter::Except(vec!["chain".into()])).unwrap();
    assert_eq!(layers_of(&except.ledger), ["bank", "cards"]);
    assert_eq!(except.ledger.txns().len(), 60);
    assert!(except.ledger.prices().is_empty(), "the price was in chain");
    assert!(except.ledger.find("chain:0").is_none());

    let all = Ledger::load(&store, LayerFilter::All).unwrap();
    assert_eq!(layers_of(&all.ledger), ["bank", "cards", "chain"]);
    assert!(all.ledger.excluded().is_empty());
}

/// A skipped segment is never read, even though the store has it, and a
/// refresh leaves it out too; the record id it would have replaced keeps
/// its older row.
#[test]
fn load_without_leaves_skipped_segments_out() {
    let tmp = tempfile::tempdir().unwrap();
    let store = JournalStore::new(tmp.path());
    let old = Batch::new("a", 1, "x", &txn("2024-01-01", "a:1", "assets:a", "1 CAD"));
    let newer = Batch::new("a", 2, "x", &txn("2024-01-01", "a:1", "assets:a", "2 CAD"));
    let other = Batch::new("b", 1, "x", &txn("2024-01-02", "b:1", "assets:b", "3 CAD"));
    for batch in [&old, &newer, &other] {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }

    let all = Ledger::load(&store, LayerFilter::All).unwrap();
    assert!(export(&all.ledger).contains("-2 CAD"));
    assert!(all.report.skipped.is_empty());

    let skip = std::collections::HashSet::from([newer.hash()]);
    let mut loaded = Ledger::load_without(&store, LayerFilter::All, skip).unwrap();
    assert_eq!(loaded.report.skipped, [newer.hash()]);
    assert_eq!(loaded.report.loaded.len(), 2);
    assert!(!loaded.ledger.has_seen(newer.hash()));
    let text = export(&loaded.ledger);
    assert!(
        text.contains("-1 CAD") && !text.contains("-2 CAD"),
        "{text}"
    );

    let refreshed = loaded.ledger.refresh(&store).unwrap();
    assert_eq!(refreshed.skipped, [newer.hash()]);
    assert!(refreshed.loaded.is_empty());
    assert_eq!(export(&loaded.ledger), text);
}

#[test]
fn coarser_precision_is_scaled_up_in_either_order() {
    let coarse = Batch::new(
        "a",
        1,
        "x",
        &txn("2024-01-01", "a:1", "assets:a", "1.5 CAD"),
    );
    let fine = Batch::new(
        "b",
        1,
        "x",
        &txn("2024-01-02", "b:1", "assets:b", "1.125 CAD"),
    );
    let one = added(&[&coarse, &fine]);
    let two = added(&[&fine, &coarse]);
    for ledger in [&one, &two] {
        let cad = ledger.journal().commodities.get("CAD").unwrap();
        assert_eq!(ledger.journal().commodities.precision(cad), 3);
        assert_eq!(qty_of(ledger, "a:1", "assets:a"), -1500);
        assert_eq!(qty_of(ledger, "b:1", "assets:b"), -1125);
    }
    assert_eq!(export(&one), export(&two));
    assert!(export(&one).contains("-1.500 CAD"));
}

#[test]
fn precision_that_would_overflow_is_refused_and_leaves_the_ledger_alone() {
    let big = Batch::new(
        "a",
        1,
        "x",
        &txn("2024-01-01", "a:1", "assets:a", "9000000000000000000 X"),
    );
    let fine = Batch::new("b", 1, "x", &txn("2024-01-02", "b:1", "assets:b", "0.5 X"));
    let mut ledger = added(&[&big]);
    let before = export(&ledger);
    let err = ledger.add_segment(fine.segment()).unwrap_err();
    assert!(
        matches!(err, ledgerdb::MergeError::Overflow { precision: 1, .. }),
        "{err}"
    );
    assert_eq!(export(&ledger), before);
    assert!(!ledger.has_seen(fine.hash()));
}

#[test]
fn costs_and_prices_survive_the_merge() {
    let trade = Batch::new(
        "x",
        1,
        "x",
        "P 2024-01-01 BTC 61234.56789 CAD\n\n\
         2024-01-03 * Later\n  ; id: x:2\n  assets:cash  -1.00 CAD\n  expenses:unknown\n\n\
         2024-01-02 * Buy\n  ; id: x:1\n  assets:btc  0.001 BTC @ 60000.00 CAD\n  assets:cash\n\n",
    );
    let other = Batch::new(
        "y",
        1,
        "y",
        &txn("2024-01-01", "y:1", "assets:cash", "3.00 CAD"),
    );
    let ledger = added(&[&trade, &other]);
    let journal = ledger.journal();
    let buy = journal.txn(ledger.find("x:1").unwrap());
    let btc = journal.accounts.lookup("assets:btc").unwrap();
    let at = buy
        .postings
        .range()
        .find(|&p| journal.postings[p].account == btc)
        .unwrap();
    let cost = journal
        .cost_of(at as u32)
        .expect("cost moved with its posting");
    assert_eq!(journal.commodities.name(cost.commodity), "CAD");
    assert_eq!(cost.value.mantissa, 6_000_000);
    assert_eq!(ledger.prices().len(), 1);
    assert_eq!(
        journal.commodities.name(ledger.prices()[0].commodity),
        "BTC"
    );
    assert_eq!(
        ledger.price_origin(0).segment as usize,
        ledger
            .segments()
            .iter()
            .position(|s| s.hash == trade.hash())
            .unwrap()
    );

    // Exporting and parsing again gives the same ledger text.
    let text = export(&ledger);
    let reparsed = parse_ok(&text);
    assert_eq!(reparsed.to_ledger(WriteOptions::default()), text);
}

#[test]
fn refresh_reads_only_new_segments() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let batches = mixed_batches();
    for batch in &batches[..4] {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }
    let mut ledger = Ledger::load(&store, LayerFilter::Except(vec!["bank".into()]))
        .unwrap()
        .ledger;

    // Corrupt every segment already seen: a refresh that read them again
    // would report them.
    for hash in store.list().unwrap() {
        fs::write(store.path_of(hash), "corrupt").unwrap();
    }
    for batch in &batches[4..] {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }
    let report = ledger.refresh(&store).unwrap();
    assert!(report.unreadable.is_empty(), "{:?}", report.unreadable);
    assert_eq!(report.loaded, [batches[5].hash()]);
    assert_eq!(report.excluded, [batches[4].hash()]);

    let fresh = added(&[&batches[0], &batches[2], &batches[3], &batches[5]]);
    assert_eq!(export(&ledger), export(&fresh));

    let again = ledger.refresh(&store).unwrap();
    assert!(again.loaded.is_empty() && again.excluded.is_empty());
}

#[test]
fn an_unreadable_segment_is_reported_and_the_rest_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = JournalStore::new(dir.path());
    let batches = mixed_batches();
    for batch in &batches {
        store.write_segment(&batch.header, &batch.journal).unwrap();
    }
    fs::write(store.path_of(batches[1].hash()), "corrupt").unwrap();

    let loaded = Ledger::load(&store, LayerFilter::All).unwrap();
    assert_eq!(loaded.report.unreadable.len(), 1);
    assert_eq!(loaded.report.loaded.len(), batches.len() - 1);
    assert!(
        !loaded.ledger.has_seen(batches[1].hash()),
        "retried next refresh"
    );
}

#[test]
fn a_commodity_takes_its_style_from_the_smallest_hash() {
    let prefix = Batch::new("a", 1, "x", &txn("2024-01-01", "a:1", "assets:a", "$1.00"));
    let suffix = Batch::new("b", 1, "x", &txn("2024-01-02", "b:1", "assets:b", "2.00 $"));
    let one = added(&[&prefix, &suffix]);
    let two = added(&[&suffix, &prefix]);
    assert_eq!(export(&one), export(&two));
    let want = if prefix.hash() < suffix.hash() {
        "-$2.00"
    } else {
        "-1.00 $"
    };
    assert!(export(&one).contains(want), "{}", export(&one));
}
