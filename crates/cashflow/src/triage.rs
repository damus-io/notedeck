//! The triage inbox's core: what is left to categorize, what was
//! categorized today, and the vault writes that categorize it.
//!
//! The dashboard's inbox and the Adventure view both read a [`Triage`]: the
//! unknown transactions grouped by payee ([`crate::unknown::groups`]), the
//! transfers worth linking ([`suggestions`]), and the [`Progress`] figure,
//! "N unknown left (−M today)". It is worked out off the UI thread with the
//! books, from the very [`DeriveInputs`] they were derived with, so a
//! transaction's [`Why`] indexes the edits it holds.
//!
//! "Today" is counted from the vault, not from what one machine saw: a
//! transaction counts when the note that now decides it (an override,
//! split, link or rule, [`DeriveInputs::written_at`]) was written since the
//! day began. Every machine holding the same notes counts the same.
//!
//! [`apply`] makes one inbox [`Action`] a vault write, with the rules the
//! CLI's `categorize`, `rule add` and `transfer` keep: an override and a
//! split on one record replace each other, a record in a live link can't
//! be categorized and a categorized one can't be linked, and a dismissed
//! suggestion is a cleared link note, so it is never suggested again.

use std::sync::Arc;

use ledgerdb::{Ledger, TxnId, Why};
use nostrdb::{Ndb, Transaction};

use crate::transfers::{self, Suggestion};
use crate::unknown::{self, PayeeGroup, Scope};
use crate::vault::{Vault, VaultError};
use crate::vault_edits::{
    EditError, LinkBody, LinkNote, OverrideBody, OverrideNote, SplitNote, VaultEdits, link_id,
};
use crate::vault_rules::{Place, PlaceError, RuleError, RuleNote, VaultRules, new_rule_id};
use crate::{Books, DeriveInputs};

/// How much of the triage queue is left, and how much went today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Transactions nothing has categorized yet ([`unknown::count`]).
    pub left: usize,
    /// Transactions whose deciding note was written since the day began.
    pub today: usize,
}

/// [`Progress`] over `books`, derived with `inputs`, counting as today
/// every transaction decided by a note written at or after `since` (unix
/// seconds).
#[profiling::function]
pub fn progress(books: &Books, inputs: &DeriveInputs, since: u64) -> Progress {
    let mut progress = Progress::default();
    for i in 0..books.ledger().txns().len() {
        let why = books.derived.why(TxnId(i as u32));
        if why == Why::Default {
            progress.left += 1;
        } else if inputs.written_at(why).is_some_and(|at| at >= since) {
            progress.today += 1;
        }
    }
    progress
}

/// Transfers in `raw` worth linking: [`transfers::suggest`]'s pairs, less
/// the ones whose link note `inputs` holds cleared, which were dismissed or
/// unlinked.
#[profiling::function]
pub fn suggestions(raw: &Ledger, inputs: &DeriveInputs) -> Vec<Suggestion> {
    let cleared = &inputs.provenance.cleared_links;
    let mut found = transfers::suggest(raw, &inputs.edits);
    found.retain(|s| link_id(&s.link.a, &s.link.b).is_ok_and(|id| !cleared.contains(&id)));
    found
}

/// Everything the triage inbox shows, worked out with one set of books.
#[derive(Clone, Debug)]
pub struct Triage {
    /// What the books were derived with: the edits a [`Why`] indexes.
    pub inputs: Arc<DeriveInputs>,
    /// The unknown transactions, grouped and ranked.
    pub groups: Vec<PayeeGroup>,
    /// The transfers worth linking, in date order.
    pub suggestions: Vec<Suggestion>,
    /// How much is left, and how much went today.
    pub progress: Progress,
}

impl Triage {
    /// The triage of `books`, derived with `inputs` from `raw`, counting
    /// today from `since` (unix seconds). Without the raw ledger there are
    /// no transfer suggestions, since a rule may already have moved the
    /// counter legs they pair on.
    #[profiling::function]
    pub fn new(raw: Option<&Ledger>, books: &Books, inputs: Arc<DeriveInputs>, since: u64) -> Self {
        Triage {
            groups: unknown::groups(books, &inputs.edits, Scope::default()),
            suggestions: raw.map_or_else(Vec::new, |raw| suggestions(raw, &inputs)),
            progress: progress(books, &inputs, since),
            inputs,
        }
    }

