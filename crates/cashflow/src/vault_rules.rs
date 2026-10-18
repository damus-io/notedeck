//! Categorization rules in the vault: one note per rule, ordered by rank.
//!
//! A rule is one [`RuleNote`] at `d = rule:<id>` ([`kinds::RULE`]), with the
//! id a random word-id so a person can type it (`cashflow rule rm
//! oak-flat-lamp`). It holds what a `[[rule]]` in a local rules file holds
//! ([`crate::rules`]): the account, `match` payee patterns (any one), `meta`
//! needles (every one), an amount range, a source and a layer. A file rule
//! and a vault rule therefore compile to ledgerdb [`Rule`]s by the same code,
//! [`RuleNote::push_rules`], so the two can't categorize differently.
//!
//! ## Order
//!
//! Rules are tried in order and the first match wins. The order is each
//! note's `rank` ([`crate::rank`]), a string that sorts between its
//! neighbours, so moving a rule rewrites only that rule's note and two
//! machines reordering different rules never collide. Two rules that tie on
//! a rank (two machines appending at once) sort by id.
//!
//! ## Deleting
//!
//! A deleted rule is its note rewritten with `["deleted", "1"]`, a tombstone,
//! not a NIP-09 deletion: the vault keeps the newest revision of every `d`,
//! so a tombstone wins over the live revisions everywhere it syncs, and
//! nothing has to honour a deletion request.
//!
//! ## Provenance
//!
//! A rule written from an agentium session carries `["agentium",
//! "agentium:<word-id>"]`, the tag `headway review` writes, so `cashflow rule
//! ls` can say which session added it.

use std::borrow::Cow;
use std::fmt;

use ledgerdb::{Accounts, Cond, Edits, Field, Ledger, Rule, TxnId, Why, derive};
use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use crate::kinds;
use crate::rank;
use crate::rules;
use crate::vault::{self, Record, SCHEMA_VERSION, Stored, TagWriter, tag_text};

/// One categorization rule: the body of its vault note, borrowing from the
/// note when read.
///
/// `patterns` and `meta` needles are [`Cow`]s because a needle may be 64 hex
/// characters (a pubkey, say), which nostrdb stores as an id; such a needle
/// reads back as owned hex. Everything else borrows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleNote<'a> {
    /// Where a matching transaction's counter leg goes.
    pub account: &'a str,
    /// Its place in the order ([`crate::rank`]).
    pub rank: &'a str,
    /// Payee substrings, any one of which matches; none puts no condition on
    /// the payee.
    pub patterns: Vec<Cow<'a, str>>,
    /// Metadata substrings, every one of which must match.
    pub meta: Vec<MetaNeedle<'a>>,
    /// The counter leg's magnitude, if limited.
    pub amount: Option<AmountCond<'a>>,
    /// The segment's `; source:`, if limited.
    pub source: Option<&'a str>,
    /// The segment's layer, if limited.
    pub layer: Option<&'a str>,
    /// Whether this revision is a tombstone.
    pub deleted: bool,
    /// The `agentium:<word-id>` session that wrote the rule, if one did.
    pub agentium: Option<&'a str>,
}

/// A `meta` condition: the `; key: value` metadata `key` contains `needle`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaNeedle<'a> {
    /// The metadata key, such as `raw-city`.
    pub key: &'a str,
    /// The substring its value must contain, ignoring case.
    pub needle: Cow<'a, str>,
}

/// An `amount` condition: the counter leg's quantity, sign ignored, in
/// `min..=max` smallest units of `commodity` (any when `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmountCond<'a> {
    /// The smallest magnitude that matches.
    pub min: i64,
    /// The largest magnitude that matches.
    pub max: i64,
    /// The commodity the counter leg must be in.
    pub commodity: Option<&'a str>,
}

