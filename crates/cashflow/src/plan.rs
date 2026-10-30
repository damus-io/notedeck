//! What a write would do to the books, worked out before it is made.
//!
//! [`plan`] takes one [`Action`] and the [`DeriveInputs`] the books were
//! derived with, and derives the raw ledger twice: with the edits as they
//! stand, and with the action applied to a copy of them. The difference is
//! a [`Plan`]: every transaction the action moves, before and after, and
//! how many rows a rule matches but leaves where they are.
//!
//! It makes the write rules' refusals first ([`crate::triage::refuse`],
//! read from the folded edits instead of the vault's notes), then the one
//! only a derive can make: an override, split or link that derive wouldn't
//! apply, because the transaction has no counter leg, a split's legs don't
//! sum, or a link's don't net. [`crate::triage::apply`] writes nothing a
//! plan wouldn't have refused, but it doesn't derive, so a caller that
//! wants that last check plans first. Derive takes about 90 ms on a large
//! journal, so the app plans on its job pool; the CLI plans every write,
//! and `--dry-run` stops at the plan.
//!
//! [`moves`] is the difference itself, for any two sets of edits: the CLI's
//! `rule mv`, `rule rm` and `rule import` reorder rules in ways no single
//! [`Action`] says, and diff with it directly.

use ledgerdb::{
    Derived, Edits, Ledger, Override, Split, TransferLink, TxnId, Why, derive, rederive,
};

use crate::DeriveInputs;
use crate::triage::{self, Action, WriteError};
use crate::vault_rules::{Compiled, RuleNote};

/// What a write would do: the transactions it moves, before and after.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Every transaction the write reaches: each record an override,
    /// split, uncategorize or link names (moved or not), or every row a
    /// rule takes.
    pub moves: Vec<Move>,
    /// For a rule, the rows its conditions match that keep how they are
    /// decided, because an override, split, link or earlier rule already
    /// decides them; 0 for any other write.
    pub unchanged: usize,
}

/// One transaction a write reaches, before and after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// The transaction, in the raw ledger (and either derived one).
    pub txn: TxnId,
    /// Its record id, if it has one.
    pub record: Option<String>,
    /// How it is decided now.
    pub before: Categorized,
    /// How it would be decided after the write.
    pub after: Categorized,
}

/// How one derive left one transaction: what decided it, and the legs it
/// decided, leaving out the source's own legs, which no edit moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Categorized {
    /// What decided it, indexing the edits its side was derived with.
    pub why: Why,
    /// The legs it decided: the counter leg, or a split's parts.
    pub legs: Vec<PlanLeg>,
}

/// One leg of a [`Categorized`] transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanLeg {
    /// Its account, as derived (after any alias).
    pub account: String,
    /// Its amount, written as the journal writes it.
    pub amount: String,
}

impl Categorized {
    /// Transaction `txn` of `derived`, whose raw form is in `raw` and whose
    /// default accounts are `edits.defaults`.
    pub fn of(raw: &Ledger, derived: &Derived, edits: &Edits, txn: TxnId) -> Self {
        let raw_journal = raw.journal();
        let own: Vec<&str> = raw_journal
            .postings_of(raw_journal.txn(txn))
            .iter()
            .map(|p| raw_journal.accounts.full_name(p.account))
            .filter(|name| !edits.defaults.iter().any(|d| d == name))
            .collect();
        let journal = derived.ledger().journal();
        let legs = journal
            .postings_of(journal.txn(txn))
            .iter()
            .filter(|p| !own.contains(&journal.accounts.full_name(p.account)))
            .map(|p| PlanLeg {
                account: journal.accounts.full_name(p.account).to_owned(),
                amount: p.amount().display(&journal.commodities).to_string(),
            })
            .collect();
        Categorized {
            why: derived.why(txn),
            legs,
        }
    }

    /// The account its first leg goes to: where a rule or the importer's
    /// default sent the counter leg. Empty with no legs.
    pub fn account(&self) -> &str {
        self.legs.first().map_or("", |l| l.account.as_str())
    }
}

/// The name derive's report, the CLI and the app give the tier `why`
/// names.
pub fn tier(why: Why) -> &'static str {
    match why {
        Why::Untouched => "untouched",
        Why::Default => "default",
        Why::Rule(_) => "rule",
        Why::Transfer(_) => "transfer",
        Why::Override(_) => "override",
        Why::Split(_) => "split",
    }
}

