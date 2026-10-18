//! The derive step: priority between edits and rules, first-match rules,
//! condition semantics, split validation, transfer netting, aliases,
//! determinism, explain, and re-deriving from a previous result.

use ledgerdb::{
    Added, Alias, Cond, Derived, EditRef, Edits, Field, Journal, LayerFilter, Ledger, Leg,
    Override, Parsed, Problem, Rejected, Rule, Segment, SegmentHash, SegmentHeader, Split,
    TransferLink, Why, WriteOptions, derive, rederive,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

/// One segment's worth of text from `source`, layer `layer`.
fn segment(source: &str, imported: u64, layer: &str, text: &str) -> Segment {
    let header = SegmentHeader {
        source: source.to_owned(),
        imported,
        layer: layer.to_owned(),
    };
    let bytes = Segment::encode(&header, &parse_ok(text)).unwrap();
    Segment::parse(SegmentHash::of(bytes.as_bytes()), &bytes).unwrap()
}

/// A ledger of `segments`, added in the order given.
fn ledger(segments: Vec<Segment>) -> Ledger {
    let mut ledger = Ledger::new(LayerFilter::All);
    for segment in segments {
        assert_eq!(ledger.add_segment(segment), Ok(Added::Loaded));
    }
    ledger
}

/// A card purchase the importer could not categorize.
fn purchase(id: &str, merchant: &str, cad: &str) -> String {
    format!(
        "2024-01-02 * {merchant}\n  ; id: {id}\n  ; raw-merchant: {merchant}\n  liabilities:card  -{cad} CAD\n  expenses:unknown\n\n"
    )
}

/// A card statement: three uncategorized purchases.
fn card() -> Segment {
    let text = purchase("card:1", "Bean There Cafe", "4.50")
        + &purchase("card:2", "CORNER GROCER", "30.00")
        + &purchase("card:3", "Bean There Cafe", "6.25");
    segment("card", 100, "cards", &text)
}

/// Edits with the usual importer defaults and no edits yet.
fn edits() -> Edits {
    Edits {
        defaults: vec!["expenses:unknown".to_owned(), "income:unknown".to_owned()],
        ..Edits::default()
    }
}

fn rule(account: &str, conditions: Vec<Cond>) -> Rule {
    Rule {
        account: account.to_owned(),
        conditions,
        layer: None,
    }
}

fn merchant_contains(needle: &str) -> Cond {
    Cond::Contains {
        field: Field::Meta("raw-merchant".to_owned()),
        needle: needle.to_owned(),
    }
}

fn over(record_id: &str, account: &str) -> Override {
    Override {
        record_id: record_id.to_owned(),
        account: account.to_owned(),
    }
}

/// The accounts of the postings of the transaction with record id `id`, in
/// order, with their quantities.
fn legs_of(derived: &Derived, id: &str) -> Vec<String> {
    let ledger = derived.ledger();
    let journal = ledger.journal();
    let txn = journal.txn(ledger.find(id).expect("record id loaded"));
    journal
        .postings_of(txn)
        .iter()
        .map(|p| format!("{} {}", journal.accounts.full_name(p.account), p.qty))
        .collect()
}

/// The sum of every posting to `account` itself, in smallest units.
fn balance(ledger: &Ledger, account: &str) -> i64 {
    let Some(account) = ledger.journal().accounts.lookup(account) else {
        return 0;
    };
    ledger.postings_to(account).map(|p| p.qty).sum()
}

fn export(ledger: &Ledger) -> String {
    ledger.journal().to_ledger(WriteOptions::default())
}

#[test]
fn no_edits_changes_nothing() {
    let raw = ledger(vec![card()]);
    let derived = derive(&raw, &edits());
    assert_eq!(export(derived.ledger()), export(&raw));
    assert_eq!(derived.explain("card:1"), Some(Why::Default));
    assert_eq!(derived.report(), &Default::default());
}

#[test]
fn the_first_matching_rule_wins() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.rules = vec![
        rule("expenses:coffee", vec![merchant_contains("bean there")]),
        rule("expenses:food", vec![merchant_contains("e")]),
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(legs_of(&derived, "card:1")[1], "expenses:coffee 450");
    assert_eq!(legs_of(&derived, "card:2")[1], "expenses:food 3000");
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("card:2"), Some(Why::Rule(1)));
    assert_eq!(balance(derived.ledger(), "expenses:unknown"), 0);
    assert_eq!(balance(&raw, "expenses:unknown"), 450 + 3000 + 625);
}

