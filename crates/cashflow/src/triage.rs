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
//! [`apply`] makes one [`Action`] a vault write, and is the one copy of the
//! write rules the app and the CLI's `categorize`, `split`, `uncategorize`,
//! `transfer` and `account set --project` share: an override and a split on
//! one record replace each other, a record in a live link can't be
//! categorized and a categorized one can't be linked, and a dismissed
//! suggestion is a cleared link note, so it is never suggested again.
//! [`check`] makes the same refusals without writing, and
//! [`crate::plan::plan`] makes them off the UI thread from the folded
//! edits, before saying what the write would move.

use std::sync::Arc;

use ledgerdb::{Edits, Ledger, Leg, TxnId, Why};
use nostrdb::{Ndb, Transaction};
use nostrdb_net::Pubkey;

use crate::chart::{AccountMeta, Chart, ChartError, record_id};
use crate::transfers::{self, Suggestion};
use crate::unknown::{self, PayeeGroup, Scope};
use crate::vault::{Record, Stored, Vault, VaultError, Written};
use crate::vault_edits::{
    EditBody, EditError, EditNote, LinkBody, LinkNote, OverrideBody, OverrideNote, SplitBody,
    SplitLeg, SplitNote, VaultEdits, link_id,
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

/// One write to the vault's edits, from the triage inbox, the detail view
/// or the CLI.
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
    /// Categorize one transaction as `account`: an override, replacing its
    /// split if it has one.
    Override {
        /// Its record id.
        record: String,
        /// Where its counter leg goes.
        account: String,
    },
    /// Split one transaction's counter leg into `legs`, replacing its
    /// override if it has one.
    Split {
        /// Its record id.
        record: String,
        /// The legs, in the counter leg's smallest units and sign, as
        /// [`crate::split::legs`] makes them from what a person wrote.
        legs: Vec<Leg>,
    },
    /// Clear one transaction's override and split, so it is unknown again
    /// (or a rule's).
    Uncategorize {
        /// Its record id.
        record: String,
    },
    /// Accept a suggested transfer: link `a` (money out) and `b` (in).
    Link {
        /// The side money left.
        a: String,
        /// The side it arrived on.
        b: String,
    },
    /// Undo a live transfer link: clear its note.
    Unlink {
        /// One side.
        a: String,
        /// The other.
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
    /// Tag `account`'s subtree with `project` (`None` clears it), keeping
    /// the account's other metadata.
    SetProject {
        /// The account.
        account: String,
        /// The project, or `None` for none.
        project: Option<String>,
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
    /// The record has no override or split to uncategorize.
    #[error("{0} has no override or split to clear")]
    NotCategorized(String),
    /// The two records aren't linked, so there is nothing to unlink.
    #[error("{a} and {b} aren't linked")]
    NotLinked {
        /// One side.
        a: String,
        /// The other.
        b: String,
    },
    /// A live rule already does exactly this.
    #[error("rule {0} already does this")]
    Duplicate(String),
    /// The loaded journal holds no transaction with this record id.
    #[error("no transaction with id '{0}' in the journal")]
    NoRecord(String),
    /// Derive wouldn't apply the edit: something else decides the
    /// transaction, it has no counter leg, or the edit is rejected.
    #[error("the {what} on {record} wouldn't apply: {why}")]
    NotApplied {
        /// The edit: "override", "split" or "link".
        what: &'static str,
        /// The record it names.
        record: String,
        /// What derive did instead.
        why: String,
    },
    /// The rule doesn't validate.
    #[error(transparent)]
    Rule(#[from] RuleError),
    /// The edit doesn't validate.
    #[error(transparent)]
    Edit(#[from] EditError),
    /// The account metadata doesn't validate.
    #[error(transparent)]
    Chart(#[from] ChartError),
    /// No rank sorts after the last rule.
    #[error(transparent)]
    Place(#[from] PlaceError),
    /// Signing, sealing or ingesting failed.
    #[error(transparent)]
    Vault(#[from] VaultError),
}

/// What [`apply`] wrote, or [`check`] would.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    /// The notes written, in order, each with its envelope's frame for a
    /// caller with a relay to publish. Always empty from [`check`].
    pub written: Vec<Written>,
    /// The `d` tags whose live revision the write rewrote or cleared, such
    /// as the `split:` an override replaced.
    pub replaced: Vec<String>,
}

/// Write `action` into `vault`, reading what it replaces from `ndb`, signed
/// as written by the `agentium:<word-id>` session `agentium` (`None` from
/// the app). The notes are ingested locally; the app's host fans the
/// envelopes out to the relays, and the CLI publishes [`Applied::written`].
pub fn apply(
    vault: &Vault,
    ndb: &Ndb,
    action: &Action,
    agentium: Option<&str>,
) -> Result<Applied, WriteError> {
    let sink = Sink {
        to: Some(To { vault, ndb }),
        applied: Applied::default(),
    };
    run(vault.author(), ndb, action, agentium, sink)
}

/// The refusal [`apply`] would make of `action`, if any, writing nothing;
/// on success, what it would replace.
pub fn check(vault: &Vault, ndb: &Ndb, action: &Action) -> Result<Applied, WriteError> {
    let sink = Sink {
        to: None,
        applied: Applied::default(),
    };
    run(vault.author(), ndb, action, None, sink)
}

/// Where an action's writes go: the vault, or nowhere for [`check`].
struct Sink<'v> {
    to: Option<To<'v>>,
    applied: Applied,
}

/// The vault a [`Sink`] writes into.
struct To<'v> {
    vault: &'v Vault,
    ndb: &'v Ndb,
}

impl Sink<'_> {
    /// Write `note` as the newest revision of record `id`, stamped past
    /// `after`; `replaces` when the revision it supersedes is live.
    fn put<'r, T: Record<'r>>(
        &mut self,
        id: &str,
        note: &T,
        after: u64,
        replaces: bool,
    ) -> Result<(), WriteError> {
        if replaces {
            self.applied.replaced.push(format!("{}:{id}", T::PREFIX));
        }
        if let Some(to) = &self.to {
            self.applied
                .written
                .push(to.vault.write(to.ndb, id, note, after)?);
        }
        Ok(())
    }

    /// Clear record `record`'s edit `stored` if it is live, written by
    /// `agentium`.
    fn clear<'a, B: EditBody<'a>>(
        &mut self,
        record: &str,
        stored: Option<&Stored<'a, EditNote<'a, B>>>,
        agentium: Option<&'a str>,
    ) -> Result<(), WriteError> {
        match stored {
            Some(s) if s.body.body.is_some() => self.put(
                record,
                &EditNote::<B>::cleared(agentium),
                s.created_at,
                true,
            ),
            _ => Ok(()),
        }
    }
}

/// Whether `stored` holds a live edit, not a cleared one.
fn live<B>(stored: Option<&Stored<'_, EditNote<'_, B>>>) -> bool {
    stored.is_some_and(|s| s.body.body.is_some())
}

/// [`apply`] and [`check`]: every refusal comes before the first write.
fn run<'a>(
    author: &Pubkey,
    ndb: &Ndb,
    action: &'a Action,
    agentium: Option<&'a str>,
    mut sink: Sink,
) -> Result<Applied, WriteError> {
    let txn = Transaction::new(ndb)?;
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
                agentium,
                ..RuleNote::new(account, &rank)
            };
            note.check()?;
            if let Some(id) = rules.duplicate_of(&note) {
                return Err(WriteError::Duplicate(id.to_owned()));
            }
            sink.put(&new_rule_id(&rules), &note, 0, false)?;
        }
        Action::Override { record, account } => {
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse(&stored, action)?;
            let note = OverrideNote::new(OverrideBody { account }, agentium);
            sink.clear(record, stored.split_of(record), agentium)?;
            let old = stored.override_of(record);
            sink.put(record, &note, old.map_or(0, |o| o.created_at), live(old))?;
        }
        Action::Split { record, legs } => {
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse(&stored, action)?;
            let note = SplitNote::new(
                SplitBody {
                    legs: split_legs(legs),
                },
                agentium,
            );
            sink.clear(record, stored.override_of(record), agentium)?;
            let old = stored.split_of(record);
            sink.put(record, &note, old.map_or(0, |s| s.created_at), live(old))?;
        }
        Action::Uncategorize { record } => {
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse(&stored, action)?;
            sink.clear(record, stored.override_of(record), agentium)?;
            sink.clear(record, stored.split_of(record), agentium)?;
        }
        Action::Link { a, b } => {
            let id = link_id(a, b)?;
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse(&stored, action)?;
            let old = stored.link(&id);
            let note = LinkNote::new(LinkBody { a, b }, agentium);
            sink.put(&id, &note, old.map_or(0, |l| l.created_at), live(old))?;
        }
        Action::Unlink { a, b } => {
            let id = link_id(a, b)?;
            let stored = VaultEdits::load(ndb, &txn, author);
            refuse(&stored, action)?;
            sink.clear(&id, stored.link(&id), agentium)?;
        }
        Action::Dismiss { a, b } => {
            let id = link_id(a, b)?;
            let stored = VaultEdits::load(ndb, &txn, author);
            let old = stored.link(&id);
            let note = LinkNote::cleared(agentium);
            sink.put(&id, &note, old.map_or(0, |l| l.created_at), live(old))?;
        }
        Action::SetProject { account, project } => {
            let chart = Chart::load(ndb, &txn, author);
            let stored = chart.meta(account);
            let meta = AccountMeta {
                project: project.as_deref(),
                agentium,
                ..stored.map(|s| s.body.clone()).unwrap_or_default()
            };
            meta.check(account)?;
            let after = stored.map_or(0, |s| s.created_at);
            sink.put(&record_id(account), &meta, after, stored.is_some())?;
        }
    }
    Ok(sink.applied)
}