/// One side of a [`moves`] diff: the edits and their derive.
#[derive(Clone, Copy)]
pub struct Side<'a> {
    /// The edits.
    pub edits: &'a Edits,
    /// `raw` derived with them.
    pub derived: &'a Derived,
}

/// The transactions of `raw` that differ between `before` and `after`:
/// first each of `named`, in that order, whether it moved or not, then
/// every other one whose tier changed, or whose rule did, in ledger order.
///
/// Two rules are the same when `same_rule` says so of their indices into
/// each side's `edits.rules` (the [`Why::Rule`] values). An override,
/// split or link moving within its tier only moves the record it names,
/// so only `named` reports those.
pub fn moves(
    raw: &Ledger,
    before: Side,
    after: Side,
    same_rule: impl Fn(u32, u32) -> bool,
    named: &[TxnId],
) -> Vec<Move> {
    let journal = raw.journal();
    let reach = |txn: TxnId| Move {
        txn,
        record: journal
            .txn(txn)
            .id
            .map(|id| journal.strings.get(id).to_owned()),
        before: Categorized::of(raw, before.derived, before.edits, txn),
        after: Categorized::of(raw, after.derived, after.edits, txn),
    };
    let mut out: Vec<Move> = named.iter().map(|&txn| reach(txn)).collect();
    for i in 0..raw.txns().len() {
        let txn = TxnId(i as u32);
        let moved = match (before.derived.why(txn), after.derived.why(txn)) {
            (Why::Rule(a), Why::Rule(b)) => !same_rule(a, b),
            (a, b) => tier(a) != tier(b),
        };
        if moved && !named.contains(&txn) {
            out.push(reach(txn));
        }
    }
    out
}

/// What `action` would do to `raw`, derived with `inputs`: the refusal it
/// meets, or the [`Plan`]. Blocking: derive takes about 90 ms on a large
/// journal, so the app runs it on its job pool.
#[profiling::function]
pub fn plan(raw: &Ledger, inputs: &DeriveInputs, action: &Action) -> Result<Plan, WriteError> {
    let named = named(raw, action)?;
    triage::refuse(&inputs.edits, action)?;
    let mut after = inputs.edits.clone();
    let rules = match action {
        Action::Rule {
            pattern,
            source,
            account,
        } => {
            let note = RuleNote {
                patterns: vec![pattern.as_str().into()],
                source: source.as_deref(),
                ..RuleNote::new(account, "")
            };
            note.check()?;
            let mut compiled = Compiled::empty();
            note.push_rules(0, &mut compiled);
            after.rules.extend(compiled.edits.rules.iter().cloned());
            Some(compiled.edits.rules)
        }
        _ => {
            edit(&mut after, action);
            None
        }
    };
    let old = derive(raw, &inputs.edits);
    let new = rederive(raw, &after, &old);
    for &txn in &named {
        require_applied(raw, &new, txn, action)?;
    }
    let before = Side {
        edits: &inputs.edits,
        derived: &old,
    };
    let after = Side {
        edits: &after,
        derived: &new,
    };
    // Rules only append, so a rule index means the same rule both sides.
    let moves = moves(raw, before, after, |a, b| a == b, &named);
    let unchanged = rules.map_or(0, |rules| {
        let alone = Edits {
            defaults: inputs.edits.defaults.clone(),
            transfers: inputs.edits.transfers.clone(),
            rules,
            ..Edits::default()
        };
        matched(raw, &alone).saturating_sub(moves.len())
    });
    Ok(Plan { moves, unchanged })
}

/// The transactions `action` names, each of which must be in `raw` (an
/// unlink's sides needn't be: it clears the note either way).
fn named(raw: &Ledger, action: &Action) -> Result<Vec<TxnId>, WriteError> {
    let find = |record: &String| {
        raw.find(record)
            .ok_or_else(|| WriteError::NoRecord(record.clone()))
    };
    match action {
        Action::Override { record, .. }
        | Action::Split { record, .. }
        | Action::Uncategorize { record } => Ok(vec![find(record)?]),
        Action::Link { a, b } => Ok(vec![find(a)?, find(b)?]),
        Action::Unlink { a, b } => Ok([a, b].into_iter().filter_map(|r| raw.find(r)).collect()),
        Action::Rule { .. } | Action::Dismiss { .. } | Action::SetProject { .. } => Ok(Vec::new()),
    }
}