#[test]
fn every_condition_must_hold() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    let big = Cond::Amount {
        commodity: Some("CAD".to_owned()),
        min: 500,
        max: 1000,
    };
    edits.rules = vec![rule(
        "expenses:coffee:big",
        vec![merchant_contains("BEAN"), big],
    )];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Default), "450 < 500");
    assert_eq!(derived.explain("card:3"), Some(Why::Rule(0)));
}

#[test]
fn amount_ranges_are_inclusive_magnitudes() {
    let text = "2024-01-02 * zap\n  ; id: ln:1\n  assets:node  1971000 msat\n  income:unknown\n\n\
                2024-01-02 * zap\n  ; id: ln:2\n  assets:node  1978000 msat\n  income:unknown\n\n\
                2024-01-02 * zap\n  ; id: ln:3\n  assets:node  1978001 msat\n  income:unknown\n\n";
    let raw = ledger(vec![segment("ln", 1, "lightning", text)]);
    let mut edits = edits();
    let bucket = Cond::Amount {
        commodity: Some("msat".to_owned()),
        min: 1_971_000,
        max: 1_978_000,
    };
    edits.rules = vec![rule("income:zaps:bucket", vec![bucket])];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("ln:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("ln:2"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("ln:3"), Some(Why::Default));
    assert_eq!(legs_of(&derived, "ln:1")[1], "income:zaps:bucket -1971000");

    // A commodity the ledger has never seen matches nothing.
    edits.rules = vec![rule(
        "income:zaps:bucket",
        vec![Cond::Amount {
            commodity: Some("sat".to_owned()),
            min: 0,
            max: i64::MAX,
        }],
    )];
    assert_eq!(derive(&raw, &edits).explain("ln:1"), Some(Why::Default));
}

#[test]
fn contains_ignores_case_and_equals_does_not() {
    let text = purchase("card:1", "Café Ünïcode", "1.00") + &purchase("card:2", "plain", "1.00");
    let raw = ledger(vec![segment("card", 1, "cards", &text)]);
    let mut edits = edits();
    edits.rules = vec![
        rule("expenses:cafe", vec![merchant_contains("CAFÉ ÜN")]),
        rule(
            "expenses:plain",
            vec![Cond::Equals {
                field: Field::Payee,
                value: "PLAIN".to_owned(),
            }],
        ),
        rule(
            "expenses:payee",
            vec![Cond::Contains {
                field: Field::Payee,
                needle: "LAI".to_owned(),
            }],
        ),
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("card:2"), Some(Why::Rule(2)));

    edits.rules[1] = rule(
        "expenses:plain",
        vec![Cond::Equals {
            field: Field::Meta("raw-merchant".to_owned()),
            value: "plain".to_owned(),
        }],
    );
    assert_eq!(derive(&raw, &edits).explain("card:2"), Some(Why::Rule(1)));
}

#[test]
fn source_and_layer_scope_a_rule() {
    let bank = "2024-01-03 * Bean There Cafe\n  ; id: bank:1\n  ; raw-merchant: Bean There Cafe\n  assets:chequing  -2.00 CAD\n  expenses:unknown\n\n";
    let raw = ledger(vec![card(), segment("bank", 1, "banks", bank)]);
    let mut edits = edits();
    edits.rules = vec![
        rule(
            "expenses:coffee:card",
            vec![Cond::Source("card".to_owned()), merchant_contains("bean")],
        ),
        Rule {
            layer: Some("banks".to_owned()),
            ..rule("expenses:coffee:bank", vec![])
        },
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("bank:1"), Some(Why::Rule(1)));
    assert_eq!(derived.explain("card:2"), Some(Why::Default));
}

#[test]
fn priority_is_split_override_link_rule_default() {
    let mut text = purchase("card:1", "Bean There Cafe", "4.50");
    text += "2024-01-04 * Payment\n  ; id: card:pay\n  liabilities:card  100.00 CAD\n  income:unknown\n\n";
    let bank = "2024-01-04 * Card payment\n  ; id: bank:pay\n  assets:chequing  -100.00 CAD\n  expenses:unknown\n\n";
    let raw = ledger(vec![
        segment("card", 1, "cards", &text),
        segment("bank", 1, "banks", bank),
    ]);
    let mut edits = edits();
    // A catch-all rule matches everything a higher tier does not claim.
    edits.rules = vec![rule("expenses:misc", vec![])];
    edits.links = vec![TransferLink {
        a: "card:pay".to_owned(),
        b: "bank:pay".to_owned(),
    }];
    assert_eq!(derive(&raw, &edits).explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(
        derive(&raw, &edits).explain("bank:pay"),
        Some(Why::Transfer(0))
    );

    edits.overrides = vec![over("card:1", "expenses:treats")];
    assert_eq!(
        derive(&raw, &edits).explain("card:1"),
        Some(Why::Override(0))
    );

    // An override takes the link's transaction, so the link takes neither
    // side rather than half-netting.
    edits.overrides.push(over("bank:pay", "expenses:oops"));
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("bank:pay"), Some(Why::Override(1)));
    assert_eq!(derived.explain("card:pay"), Some(Why::Rule(0)));
    assert_eq!(
        derived.report().rejected,
        vec![Rejected {
            edit: EditRef::Link(0),
            problem: Problem::Shadowed {
                by: EditRef::Override(1)
            },
        }]
    );

    // A split beats an override of the same transaction.
    edits.splits = vec![Split {
        record_id: "card:1".to_owned(),
        legs: vec![Leg {
            account: "expenses:coffee".to_owned(),
            qty: 450,
        }],
    }];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Split(0)));
    assert!(derived.report().rejected.contains(&Rejected {
        edit: EditRef::Override(0),
        problem: Problem::Shadowed {
            by: EditRef::Split(0)
        },
    }));
}

