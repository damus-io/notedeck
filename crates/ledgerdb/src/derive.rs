//! The derive step: the raw ledger, as importers wrote it, turned into the
//! categorized one by deciding where each transaction's counter leg goes.
//!
//! An importer knows one side of a transaction for certain (the card, the
//! bank account, the wallet) and posts the other side, the **counter leg**,
//! to a structural default such as `expenses:unknown`, leaving what it read
//! as `; raw-*` metadata. [`derive`] moves that leg to its real account.
//! The edits that say where are input data here; how they are stored and
//! synced is the caller's business.
//!
//! - **The counter leg** of a transaction is its one posting to an account
//!   listed in [`Edits::defaults`]. A transaction with no such posting, or
//!   with more than one, has no counter leg and passes through untouched:
//!   that is how hand-entered, fully categorized transactions survive.
//! - **Priority.** A [`Split`] or an [`Override`] of the transaction's
//!   record id wins, then a [`TransferLink`] naming it, then the first
//!   [`Rule`] whose conditions all hold, then the importer's default stays.
//! - **First wins.** Rules are tried in order and the first match decides.
//!   Two edits that claim the same transaction resolve the same way: a split
//!   beats an override, and otherwise the one earlier in its list wins. The
//!   loser is reported as [`Problem::Shadowed`], never silently dropped.
//! - **Nothing invalid applies.** A split whose legs do not sum to the
//!   counter leg, a link whose legs do not net to zero, or an account name
//!   that is not valid is rejected whole and reported in the
//!   [`DeriveReport`]; the transaction falls through to the next tier.
//!   Edits naming a record id the ledger does not hold (its layer is off,
//!   or its segment has not arrived) are listed as unmatched, not rejected.
//! - **Aliases come last.** After every counter leg is decided, each
//!   [`Alias`] renames an account subtree in the whole derived ledger, so a
//!   chart of accounts can be restructured without touching the rules and
//!   overrides that name the old accounts. See [`Alias`] for which alias
//!   applies to which account.
//! - **Deterministic.** The result depends only on the raw ledger's
//!   contents and the edits, so every machine holding the same segments and
//!   edits derives the same ledger. Edits are keyed by account names and
//!   record-id strings, never by interned ids, which follow load order.
//!
//! Every transaction keeps its position, so a [`TxnId`] means the same
//! transaction in the raw and the derived ledger, and [`Derived::why`] says
//! which tier decided it. Re-deriving after an edit changes is a full pass:
//! the derive benchmark puts it near 100 ms for 700k postings and 200
//! rules, so nothing finer is kept. Each transaction's decision depends
//! only on itself and the edits, so a partial re-derive can be added if
//! that ever gets slow.

use aho_corasick::AhoCorasick;

use crate::{
    AccountError, AccountId, Accounts, CommodityId, Journal, Ledger, Posting, PostingFlags, Span,
    StrId, Txn, TxnId,
};

/// A transaction field a [`Cond`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// The payee or description line.
    Payee,
    /// The values of the `; key: value` metadata entries with this key,
    /// the transaction's own and its postings'. A condition on it holds
    /// when any one entry satisfies it.
    Meta(String),
}

/// One test a [`Rule`] applies to a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cond {
    /// The field contains `needle`, ignoring case.
    Contains {
        /// The field to search.
        field: Field,
        /// The text to find.
        needle: String,
    },
    /// The field is exactly `value`.
    Equals {
        /// The field to compare.
        field: Field,
        /// The value it must have.
        value: String,
    },
    /// The counter leg's quantity, ignoring its sign, is in `min..=max`
    /// smallest units of `commodity` (any commodity when `None`). Units are
    /// the ledger's precision for the commodity, the finest any loaded
    /// segment wrote it with.
    Amount {
        /// The commodity the counter leg must be in, if any.
        commodity: Option<String>,
        /// The smallest magnitude that matches.
        min: i64,
        /// The largest magnitude that matches.
        max: i64,
    },
    /// The transaction's segment has this `; source:` header.
    Source(String),
}

/// Move the counter leg of every transaction matching all of `conditions`
/// to `account`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Where the counter leg goes.
    pub account: String,
    /// What must hold, all of it; an empty list matches every transaction.
    pub conditions: Vec<Cond>,
    /// When set, the rule applies only to transactions from segments in this
    /// layer.
    pub layer: Option<String>,
}

/// Move one transaction's counter leg to `account`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    /// The transaction's `; id:`.
    pub record_id: String,
    /// Where its counter leg goes.
    pub account: String,
}

/// One part of a [`Split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leg {
    /// Where this part goes.
    pub account: String,
    /// How much, in the counter leg's commodity and smallest units, with the
    /// counter leg's sign.
    pub qty: i64,
}

