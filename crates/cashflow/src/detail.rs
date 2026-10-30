//! One record's facts and why: what the transaction detail view and
//! `cashflow show <record-id>` print.
//!
//! [`of`] reads one transaction out of the books by its record id (`; id:`)
//! and owns everything it shows, so a caller keeps it across frames and
//! rebuilds it only when the books or the record change; building one costs
//! microseconds.
//!
//! - **The facts** are the derived ledger's: date, payee, every posting with
//!   its amount converted into the home commodity, the transaction's
//!   metadata (the importers' `raw-*` among it) and comments, and the
//!   segment it was loaded from.
//! - **The why** is the [`Decision`]: which tier of derive decided the
//!   counter leg, enriched from the [`DeriveInputs`] the books were derived
//!   with (the rule's patterns, the override's account, the split's legs,
//!   the transfer's other side) and from their [`crate::edits_cache::Provenance`],
//!   which says when the deciding note was written and by which agentium
//!   session.
//! - **What didn't apply** is [`TxnDetail::rejected`]: this record's edits
//!   that derive refused, worded so a person can see why.
//!
//! Each posting also says which project it is in and which account puts it
//! there ([`ProjectFrom`]), since projects are set per account (see
//! [`crate::projects`]).
//!
//! No egui, and keyed by record id rather than [`TxnId`], which a rescan or a
//! layer toggle renumbers.

use ledgerdb::{
    Amount, Commodities, CommodityId, Cond, Date, EditRef, Field, Journal, Posting, Problem,
    Rejected, SegmentHash, TxnFlag, TxnId, Why,
};

use crate::projects::Projects;
use crate::transfers;
use crate::{Books, DeriveInputs};

/// One transaction's facts and why, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxnDetail {
    /// Its record id, the `; id:` it was found by.
    pub record: String,
    /// Its date.
    pub date: Date,
    /// Its payee or description.
    pub payee: String,
    /// The `(code)` after the mark, if any.
    pub code: Option<String>,
    /// Its clearing mark.
    pub flag: TxnFlag,
    /// The source its segment was imported from.
    pub source: String,
    /// The layer its segment is in.
    pub layer: String,
    /// The segment it was loaded from.
    pub segment: SegmentHash,
    /// When that segment was imported, in unix seconds.
    pub imported: u64,
    /// Its postings in the derived ledger, in the order written.
    pub postings: Vec<PostingLine>,
    /// The transaction's own metadata, in the order written, less its `id`.
    pub meta: Vec<MetaLine>,
    /// Its plain comments, its own and its postings', in the order written.
    pub comments: Vec<String>,
    /// What decided its counter leg.
    pub decided: Decision,
    /// This record's edits that derive refused, each worded with why.
    pub rejected: Vec<String>,
}

/// One posting of a [`TxnDetail`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingLine {
    /// The account's full name in the derived ledger.
    pub account: String,
    /// The amount, as written.
    pub amount: Amount,
    /// The amount in the books' home commodity on the transaction's date;
    /// `None` without a home commodity or a rate to it.
    pub home: Option<Amount>,
    /// Whether the account is the owner's own ([`transfers::OWN_ROOTS`]).
    pub own: bool,
    /// The project the account is in, if any.
    pub project: Option<ProjectFrom>,
}

/// Which project a posting's account is in, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFrom {
    /// The project.
    pub project: String,
    /// The account that carries the project: the posting's own, or its
    /// nearest ancestor that sets one.
    pub root: String,
}

/// One `; key: value` metadata entry of a [`TxnDetail`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaLine {
    /// The key, such as `raw-merchant`.
    pub key: String,
    /// The value; empty for a bare `; key:` tag.
    pub value: String,
}

/// What decided a transaction's counter leg, with the edit that did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// It has no counter leg: as the importer wrote it.
    AsImported,
    /// Nothing: it sits on the importer's default account.
    Unknown {
        /// That account.
        default: String,
    },
    /// The first rule to match it.
    Rule {
        /// The vault rule's id; `None` for one from a rules file.
        id: Option<String>,
        /// Where it sends the counter leg.
        account: String,
        /// The payee substrings the rule matches on, every one of them.
        patterns: Vec<String>,
        /// When the vault rule was written; `None` for a rules file's.
        written: Option<Written>,
    },
    /// An override on this record.
    Override {
        /// Where it sends the counter leg.
        account: String,
        /// When it was written.
        written: Option<Written>,
    },
    /// A split of this record.
    Split {
        /// The legs that replace the counter leg, in order.
        legs: Vec<SplitLine>,
        /// When it was written.
        written: Option<Written>,
    },
    /// A transfer link to another record.
    Transfer {
        /// The other side's record id.
        other: String,
        /// When it was written.
        written: Option<Written>,
    },
}