#[test]
fn the_earlier_of_two_overrides_wins() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.overrides = vec![over("card:1", "expenses:a"), over("card:1", "expenses:b")];
    let derived = derive(&raw, &edits);
    assert_eq!(legs_of(&derived, "card:1")[1], "expenses:a 450");
    assert_eq!(
        derived.report().rejected,
        vec![Rejected {
            edit: EditRef::Override(1),
            problem: Problem::Shadowed {
                by: EditRef::Override(0)
            },
        }]
    );
}

#[test]
fn a_split_must_sum_to_the_counter_leg() {
    let text = "2024-01-05 * Groceries\n  ; id: card:9\n  expenses:unknown  30.00 CAD\n  ; raw-merchant: CORNER GROCER\n  liabilities:card  -30.00 CAD\n  ; card: gold\n\n";
    let raw = ledger(vec![segment("card", 1, "cards", text)]);
    let mut edits = edits();
    let legs = |a: i64, b: i64| {
        vec![
            Leg {
                account: "expenses:food".to_owned(),
                qty: a,
            },
            Leg {
                account: "expenses:household".to_owned(),
                qty: b,
            },
        ]
    };
    edits.splits = vec![Split {
        record_id: "card:9".to_owned(),
        legs: legs(2000, 999),
    }];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:9"), Some(Why::Default));
    assert_eq!(
        derived.report().rejected,
        vec![Rejected {
            edit: EditRef::Split(0),
            problem: Problem::SplitUnbalanced {
                want: 3000,
                got: 2999
            },
        }]
    );

    edits.splits[0].legs = legs(2000, 1000);
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:9"), Some(Why::Split(0)));
    assert_eq!(
        legs_of(&derived, "card:9"),
        [
            "expenses:food 2000",
            "expenses:household 1000",
            "liabilities:card -3000"
        ]
    );
    // Notes stay on the posting they were written under: the counter leg's
    // go to its first leg, the card's stay on the card.
    let text = export(derived.ledger());
    let want = "\texpenses:food\t20.00 CAD\n\t; raw-merchant: CORNER GROCER\n\texpenses:household\t10.00 CAD\n\tliabilities:card\t-30.00 CAD\n\t; card: gold\n";
    assert!(text.contains(want), "{text}");
    // And the result is valid ledger text that balances.
    parse_ok(&text);

    edits.splits[0].legs.clear();
    let derived = derive(&raw, &edits);
    assert_eq!(derived.report().rejected[0].problem, Problem::SplitEmpty);
}