/// Replace one transaction's counter leg with several postings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// The transaction's `; id:`.
    pub record_id: String,
    /// The postings, which must sum to the counter leg's quantity.
    pub legs: Vec<Leg>,
}

/// Mark two transactions as one move between the owner's own accounts, such
/// as paying a card from a chequing account. Both counter legs go to
/// [`Edits::transfers`], where they net out, so the move stops counting as
/// income or expense.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferLink {
    /// One side's `; id:`.
    pub a: String,
    /// The other side's `; id:`.
    pub b: String,
}

/// Rename an account subtree in the derived ledger: every posting to `from`,
/// or to an account under it, moves to `to` with the rest of its name kept,
/// so with `expenses:food` → `expenses:groceries`, `expenses:food:produce`
/// becomes `expenses:groceries:produce`.
///
/// Aliases run after categorization, on the accounts rules, overrides,
/// splits and links decided, and on every other posting too.
///
/// - **Nearest wins.** An account takes the alias of its nearest ancestor
///   (or itself) that is some alias's `from`, so `expenses:food:coffee` →
///   `x` beats `expenses:food` → `y` for coffee and everything under it.
/// - **One pass.** A renamed account is not renamed again, even when its new
///   name falls under another alias's `from`, so aliases never chain or loop.
/// - **Moving a subtree under itself.** An account already at or under `to`
///   stays where it is, so `expenses:food` → `expenses:food:groceries`
///   files the rest of `expenses:food` under groceries without renaming
///   groceries itself to `expenses:food:groceries:groceries`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    /// The subtree to rename, by its root's name.
    pub from: String,
    /// The name `from` itself becomes.
    pub to: String,
}

/// Everything [`derive`] applies to a raw ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edits {
    /// The accounts importers post a counter leg to when they do not know
    /// its category, such as `expenses:unknown`. Exact names; a descendant
    /// is not a default.
    pub defaults: Vec<String>,
    /// The account linked transfers' counter legs go to.
    pub transfers: String,
    /// Rules, in priority order.
    pub rules: Vec<Rule>,
    /// Per-transaction overrides.
    pub overrides: Vec<Override>,
    /// Per-transaction splits.
    pub splits: Vec<Split>,
    /// Transfer links.
    pub links: Vec<TransferLink>,
    /// Account renames, applied after everything else; for two with the
    /// same `from`, the first wins.
    pub aliases: Vec<Alias>,
}

impl Default for Edits {
    /// No edits, no default accounts, and transfers to `transfers`.
    fn default() -> Self {
        Self {
            defaults: Vec::new(),
            transfers: "transfers".to_owned(),
            rules: Vec::new(),
            overrides: Vec::new(),
            splits: Vec::new(),
            links: Vec::new(),
            aliases: Vec::new(),
        }
    }
}

/// One edit, by its position in its list in [`Edits`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum EditRef {
    /// `edits.rules[i]`.
    Rule(u32),
    /// `edits.overrides[i]`.
    Override(u32),
    /// `edits.splits[i]`.
    Split(u32),
    /// `edits.links[i]`.
    Link(u32),
    /// `edits.aliases[i]`.
    Alias(u32),
}

/// Which tier decided a transaction's counter leg.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// The transaction has no counter leg, so it was left as written.
    Untouched,
    /// Nothing matched; the counter leg keeps the importer's default.
    Default,
    /// `edits.rules[i]` was the first rule to match.
    Rule(u32),
    /// `edits.links[i]` linked it to another transaction as a transfer.
    Transfer(u32),
    /// `edits.overrides[i]` named it.
    Override(u32),
    /// `edits.splits[i]` named it.
    Split(u32),
}

/// Why an edit was not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// An account the edit names is not a valid account name.
    BadAccount {
        /// The name.
        account: String,
        /// What is wrong with it.
        error: AccountError,
    },
    /// The transaction has no counter leg to move.
    NoCounterLeg,
    /// An earlier or higher-priority edit already claimed the transaction.
    Shadowed {
        /// The edit that did.
        by: EditRef,
    },
    /// A split with no legs.
    SplitEmpty,
    /// A split whose legs do not sum to the counter leg.
    SplitUnbalanced {
        /// The counter leg's quantity.
        want: i64,
        /// What the legs sum to.
        got: i128,
    },
    /// The counter leg carries a cost, which a split cannot share out.
    SplitCost,
    /// A link from a transaction to itself.
    TransferSelf,
    /// A link whose two counter legs are in different commodities or do
    /// not sum to zero.
    TransferUnbalanced,
    /// An alias whose `from` and `to` are the same account.
    AliasSelf,
}