impl Decision {
    /// When the vault note behind it was written and by whom, if one is.
    pub fn written(&self) -> Option<&Written> {
        match self {
            Decision::AsImported | Decision::Unknown { .. } => None,
            Decision::Rule { written, .. }
            | Decision::Override { written, .. }
            | Decision::Split { written, .. }
            | Decision::Transfer { written, .. } => written.as_ref(),
        }
    }
}

/// One leg of a [`Decision::Split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitLine {
    /// Where this part goes.
    pub account: String,
    /// How much, in the counter leg's commodity.
    pub amount: Amount,
}

/// When the vault note that decided a transaction was written, and by
/// which session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// Its `created_at`, in unix seconds.
    pub at: u64,
    /// The `agentium:<word-id>` session that wrote it, if one did.
    pub agentium: Option<String>,
}

/// The detail of the transaction with record id `record` in `books`,
/// derived with `inputs`, its accounts placed in `projects`; `None` when the
/// loaded books don't hold it (its layer may be off, or its segment not
/// synced here).
#[profiling::function]
pub fn of(
    books: &Books,
    inputs: &DeriveInputs,
    projects: &Projects,
    record: &str,
) -> Option<TxnDetail> {
    let ledger = books.ledger();
    let txn = ledger.find(record)?;
    let journal = ledger.journal();
    let row = journal.txn(txn);
    let segment = ledger.segment_of(txn);
    let accounts = &journal.accounts;
    let home = books.home();
    let postings = journal
        .postings_of(row)
        .iter()
        .map(|p| {
            let account = accounts.full_name(p.account);
            PostingLine {
                account: account.to_owned(),
                amount: p.amount(),
                home: home.and_then(|home| books.prices.convert(p.amount(), home, row.date)),
                own: transfers::is_own(account),
                project: projects.root_of(account).map(|root| ProjectFrom {
                    project: root.project.clone(),
                    root: root.account.clone(),
                }),
            }
        })
        .collect();
    let meta = journal
        .meta_of(row)
        .iter()
        .filter(|m| m.posting.is_none())
        .map(|m| MetaLine {
            key: journal.strings.get(m.key).to_owned(),
            value: journal.strings.get(m.value).to_owned(),
        })
        .filter(|m| m.key != "id")
        .collect();
    let counter = Counter::of(journal, txn, inputs);
    Some(TxnDetail {
        record: record.to_owned(),
        date: row.date,
        payee: journal.strings.get(row.payee).to_owned(),
        code: row.code.map(|c| journal.strings.get(c).to_owned()),
        flag: row.flag,
        source: segment.header.source.clone(),
        layer: segment.header.layer.clone(),
        segment: segment.hash,
        imported: segment.header.imported,
        postings,
        meta,
        comments: journal
            .comments_of(row)
            .iter()
            .map(|c| journal.strings.get(c.text).to_owned())
            .collect(),
        decided: decision(books, inputs, txn, record, &counter),
        rejected: books
            .derived
            .report()
            .rejected
            .iter()
            .filter_map(|r| refusal(inputs, record, r, &counter, &journal.commodities))
            .collect(),
    })
}

/// The counter leg as derive left it: the posting on an importer's default
/// account, else the first that isn't the owner's own.
struct Counter {
    /// Its account's full name; empty when every posting is the owner's.
    account: String,
    /// Its commodity: what a split's legs and an unbalanced split's sums are
    /// in.
    commodity: Option<CommodityId>,
}

impl Counter {
    /// The counter leg of transaction `txn` of `journal`, derived with
    /// `inputs`.
    fn of(journal: &Journal, txn: TxnId, inputs: &DeriveInputs) -> Counter {
        let postings = journal.postings_of(journal.txn(txn));
        let name = |p: &Posting| journal.accounts.full_name(p.account);
        let found = postings
            .iter()
            .find(|p| inputs.edits.defaults.iter().any(|d| d == name(p)))
            .or_else(|| postings.iter().find(|p| !transfers::is_own(name(p))));
        Counter {
            account: found.map_or_else(String::new, |p| name(p).to_owned()),
            commodity: found.or(postings.first()).map(|p| p.commodity),
        }
    }