#[test]
fn an_elided_counter_leg_splits_into_explicit_amounts() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.splits = vec![Split {
        record_id: "card:1".to_owned(),
        legs: vec![
            Leg {
                account: "expenses:coffee".to_owned(),
                qty: 400,
            },
            Leg {
                account: "expenses:tips".to_owned(),
                qty: 50,
            },
        ],
    }];
    let text = export(derive(&raw, &edits).ledger());
    assert!(
        text.contains("\texpenses:coffee\t4.00 CAD\n\texpenses:tips\t0.50 CAD\n"),
        "{text}"
    );
    parse_ok(&text);
}

#[test]
fn a_transfer_link_nets_out() {
    let card = "2024-01-04 * Payment\n  ; id: card:pay\n  liabilities:card  100.00 CAD\n  income:unknown\n\n";
    let bank = "2024-01-04 * Card payment\n  ; id: bank:pay\n  assets:chequing  -100.00 CAD\n  expenses:unknown\n\n\
                2024-01-05 * Card payment\n  ; id: bank:pay2\n  assets:chequing  -99.00 CAD\n  expenses:unknown\n\n";
    let raw = ledger(vec![
        segment("card", 1, "cards", card),
        segment("bank", 1, "banks", bank),
    ]);
    assert_eq!(balance(&raw, "income:unknown"), -10000);
    assert_eq!(balance(&raw, "expenses:unknown"), 19900);

    let mut edits = edits();
    edits.links = vec![
        TransferLink {
            a: "card:pay".to_owned(),
            b: "bank:pay".to_owned(),
        },
        TransferLink {
            a: "card:pay".to_owned(),
            b: "bank:pay2".to_owned(),
        },
        TransferLink {
            a: "bank:pay2".to_owned(),
            b: "bank:pay2".to_owned(),
        },
    ];
    let derived = derive(&raw, &edits);
    let ledger = derived.ledger();
    assert_eq!(derived.explain("card:pay"), Some(Why::Transfer(0)));
    assert_eq!(derived.explain("bank:pay"), Some(Why::Transfer(0)));
    assert_eq!(balance(ledger, "transfers"), 0);
    assert_eq!(balance(ledger, "income:unknown"), 0);
    assert_eq!(balance(ledger, "expenses:unknown"), 9900);
    assert_eq!(
        derived.report().rejected,
        vec![
            Rejected {
                edit: EditRef::Link(1),
                problem: Problem::Shadowed {
                    by: EditRef::Link(0)
                },
            },
            Rejected {
                edit: EditRef::Link(2),
                problem: Problem::TransferSelf,
            },
        ]
    );

    // Legs that do not net are refused whole.
    edits.links = vec![TransferLink {
        a: "card:pay".to_owned(),
        b: "bank:pay2".to_owned(),
    }];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:pay"), Some(Why::Default));
    assert_eq!(
        derived.report().rejected[0].problem,
        Problem::TransferUnbalanced
    );
}

#[test]
fn a_transaction_without_one_counter_leg_passes_through() {
    let text = "2024-01-02 * Hand entered\n  ; id: hand:1\n  expenses:coffee  3.00 CAD\n  assets:cash\n\n\
                2024-01-02 * Two unknowns\n  ; id: hand:2\n  expenses:unknown  3.00 CAD\n  income:unknown\n\n";
    let raw = ledger(vec![segment("hand", 1, "manual", text)]);
    let mut edits = edits();
    edits.rules = vec![rule("expenses:misc", vec![])];
    edits.overrides = vec![over("hand:1", "expenses:other")];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("hand:1"), Some(Why::Untouched));
    assert_eq!(derived.explain("hand:2"), Some(Why::Untouched));
    assert_eq!(export(derived.ledger()), export(&raw));
    assert_eq!(
        derived.report().rejected,
        vec![Rejected {
            edit: EditRef::Override(0),
            problem: Problem::NoCounterLeg,
        }]
    );
}