/// An edit that was not applied, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    /// The edit.
    pub edit: EditRef,
    /// What was wrong.
    pub problem: Problem,
}

/// What [`derive`] could not apply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeriveReport {
    /// Edits that were invalid or lost to another, in the order checked:
    /// rules, splits, overrides, links, aliases.
    pub rejected: Vec<Rejected>,
    /// Overrides, splits and links naming a record id the ledger does not
    /// hold, in the same order. They apply once it does.
    pub unmatched: Vec<EditRef>,
}

/// The categorized ledger, with why each transaction ended up as it did.
#[derive(Debug)]
pub struct Derived {
    ledger: Ledger,
    why: Vec<Why>,
    report: DeriveReport,
}

impl Derived {
    /// The derived ledger. Its transactions are the raw ledger's, at the
    /// same [`TxnId`]s.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Which tier decided transaction `txn`.
    pub fn why(&self, txn: TxnId) -> Why {
        self.why[txn.index()]
    }

    /// Which tier decided the transaction carrying record id `record_id`,
    /// if the ledger holds one.
    pub fn explain(&self, record_id: &str) -> Option<Why> {
        self.ledger.find(record_id).map(|txn| self.why(txn))
    }

    /// The edits that were not applied.
    pub fn report(&self) -> &DeriveReport {
        &self.report
    }
}

/// Derive the categorized ledger from `raw` by applying `edits` to every
/// transaction's counter leg. See the module docs for the rules.
#[profiling::function]
pub fn derive(raw: &Ledger, edits: &Edits) -> Derived {
    let mut plan = Plan::new(raw, edits);
    plan.claim_splits(edits);
    plan.claim_overrides(edits);
    plan.claim_links(edits);
    let rules = plan.compile_rules(edits);
    plan.decide(&rules);
    plan.resolve_aliases(edits);
    let journal = plan.materialize(edits);
    Derived {
        ledger: raw.with_journal(journal),
        why: plan
            .why
            .into_iter()
            .map(|w| w.unwrap_or(Why::Untouched))
            .collect(),
        report: plan.report,
    }
}

/// Every rule that can match in one ledger, resolved against its tables.
///
/// Rules are indexed by their first `Contains` condition, their anchor: a
/// transaction only tries the rules anchored on a needle its fields contain,
/// plus the rules with no anchor, so a few hundred merchant rules cost a
/// handful of checks instead of one each. Indices into `compiled` follow
/// rule order, so the lowest matching index is the first match.
struct Rules {
    compiled: Vec<CompiledRule>,
    /// For each loaded segment, by index into [`Ledger::segments`], whether
    /// it meets each compiled rule's segment conditions.
    in_scope: Vec<Vec<bool>>,
    /// The rules anchored on each needle, by found-set bit, in rule order.
    anchored: Vec<Vec<u32>>,
    /// The rules with no `Contains` condition, in rule order.
    unanchored: Vec<u32>,
    /// The `Contains` needles, searched together once per transaction.
    searches: Vec<Search>,
    /// How many needles there are: the bits in a transaction's found set.
    needles: usize,
}

impl Rules {
    /// The first rule that holds for `txn`, from segment `segment`, whose
    /// counter leg is `counter` and whose fields contain the needles in
    /// `found`.
    fn first_match(
        &self,
        segment: usize,
        txn: &Txn,
        counter: &Posting,
        journal: &Journal,
        found: &[u64],
    ) -> Option<&CompiledRule> {
        let in_scope = &self.in_scope[segment];
        let holds = |r: u32| {
            in_scope[r as usize]
                && self.compiled[r as usize]
                    .tests
                    .iter()
                    .all(|t| t.holds(txn, counter, journal, found))
        };
        // Each list is in rule order, so its first rule that holds is the
        // only one that can beat the best so far.
        let mut best: Option<u32> = None;
        let mut consider = |list: &[u32]| {
            let limit = best.unwrap_or(u32::MAX);
            if let Some(&r) = list.iter().take_while(|&&r| r < limit).find(|&&r| holds(r)) {
                best = Some(r);
            }
        };
        for (word, &bits) in found.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let bit = word * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                consider(&self.anchored[bit]);
            }
        }
        consider(&self.unanchored);
        best.map(|r| &self.compiled[r as usize])
    }
}

/// A [`Rule`] resolved against one ledger's tables.
struct CompiledRule {
    /// Its index in `edits.rules`.
    index: u32,
    tests: Vec<Test>,
}

/// One transaction-level [`Cond`] resolved against a ledger's tables. Segment
/// conditions are checked once per segment instead.
enum Test {
    /// A `Contains` condition: the needle with this bit in the transaction's
    /// found set was found in its field.
    Contains(u32),
    PayeeEquals(StrId),
    MetaEquals {
        key: StrId,
        value: StrId,
    },
    Amount {
        commodity: Option<CommodityId>,
        min: i64,
        max: i64,
    },
}