/// Why a rule can't be stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// The account is not a valid account name.
    #[error("'{account}' isn't a valid account: {error}")]
    BadAccount {
        account: String,
        error: ledgerdb::AccountError,
    },
    /// The amount range is empty or negative.
    #[error("the amount range needs 0 <= min <= max")]
    BadRange,
    /// A `meta` key is empty or has whitespace in it.
    #[error("'{0}' isn't a metadata key")]
    BadMetaKey(String),
}

impl<'a> RuleNote<'a> {
    /// A live rule to `account` at `rank` with no conditions yet.
    pub fn new(account: &'a str, rank: &'a str) -> Self {
        RuleNote {
            account,
            rank,
            patterns: Vec::new(),
            meta: Vec::new(),
            amount: None,
            source: None,
            layer: None,
            deleted: false,
            agentium: None,
        }
    }

    /// Check the rule can be stored and applied.
    pub fn check(&self) -> Result<(), RuleError> {
        if let Err(error) = Accounts::new().intern(self.account) {
            return Err(RuleError::BadAccount {
                account: self.account.to_owned(),
                error,
            });
        }
        if let Some(amount) = self.amount
            && (amount.min < 0 || amount.min > amount.max)
        {
            return Err(RuleError::BadRange);
        }
        if let Some(bad) = self
            .meta
            .iter()
            .find(|m| m.key.is_empty() || m.key.contains(char::is_whitespace))
        {
            return Err(RuleError::BadMetaKey(bad.key.to_owned()));
        }
        Ok(())
    }

    /// Whether `other` matches exactly what this rule matches and sends it to
    /// the same account: everything but the rank, the tombstone and who
    /// wrote it. A re-run `rule import` skips a rule the vault already has.
    pub fn same_rule(&self, other: &RuleNote) -> bool {
        self.account == other.account
            && self.patterns == other.patterns
            && self.meta == other.meta
            && self.amount == other.amount
            && self.source == other.source
            && self.layer == other.layer
    }

    /// Whether the rule has any condition at all. One without matches every
    /// transaction it is in scope for.
    pub fn has_conditions(&self) -> bool {
        !self.patterns.is_empty()
            || !self.meta.is_empty()
            || self.amount.is_some()
            || self.source.is_some()
    }

    /// Append the rule to `out` as ledgerdb [`Rule`]s, one per payee pattern
    /// (or one when it has none), recording `rule` as each one's owner.
    ///
    /// A rule's patterns are *any of* while a ledgerdb rule is *all of*, so
    /// one per pattern, in pattern order, keeps first-match-wins exact. The
    /// pattern goes first in each: ledgerdb indexes a rule by its first
    /// `Contains`, so the payee pattern is what narrows the candidates.
    pub fn push_rules(&self, rule: u32, out: &mut Compiled) {
        let mut shared = Vec::with_capacity(self.meta.len() + 2);
        for meta in &self.meta {
            shared.push(Cond::Contains {
                field: Field::Meta(meta.key.to_owned()),
                needle: meta.needle.clone().into_owned(),
            });
        }
        if let Some(amount) = self.amount {
            shared.push(Cond::Amount {
                commodity: amount.commodity.map(str::to_owned),
                min: amount.min,
                max: amount.max,
            });
        }
        if let Some(source) = self.source {
            shared.push(Cond::Source(source.to_owned()));
        }
        let mut push = |conditions, pattern| {
            out.edits.rules.push(Rule {
                account: self.account.to_owned(),
                conditions,
                layer: self.layer.map(str::to_owned),
            });
            out.owners.push(Owner { rule, pattern });
        };
        if self.patterns.is_empty() {
            push(shared, None);
            return;
        }
        for (i, needle) in self.patterns.iter().enumerate() {
            let mut conditions = Vec::with_capacity(shared.len() + 1);
            conditions.push(Cond::Contains {
                field: Field::Payee,
                needle: needle.clone().into_owned(),
            });
            conditions.extend(shared.iter().cloned());
            push(conditions, Some(i as u32));
        }
    }