/// Apply `action`, a per-transaction write, to `edits` as its note would:
/// an override and a split replace each other, and an uncategorize clears
/// both. A dismissal and a project change no edit.
fn edit(edits: &mut Edits, action: &Action) {
    let uncategorize = |edits: &mut Edits, record: &str| {
        edits.overrides.retain(|o| o.record_id != record);
        edits.splits.retain(|s| s.record_id != record);
    };
    match action {
        Action::Override { record, account } => {
            uncategorize(edits, record);
            edits.overrides.push(Override {
                record_id: record.clone(),
                account: account.clone(),
            });
        }
        Action::Split { record, legs } => {
            uncategorize(edits, record);
            edits.splits.push(Split {
                record_id: record.clone(),
                legs: legs.clone(),
            });
        }
        Action::Uncategorize { record } => uncategorize(edits, record),
        Action::Link { a, b } => edits.links.push(TransferLink {
            a: a.clone(),
            b: b.clone(),
        }),
        Action::Unlink { a, b } => edits
            .links
            .retain(|l| !((l.a == *a && l.b == *b) || (l.a == *b && l.b == *a))),
        Action::Rule { .. } | Action::Dismiss { .. } | Action::SetProject { .. } => {}
    }
}

/// Fail unless derive applied `action`'s edit to `txn`, which the edits
/// after it were derived into `derived` with: an override, split or link
/// decides it. Otherwise say what derive did instead.
fn require_applied(
    raw: &Ledger,
    derived: &Derived,
    txn: TxnId,
    action: &Action,
) -> Result<(), WriteError> {
    let why = derived.why(txn);
    let what = match (action, why) {
        (Action::Override { .. }, Why::Override(_))
        | (Action::Split { .. }, Why::Split(_))
        | (Action::Link { .. }, Why::Transfer(_)) => return Ok(()),
        (Action::Override { .. }, _) => "override",
        (Action::Split { .. }, _) => "split",
        (Action::Link { .. }, _) => "link",
        _ => return Ok(()),
    };
    let problems: Vec<String> = derived
        .report()
        .rejected
        .iter()
        .map(|r| format!("{:?}", r.problem))
        .collect();
    let why = match why {
        Why::Untouched => {
            "it has no counter leg (no posting, or more than one, to a default account)".to_owned()
        }
        other if problems.is_empty() => format!("a {} decides it", tier(other)),
        _ => problems.join("; "),
    };
    let journal = raw.journal();
    let record = journal.txn(txn).id.map_or("", |id| journal.strings.get(id));
    Err(WriteError::NotApplied {
        what,
        record: record.to_owned(),
        why,
    })
}