/// A field `Contains` conditions search, resolved to the ledger's tables.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Searched {
    Payee,
    Meta(StrId),
}

/// Every `Contains` needle on one field, found in one pass over it.
///
/// Rules are tried in order, so a naive check runs every rule's substring
/// search on every transaction that matches none of them, which is most of
/// the cost of a derive with a few hundred rules. One Aho-Corasick search
/// per field finds all the needles at once instead, and a rule's condition
/// is then a bit test.
struct Search {
    field: Searched,
    /// The needles, folded to lower case, matched ignoring ASCII case.
    automaton: AhoCorasick,
    /// The found-set bit of each needle, by pattern index.
    bits: Vec<u32>,
}

impl Search {
    /// Set the bit of every needle `hay` contains, ignoring case. ASCII
    /// text, the common case, is searched in place; anything else is folded
    /// to lower case first, as the needles were.
    fn mark(&self, hay: &str, found: &mut [u64]) {
        let folded;
        let hay = if hay.is_ascii() {
            hay
        } else {
            folded = hay.to_lowercase();
            folded.as_str()
        };
        for hit in self.automaton.find_overlapping_iter(hay) {
            let bit = self.bits[hit.pattern().as_usize()] as usize;
            found[bit / 64] |= 1 << (bit % 64);
        }
    }
}

/// The `Contains` needles collected while compiling rules, grouped by field.
#[derive(Default)]
struct Needles {
    fields: Vec<FieldNeedles>,
    count: u32,
}

struct FieldNeedles {
    field: Searched,
    needles: Vec<String>,
    bits: Vec<u32>,
}

impl Needles {
    /// Add `needle` on `field` and return its found-set bit.
    fn add(&mut self, field: Searched, needle: &str) -> u32 {
        let bit = self.count;
        self.count += 1;
        let at = match self.fields.iter().position(|f| f.field == field) {
            Some(at) => at,
            None => {
                self.fields.push(FieldNeedles {
                    field,
                    needles: Vec::new(),
                    bits: Vec::new(),
                });
                self.fields.len() - 1
            }
        };
        self.fields[at].needles.push(needle.to_lowercase());
        self.fields[at].bits.push(bit);
        bit
    }

    /// One search per field.
    fn build(self) -> Vec<Search> {
        self.fields
            .into_iter()
            .map(|f| Search {
                field: f.field,
                automaton: AhoCorasick::builder()
                    .ascii_case_insensitive(true)
                    .build(&f.needles)
                    .expect("rule needles fit an automaton"),
                bits: f.bits,
            })
            .collect()
    }
}

/// The working state of one [`derive`] call.
struct Plan<'a> {
    raw: &'a Ledger,
    /// The derived ledger's account table: the raw one plus every account
    /// the edits name.
    accounts: Accounts,
    /// Whether each raw account is an importer default, by account id.
    is_default: Vec<bool>,
    /// The decision for each transaction, filled in tier by tier.
    why: Vec<Option<Why>>,
    /// Each edit's target account, by index into its list; `None` when the
    /// edit was rejected.
    rule_accounts: Vec<Option<AccountId>>,
    override_accounts: Vec<Option<AccountId>>,
    split_accounts: Vec<Vec<AccountId>>,
    transfers: Option<AccountId>,
    /// What each account is renamed to by the aliases, by account id;
    /// empty when there are none, and an account past its end keeps its id.
    renamed: Vec<AccountId>,
    report: DeriveReport,
}

impl<'a> Plan<'a> {
    fn new(raw: &'a Ledger, edits: &Edits) -> Self {
        let journal = raw.journal();
        let mut is_default = vec![false; journal.accounts.len()];
        for name in &edits.defaults {
            if let Some(id) = journal.accounts.lookup(name) {
                is_default[id.index()] = true;
            }
        }
        Self {
            raw,
            accounts: journal.accounts.clone(),
            is_default,
            why: vec![None; journal.txns.len()],
            rule_accounts: Vec::new(),
            override_accounts: vec![None; edits.overrides.len()],
            split_accounts: vec![Vec::new(); edits.splits.len()],
            transfers: None,
            renamed: Vec::new(),
            report: DeriveReport::default(),
        }
    }

    fn reject(&mut self, edit: EditRef, problem: Problem) {
        self.report.rejected.push(Rejected { edit, problem });
    }