    /// The rule's conditions as one line: `match:a|b meta:k~v
    /// amount:1..2msat source:s layer:l`, or `(matches everything)`.
    pub fn conditions(&self) -> ConditionsDisplay<'_, 'a> {
        ConditionsDisplay(self)
    }
}

/// [`RuleNote::conditions`]'s display.
pub struct ConditionsDisplay<'r, 'a>(&'r RuleNote<'a>);

impl fmt::Display for ConditionsDisplay<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rule = self.0;
        let mut sep = "";
        if !rule.patterns.is_empty() {
            f.write_str("match:")?;
            for (i, pattern) in rule.patterns.iter().enumerate() {
                if i > 0 {
                    f.write_str("|")?;
                }
                f.write_str(pattern)?;
            }
            sep = " ";
        }
        for meta in &rule.meta {
            write!(f, "{sep}meta:{}~{}", meta.key, meta.needle)?;
            sep = " ";
        }
        if let Some(amount) = rule.amount {
            write!(f, "{sep}amount:{}..", amount.min)?;
            if amount.max != i64::MAX {
                write!(f, "{}", amount.max)?;
            }
            if let Some(commodity) = amount.commodity {
                f.write_str(commodity)?;
            }
            sep = " ";
        }
        if let Some(source) = rule.source {
            write!(f, "{sep}source:{source}")?;
            sep = " ";
        }
        if let Some(layer) = rule.layer {
            write!(f, "{sep}layer:{layer}")?;
            sep = " ";
        }
        if sep.is_empty() {
            f.write_str("(matches everything)")?;
        }
        Ok(())
    }
}

impl<'a> Record<'a> for RuleNote<'a> {
    const KIND: u32 = kinds::RULE;
    const PREFIX: &'static str = "rule";

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        tags = tags
            .tag("account", &[self.account])
            .tag("rank", &[self.rank]);
        for pattern in &self.patterns {
            tags = tags.text_tag("match", &[pattern]);
        }
        for meta in &self.meta {
            tags = tags.text_tag("meta", &[meta.key, &meta.needle]);
        }
        if let Some(amount) = self.amount {
            let (min, max) = (amount.min.to_string(), amount.max.to_string());
            tags = match amount.commodity {
                Some(commodity) => tags.tag("amount", &[&min, &max, commodity]),
                None => tags.tag("amount", &[&min, &max]),
            };
        }
        if let Some(source) = self.source {
            tags = tags.tag("source", &[source]);
        }
        if let Some(layer) = self.layer {
            tags = tags.tag("layer", &[layer]);
        }
        if self.deleted {
            tags = tags.tag("deleted", &["1"]);
        }
        if let Some(agentium) = self.agentium {
            tags = tags.tag("agentium", &[agentium]);
        }
        tags
    }

    /// `None` for a note of another schema version as well as a malformed
    /// one: a newer writer's rule may carry a condition this reader doesn't
    /// know, and applying the rest would match more than its writer meant.
    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        if vault::tag_value(tags, "v") != Some(SCHEMA_VERSION) {
            return None;
        }
        let mut account = None;
        let mut rank = None;
        let mut rule = RuleNote::new("", "");
        for tag in tags.iter() {
            if tag.count() < 2 {
                continue;
            }
            match tag.get_str(0)? {
                "account" => account = tag.get_str(1),
                "rank" => rank = tag.get_str(1),
                "match" => rule.patterns.push(tag_text(&tag, 1)?),
                "meta" => rule.meta.push(MetaNeedle {
                    key: tag.get_str(1)?,
                    needle: tag_text(&tag, 2)?,
                }),
                "amount" => {
                    rule.amount = Some(AmountCond {
                        min: tag.get_str(1)?.parse().ok()?,
                        max: tag.get_str(2)?.parse().ok()?,
                        commodity: tag.get_str(3),
                    })
                }
                "source" => rule.source = tag.get_str(1),
                "layer" => rule.layer = tag.get_str(1),
                "deleted" => rule.deleted = tag.get_str(1) == Some("1"),
                "agentium" => rule.agentium = tag.get_str(1),
                _ => {}
            }
        }
        rule.account = account?;
        rule.rank = rank.filter(|r| rank::is_rank(r))?;
        Some(rule)
    }
}