    /// `qty` of the counter leg's commodity; `None` when it has none.
    fn amount(&self, qty: i64) -> Option<Amount> {
        self.commodity.map(|commodity| Amount { qty, commodity })
    }
}

/// The [`Decision`] behind transaction `txn` (record `record`).
fn decision(
    books: &Books,
    inputs: &DeriveInputs,
    txn: TxnId,
    record: &str,
    counter: &Counter,
) -> Decision {
    let why = books.derived.why(txn);
    let edits = &inputs.edits;
    let written = inputs.stamp(why).map(|s| Written {
        at: s.created_at,
        agentium: s.agentium.clone(),
    });
    match why {
        Why::Untouched => Decision::AsImported,
        Why::Default => Decision::Unknown {
            default: counter.account.clone(),
        },
        Why::Rule(i) => Decision::Rule {
            id: inputs.rule_id(why).map(str::to_owned),
            account: edits.rules[i as usize].account.clone(),
            patterns: rule_patterns(inputs, i as usize),
            written,
        },
        Why::Override(i) => Decision::Override {
            account: edits.overrides[i as usize].account.clone(),
            written,
        },
        Why::Split(i) => Decision::Split {
            legs: edits.splits[i as usize]
                .legs
                .iter()
                .filter_map(|leg| {
                    Some(SplitLine {
                        account: leg.account.clone(),
                        amount: counter.amount(leg.qty)?,
                    })
                })
                .collect(),
            written,
        },
        Why::Transfer(i) => {
            let link = &edits.links[i as usize];
            let other = if link.a == record { &link.b } else { &link.a };
            Decision::Transfer {
                other: other.clone(),
                written,
            }
        }
    }
}

/// The payee patterns of the rule `edits.rules[i]` came from: every
/// compiled rule of the same vault rule (one per pattern), or just its own
/// for a rules file's.
fn rule_patterns(inputs: &DeriveInputs, i: usize) -> Vec<String> {
    let rules = &inputs.edits.rules;
    let owner = inputs.owners.get(i).map(|o| o.rule);
    let same_rule = |j: &usize| match owner {
        Some(rule) => inputs.owners.get(*j).is_some_and(|o| o.rule == rule),
        None => *j == i,
    };
    (0..rules.len())
        .filter(same_rule)
        .flat_map(|j| &rules[j].conditions)
        .filter_map(|cond| match cond {
            Cond::Contains {
                field: Field::Payee,
                needle,
            } => Some(needle.clone()),
            _ => None,
        })
        .collect()
}

/// `rejected`, worded for a person, when it is an edit of `record`.
fn refusal(
    inputs: &DeriveInputs,
    record: &str,
    rejected: &Rejected,
    counter: &Counter,
    commodities: &Commodities,
) -> Option<String> {
    let edits = &inputs.edits;
    let what = match rejected.edit {
        EditRef::Override(i) => {
            let edit = edits.overrides.get(i as usize)?;
            if edit.record_id != record {
                return None;
            }
            format!("override to {}", edit.account)
        }
        EditRef::Split(i) => {
            if edits.splits.get(i as usize)?.record_id != record {
                return None;
            }
            "split".to_owned()
        }
        EditRef::Link(i) => {
            let link = edits.links.get(i as usize)?;
            let other = if link.a == record {
                &link.b
            } else if link.b == record {
                &link.a
            } else {
                return None;
            };
            format!("transfer link with {other}")
        }
        EditRef::Rule(_) | EditRef::Alias(_) => return None,
    };
    let why = problem(&rejected.problem, counter, commodities);
    Some(format!("{what} not applied: {why}"))
}

/// Why derive refused an edit, worded for a person.
fn problem(problem: &Problem, counter: &Counter, commodities: &Commodities) -> String {
    match problem {
        Problem::BadAccount { account, error } => {
            format!("'{account}' isn't a valid account: {error}")
        }
        Problem::NoCounterLeg => "it has no counter leg to move".to_owned(),
        Problem::Shadowed { by } => format!("{} decides it first", edit_name(*by)),
        Problem::SplitEmpty => "the split has no legs".to_owned(),
        Problem::SplitUnbalanced { want, got } => {
            let show = |qty: i128| match (counter.commodity, i64::try_from(qty)) {
                (Some(commodity), Ok(qty)) => {
                    Amount { qty, commodity }.display(commodities).to_string()
                }
                _ => qty.to_string(),
            };
            format!(
                "split doesn't add up: want {}, got {}",
                show(i128::from(*want)),
                show(*got)
            )
        }
        Problem::SplitCost => {
            "the counter leg carries a cost, which a split can't share out".to_owned()
        }
        Problem::TransferSelf => "it links the transaction to itself".to_owned(),
        Problem::TransferUnbalanced => {
            "the two sides aren't one commodity netting to zero".to_owned()
        }
        Problem::AliasSelf => "an alias renames an account to itself".to_owned(),
    }
}