    /// Intern `account` into the derived table, or reject `edit` for it.
    fn account(&mut self, edit: EditRef, account: &str) -> Option<AccountId> {
        match self.accounts.intern(account) {
            Ok(id) => Some(id),
            Err(error) => {
                let account = account.to_owned();
                self.reject(edit, Problem::BadAccount { account, error });
                None
            }
        }
    }

    /// The offset of `txn`'s counter leg among its postings, if it has
    /// exactly one.
    fn counter_leg(&self, txn: &Txn) -> Option<u32> {
        counter_leg(self.raw.journal(), &self.is_default, txn)
    }

    /// The transaction `record_id` names and its counter leg, or `None`
    /// after reporting why there is none to edit.
    fn target(&mut self, edit: EditRef, record_id: &str) -> Option<Target> {
        let Some(txn) = self.raw.find(record_id) else {
            self.report.unmatched.push(edit);
            return None;
        };
        let Some(counter) = self.counter_leg(self.raw.journal().txn(txn)) else {
            self.reject(edit, Problem::NoCounterLeg);
            return None;
        };
        if let Some(by) = self.why[txn.index()].and_then(claimed_by) {
            self.reject(edit, Problem::Shadowed { by });
            return None;
        }
        let journal = self.raw.journal();
        let at = journal.txn(txn).postings.start + counter;
        Some(Target {
            txn,
            index: at,
            posting: journal.postings[at as usize],
        })
    }

    fn claim_splits(&mut self, edits: &Edits) {
        for (i, split) in edits.splits.iter().enumerate() {
            let edit = EditRef::Split(i as u32);
            let Some(target) = self.target(edit, &split.record_id) else {
                continue;
            };
            if split.legs.is_empty() {
                self.reject(edit, Problem::SplitEmpty);
                continue;
            }
            let got: i128 = split.legs.iter().map(|leg| i128::from(leg.qty)).sum();
            if got != i128::from(target.posting.qty) {
                let want = target.posting.qty;
                self.reject(edit, Problem::SplitUnbalanced { want, got });
                continue;
            }
            if self.raw.journal().cost_of(target.index).is_some() {
                self.reject(edit, Problem::SplitCost);
                continue;
            }
            let accounts: Option<Vec<AccountId>> = split
                .legs
                .iter()
                .map(|leg| self.account(edit, &leg.account))
                .collect();
            let Some(accounts) = accounts else {
                continue;
            };
            self.split_accounts[i] = accounts;
            self.why[target.txn.index()] = Some(Why::Split(i as u32));
        }
    }

    fn claim_overrides(&mut self, edits: &Edits) {
        for (i, over) in edits.overrides.iter().enumerate() {
            let edit = EditRef::Override(i as u32);
            let Some(target) = self.target(edit, &over.record_id) else {
                continue;
            };
            let Some(account) = self.account(edit, &over.account) else {
                continue;
            };
            self.override_accounts[i] = Some(account);
            self.why[target.txn.index()] = Some(Why::Override(i as u32));
        }
    }

    /// Claim both sides of each link, or neither: a link that cannot take
    /// both transactions takes none, so a transfer never goes half-netted.
    fn claim_links(&mut self, edits: &Edits) {
        if edits.links.is_empty() {
            return;
        }
        if let Err(error) = self.accounts.intern(&edits.transfers) {
            for i in 0..edits.links.len() as u32 {
                let account = edits.transfers.clone();
                let error = error.clone();
                self.reject(EditRef::Link(i), Problem::BadAccount { account, error });
            }
            return;
        }
        self.transfers = self.accounts.lookup(&edits.transfers);
        for (i, link) in edits.links.iter().enumerate() {
            let edit = EditRef::Link(i as u32);
            let Some(a) = self.target(edit, &link.a) else {
                continue;
            };
            let Some(b) = self.target(edit, &link.b) else {
                continue;
            };
            if a.txn == b.txn {
                self.reject(edit, Problem::TransferSelf);
                continue;
            }
            let nets = a.posting.commodity == b.posting.commodity
                && i128::from(a.posting.qty) + i128::from(b.posting.qty) == 0;
            if !nets {
                self.reject(edit, Problem::TransferUnbalanced);
                continue;
            }
            self.why[a.txn.index()] = Some(Why::Transfer(i as u32));
            self.why[b.txn.index()] = Some(Why::Transfer(i as u32));
        }
    }