#[test]
fn bad_accounts_and_unknown_ids_are_reported() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.rules = vec![rule("expenses::bad", vec![]), rule("expenses:ok", vec![])];
    edits.overrides = vec![over("card:404", "expenses:x")];
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(1)));
    assert_eq!(derived.report().rejected.len(), 1);
    assert_eq!(derived.report().rejected[0].edit, EditRef::Rule(0));
    assert!(matches!(
        derived.report().rejected[0].problem,
        Problem::BadAccount { .. }
    ));
    assert_eq!(derived.report().unmatched, vec![EditRef::Override(0)]);
    assert_eq!(derived.explain("card:404"), None);
}

#[test]
fn derive_is_deterministic_across_load_orders() {
    let bank = "2024-01-03 * Bean There Cafe\n  ; id: bank:1\n  ; raw-merchant: Bean There Cafe\n  assets:chequing  -2.00 CAD\n  expenses:unknown\n\n";
    let segments = || vec![card(), segment("bank", 1, "banks", bank)];
    let mut reversed = segments();
    reversed.reverse();
    let one = ledger(segments());
    let two = ledger(reversed);

    let mut edits = edits();
    edits.rules = vec![
        rule("expenses:food", vec![merchant_contains("grocer")]),
        rule("expenses:coffee", vec![merchant_contains("bean")]),
    ];
    edits.overrides = vec![over("bank:1", "expenses:treats")];
    edits.splits = vec![Split {
        record_id: "card:3".to_owned(),
        legs: vec![
            Leg {
                account: "expenses:coffee".to_owned(),
                qty: 600,
            },
            Leg {
                account: "expenses:tips".to_owned(),
                qty: 25,
            },
        ],
    }];
    let a = derive(&one, &edits);
    let b = derive(&two, &edits);
    assert_eq!(export(a.ledger()), export(b.ledger()));
    assert_eq!(export(a.ledger()), export(derive(&one, &edits).ledger()));
    for id in ["card:1", "card:2", "card:3", "bank:1"] {
        assert_eq!(a.explain(id), b.explain(id), "{id}");
    }
    assert_eq!(a.explain("card:2"), Some(Why::Rule(0)));
    assert_eq!(a.explain("card:3"), Some(Why::Split(0)));
    assert_eq!(a.explain("bank:1"), Some(Why::Override(0)));
    // The derived ledger's indexes follow its new postings.
    assert_eq!(balance(a.ledger(), "expenses:tips"), 25);
    assert_eq!(
        a.ledger().txns().len(),
        one.txns().len(),
        "same transactions"
    );
}

#[test]
fn rule_order_holds_across_indexed_and_unindexed_rules() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    let over = |min| Cond::Amount {
        commodity: None,
        min,
        max: i64::MAX,
    };
    edits.rules = vec![
        // Indexed on its needle, but its amount test fails for card:1.
        rule("expenses:a", vec![merchant_contains("bean"), over(500)]),
        // No needle: tried for every transaction.
        rule("expenses:b", vec![over(1000)]),
        // The same needle as the first, later.
        rule("expenses:c", vec![merchant_contains("BEAN")]),
        rule("expenses:d", vec![]),
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(
        derived.explain("card:1"),
        Some(Why::Rule(2)),
        "450: a fails"
    );
    assert_eq!(derived.explain("card:2"), Some(Why::Rule(1)), "no needle");
    assert_eq!(derived.explain("card:3"), Some(Why::Rule(0)), "625");

    // An unindexed rule earlier than an indexed one wins.
    edits.rules.swap(0, 3);
    let derived = derive(&raw, &edits);
    for id in ["card:1", "card:2", "card:3"] {
        assert_eq!(derived.explain(id), Some(Why::Rule(0)), "{id}");
    }

    // An empty needle is in every value of its field, and only there.
    edits.rules = vec![rule("expenses:any", vec![merchant_contains("")])];
    let text = "2024-01-02 * No merchant\n  ; id: card:7\n  liabilities:card  -1.00 CAD\n  expenses:unknown\n\n";
    let raw = ledger(vec![card(), segment("card", 2, "cards", text)]);
    let derived = derive(&raw, &edits);
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("card:7"), Some(Why::Default));
}