    /// Why transaction `txn` of the books is categorized as it is.
    pub fn explain<'t>(&'t self, books: &'t Books, txn: TxnId) -> Decided<'t> {
        let why = books.derived.why(txn);
        let inputs = &*self.inputs;
        match why {
            Why::Untouched => Decided::AsImported,
            Why::Default => Decided::Unknown,
            Why::Rule(_) => Decided::Rule {
                id: inputs.rule_id(why),
            },
            Why::Transfer(i) => {
                let journal = books.ledger().journal();
                let own = journal.txn(txn).id.map(|id| journal.strings.get(id));
                let other = inputs.edits.links.get(i as usize).and_then(|link| {
                    let own = own?;
                    Some(if link.a == own { &link.b } else { &link.a }.as_str())
                });
                Decided::Transfer { other }
            }
            Why::Override(_) => Decided::Override,
            Why::Split(_) => Decided::Split,
        }
    }
}

/// What decided a transaction's counter leg, as [`Triage::explain`] tells
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decided<'t> {
    /// It has no counter leg: as the importer wrote it.
    AsImported,
    /// Nothing: it sits on the importer's default.
    Unknown,
    /// A vault rule, by id (`None` for one from a rules file).
    Rule {
        /// The rule's id.
        id: Option<&'t str>,
    },
    /// A transfer link, with the other side's record id.
    Transfer {
        /// The other side.
        other: Option<&'t str>,
    },
    /// An override on this record.
    Override,
    /// A split of this record.
    Split,
}

/// One write the triage inbox asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Categorize every transaction whose payee contains `pattern` (from
    /// `source`, if given) as `account`: a rule, tried after every other.
    Rule {
        /// The payee substring.
        pattern: String,
        /// The segment source to limit it to.
        source: Option<String>,
        /// Where it sends them.
        account: String,
    },
    /// Categorize one transaction as `account`: an override.
    Override {
        /// Its record id.
        record: String,
        /// Where its counter leg goes.
        account: String,
    },
    /// Accept a suggested transfer: link `a` (money out) and `b` (in).
    Link {
        /// The side money left.
        a: String,
        /// The side it arrived on.
        b: String,
    },
    /// Dismiss a suggested transfer: clear its link note, so it is never
    /// suggested again.
    Dismiss {
        /// One side.
        a: String,
        /// The other.
        b: String,
    },
}