    /// Resolve every rule against the ledger's tables, and list, for each
    /// loaded segment, the rules whose segment conditions it meets. A rule
    /// that cannot match anything here (a metadata key or a commodity the
    /// ledger has never seen) is left out.
    fn compile_rules(&mut self, edits: &Edits) -> Rules {
        let journal = self.raw.journal();
        let mut compiled = Vec::new();
        let mut scopes = Vec::new();
        let mut needles = Needles::default();
        self.rule_accounts = vec![None; edits.rules.len()];
        for (i, rule) in edits.rules.iter().enumerate() {
            let Some(account) = self.account(EditRef::Rule(i as u32), &rule.account) else {
                continue;
            };
            self.rule_accounts[i] = Some(account);
            let Some(scope) = Scope::of(rule) else {
                continue;
            };
            let mut tests = Vec::with_capacity(rule.conditions.len());
            let mut possible = true;
            for cond in &rule.conditions {
                match compile(journal, cond, &mut needles) {
                    Compiled::Segment => {}
                    Compiled::Never => possible = false,
                    Compiled::Test(test) => tests.push(test),
                }
            }
            if !possible {
                continue;
            }
            compiled.push(CompiledRule {
                index: i as u32,
                tests,
            });
            scopes.push(scope);
        }

        let in_scope = self
            .raw
            .segments()
            .iter()
            .map(|segment| {
                let header = &segment.header;
                scopes
                    .iter()
                    .map(|scope| scope.admits(&header.source, &header.layer))
                    .collect()
            })
            .collect();
        let mut anchored = vec![Vec::new(); needles.count as usize];
        let mut unanchored = Vec::new();
        for (r, rule) in compiled.iter().enumerate() {
            let anchor = rule.tests.iter().find_map(|test| match test {
                Test::Contains(bit) => Some(*bit),
                _ => None,
            });
            match anchor {
                Some(bit) => anchored[bit as usize].push(r as u32),
                None => unanchored.push(r as u32),
            }
        }
        Rules {
            compiled,
            in_scope,
            anchored,
            unanchored,
            needles: needles.count as usize,
            searches: needles.build(),
        }
    }

    /// Give every unclaimed transaction its first matching rule, or the
    /// default.
    #[profiling::function]
    fn decide(&mut self, rules: &Rules) {
        let journal = self.raw.journal();
        let mut found = vec![0u64; rules.needles.div_ceil(64)];
        for (at, txn) in journal.txns.iter().enumerate() {
            if self.why[at].is_some() {
                continue;
            }
            let Some(counter) = counter_leg(journal, &self.is_default, txn) else {
                self.why[at] = Some(Why::Untouched);
                continue;
            };
            let posting = &journal.postings[(txn.postings.start + counter) as usize];
            let segment = self.raw.origin(TxnId(at as u32)).segment as usize;
            search(journal, txn, &rules.searches, &mut found);
            let matched = rules.first_match(segment, txn, posting, journal, &found);
            self.why[at] = Some(matched.map_or(Why::Default, |rule| Why::Rule(rule.index)));
        }
    }

    /// Work out what every account is renamed to by `edits.aliases`
    /// ([`Alias`] has the rules), interning the new names, and reject the
    /// aliases that can't apply.
    fn resolve_aliases(&mut self, edits: &Edits) {
        if edits.aliases.is_empty() {
            return;
        }
        // The alias rooted at each account, by account id.
        let mut rooted: Vec<Option<u32>> = vec![None; self.accounts.len()];
        for (i, alias) in edits.aliases.iter().enumerate() {
            let edit = EditRef::Alias(i as u32);
            let mut scratch = Accounts::new();
            if [&alias.from, &alias.to]
                .into_iter()
                .any(|name| self.check_name(edit, &mut scratch, name).is_none())
            {
                continue;
            }
            if alias.from == alias.to {
                self.reject(edit, Problem::AliasSelf);
                continue;
            }
            let Some(from) = self.accounts.lookup(&alias.from) else {
                continue;
            };
            match rooted[from.index()] {
                Some(by) => self.reject(
                    edit,
                    Problem::Shadowed {
                        by: EditRef::Alias(by),
                    },
                ),
                None => rooted[from.index()] = Some(i as u32),
            }
        }

        let mut moves: Vec<(AccountId, String)> = Vec::new();
        for id in self.accounts.ids() {
            let nearest = std::iter::once(id)
                .chain(self.accounts.ancestors(id))
                .find_map(|a| rooted[a.index()].map(|i| (a, i)));
            let Some((root, i)) = nearest else {
                continue;
            };
            let alias = &edits.aliases[i as usize];
            let under_target = self
                .accounts
                .lookup(&alias.to)
                .is_some_and(|to| to == id || self.accounts.is_ancestor(to, id));
            if under_target {
                continue;
            }
            let rest = &self.accounts.full_name(id)[self.accounts.full_name(root).len()..];
            moves.push((id, format!("{}{rest}", alias.to)));
        }
        if moves.is_empty() {
            return;
        }
        self.renamed = self.accounts.ids().collect();
        for (id, name) in moves {
            // Both halves were checked above, so the joined name is valid.
            if let Ok(to) = self.accounts.intern(&name) {
                self.renamed[id.index()] = to;
            }
        }
    }