fn alias(from: &str, to: &str) -> Alias {
    Alias {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

/// Aliases rename what rules, overrides and splits decided, and every other
/// posting too, without touching why each transaction was decided.
#[test]
fn aliases_rename_after_every_other_edit() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.rules = vec![rule(
        "expenses:food:coffee",
        vec![merchant_contains("bean")],
    )];
    edits.overrides = vec![over("card:2", "expenses:food")];
    edits.splits = vec![Split {
        record_id: "card:3".to_owned(),
        legs: vec![
            Leg {
                account: "expenses:food:coffee".to_owned(),
                qty: 500,
            },
            Leg {
                account: "expenses:gifts".to_owned(),
                qty: 125,
            },
        ],
    }];
    edits.aliases = vec![
        alias("expenses:food", "expenses:eating"),
        alias("liabilities:card", "liabilities:amex"),
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(
        legs_of(&derived, "card:1"),
        ["liabilities:amex -450", "expenses:eating:coffee 450"]
    );
    assert_eq!(
        legs_of(&derived, "card:2"),
        ["liabilities:amex -3000", "expenses:eating 3000"]
    );
    assert_eq!(
        legs_of(&derived, "card:3"),
        [
            "liabilities:amex -625",
            "expenses:eating:coffee 500",
            "expenses:gifts 125"
        ]
    );
    assert_eq!(derived.explain("card:1"), Some(Why::Rule(0)));
    assert_eq!(derived.explain("card:2"), Some(Why::Override(0)));
    assert_eq!(derived.explain("card:3"), Some(Why::Split(0)));
    assert_eq!(balance(derived.ledger(), "expenses:food"), 0);
    assert_eq!(balance(derived.ledger(), "expenses:food:coffee"), 0);
    assert_eq!(derived.report(), &Default::default());
}

/// The nearest alias wins, a renamed account isn't renamed again, and moving
/// a subtree under itself leaves what's already under the target alone.
#[test]
fn the_nearest_alias_wins_once() {
    let text = "2024-01-02 * a\n  ; id: t:1\n  assets:bank  -1.00 CAD\n  expenses:food:coffee:beans\n\n\
                2024-01-02 * b\n  ; id: t:2\n  assets:bank  -2.00 CAD\n  expenses:food:produce\n\n\
                2024-01-02 * c\n  ; id: t:3\n  assets:bank  -3.00 CAD\n  expenses:food:groceries:milk\n\n\
                2024-01-02 * d\n  ; id: t:4\n  assets:bank  -4.00 CAD\n  expenses:fun\n\n";
    let raw = ledger(vec![segment("t", 1, "bank", text)]);
    let mut edits = edits();
    edits.aliases = vec![
        alias("expenses:food", "expenses:food:groceries"),
        alias("expenses:food:coffee", "expenses:fun:cafe"),
        // `expenses:fun` is renamed, but coffee's new name isn't renamed
        // again by it.
        alias("expenses:fun", "expenses:play"),
    ];
    let derived = derive(&raw, &edits);
    assert_eq!(legs_of(&derived, "t:1")[1], "expenses:fun:cafe:beans 100");
    assert_eq!(
        legs_of(&derived, "t:2")[1],
        "expenses:food:groceries:produce 200"
    );
    assert_eq!(
        legs_of(&derived, "t:3")[1],
        "expenses:food:groceries:milk 300"
    );
    assert_eq!(legs_of(&derived, "t:4")[1], "expenses:play 400");
    assert_eq!(derived.report(), &Default::default());
}

/// A bad name, a no-op alias and a second alias of one `from` are reported
/// and left out; an alias of an account nobody posts to does nothing.
#[test]
fn bad_aliases_are_rejected() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.aliases = vec![
        alias("expenses:unknown", "expenses::bad"),
        alias("expenses:unknown", "expenses:unknown"),
        alias("expenses:unknown", "expenses:triage"),
        alias("expenses:unknown", "expenses:later"),
        alias("expenses:nobody", "expenses:somebody"),
    ];
    let derived = derive(&raw, &edits);
    let rejected: Vec<(EditRef, Problem)> = derived
        .report()
        .rejected
        .iter()
        .map(|r| (r.edit, r.problem.clone()))
        .collect();
    assert!(matches!(
        rejected[0],
        (EditRef::Alias(0), Problem::BadAccount { .. })
    ));
    assert_eq!(rejected[1], (EditRef::Alias(1), Problem::AliasSelf));
    assert_eq!(
        rejected[2],
        (
            EditRef::Alias(3),
            Problem::Shadowed {
                by: EditRef::Alias(2)
            }
        )
    );
    assert_eq!(rejected.len(), 3);
    assert_eq!(legs_of(&derived, "card:1")[1], "expenses:triage 450");
    assert!(
        derived
            .ledger()
            .journal()
            .accounts
            .lookup("expenses:somebody")
            .is_none()
    );
}

/// `rederive` from `previous` gives exactly what a full `derive` does, and
/// says whether it reused the rule matches.
fn rederive_matches_derive(raw: &Ledger, edits: &Edits, previous: &Derived) -> Derived {
    let full = derive(raw, edits);
    let again = rederive(raw, edits, previous);
    assert_eq!(export(again.ledger()), export(full.ledger()));
    for at in 0..raw.txns().len() as u32 {
        let txn = ledgerdb::TxnId(at);
        assert_eq!(again.why(txn), full.why(txn), "why of txn {at}");
    }
    assert_eq!(again.report(), full.report());
    again
}

#[test]
fn rederive_reuses_rule_matches_when_only_per_transaction_edits_change() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.rules = vec![rule("expenses:coffee", vec![merchant_contains("bean")])];
    let first = derive(&raw, &edits);
    assert!(!first.reused_rule_matches());

    // An override on a rule-matched transaction, then on an unmatched one.
    edits.overrides = vec![
        over("card:1", "expenses:treats"),
        over("card:2", "expenses:food"),
    ];
    let second = rederive_matches_derive(&raw, &edits, &first);
    assert!(second.reused_rule_matches());
    assert_eq!(second.explain("card:1"), Some(Why::Override(0)));

    // Dropping the override hands card:1 back to its rule, from the memo.
    edits.overrides = vec![over("card:2", "expenses:food")];
    let third = rederive_matches_derive(&raw, &edits, &second);
    assert!(third.reused_rule_matches());
    assert_eq!(third.explain("card:1"), Some(Why::Rule(0)));

    // A split and an alias reuse them too.
    edits.splits = vec![Split {
        record_id: "card:3".to_owned(),
        legs: vec![
            Leg {
                account: "expenses:coffee".to_owned(),
                qty: 425,
            },
            Leg {
                account: "expenses:treats".to_owned(),
                qty: 200,
            },
        ],
    }];
    edits.aliases = vec![alias("expenses:coffee", "expenses:food:coffee")];
    let fourth = rederive_matches_derive(&raw, &edits, &third);
    assert!(fourth.reused_rule_matches());
    assert_eq!(fourth.explain("card:3"), Some(Why::Split(0)));
}

#[test]
fn rederive_matches_again_when_rules_defaults_or_the_ledger_change() {
    let raw = ledger(vec![card()]);
    let mut edits = edits();
    edits.rules = vec![rule("expenses:coffee", vec![merchant_contains("bean")])];
    let first = derive(&raw, &edits);

    edits
        .rules
        .push(rule("expenses:food", vec![merchant_contains("grocer")]));
    let ruled = rederive_matches_derive(&raw, &edits, &first);
    assert!(!ruled.reused_rule_matches());
    assert_eq!(ruled.explain("card:2"), Some(Why::Rule(1)));

    edits.defaults.pop();
    let defaulted = rederive_matches_derive(&raw, &edits, &ruled);
    assert!(!defaulted.reused_rule_matches());

    let more = ledger(vec![
        card(),
        segment(
            "card",
            101,
            "cards",
            &purchase("card:4", "Bean There Cafe", "3.00"),
        ),
    ]);
    let grown = rederive_matches_derive(&more, &edits, &defaulted);
    assert!(!grown.reused_rule_matches());
    assert_eq!(grown.explain("card:4"), Some(Why::Rule(0)));
}