/// Why [`apply`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// nostrdb couldn't open a read transaction.
    #[error("couldn't read the vault: {0}")]
    Read(#[from] nostrdb::Error),
    /// The record is one side of a live transfer link.
    #[error("{record} is linked as a transfer with {other}: unlink it first")]
    Linked {
        /// The record.
        record: String,
        /// The link's other side.
        other: String,
    },
    /// The record already has an override or split, so it can't be linked.
    #[error("{0} is already categorized: uncategorize it first")]
    Categorized(String),
    /// A live rule already does exactly this.
    #[error("rule {0} already does this")]
    Duplicate(String),
    /// The rule doesn't validate.
    #[error(transparent)]
    Rule(#[from] RuleError),
    /// The edit doesn't validate.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// No rank sorts after the last rule.
    #[error(transparent)]
    Place(#[from] PlaceError),
    /// Signing, sealing or ingesting failed.
    #[error(transparent)]
    Vault(#[from] VaultError),
}

/// Write `action` into `vault`, reading what it replaces from `ndb`. The
/// notes are ingested locally; the host fans the envelopes out to the
/// relays.
pub fn apply(vault: &Vault, ndb: &Ndb, action: &Action) -> Result<(), WriteError> {
    let txn = Transaction::new(ndb)?;
    let author = vault.author();
    match action {
        Action::Rule {
            pattern,
            source,
            account,
        } => {
            let rules = VaultRules::load(ndb, &txn, author);
            let rank = rules.ranks_at(Place::Last, 1, None)?.remove(0);
            let note = RuleNote {
                patterns: vec![pattern.as_str().into()],
                source: source.as_deref(),
                ..RuleNote::new(account, &rank)
            };
            note.check()?;
            if let Some(id) = rules.duplicate_of(&note) {
                return Err(WriteError::Duplicate(id.to_owned()));
            }
            vault.write(ndb, &new_rule_id(&rules), &note, 0)?;
        }
        Action::Override { record, account } => {
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse_linked(&stored, record)?;
            let note = OverrideNote::new(OverrideBody { account }, None);
            note.check()?;
            if let Some(split) = stored.split_of(record).filter(|s| s.body.body.is_some()) {
                vault.write(ndb, record, &SplitNote::cleared(None), split.created_at)?;
            }
            let after = stored.override_of(record).map_or(0, |o| o.created_at);
            vault.write(ndb, record, &note, after)?;
        }
        Action::Link { a, b } => {
            let id = link_id(a, b)?;
            let stored = VaultEdits::load(ndb, &txn, author);
            for side in [a, b] {
                linkable(&stored, side)?;
            }
            let after = stored.link(&id).map_or(0, |l| l.created_at);
            let note = LinkNote::new(LinkBody { a, b }, None);
            vault.write(ndb, &id, &note, after)?;
        }
        Action::Dismiss { a, b } => {
            let id = link_id(a, b)?;
            let stored = VaultEdits::load(ndb, &txn, author);
            let after = stored.link(&id).map_or(0, |l| l.created_at);
            vault.write(ndb, &id, &LinkNote::cleared(None), after)?;
        }
    }
    Ok(())
}

/// Fail when `record` is in a live link: an override would shadow it and
/// leave the other side half-netted.
fn refuse_linked(stored: &VaultEdits, record: &str) -> Result<(), WriteError> {
    match stored.links_of(record).next() {
        Some(link) => Err(WriteError::Linked {
            record: record.to_owned(),
            other: link.body.body.map_or("", |b| b.other(record)).to_owned(),
        }),
        None => Ok(()),
    }
}

/// Fail unless `record` can join a new link: no override, split or other
/// link decides it.
fn linkable(stored: &VaultEdits, record: &str) -> Result<(), WriteError> {
    let live_override = stored
        .override_of(record)
        .is_some_and(|o| o.body.body.is_some());
    let live_split = stored
        .split_of(record)
        .is_some_and(|s| s.body.body.is_some());
    if live_override || live_split {
        return Err(WriteError::Categorized(record.to_owned()));
    }
    refuse_linked(stored, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{JournalStore, LayerFilter, SegmentHeader};
    use nostrdb::Filter;

    use crate::edits_cache::EditsCache;
    use crate::kinds;
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    const SECRET: [u8; 32] = [21u8; 32];

    /// A card payment out of chequing and into the card, two days apart,
    /// two coffees, and a salary.
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

2024-03-06 * Acme Corp
    ; id: bank:2
    assets:checking  4000.00 CAD
    income:unknown
";

    struct Fixture {
        ndb: TempNdb,
        vault: Vault,
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
        let ndb = TempNdb::new();
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(&ndb.ndb);
        Fixture {
            ndb,
            vault,
            raw,
            _dir: dir,
        }
    }

    impl Fixture {
        /// Apply `action` and wait for its `notes` to land.
        fn apply(&self, action: &Action, notes: u32) -> Result<(), WriteError> {
            let ndb = &self.ndb.ndb;
            let all = Filter::new().kinds(kinds::ALL.map(u64::from)).build();
            let landed = ndb.subscribe(&[all]).unwrap();
            apply(&self.vault, ndb, action)?;
            let wait = ndb.wait_for_all_notes_within(landed, notes, INGEST_TIMEOUT);
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(wait)
                .expect("the notes landed");
            Ok(())
        }

        /// The vault folded, the books derived with it, and their triage.
        fn triage(&self, since: u64) -> Derived {
            let ndb = &self.ndb.ndb;
            let txn = Transaction::new(ndb).unwrap();
            let inputs = Arc::new(DeriveInputs::load(ndb, &txn, self.vault.author()));
            let books = Books::from_raw(&self.raw, Default::default(), &inputs.edits, None, "CAD");
            let triage = Triage::new(Some(&self.raw), &books, inputs, since);
            Derived { books, triage }
        }
    }

    /// What [`Fixture::triage`] works out.
    struct Derived {
        books: Books,
        triage: Triage,
    }

    /// Before any edit: everything is unknown, the coffees group under one
    /// pattern, and the card payment is suggested as a transfer.
    #[test]
    fn a_fresh_vault_has_everything_to_triage() {
        let f = fixture();
        let Derived { triage, .. } = f.triage(0);
        assert_eq!(triage.progress, Progress { left: 5, today: 0 });
        let coffee = triage
            .groups
            .iter()
            .find(|g| g.key == "sq coffee shop")
            .unwrap();
        assert_eq!(coffee.rows.len(), 2);
        assert_eq!(coffee.pattern.as_deref(), Some("sq *coffee shop"));
        assert_eq!(triage.suggestions.len(), 1);
        assert_eq!(triage.suggestions[0].link.a, "bank:1");
    }

    /// A rule from a group takes every row in it, an override takes one,
    /// and both count as today's when written after `since`.
    #[test]
    fn rules_and_overrides_burn_the_queue_down() {
        let f = fixture();
        f.apply(
            &Action::Rule {
                pattern: "sq *coffee shop".to_owned(),
                source: Some("t".to_owned()),
                account: "expenses:food:coffee".to_owned(),
            },
            1,
        )
        .unwrap();
        f.apply(
            &Action::Override {
                record: "bank:2".to_owned(),
                account: "income:salary".to_owned(),
            },
            1,
        )
        .unwrap();
        let Derived { books, triage } = f.triage(0);
        assert_eq!(triage.progress, Progress { left: 2, today: 3 });
        let coffee = books.ledger().find("card:2").unwrap();
        assert!(matches!(
            triage.explain(&books, coffee),
            Decided::Rule { id: Some(_) }
        ));
        let salary = books.ledger().find("bank:2").unwrap();
        assert_eq!(triage.explain(&books, salary), Decided::Override);

        // Written before a later `since`, they are not today's.
        let later = f.triage(u64::MAX).triage;
        assert_eq!(later.progress.today, 0);

        // The same rule again is refused.
        let again = f.apply(
            &Action::Rule {
                pattern: "sq *coffee shop".to_owned(),
                source: Some("t".to_owned()),
                account: "expenses:food:coffee".to_owned(),
            },
            1,
        );
        assert!(matches!(again, Err(WriteError::Duplicate(_))));
    }

    /// Accepting a suggestion links both sides; a linked record refuses an
    /// override; dismissing one clears the note, and it isn't suggested
    /// again.
    #[test]
    fn accepting_and_dismissing_transfers() {
        let f = fixture();
        let link = Action::Link {
            a: "bank:1".to_owned(),
            b: "card:1".to_owned(),
        };
        f.apply(&link, 1).unwrap();
        let Derived { books, triage } = f.triage(0);
        assert!(triage.suggestions.is_empty());
        assert_eq!(triage.progress.left, 3);
        let side = books.ledger().find("bank:1").unwrap();
        assert_eq!(
            triage.explain(&books, side),
            Decided::Transfer {
                other: Some("card:1")
            }
        );
        let refused = apply(
            &f.vault,
            &f.ndb.ndb,
            &Action::Override {
                record: "card:1".to_owned(),
                account: "expenses:x".to_owned(),
            },
        );
        assert!(matches!(refused, Err(WriteError::Linked { .. })));

        f.apply(
            &Action::Dismiss {
                a: "bank:1".to_owned(),
                b: "card:1".to_owned(),
            },
            1,
        )
        .unwrap();
        let Derived { triage, .. } = f.triage(0);
        assert!(triage.suggestions.is_empty(), "a dismissed pair stays gone");
        assert_eq!(triage.progress.left, 5, "and is unknown again");
    }

    /// The live cache folds the same provenance the one-shot load does.
    #[test]
    fn the_cache_carries_provenance() {
        let f = fixture();
        let ndb = &f.ndb.ndb;
        let mut cache = EditsCache::new(ndb, *f.vault.author());
        f.apply(
            &Action::Dismiss {
                a: "bank:1".to_owned(),
                b: "card:1".to_owned(),
            },
            1,
        )
        .unwrap();
        let txn = Transaction::new(ndb).unwrap();
        cache.poll(ndb, &txn);
        let inputs = cache.inputs().unwrap();
        assert!(inputs.provenance.cleared_links.contains("bank:1|card:1"));
    }
}