    /// Check `name` is a valid account name by interning it into `scratch`,
    /// rejecting `edit` if it isn't.
    fn check_name(&mut self, edit: EditRef, scratch: &mut Accounts, name: &str) -> Option<()> {
        match scratch.intern(name) {
            Ok(_) => Some(()),
            Err(error) => {
                let account = name.to_owned();
                self.reject(edit, Problem::BadAccount { account, error });
                None
            }
        }
    }

    /// What `account` is renamed to by the aliases: itself when none
    /// applies.
    fn rename(&self, account: AccountId) -> AccountId {
        self.renamed
            .get(account.index())
            .copied()
            .unwrap_or(account)
    }

    /// The raw journal's rows with every decided counter leg moved, and
    /// split ones replaced by their legs.
    #[profiling::function]
    fn materialize(&mut self, edits: &Edits) -> Journal {
        let old = self.raw.journal();
        let mut journal = Journal {
            accounts: std::mem::take(&mut self.accounts),
            commodities: old.commodities.clone(),
            strings: old.strings.clone(),
            txns: Vec::with_capacity(old.txns.len()),
            postings: Vec::with_capacity(old.postings.len()),
            meta: Vec::with_capacity(old.meta.len()),
            comments: Vec::with_capacity(old.comments.len()),
            costs: Vec::with_capacity(old.costs.len()),
            prices: old.prices.clone(),
            file_comments: Vec::new(),
        };
        for (at, txn) in old.txns.iter().enumerate() {
            let why = self.why[at].unwrap_or(Why::Untouched);
            let counter = match why {
                Why::Untouched | Why::Default => None,
                _ => counter_leg(old, &self.is_default, txn),
            };
            let start = journal.postings.len() as u32;
            // Postings a split adds after the counter leg, which shift the
            // offsets of later postings' notes.
            let mut added = 0u32;
            for (offset, posting) in old.postings_of(txn).iter().enumerate() {
                if let Some(cost) = old.cost_of(txn.postings.start + offset as u32) {
                    journal.costs.push(crate::Cost {
                        posting: journal.postings.len() as u32,
                        ..*cost
                    });
                }
                if counter != Some(offset as u32) {
                    journal.postings.push(Posting {
                        account: self.rename(posting.account),
                        ..*posting
                    });
                    continue;
                }
                if let Why::Split(i) = why {
                    let legs = &edits.splits[i as usize].legs;
                    let accounts = &self.split_accounts[i as usize];
                    journal
                        .postings
                        .extend(legs.iter().zip(accounts).map(|(leg, &account)| Posting {
                            account: self.rename(account),
                            qty: leg.qty,
                            flags: PostingFlags(posting.flags.0 & !PostingFlags::ELIDED.0),
                            ..*posting
                        }));
                    added = legs.len() as u32 - 1;
                    continue;
                }
                journal.postings.push(Posting {
                    account: self.rename(self.moved_to(why).unwrap_or(posting.account)),
                    ..*posting
                });
            }
            let shift = |note: Option<u32>| match (note, counter) {
                (Some(p), Some(c)) if p > c => Some(p + added),
                _ => note,
            };
            let meta = Span {
                start: journal.meta.len() as u32,
                len: txn.meta.len,
            };
            journal
                .meta
                .extend(old.meta_of(txn).iter().map(|m| crate::Meta {
                    posting: shift(m.posting),
                    ..*m
                }));
            let comments = Span {
                start: journal.comments.len() as u32,
                len: txn.comments.len,
            };
            journal
                .comments
                .extend(old.comments_of(txn).iter().map(|c| crate::Comment {
                    posting: shift(c.posting),
                    ..*c
                }));
            journal.txns.push(Txn {
                postings: Span {
                    start,
                    len: journal.postings.len() as u32 - start,
                },
                meta,
                comments,
                ..*txn
            });
        }
        journal
    }

    /// The account a non-split decision moves the counter leg to.
    fn moved_to(&self, why: Why) -> Option<AccountId> {
        match why {
            Why::Rule(i) => self.rule_accounts[i as usize],
            Why::Override(i) => self.override_accounts[i as usize],
            Why::Transfer(_) => self.transfers,
            Why::Untouched | Why::Default | Why::Split(_) => None,
        }
    }
}

/// A transaction an edit names and its counter leg.
struct Target {
    txn: TxnId,
    /// The counter leg's index into the raw journal's postings.
    index: u32,
    posting: Posting,
}