/// Which rule a compiled ledgerdb [`Rule`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    /// The rule's index in the list that was compiled.
    pub rule: u32,
    /// Which of its `patterns` this one matches on, if it has any.
    pub pattern: Option<u32>,
}

/// Rules compiled for [`derive()`]: the edits, and which rule each of
/// `edits.rules` came from, so a [`Why::Rule`] can be traced back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compiled {
    /// The default counter accounts and the compiled rules, nothing else.
    pub edits: Edits,
    /// `owners[i]` is where `edits.rules[i]` came from.
    pub owners: Vec<Owner>,
}

impl Compiled {
    /// No rules: the default counter accounts alone.
    pub fn empty() -> Self {
        Compiled {
            edits: rules::default_edits(),
            owners: Vec::new(),
        }
    }

    /// `notes` compiled in order.
    pub fn of<'n, 'a: 'n>(notes: impl IntoIterator<Item = &'n RuleNote<'a>>) -> Self {
        let mut compiled = Compiled::empty();
        for (i, note) in notes.into_iter().enumerate() {
            note.push_rules(i as u32, &mut compiled);
        }
        compiled
    }

    /// The rule that decided a transaction, by its index in the compiled
    /// list, if a rule did.
    pub fn owner(&self, why: Why) -> Option<Owner> {
        match why {
            Why::Rule(i) => self.owners.get(i as usize).copied(),
            _ => None,
        }
    }
}

/// Every rule in the vault, tombstones included, in order.
pub struct VaultRules<'a> {
    rules: Vec<Stored<'a, RuleNote<'a>>>,
}

/// Why a rule selector didn't pick one rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FindError {
    /// No live rule's id starts with it.
    #[error("no rule '{0}' (see `cashflow rule ls`)")]
    None(String),
    /// More than one live rule's id starts with it.
    #[error("'{selector}' could be {}", .ids.join(" or "))]
    Ambiguous { selector: String, ids: Vec<String> },
}