/// How many transactions of `raw` a rule of `edits` decides.
fn matched(raw: &Ledger, edits: &Edits) -> usize {
    let derived = derive(raw, edits);
    (0..raw.txns().len())
        .filter(|&i| matches!(derived.why(TxnId(i as u32)), Why::Rule(_)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{JournalStore, LayerFilter, Leg, SegmentHeader};

    /// Three coffees on the card, a card payment from chequing, and a
    /// hand-entered transaction with no counter leg.
    const TEXT: &str = "\
2024-03-01 * Card payment
    ; id: bank:1
    assets:checking  -500.00 CAD
    expenses:unknown

2024-03-03 * Payment received
    ; id: card:1
    liabilities:card  500.00 CAD
    income:unknown

2024-03-04 * SQ *COFFEE SHOP #12
    ; id: card:2
    liabilities:card  -4.50 CAD
    expenses:unknown

2024-03-05 * SQ *COFFEE SHOP #9
    ; id: card:3
    liabilities:card  -5.00 CAD
    expenses:unknown

2024-03-06 * SQ *COFFEE SHOP #3
    ; id: card:4
    liabilities:card  -6.00 CAD
    expenses:unknown

2024-03-07 * Rent
    ; id: bank:2
    assets:checking  -1500.00 CAD
    expenses:rent
";

    /// The raw ledger of [`TEXT`], and the store it loaded from.
    struct Fixture {
        raw: Ledger,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path());
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        store
            .write_segment(&header, &ledgerdb::Journal::parse(TEXT).journal)
            .unwrap();
        let raw = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        Fixture { raw, _dir: dir }
    }

    /// The default edits with `edit` made to them.
    fn inputs(edit: impl FnOnce(&mut Edits)) -> DeriveInputs {
        let mut edits = crate::rules::default_edits();
        edit(&mut edits);
        DeriveInputs::from_edits(edits)
    }

    fn rule(pattern: &str, account: &str) -> Action {
        Action::Rule {
            pattern: pattern.to_owned(),
            source: None,
            account: account.to_owned(),
        }
    }

    fn over(record: &str, account: &str) -> Action {
        Action::Override {
            record: record.to_owned(),
            account: account.to_owned(),
        }
    }

    /// A rule plan lists the rows it takes; a row an override decides
    /// doesn't move, and counts as unchanged.
    #[test]
    fn a_rule_counts_what_it_moves() {
        let f = fixture();
        let inputs = inputs(|e| {
            e.overrides.push(Override {
                record_id: "card:3".to_owned(),
                account: "expenses:food".to_owned(),
            })
        });
        let plan = plan(&f.raw, &inputs, &rule("coffee shop", "expenses:coffee")).unwrap();
        let moved: Vec<_> = plan.moves.iter().map(|m| m.record.as_deref()).collect();
        assert_eq!(moved, [Some("card:2"), Some("card:4")]);
        assert_eq!(plan.unchanged, 1);
        let first = &plan.moves[0];
        assert_eq!(first.before.why, Why::Default);
        assert_eq!(first.before.account(), "expenses:unknown");
        assert_eq!(tier(first.after.why), "rule");
        assert_eq!(
            first.after.legs,
            [PlanLeg {
                account: "expenses:coffee".to_owned(),
                amount: "4.50 CAD".to_owned(),
            }]
        );
    }

    /// An override plan names its record, before and after.
    #[test]
    fn an_override_moves_its_record() {
        let f = fixture();
        let plan = plan(&f.raw, &inputs(|_| {}), &over("card:2", "expenses:food")).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.unchanged, 0);
        assert_eq!(plan.moves[0].before.account(), "expenses:unknown");
        assert_eq!(tier(plan.moves[0].after.why), "override");
        assert_eq!(plan.moves[0].after.account(), "expenses:food");
    }

    /// An edit derive wouldn't apply fails with what derive did instead:
    /// no counter leg, or the problem it rejected the edit for.
    #[test]
    fn what_derive_rejects_fails() {
        let f = fixture();
        let none = inputs(|_| {});
        let err = plan(&f.raw, &none, &over("bank:2", "expenses:food")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the override on bank:2 wouldn't apply: it has no counter leg (no posting, or more than one, to a default account)"
        );

        let short = Action::Split {
            record: "card:2".to_owned(),
            legs: vec![Leg {
                account: "expenses:food".to_owned(),
                qty: -100,
            }],
        };
        let err = plan(&f.raw, &none, &short).unwrap_err().to_string();
        assert!(
            err.starts_with("the split on card:2 wouldn't apply: SplitUnbalanced"),
            "{err}"
        );

        // Two coffees of different sizes don't net.
        let link = Action::Link {
            a: "card:2".to_owned(),
            b: "card:3".to_owned(),
        };
        let err = plan(&f.raw, &none, &link).unwrap_err().to_string();
        assert!(err.contains("TransferUnbalanced"), "{err}");

        let err = plan(&f.raw, &none, &over("card:99", "expenses:food")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no transaction with id 'card:99' in the journal"
        );
    }

    /// The write rules hold in a plan: a categorized record can't be
    /// linked, and a linked one can't be categorized.
    #[test]
    fn links_and_categorization_exclude_each_other() {
        let f = fixture();
        let link = Action::Link {
            a: "bank:1".to_owned(),
            b: "card:1".to_owned(),
        };
        let categorized = inputs(|e| {
            e.overrides.push(Override {
                record_id: "card:1".to_owned(),
                account: "income:refunds".to_owned(),
            })
        });
        let err = plan(&f.raw, &categorized, &link).unwrap_err();
        assert!(matches!(err, WriteError::Categorized(r) if r == "card:1"));

        let linked = inputs(|e| {
            e.links.push(TransferLink {
                a: "bank:1".to_owned(),
                b: "card:1".to_owned(),
            })
        });
        let err = plan(&f.raw, &linked, &over("card:1", "income:refunds")).unwrap_err();
        assert!(
            matches!(err, WriteError::Linked { record, other } if record == "card:1" && other == "bank:1")
        );

        // The link itself plans, moving both sides to transfers.
        let plan = plan(&f.raw, &inputs(|_| {}), &link).unwrap();
        let after: Vec<_> = plan.moves.iter().map(|m| tier(m.after.why)).collect();
        assert_eq!(after, ["transfer", "transfer"]);
    }
}