/// The edit behind a decision made by an override, split or link.
fn claimed_by(why: Why) -> Option<EditRef> {
    match why {
        Why::Split(i) => Some(EditRef::Split(i)),
        Why::Override(i) => Some(EditRef::Override(i)),
        Why::Transfer(i) => Some(EditRef::Link(i)),
        Why::Untouched | Why::Default | Why::Rule(_) => None,
    }
}

/// The offset among `txn`'s postings of its one posting to a default
/// account, or `None` when it has none or several.
fn counter_leg(journal: &Journal, is_default: &[bool], txn: &Txn) -> Option<u32> {
    let mut found = None;
    for (offset, posting) in journal.postings_of(txn).iter().enumerate() {
        if !is_default
            .get(posting.account.index())
            .copied()
            .unwrap_or(false)
        {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(offset as u32);
    }
    found
}

/// A rule's segment-level conditions: its layer and any `Source` conds.
struct Scope<'a> {
    source: Option<&'a str>,
    layer: Option<&'a str>,
}

impl<'a> Scope<'a> {
    /// The rule's scope, or `None` when its `Source` conditions contradict
    /// each other so it can never match.
    fn of(rule: &'a Rule) -> Option<Self> {
        let mut source = None;
        for cond in &rule.conditions {
            let Cond::Source(s) = cond else {
                continue;
            };
            if source.is_some_and(|have| have != s.as_str()) {
                return None;
            }
            source = Some(s.as_str());
        }
        Some(Self {
            source,
            layer: rule.layer.as_deref(),
        })
    }

    fn admits(&self, source: &str, layer: &str) -> bool {
        self.source.is_none_or(|s| s == source) && self.layer.is_none_or(|l| l == layer)
    }
}

/// What one [`Cond`] becomes against a journal's tables.
enum Compiled {
    /// A segment-level condition, which [`Scope`] checks.
    Segment,
    /// A condition that cannot hold in this journal, such as one naming a
    /// metadata key no transaction has.
    Never,
    /// A test to run per transaction.
    Test(Test),
}

/// `cond` resolved against `journal`'s tables, adding any needle to
/// `needles`.
fn compile(journal: &Journal, cond: &Cond, needles: &mut Needles) -> Compiled {
    let strings = &journal.strings;
    let test = match cond {
        Cond::Source(_) => return Compiled::Segment,
        Cond::Contains {
            field: Field::Payee,
            needle,
        } => Some(Test::Contains(needles.add(Searched::Payee, needle))),
        Cond::Contains {
            field: Field::Meta(key),
            needle,
        } => strings
            .lookup(key)
            .map(|key| Test::Contains(needles.add(Searched::Meta(key), needle))),
        Cond::Equals {
            field: Field::Payee,
            value,
        } => strings.lookup(value).map(Test::PayeeEquals),
        Cond::Equals {
            field: Field::Meta(key),
            value,
        } => strings
            .lookup(key)
            .zip(strings.lookup(value))
            .map(|(key, value)| Test::MetaEquals { key, value }),
        Cond::Amount {
            commodity,
            min,
            max,
        } => {
            let commodity = match commodity {
                Some(name) => journal.commodities.get(name).map(Some),
                None => Some(None),
            };
            commodity.map(|commodity| Test::Amount {
                commodity,
                min: *min,
                max: *max,
            })
        }
    };
    test.map_or(Compiled::Never, Compiled::Test)
}

/// Fill `found` with the needles `txn`'s fields contain.
fn search(journal: &Journal, txn: &Txn, searches: &[Search], found: &mut [u64]) {
    found.fill(0);
    for search in searches {
        match search.field {
            Searched::Payee => search.mark(journal.strings.get(txn.payee), found),
            Searched::Meta(key) => {
                for meta in journal.meta_of(txn).iter().filter(|m| m.key == key) {
                    search.mark(journal.strings.get(meta.value), found);
                }
            }
        }
    }
}

impl Test {
    /// Whether the test holds for `txn`, whose counter leg is `counter` and
    /// whose fields contain the needles in `found`.
    fn holds(&self, txn: &Txn, counter: &Posting, journal: &Journal, found: &[u64]) -> bool {
        match self {
            Test::Contains(bit) => found[*bit as usize / 64] & (1 << (bit % 64)) != 0,
            Test::PayeeEquals(value) => txn.payee == *value,
            Test::MetaEquals { key, value } => journal
                .meta_of(txn)
                .iter()
                .any(|m| m.key == *key && m.value == *value),
            Test::Amount {
                commodity,
                min,
                max,
            } => {
                let magnitude = i128::from(counter.qty).abs();
                commodity.is_none_or(|c| c == counter.commodity)
                    && i128::from(*min) <= magnitude
                    && magnitude <= i128::from(*max)
            }
        }
    }
}