/// Where to put a rule among the live ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place<'s> {
    /// Before every rule: tried first.
    First,
    /// After every rule: tried last.
    Last,
    /// Just before the rule this selects.
    Before(&'s str),
    /// Just after the rule this selects.
    After(&'s str),
}

/// Why no rank could be found for a placement.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceError {
    /// The rule to place against isn't one.
    #[error(transparent)]
    Find(#[from] FindError),
    /// The two neighbours share a rank, so nothing sorts between them.
    #[error("rules {0} and {1} share a rank; move one of them first")]
    Tied(String, String),
}

impl<'a> VaultRules<'a> {
    /// Fold `author`'s rule notes, borrowed from `txn`.
    pub fn load(ndb: &Ndb, txn: &'a Transaction, author: &Pubkey) -> Self {
        Self::from_stored(vault::latest_all::<RuleNote>(ndb, txn, author))
    }

    /// Order `rules` by rank, then id.
    pub fn from_stored(mut rules: Vec<Stored<'a, RuleNote<'a>>>) -> Self {
        rules.sort_by(|a, b| (a.body.rank, a.id).cmp(&(b.body.rank, b.id)));
        VaultRules { rules }
    }

    /// Every rule, tombstones included, in order.
    pub fn all(&self) -> &[Stored<'a, RuleNote<'a>>] {
        &self.rules
    }

    /// The live rules, in order: the ones derive applies.
    pub fn live(&self) -> impl Iterator<Item = &Stored<'a, RuleNote<'a>>> {
        self.rules.iter().filter(|r| !r.body.deleted)
    }

    /// The live rules compiled for [`derive()`]; an [`Owner::rule`] indexes
    /// [`Self::live`].
    pub fn compile(&self) -> Compiled {
        Compiled::of(self.live().map(|r| &r.body))
    }

    /// The live rule whose id is `selector`, or the one whose id starts with
    /// it.
    pub fn find(&self, selector: &str) -> Result<&Stored<'a, RuleNote<'a>>, FindError> {
        if let Some(exact) = self.live().find(|r| r.id == selector) {
            return Ok(exact);
        }
        let mut found = self.live().filter(|r| r.id.starts_with(selector));
        let Some(first) = found.next() else {
            return Err(FindError::None(selector.to_owned()));
        };
        if let Some(second) = found.next() {
            let mut ids = vec![first.id.to_owned(), second.id.to_owned()];
            ids.extend(found.map(|r| r.id.to_owned()));
            return Err(FindError::Ambiguous {
                selector: selector.to_owned(),
                ids,
            });
        }
        Ok(first)
    }

    /// `n` ranks, in order, that put new rules at `place`, as if the rule
    /// `moving` (being moved) weren't there.
    pub fn ranks_at(
        &self,
        place: Place,
        n: usize,
        moving: Option<&str>,
    ) -> Result<Vec<String>, PlaceError> {
        let others: Vec<&Stored<RuleNote>> = self.live().filter(|r| Some(r.id) != moving).collect();
        let at = match place {
            Place::First => 0,
            Place::Last => others.len(),
            Place::Before(sel) | Place::After(sel) => {
                let target = self.find(sel)?.id;
                let index = others.iter().position(|r| r.id == target);
                // Placing a rule next to itself leaves it where it is.
                let index = index.ok_or_else(|| FindError::None(sel.to_owned()))?;
                if matches!(place, Place::After(_)) {
                    index + 1
                } else {
                    index
                }
            }
        };
        let left = at.checked_sub(1).map(|i| others[i]);
        let right = others.get(at).copied();
        rank::spread(left.map(|r| r.body.rank), right.map(|r| r.body.rank), n).ok_or_else(|| {
            let id = |r: Option<&Stored<RuleNote>>| r.map_or("-", |r| r.id).to_owned();
            PlaceError::Tied(id(left), id(right))
        })
    }

    /// The id of the first live rule exactly like `note`, if the vault has
    /// one.
    pub fn duplicate_of(&self, note: &RuleNote) -> Option<&'a str> {
        self.live().find(|r| r.body.same_rule(note)).map(|r| r.id)
    }
}

/// A fresh rule id: a random word-id no rule in `existing` has.
pub fn new_rule_id(existing: &VaultRules) -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let random = SystemRandom::new();
    loop {
        let mut bytes = [0u8; 32];
        random.fill(&mut bytes).expect("the system has randomness");
        let id = wordid::encode(&bytes);
        if existing.all().iter().all(|r| r.id != id) {
            return id;
        }
    }
}

/// Where one transaction's counter leg went, and which rule sent it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// The rule that decided it, by index into the compiled list, or `None`
    /// when no rule did and it kept the importer's default.
    pub rule: Option<u32>,
    /// The account the counter leg went to.
    pub account: String,
}

/// A transaction a rule change moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The transaction, in the raw ledger.
    pub txn: TxnId,
    /// Where it went before the change.
    pub before: Decision,
    /// Where it goes after.
    pub after: Decision,
}