/// What the write rules read of a record's edits as they stand: the
/// vault's notes ([`VaultEdits`]) when writing, or the edits they fold
/// into ([`Edits`]) when planning off the UI thread, so both refuse alike.
pub(crate) trait Standing {
    /// The other side of a live link naming `record`, if one does.
    fn linked_with(&self, record: &str) -> Option<&str>;
    /// Whether `record` has a live override or split.
    fn categorized(&self, record: &str) -> bool;
    /// Whether `a` and `b` are linked.
    fn links(&self, a: &str, b: &str) -> bool;
}

impl Standing for VaultEdits<'_> {
    fn linked_with(&self, record: &str) -> Option<&str> {
        let link = self.links_of(record).next()?;
        Some(link.body.body.map_or("", |b| b.other(record)))
    }

    fn categorized(&self, record: &str) -> bool {
        live(self.override_of(record)) || live(self.split_of(record))
    }

    fn links(&self, a: &str, b: &str) -> bool {
        link_id(a, b).is_ok_and(|id| live(self.link(&id)))
    }
}

impl Standing for Edits {
    fn linked_with(&self, record: &str) -> Option<&str> {
        let link = self.links.iter().find(|l| l.a == record || l.b == record)?;
        Some(if link.a == record { &link.b } else { &link.a })
    }