/// The kind of edit `edit` is, as a person names it.
fn edit_name(edit: EditRef) -> &'static str {
    match edit {
        EditRef::Rule(_) => "a rule",
        EditRef::Override(_) => "an override",
        EditRef::Split(_) => "a split",
        EditRef::Link(_) => "a transfer link",
        EditRef::Alias(_) => "an alias",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{Edits, Leg, Override, Split, TransferLink};

    use crate::edits_cache::Stamp;
    use crate::projects::ProjectRoot;
    use crate::test_util::{TestSegment, books_of, books_with};
    use crate::vault_rules::Owner;

    /// A grocer, a card payment's two sides, a USD charge, and a coffee.
    const TEXT: &str = "\
2024-03-01 * (A1) Corner Grocer
    ; raw-merchant: corner grocer
    ; raw-city: toronto
    ; just a comment
    liabilities:card  -4.50 CAD
    expenses:unknown

2024-03-02 * Card payment
    assets:checking  -500.00 CAD
    expenses:unknown

2024-03-03 * Payment received
    liabilities:card  500.00 CAD
    income:unknown

2024-03-04 * US Store
    liabilities:card  -10.00 USD
    expenses:unknown

2024-03-05 * Coffee
    liabilities:card  -6.00 CAD
    expenses:unknown

2024-03-06 * Opening
    assets:checking  100.00 CAD
    equity:opening  -100.00 CAD
";

    fn edits(f: impl FnOnce(&mut Edits)) -> Edits {
        let mut edits = crate::rules::default_edits();
        f(&mut edits);
        edits
    }

    fn rule(account: &str, needle: &str) -> ledgerdb::Rule {
        ledgerdb::Rule {
            account: account.to_owned(),
            conditions: vec![ledgerdb::Cond::Contains {
                field: ledgerdb::Field::Payee,
                needle: needle.to_owned(),
            }],
            layer: None,
        }
    }

    fn stamp(id: &str, at: u64, agentium: Option<&str>) -> Stamp {
        Stamp {
            id: id.to_owned(),
            created_at: at,
            agentium: agentium.map(str::to_owned),
        }
    }

    fn detail(edits: Edits, inputs: impl FnOnce(&mut DeriveInputs), record: &str) -> TxnDetail {
        let books = books_with(TEXT, &edits);
        let mut derive = DeriveInputs::from_edits(edits);
        inputs(&mut derive);
        of(&books, &derive, &Projects::default(), record).unwrap()
    }

    /// The facts: header, segment, postings, raw metadata and comments.
    #[test]
    fn the_facts_of_one_record() {
        let d = detail(edits(|_| {}), |_| {}, "t:1");
        assert_eq!(d.record, "t:1");
        assert_eq!(d.date, Date::parse("2024-03-01").unwrap());
        assert_eq!(d.payee, "Corner Grocer");
        assert_eq!(d.code.as_deref(), Some("A1"));
        assert_eq!(d.flag, TxnFlag::Cleared);
        assert_eq!((d.source.as_str(), d.layer.as_str()), ("t", "t"));
        assert_eq!(d.imported, 1);
        let accounts: Vec<&str> = d.postings.iter().map(|p| p.account.as_str()).collect();
        assert_eq!(accounts, ["liabilities:card", "expenses:unknown"]);
        assert_eq!(d.postings[0].amount.qty, -450);
        assert!(d.postings[0].own && !d.postings[1].own);
        let meta: Vec<(&str, &str)> = d
            .meta
            .iter()
            .map(|m| (m.key.as_str(), m.value.as_str()))
            .collect();
        assert_eq!(
            meta,
            [("raw-merchant", "corner grocer"), ("raw-city", "toronto")]
        );
        assert_eq!(d.comments, ["just a comment"]);
        assert!(d.rejected.is_empty());
    }

    /// A record the books don't hold has no detail.
    #[test]
    fn an_unknown_record_is_none() {
        let books = books_with(TEXT, &edits(|_| {}));
        let inputs = DeriveInputs::from_edits(edits(|_| {}));
        assert_eq!(of(&books, &inputs, &Projects::default(), "t:99"), None);
    }

    /// The home amount is the posting's own in home, converted with a
    /// price, and `None` without one.
    #[test]
    fn home_amounts_convert_when_a_rate_exists() {
        let d = detail(edits(|_| {}), |_| {}, "t:1");
        assert_eq!(d.postings[0].home, Some(d.postings[0].amount));

        let d = detail(edits(|_| {}), |_| {}, "t:4");
        assert_eq!(d.postings[0].home, None, "no USD→CAD price");

        // The fixture numbers the `P` line too, so the US store is t:5 here.
        let priced = format!("P 2024-03-01 USD 1.35 CAD\n\n{TEXT}");
        let books = books_with(&priced, &edits(|_| {}));
        let inputs = DeriveInputs::from_edits(edits(|_| {}));
        let d = of(&books, &inputs, &Projects::default(), "t:5").unwrap();
        assert_eq!(d.payee, "US Store");
        let home = d.postings[0].home.unwrap();
        assert_eq!(home.qty, -1350);
        assert_eq!(Some(home.commodity), books.home());
    }

    /// The unknown tier names the default account; a transaction with no
    /// counter leg is as imported.
    #[test]
    fn unknown_and_as_imported() {
        let d = detail(edits(|_| {}), |_| {}, "t:1");
        assert_eq!(
            d.decided,
            Decision::Unknown {
                default: "expenses:unknown".to_owned()
            }
        );
        let d = detail(edits(|_| {}), |_| {}, "t:6");
        assert_eq!(d.decided, Decision::AsImported);
    }

    /// A vault rule names its id, every pattern and when it was written; a
    /// rules file's has neither id nor time.
    #[test]
    fn rules_from_the_vault_and_a_file() {
        let vault = edits(|e| {
            e.rules.push(rule("expenses:food", "grocer"));
            e.rules.push(rule("expenses:food", "market"));
        });
        let d = detail(
            vault,
            |i| {
                i.owners = vec![
                    Owner {
                        rule: 0,
                        pattern: Some(0),
                    },
                    Owner {
                        rule: 0,
                        pattern: Some(1),
                    },
                ];
                i.provenance.rules = vec![stamp("oak-flat-lamp", 7, Some("agentium:a-b-c"))];
            },
            "t:1",
        );
        assert_eq!(
            d.decided,
            Decision::Rule {
                id: Some("oak-flat-lamp".to_owned()),
                account: "expenses:food".to_owned(),
                patterns: vec!["grocer".to_owned(), "market".to_owned()],
                written: Some(Written {
                    at: 7,
                    agentium: Some("agentium:a-b-c".to_owned())
                }),
            }
        );

        let file = edits(|e| e.rules.push(rule("expenses:food", "grocer")));
        let d = detail(file, |_| {}, "t:1");
        assert_eq!(
            d.decided,
            Decision::Rule {
                id: None,
                account: "expenses:food".to_owned(),
                patterns: vec!["grocer".to_owned()],
                written: None,
            }
        );
    }

    /// An override, a split's legs and a transfer's other side, each with
    /// its stamp.
    #[test]
    fn overrides_splits_and_transfers() {
        let all = edits(|e| {
            e.overrides.push(Override {
                record_id: "t:1".to_owned(),
                account: "expenses:home".to_owned(),
            });
            e.splits.push(Split {
                record_id: "t:5".to_owned(),
                legs: vec![
                    Leg {
                        account: "expenses:food".to_owned(),
                        qty: 400,
                    },
                    Leg {
                        account: "expenses:gifts".to_owned(),
                        qty: 200,
                    },
                ],
            });
            e.links.push(TransferLink {
                a: "t:2".to_owned(),
                b: "t:3".to_owned(),
            });
        });
        let provenance = |i: &mut DeriveInputs| {
            i.provenance.overrides = vec![stamp("t:1", 10, None)];
            i.provenance.splits = vec![stamp("t:5", 11, Some("agentium:x-y-z"))];
            i.provenance.links = vec![stamp("t:2|t:3", 12, None)];
        };

        let d = detail(all.clone(), provenance, "t:1");
        assert_eq!(
            d.decided,
            Decision::Override {
                account: "expenses:home".to_owned(),
                written: Some(Written {
                    at: 10,
                    agentium: None
                }),
            }
        );

        let d = detail(all.clone(), provenance, "t:5");
        let Decision::Split { legs, written } = &d.decided else {
            panic!("{:?}", d.decided);
        };
        let legs: Vec<(&str, i64)> = legs
            .iter()
            .map(|l| (l.account.as_str(), l.amount.qty))
            .collect();
        assert_eq!(legs, [("expenses:food", 400), ("expenses:gifts", 200)]);
        assert_eq!(
            written.as_ref().unwrap().agentium.as_deref(),
            Some("agentium:x-y-z")
        );

        for (record, other) in [("t:2", "t:3"), ("t:3", "t:2")] {
            let d = detail(all.clone(), provenance, record);
            assert_eq!(
                d.decided,
                Decision::Transfer {
                    other: other.to_owned(),
                    written: Some(Written {
                        at: 12,
                        agentium: None
                    }),
                }
            );
        }
    }

    /// A split that doesn't sum to the counter leg is refused, and the
    /// detail says by how much.
    #[test]
    fn a_rejected_split_says_why() {
        let bad = edits(|e| {
            e.splits.push(Split {
                record_id: "t:5".to_owned(),
                legs: vec![Leg {
                    account: "expenses:food".to_owned(),
                    qty: 400,
                }],
            })
        });
        let d = detail(bad, |_| {}, "t:5");
        assert!(matches!(d.decided, Decision::Unknown { .. }));
        assert_eq!(
            d.rejected,
            ["split not applied: split doesn't add up: want 6.00 CAD, got 4.00 CAD"]
        );
        let other = detail(edits(|_| {}), |_| {}, "t:1");
        assert!(other.rejected.is_empty());
    }

    /// A posting's project comes from its nearest ancestor that sets one.
    #[test]
    fn a_project_is_inherited_from_an_ancestor() {
        let books = books_of(
            &[TestSegment {
                source: "t",
                imported: 1,
                text: "2024-03-01 * Zap\n    assets:wallet  21.00 CAD\n    income:damus:zaps\n",
            }],
            &edits(|_| {}),
        );
        let projects = Projects::new([ProjectRoot {
            account: "income:damus".to_owned(),
            project: "damus".to_owned(),
        }]);
        let inputs = DeriveInputs::from_edits(edits(|_| {}));
        let d = of(&books, &inputs, &projects, "t:1").unwrap();
        assert_eq!(d.postings[0].project, None);
        assert_eq!(
            d.postings[1].project,
            Some(ProjectFrom {
                project: "damus".to_owned(),
                root: "income:damus".to_owned(),
            })
        );
    }

    /// An override written from an agentium session carries its ref
    /// through the vault into the detail, and the ref alone never makes
    /// two folds differ.
    #[test]
    fn agentium_round_trips_from_the_vault() {
        use nostrdb::{Filter, Transaction};

        use crate::Vault;
        use crate::kinds;
        use crate::test_util::{INGEST_TIMEOUT, TempNdb};
        use crate::vault_edits::{OverrideBody, OverrideNote};

        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&[31u8; 32]).unwrap();
        vault.register(ndb);
        let landed = ndb
            .subscribe(&[Filter::new().kinds(kinds::ALL.map(u64::from)).build()])
            .unwrap();
        let note = OverrideNote::new(
            OverrideBody {
                account: "expenses:home",
            },
            Some("agentium:oak-flat-lamp"),
        );
        let written = vault.write(ndb, "t:1", &note, 0).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(ndb.wait_for_all_notes_within(landed, 1, INGEST_TIMEOUT))
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let inputs = DeriveInputs::load(ndb, &txn, vault.author());
        let books = books_with(TEXT, &inputs.edits);
        let d = of(&books, &inputs, &Projects::default(), "t:1").unwrap();
        assert_eq!(
            d.decided,
            Decision::Override {
                account: "expenses:home".to_owned(),
                written: Some(Written {
                    at: written.created_at,
                    agentium: Some("agentium:oak-flat-lamp".to_owned()),
                }),
            }
        );

        let mut unsigned = inputs.clone();
        unsigned.provenance.overrides[0].agentium = None;
        unsigned.provenance.overrides[0].created_at += 1;
        assert_eq!(unsigned, inputs);
    }
}