/// Every transaction in `raw` whose deciding rule differs between the
/// rules `before` and `after`, in ledger order, with `before_rule` and
/// `after_rule` mapping each side's compiled index to a rule's identity.
///
/// Transactions an override, split or transfer link decides are never in
/// it: those win over every rule, so no rule change reaches them. Both
/// sides must carry the same defaults.
pub fn moves<K: PartialEq>(
    raw: &Ledger,
    before: &Compiled,
    after: &Compiled,
    before_rule: impl Fn(u32) -> K,
    after_rule: impl Fn(u32) -> K,
) -> Vec<Moved> {
    let old = derive(raw, &before.edits);
    let new = derive(raw, &after.edits);
    let mut moved = Vec::new();
    for i in 0..raw.txns().len() {
        let txn = TxnId(i as u32);
        let Some(was) = Decision::of(raw, before, txn, old.why(txn)) else {
            continue;
        };
        let Some(now) = Decision::of(raw, after, txn, new.why(txn)) else {
            continue;
        };
        let same = match (was.rule, now.rule) {
            (None, None) => true,
            (Some(a), Some(b)) => before_rule(a) == after_rule(b),
            _ => false,
        };
        if !same {
            moved.push(Moved {
                txn,
                before: was,
                after: now,
            });
        }
    }
    moved
}