    fn categorized(&self, record: &str) -> bool {
        self.overrides.iter().any(|o| o.record_id == record)
            || self.splits.iter().any(|s| s.record_id == record)
    }

    fn links(&self, a: &str, b: &str) -> bool {
        self.links
            .iter()
            .any(|l| (l.a == a && l.b == b) || (l.a == b && l.b == a))
    }
}

/// The refusal `action` meets against `standing`, the record's edits as
/// they stand: an override or split of a linked record, or one that won't
/// store (an account name that isn't one), a link of a categorized or
/// already linked one, an uncategorize or unlink of what isn't there.
/// Rules, dismissals and projects meet none here.
pub(crate) fn refuse(standing: &impl Standing, action: &Action) -> Result<(), WriteError> {
    match action {
        Action::Override { record, account } => {
            refuse_linked(standing, record)?;
            Ok(OverrideBody { account }.check()?)
        }
        Action::Split { record, legs } => {
            refuse_linked(standing, record)?;
            Ok(SplitBody {
                legs: split_legs(legs),
            }
            .check()?)
        }
        Action::Uncategorize { record } if !standing.categorized(record) => {
            Err(WriteError::NotCategorized(record.clone()))
        }
        Action::Link { a, b } => {
            link_id(a, b)?;
            for side in [a, b] {
                if standing.categorized(side) {
                    return Err(WriteError::Categorized(side.clone()));
                }
                refuse_linked(standing, side)?;
            }
            Ok(())
        }
        Action::Unlink { a, b } if !standing.links(a, b) => Err(WriteError::NotLinked {
            a: a.clone(),
            b: b.clone(),
        }),
        _ => Ok(()),
    }
}

/// `legs` as a split note stores them.
fn split_legs(legs: &[Leg]) -> Vec<SplitLeg<'_>> {
    legs.iter()
        .map(|l| SplitLeg {
            account: &l.account,
            qty: l.qty,
        })
        .collect()
}