impl Decision {
    /// Where `why`, from deriving `raw` with `compiled`, sent `txn`'s
    /// counter leg, when a rule or the importer's default decided it; `None`
    /// for an override, split, transfer link or a transaction with no
    /// counter leg.
    pub fn of(raw: &Ledger, compiled: &Compiled, txn: TxnId, why: Why) -> Option<Decision> {
        match why {
            Why::Rule(i) => Some(Decision {
                rule: compiled.owner(why).map(|o| o.rule),
                account: compiled.edits.rules[i as usize].account.clone(),
            }),
            Why::Default => {
                let journal = raw.journal();
                let account = journal
                    .postings_of(journal.txn(txn))
                    .iter()
                    .map(|p| journal.accounts.full_name(p.account))
                    .find(|name| compiled.edits.defaults.iter().any(|d| d == name))?;
                Some(Decision {
                    rule: None,
                    account: account.to_owned(),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use crate::vault::Vault;
    use ledgerdb::{Journal, JournalStore, LayerFilter, SegmentHeader};
    use nostrdb::Filter;

    const SECRET: [u8; 32] = [7u8; 32];
    const HEX: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn rule<'a>(account: &'a str, rank: &'a str, patterns: &[&'a str]) -> RuleNote<'a> {
        RuleNote {
            patterns: patterns.iter().map(|p| Cow::Borrowed(*p)).collect(),
            ..RuleNote::new(account, rank)
        }
    }

    fn subscribe(ndb: &Ndb) -> nostrdb::Subscription {
        ndb.subscribe(&[Filter::new().kinds([kinds::RULE as u64]).build()])
            .unwrap()
    }

    /// Every field survives the vault, a 64-hex needle included, and a
    /// tombstone reads back as one.
    #[tokio::test]
    async fn rules_round_trip_through_the_vault() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let sub = subscribe(ndb);

        let full = RuleNote {
            patterns: vec!["coffee".into(), "Espresso".into()],
            meta: vec![
                MetaNeedle {
                    key: "raw-zap-p",
                    needle: HEX.into(),
                },
                MetaNeedle {
                    key: "raw-city",
                    needle: "montreal".into(),
                },
            ],
            amount: Some(AmountCond {
                min: 100,
                max: 2000,
                commodity: Some("msat"),
            }),
            source: Some("cln"),
            layer: Some("lightning"),
            agentium: Some("agentium:oak-flat-lamp"),
            ..RuleNote::new("expenses:food:coffee", "m")
        };
        let gone = RuleNote {
            deleted: true,
            ..rule("expenses:travel", "t", &["air"])
        };
        vault.write(ndb, "one", &full, 0).unwrap();
        vault.write(ndb, "two", &gone, 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 2, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let rules = VaultRules::load(ndb, &txn, vault.author());
        assert_eq!(rules.all().len(), 2);
        assert_eq!(rules.all()[0].body, full);
        assert_eq!(rules.all()[1].body, gone);
        let live: Vec<&str> = rules.live().map(|r| r.id).collect();
        assert_eq!(live, ["one"]);
        assert!(rules.find("two").is_err(), "a tombstone is findable");
        assert_eq!(
            full.conditions().to_string(),
            format!(
                "match:coffee|Espresso meta:raw-zap-p~{HEX} meta:raw-city~montreal \
                 amount:100..2000msat source:cln layer:lightning"
            )
        );
    }

    /// Order is rank, then id, and a move rewrites the one rule's rank.
    #[test]
    fn rank_orders_and_a_move_touches_one_rule() {
        let stored = |id: &'static str, note: RuleNote<'static>| Stored {
            id,
            created_at: 1,
            body: note,
        };
        let rules = VaultRules::from_stored(vec![
            stored("c", rule("expenses:c", "t", &["c"])),
            stored("a", rule("expenses:a", "f", &["a"])),
            stored("b2", rule("expenses:b", "m", &["b"])),
            stored("b1", rule("expenses:b", "m", &["b"])),
        ]);
        let order: Vec<&str> = rules.live().map(|r| r.id).collect();
        assert_eq!(order, ["a", "b1", "b2", "c"]);

        // `c` to the front: one rank before `a`'s.
        let rank = rules.ranks_at(Place::First, 1, Some("c")).unwrap();
        assert!(rank[0].as_str() < "f");
        // `a` after `c`: past every other rank.
        let rank = rules.ranks_at(Place::After("c"), 1, Some("a")).unwrap();
        assert!(rank[0].as_str() > "t");
        // Between `a` and the tied `b`s, and not between the tie itself.
        let rank = rules.ranks_at(Place::Before("b1"), 1, None).unwrap();
        assert!("f" < rank[0].as_str() && rank[0].as_str() < "m");
        assert_eq!(
            rules.ranks_at(Place::After("b1"), 1, None),
            Err(PlaceError::Tied("b1".to_owned(), "b2".to_owned()))
        );
        assert!(matches!(
            rules.ranks_at(Place::After("b"), 1, None),
            Err(PlaceError::Find(FindError::Ambiguous { .. }))
        ));
        let three = rules.ranks_at(Place::Last, 3, None).unwrap();
        assert!(three.windows(2).all(|w| w[0] < w[1]) && three[0].as_str() > "t");
    }

    /// A note from a newer schema, or with a junk rank, isn't applied.
    #[test]
    fn foreign_and_malformed_notes_are_skipped() {
        let note = |v: &str, rank: &str| {
            nostrdb::NoteBuilder::new()
                .kind(kinds::RULE)
                .content("")
                .start_tag()
                .tag_str("d")
                .tag_str("rule:x")
                .start_tag()
                .tag_str("v")
                .tag_str(v)
                .start_tag()
                .tag_str("account")
                .tag_str("expenses:a")
                .start_tag()
                .tag_str("rank")
                .tag_str(rank)
                .sign(&SECRET)
                .build()
                .unwrap()
        };
        assert!(RuleNote::from_tags(&note("1", "m").tags()).is_some());
        assert!(RuleNote::from_tags(&note("2", "m").tags()).is_none());
        assert!(RuleNote::from_tags(&note("1", "ma").tags()).is_none());
    }

    #[test]
    fn check_refuses_what_derive_would_reject() {
        assert!(rule("expenses:a", "m", &["x"]).check().is_ok());
        assert!(matches!(
            rule("expenses::a", "m", &[]).check(),
            Err(RuleError::BadAccount { .. })
        ));
        let backwards = RuleNote {
            amount: Some(AmountCond {
                min: 5,
                max: 1,
                commodity: None,
            }),
            ..rule("expenses:a", "m", &[])
        };
        assert_eq!(backwards.check(), Err(RuleError::BadRange));
    }

    /// A raw ledger, and the store it loaded from.
    struct Fixture {
        _store: tempfile::TempDir,
        raw: Ledger,
    }

    fn ledger() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let text = "2024-01-02 * Corner Grocer\n  ; id: t:1\n  liabilities:card  -4.50 CAD\n  expenses:unknown\n\n\
                    2024-01-03 * Grocer Airline\n  ; id: t:2\n  liabilities:card  -100.00 CAD\n  expenses:unknown\n\n\
                    2024-01-04 * Bakery\n  ; id: t:3\n  liabilities:card  -3.00 CAD\n  expenses:unknown\n";
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "cards".to_owned(),
        };
        store
            .write_segment(&header, &Journal::parse(text).journal)
            .unwrap();
        let raw = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        Fixture { _store: tmp, raw }
    }