/// Fail when `record` is in a live link: an override would shadow it and
/// leave the other side half-netted.
fn refuse_linked(standing: &impl Standing, record: &str) -> Result<(), WriteError> {
    match standing.linked_with(record) {
        Some(other) => Err(WriteError::Linked {
            record: record.to_owned(),
            other: other.to_owned(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{JournalStore, LayerFilter, SegmentHeader};
    use nostrdb::Filter;

    use crate::edits_cache::EditsCache;
    use crate::kinds;
    use crate::split::{self, LegDraft};
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
        fn apply(&self, action: &Action, notes: u32) -> Result<Applied, WriteError> {
            let ndb = &self.ndb.ndb;
            let all = Filter::new().kinds(kinds::ALL.map(u64::from)).build();
            let landed = ndb.subscribe(&[all]).unwrap();
            let applied = apply(&self.vault, ndb, action, None)?;
            assert_eq!(applied.written.len(), notes as usize);
            let wait = ndb.wait_for_all_notes_within(landed, notes, INGEST_TIMEOUT);
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(wait)
                .expect("the notes landed");
            Ok(applied)
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
        let refused = check(
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

    impl Fixture {
        /// Why record `record` is categorized as it is, freshly derived.
        fn decided(&self, record: &str) -> String {
            let Derived { books, triage } = self.triage(0);
            let txn = books.ledger().find(record).unwrap();
            format!("{:?}", triage.explain(&books, txn))
        }

        /// The split of `record` made from `drafts`, as the CLI and the
        /// split editor make it.
        fn split(&self, record: &str, drafts: &[(&str, Option<&str>)]) -> Action {
            let drafts: Vec<LegDraft> = drafts
                .iter()
                .map(|(account, amount)| LegDraft {
                    account: (*account).to_owned(),
                    amount: amount.map(str::to_owned),
                })
                .collect();
            let txn = Transaction::new(&self.ndb.ndb).unwrap();
            let inputs = DeriveInputs::load(&self.ndb.ndb, &txn, self.vault.author());
            let at = self.raw.find(record).unwrap();
            Action::Split {
                record: record.to_owned(),
                legs: split::legs(&self.raw, &inputs.edits, at, record, &drafts).unwrap(),
            }
        }
    }

    fn d<'r, T: Record<'r>>(id: &str) -> String {
        format!("{}:{id}", T::PREFIX)
    }

    /// A split replaces an override and an override replaces a split, each
    /// clearing the other in the same apply; uncategorize clears what is
    /// left, and refuses when there is nothing.
    #[test]
    fn splits_and_overrides_replace_each_other() {
        let f = fixture();
        let split = f.split(
            "card:2",
            &[("expenses:food", Some("3")), ("expenses:tip", None)],
        );
        let Action::Split { legs, .. } = &split else {
            unreachable!()
        };
        assert_eq!(legs.iter().map(|l| l.qty).collect::<Vec<_>>(), [300, 150]);
        let applied = f.apply(&split, 1).unwrap();
        assert!(applied.replaced.is_empty());
        assert_eq!(f.decided("card:2"), "Split");

        let over = Action::Override {
            record: "card:2".to_owned(),
            account: "expenses:food".to_owned(),
        };
        let applied = f.apply(&over, 2).unwrap();
        assert_eq!(applied.replaced, [d::<SplitNote>("card:2")]);
        assert_eq!(f.decided("card:2"), "Override");

        let applied = f.apply(&split, 2).unwrap();
        assert_eq!(applied.replaced, [d::<OverrideNote>("card:2")]);
        assert_eq!(f.decided("card:2"), "Split");

        let uncategorize = Action::Uncategorize {
            record: "card:2".to_owned(),
        };
        let applied = f.apply(&uncategorize, 1).unwrap();
        assert_eq!(applied.replaced, [d::<SplitNote>("card:2")]);
        assert_eq!(f.decided("card:2"), "Unknown");
        let again = check(&f.vault, &f.ndb.ndb, &uncategorize).unwrap_err();
        assert_eq!(
            again.to_string(),
            "card:2 has no override or split to clear"
        );
    }

    /// Neither a split nor an override can land on a linked record, a
    /// categorized record can't be linked, and unlinking needs a live link.
    #[test]
    fn links_and_categorization_exclude_each_other() {
        let f = fixture();
        f.apply(
            &Action::Link {
                a: "bank:1".to_owned(),
                b: "card:1".to_owned(),
            },
            1,
        )
        .unwrap();
        let split = Action::Split {
            record: "card:1".to_owned(),
            legs: vec![Leg {
                account: "income:x".to_owned(),
                qty: -50000,
            }],
        };
        let refused = check(&f.vault, &f.ndb.ndb, &split).unwrap_err();
        assert!(
            matches!(&refused, WriteError::Linked { record, other } if record == "card:1" && other == "bank:1"),
            "{refused}"
        );

        f.apply(
            &Action::Override {
                record: "card:2".to_owned(),
                account: "expenses:food".to_owned(),
            },
            1,
        )
        .unwrap();
        let link = Action::Link {
            a: "card:2".to_owned(),
            b: "card:3".to_owned(),
        };
        let refused = check(&f.vault, &f.ndb.ndb, &link).unwrap_err();
        assert!(matches!(refused, WriteError::Categorized(r) if r == "card:2"));

        let unlink = Action::Unlink {
            a: "card:1".to_owned(),
            b: "bank:1".to_owned(),
        };
        let applied = f.apply(&unlink, 1).unwrap();
        assert_eq!(applied.replaced, [d::<LinkNote>("bank:1|card:1")]);
        assert_eq!(f.decided("bank:1"), "Unknown");
        let again = check(&f.vault, &f.ndb.ndb, &unlink).unwrap_err();
        assert_eq!(again.to_string(), "card:1 and bank:1 aren't linked");
        let never = Action::Unlink {
            a: "card:2".to_owned(),
            b: "card:3".to_owned(),
        };
        assert!(matches!(
            check(&f.vault, &f.ndb.ndb, &never),
            Err(WriteError::NotLinked { .. })
        ));
    }

    /// Tagging an account with a project keeps its name and icon, and
    /// `None` takes the project off again.
    #[test]
    fn set_project_keeps_the_rest_of_the_account() {
        let f = fixture();
        let ndb = &f.ndb.ndb;
        let all = Filter::new().kinds(kinds::ALL.map(u64::from)).build();
        let landed = ndb.subscribe(&[all]).unwrap();
        let meta = AccountMeta {
            name: Some("Food"),
            icon: Some("🍔"),
            ..AccountMeta::default()
        };
        f.vault.write(ndb, "expenses:food", &meta, 0).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(ndb.wait_for_all_notes_within(landed, 1, INGEST_TIMEOUT))
            .expect("the meta landed");
        let stored = |f: &Fixture| {
            let txn = Transaction::new(&f.ndb.ndb).unwrap();
            let chart = Chart::load(&f.ndb.ndb, &txn, f.vault.author());
            let meta = &chart.meta("expenses:food").unwrap().body;
            [meta.name, meta.icon, meta.project].map(|s| s.map(str::to_owned))
        };

        let tag = |project: Option<&str>| Action::SetProject {
            account: "expenses:food".to_owned(),
            project: project.map(str::to_owned),
        };
        let applied = f.apply(&tag(Some("damus")), 1).unwrap();
        assert_eq!(applied.replaced, [d::<AccountMeta>("expenses:food")]);
        let want = |project: Option<&str>| {
            [Some("Food"), Some("🍔"), project].map(|s| s.map(str::to_owned))
        };
        assert_eq!(stored(&f), want(Some("damus")));
        f.apply(&tag(None), 1).unwrap();
        assert_eq!(stored(&f), want(None));

        let blank = check(&f.vault, ndb, &tag(Some(" "))).unwrap_err();
        assert!(matches!(blank, WriteError::Chart(_)), "{blank}");
    }

    /// What a plan says a write moves is what applying it does: apply,
    /// re-derive, and each moved transaction is as the plan's after says.
    #[test]
    fn plan_and_apply_agree() {
        use crate::plan::{Categorized, plan};

        let f = fixture();
        let actions = [
            Action::Rule {
                pattern: "coffee shop".to_owned(),
                source: None,
                account: "expenses:coffee".to_owned(),
            },
            Action::Override {
                record: "card:2".to_owned(),
                account: "expenses:food".to_owned(),
            },
            Action::Link {
                a: "bank:1".to_owned(),
                b: "card:1".to_owned(),
            },
            Action::Uncategorize {
                record: "card:2".to_owned(),
            },
        ];
        for action in &actions {
            let ndb = &f.ndb.ndb;
            let txn = Transaction::new(ndb).unwrap();
            let inputs = DeriveInputs::load(ndb, &txn, f.vault.author());
            drop(txn);
            let planned = plan(&f.raw, &inputs, action).unwrap();
            assert!(!planned.moves.is_empty(), "{action:?} moves something");
            f.apply(action, 1).unwrap();

            let txn = Transaction::new(ndb).unwrap();
            let inputs = DeriveInputs::load(ndb, &txn, f.vault.author());
            let derived = ledgerdb::derive(&f.raw, &inputs.edits);
            for moved in &planned.moves {
                let now = Categorized::of(&f.raw, &derived, &inputs.edits, moved.txn);
                assert_eq!(
                    crate::plan::tier(now.why),
                    crate::plan::tier(moved.after.why)
                );
                assert_eq!(now.legs, moved.after.legs, "{action:?}");
            }
        }
    }
}