    /// Adding a rule ahead of another moves the rows it takes, and says
    /// which rule they came from; rows no rule matched before move from the
    /// default.
    #[test]
    fn moves_name_the_rule_each_row_left() {
        let Fixture { raw, _store } = ledger();
        let grocer = rule("expenses:food", "m", &["grocer"]);
        let air = rule("expenses:travel", "c", &["airline"]);
        let before = [&grocer];
        let after = [&air, &grocer];
        let ids_before = ["grocer"];
        let ids_after = ["air", "grocer"];
        let moved = moves(
            &raw,
            &Compiled::of(before),
            &Compiled::of(after),
            |i| ids_before[i as usize],
            |i| ids_after[i as usize],
        );
        assert_eq!(moved.len(), 1);
        assert_eq!(raw.journal().txn(moved[0].txn).payee, {
            let t2 = raw.find("t:2").unwrap();
            raw.journal().txn(t2).payee
        });
        assert_eq!(moved[0].before.account, "expenses:food");
        assert_eq!(moved[0].before.rule, Some(0));
        assert_eq!(moved[0].after.account, "expenses:travel");

        let bakery = rule("expenses:food", "z", &["bakery"]);
        let after = [&grocer, &bakery];
        let ids_after = ["grocer", "bakery"];
        let moved = moves(
            &raw,
            &Compiled::of(before),
            &Compiled::of(after),
            |i| ids_before[i as usize],
            |i| ids_after[i as usize],
        );
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].before.rule, None);
        assert_eq!(moved[0].before.account, "expenses:unknown");

        // Reordering rules that don't overlap moves nothing.
        let moved = moves(
            &raw,
            &Compiled::of([&grocer, &bakery]),
            &Compiled::of([&bakery, &grocer]),
            |i| ["grocer", "bakery"][i as usize],
            |i| ["bakery", "grocer"][i as usize],
        );
        assert!(moved.is_empty());
    }

    /// The same rules from a file and from the vault derive the same ledger.
    #[test]
    fn vault_rules_derive_like_file_rules() {
        let Fixture { raw, _store } = ledger();
        let text = "[[rule]]\naccount = \"expenses:food\"\nmatch = [\"grocer\", \"bakery\"]\n\n\
                    [[rule]]\naccount = \"expenses:travel\"\nmatch = [\"airline\"]\namount = { min = 5000 }\n";
        let path = std::path::Path::new("r.toml");
        let file = rules::parse_file(text, path).unwrap();
        let ranks = rank::spread(None, None, file.rules.len()).unwrap();
        let notes: Vec<RuleNote> = file
            .rules
            .iter()
            .zip(&ranks)
            .map(|(r, rank)| r.note(rank))
            .collect();
        let from_vault = Compiled::of(&notes);
        let from_file = rules::parse(text, path).unwrap();
        assert_eq!(from_vault.edits, from_file);
        let a = derive(&raw, &from_vault.edits);
        let b = derive(&raw, &from_file);
        for i in 0..raw.txns().len() {
            let txn = TxnId(i as u32);
            assert_eq!(a.why(txn), b.why(txn));
        }
        assert_eq!(
            from_vault.owner(a.explain("t:3").unwrap()),
            Some(Owner {
                rule: 0,
                pattern: Some(1)
            })
        );
    }
}
