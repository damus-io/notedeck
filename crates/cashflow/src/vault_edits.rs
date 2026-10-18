//! Per-transaction edits in the vault: overrides, splits and transfer links.
//!
//! Where a rule ([`crate::vault_rules`]) categorizes every transaction it
//! matches, these each name transactions by record id (`; id:`) and win over
//! every rule (see [`ledgerdb::derive`]):
//!
//! - an **override** sends one transaction's counter leg to an account,
//!   `d = ov:<record-id>` ([`kinds::OVERRIDE`]), tag `["account", …]`;
//! - a **split** replaces it with several legs, `d = split:<record-id>`
//!   ([`kinds::SPLIT`]), one `["leg", <account>, <qty>]` tag per leg in
//!   order, `qty` in the counter leg's smallest units and with its sign;
//! - a **transfer link** marks two transactions as one move between the
//!   owner's own accounts, `d = xfer:<a>|<b>` with the two record ids
//!   sorted ([`link_id`]), so linking `a b` and `b a` is the same note; its
//!   sides are the tags `["a", …]` and `["b", …]`, in the order given.
//!
//! One note per edit, so two machines editing different transactions never
//! clobber each other. Record ids are a source's own strings
//! (`amex:2026-04-15:1234`, `cln:…`), always behind the `d` prefix.
//!
//! ## Clearing
//!
//! Clearing an edit rewrites its note with `["cleared", "1"]` and no body,
//! the same tombstone a deleted rule is: the vault keeps the newest revision
//! of every `d`, so the clear syncs like any other write, and writing the
//! edit again later supersedes it.
//!
//! ## Validation
//!
//! A note is checked when it is written ([`EditNote::check`]: the accounts
//! parse, a split has legs, a link has two sides). Whether it fits its
//! transaction (a split summing to the counter leg, a link netting to zero)
//! depends on the journal, so derive decides that each time and reports a
//! misfit in its [`ledgerdb::DeriveReport`] rather than applying it.
//!
//! ## Provenance
//!
//! An edit written from an agentium session carries `["agentium",
//! "agentium:<word-id>"]`, as a rule does, so a person can list what an
//! agent did and undo just that.

use ledgerdb::{Accounts, Edits, Leg, Override, Split, TransferLink};
use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use crate::edits_cache::Provenance;
use crate::kinds;
use crate::vault::{self, Record, SCHEMA_VERSION, Stored, TagWriter};

/// The body of one kind of per-transaction edit: what [`EditNote`] holds
/// when the edit isn't cleared.
pub trait EditBody<'a>: Sized {
    /// The addressable kind it is stored under.
    const KIND: u32;
    /// The `d`-tag prefix.
    const PREFIX: &'static str;

    /// Add the body's tags.
    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b>;

    /// Read the body back, or `None` when a field it needs is missing.
    fn from_tags(tags: &Tags<'a>) -> Option<Self>;

    /// Check the body can be stored and applied.
    fn check(&self) -> Result<(), EditError>;
}

/// One per-transaction edit as stored: its body, or `None` once cleared, and
/// the session that wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditNote<'a, B> {
    /// The edit, or `None` when this revision clears it.
    pub body: Option<B>,
    /// The `agentium:<word-id>` session that wrote this revision, if one did.
    pub agentium: Option<&'a str>,
}

/// An override's body: where the counter leg goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideBody<'a> {
    /// The account.
    pub account: &'a str,
}

/// A split's body: the legs that replace the counter leg, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitBody<'a> {
    /// The legs; they must sum to the counter leg's quantity.
    pub legs: Vec<SplitLeg<'a>>,
}

/// One leg of a [`SplitBody`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitLeg<'a> {
    /// Where this part goes.
    pub account: &'a str,
    /// How much, in the counter leg's smallest units and with its sign.
    pub qty: i64,
}

/// A transfer link's body: its two sides' record ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkBody<'a> {
    /// One side's record id: for a suggested link, the side money left.
    pub a: &'a str,
    /// The other side's.
    pub b: &'a str,
}

/// A stored override.
pub type OverrideNote<'a> = EditNote<'a, OverrideBody<'a>>;
/// A stored split.
pub type SplitNote<'a> = EditNote<'a, SplitBody<'a>>;
/// A stored transfer link.
pub type LinkNote<'a> = EditNote<'a, LinkBody<'a>>;

/// Why an edit can't be stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// An account the edit names is not a valid account name.
    #[error("'{account}' isn't a valid account: {error}")]
    BadAccount {
        account: String,
        error: ledgerdb::AccountError,
    },
    /// A split with no legs.
    #[error("a split needs at least one leg")]
    NoLegs,
    /// A link from a transaction to itself.
    #[error("a transfer links two different transactions")]
    LinkSelf,
    /// A record id a link can't name: `|` separates the two in its `d`.
    #[error("record id '{0}' has a '|' in it, which a transfer link can't name")]
    LinkPipe(String),
}

/// `account` if it is a valid account name.
pub(crate) fn check_account(account: &str) -> Result<(), EditError> {
    match Accounts::new().intern(account) {
        Ok(_) => Ok(()),
        Err(error) => Err(EditError::BadAccount {
            account: account.to_owned(),
            error,
        }),
    }
}

impl<'a, B: EditBody<'a>> EditNote<'a, B> {
    /// A live edit with `body`, written by `agentium` (if from a session).
    pub fn new(body: B, agentium: Option<&'a str>) -> Self {
        EditNote {
            body: Some(body),
            agentium,
        }
    }

    /// The revision that clears the edit, written by `agentium`.
    pub fn cleared(agentium: Option<&'a str>) -> Self {
        EditNote {
            body: None,
            agentium,
        }
    }

    /// Check the edit can be stored. A clear always can.
    pub fn check(&self) -> Result<(), EditError> {
        self.body.as_ref().map_or(Ok(()), B::check)
    }
}

impl<'a, B: EditBody<'a>> Record<'a> for EditNote<'a, B> {
    const KIND: u32 = B::KIND;
    const PREFIX: &'static str = B::PREFIX;

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        tags = match &self.body {
            Some(body) => body.write_tags(tags),
            None => tags.tag("cleared", &["1"]),
        };
        if let Some(agentium) = self.agentium {
            tags = tags.tag("agentium", &[agentium]);
        }
        tags
    }

    /// `None` for another schema version, as for a rule: a newer writer's
    /// edit may mean something this reader would misapply.
    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        if vault::tag_value(tags, "v") != Some(SCHEMA_VERSION) {
            return None;
        }
        let agentium = vault::tag_value(tags, "agentium");
        if vault::tag_value(tags, "cleared") == Some("1") {
            return Some(EditNote::cleared(agentium));
        }
        Some(EditNote::new(B::from_tags(tags)?, agentium))
    }
}

impl<'a> EditBody<'a> for OverrideBody<'a> {
    const KIND: u32 = kinds::OVERRIDE;
    const PREFIX: &'static str = "ov";

    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b> {
        tags.tag("account", &[self.account])
    }

    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        Some(OverrideBody {
            account: vault::tag_value(tags, "account")?,
        })
    }

    fn check(&self) -> Result<(), EditError> {
        check_account(self.account)
    }
}

impl<'a> EditBody<'a> for SplitBody<'a> {
    const KIND: u32 = kinds::SPLIT;
    const PREFIX: &'static str = "split";

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        for leg in &self.legs {
            tags = tags.tag("leg", &[leg.account, &leg.qty.to_string()]);
        }
        tags
    }

    /// `None` when a leg doesn't read, rather than a split missing a leg,
    /// which would never balance anyway.
    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        let mut legs = Vec::new();
        for tag in tags.iter() {
            if tag.count() < 2 || tag.get_str(0) != Some("leg") {
                continue;
            }
            legs.push(SplitLeg {
                account: tag.get_str(1)?,
                qty: tag.get_str(2)?.parse().ok()?,
            });
        }
        (!legs.is_empty()).then_some(SplitBody { legs })
    }

    fn check(&self) -> Result<(), EditError> {
        if self.legs.is_empty() {
            return Err(EditError::NoLegs);
        }
        self.legs
            .iter()
            .try_for_each(|leg| check_account(leg.account))
    }
}

impl<'a> EditBody<'a> for LinkBody<'a> {
    const KIND: u32 = kinds::TRANSFER;
    const PREFIX: &'static str = "xfer";

    fn write_tags<'b>(&self, tags: TagWriter<'b>) -> TagWriter<'b> {
        tags.tag("a", &[self.a]).tag("b", &[self.b])
    }

    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        Some(LinkBody {
            a: vault::tag_value(tags, "a")?,
            b: vault::tag_value(tags, "b")?,
        })
    }

    fn check(&self) -> Result<(), EditError> {
        link_id(self.a, self.b).map(|_| ())
    }
}

impl<'a> LinkBody<'a> {
    /// Whether `record` is one of the link's sides.
    pub fn names(&self, record: &str) -> bool {
        self.a == record || self.b == record
    }

    /// The side that isn't `record`.
    pub fn other(&self, record: &str) -> &'a str {
        if self.a == record { self.b } else { self.a }
    }
}

/// The record id of the link between `a` and `b`: the two sorted and joined
/// by `|`, so either order names the same note.
pub fn link_id(a: &str, b: &str) -> Result<String, EditError> {
    if a == b {
        return Err(EditError::LinkSelf);
    }
    if let Some(bad) = [a, b].into_iter().find(|id| id.contains('|')) {
        return Err(EditError::LinkPipe(bad.to_owned()));
    }
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    Ok(format!("{first}|{second}"))
}

/// Every per-transaction edit in the vault, cleared ones included, each kind
/// in id order.
pub struct VaultEdits<'a> {
    /// The overrides, by record id.
    pub overrides: Vec<Stored<'a, OverrideNote<'a>>>,
    /// The splits, by record id.
    pub splits: Vec<Stored<'a, SplitNote<'a>>>,
    /// The transfer links, by [`link_id`].
    pub links: Vec<Stored<'a, LinkNote<'a>>>,
}

impl<'a> VaultEdits<'a> {
    /// Fold `author`'s edit notes, borrowed from `txn`.
    pub fn load(ndb: &Ndb, txn: &'a Transaction, author: &Pubkey) -> Self {
        VaultEdits {
            overrides: vault::latest_all(ndb, txn, author),
            splits: vault::latest_all(ndb, txn, author),
            links: vault::latest_all(ndb, txn, author),
        }
    }

    /// No edits.
    pub fn empty() -> Self {
        VaultEdits {
            overrides: Vec::new(),
            splits: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Append the live edits to `edits`, each kind in id order, so every
    /// machine holding the same notes derives the same ledger.
    pub fn apply(&self, edits: &mut Edits) {
        edits
            .overrides
            .extend(live(&self.overrides).map(|edit| Override {
                record_id: edit.id.to_owned(),
                account: edit.body.account.to_owned(),
            }));
        edits.splits.extend(live(&self.splits).map(|edit| {
            Split {
                record_id: edit.id.to_owned(),
                legs: edit
                    .body
                    .legs
                    .iter()
                    .map(|leg| Leg {
                        account: leg.account.to_owned(),
                        qty: leg.qty,
                    })
                    .collect(),
            }
        }));
        edits
            .links
            .extend(live(&self.links).map(|edit| TransferLink {
                a: edit.body.a.to_owned(),
                b: edit.body.b.to_owned(),
            }));
    }

    /// Fill in `provenance`'s per-transaction stamps, parallel to what
    /// [`Self::apply`] appends, and its cleared links.
    pub fn stamp(&self, provenance: &mut Provenance) {
        provenance.overrides.extend(stamps(&self.overrides));
        provenance.splits.extend(stamps(&self.splits));
        provenance.links.extend(stamps(&self.links));
        provenance.cleared_links.extend(
            self.links
                .iter()
                .filter(|l| l.body.body.is_none())
                .map(|l| l.id.to_owned()),
        );
    }

    /// The stored override of `record`, cleared or not.
    pub fn override_of(&self, record: &str) -> Option<&Stored<'a, OverrideNote<'a>>> {
        find(&self.overrides, record)
    }

    /// The stored split of `record`, cleared or not.
    pub fn split_of(&self, record: &str) -> Option<&Stored<'a, SplitNote<'a>>> {
        find(&self.splits, record)
    }

    /// The stored link with id `id` ([`link_id`]), cleared or not.
    pub fn link(&self, id: &str) -> Option<&Stored<'a, LinkNote<'a>>> {
        find(&self.links, id)
    }

    /// The live links naming `record`.
    pub fn links_of<'s>(
        &'s self,
        record: &'s str,
    ) -> impl Iterator<Item = &'s Stored<'a, LinkNote<'a>>> + 's {
        self.links
            .iter()
            .filter(move |l| l.body.body.is_some_and(|b| b.names(record)))
    }
}

/// One live edit: its record id and body.
struct Live<'s, 'a, B> {
    id: &'a str,
    body: &'s B,
}

/// The live edits in `stored`.
fn live<'s, 'a, B>(
    stored: &'s [Stored<'a, EditNote<'a, B>>],
) -> impl Iterator<Item = Live<'s, 'a, B>> + 's {
    stored
        .iter()
        .filter_map(|s| s.body.body.as_ref().map(|body| Live { id: s.id, body }))
}

/// The `created_at` of each live edit in `stored`, in [`live`]'s order.
fn stamps<'s, B>(stored: &'s [Stored<'_, EditNote<'_, B>>]) -> impl Iterator<Item = u64> + 's {
    stored
        .iter()
        .filter(|s| s.body.body.is_some())
        .map(|s| s.created_at)
}

/// The record `id` in `stored`, which is sorted by id.
fn find<'s, 'a, T>(stored: &'s [Stored<'a, T>], id: &str) -> Option<&'s Stored<'a, T>> {
    stored
        .binary_search_by(|s| s.id.cmp(id))
        .ok()
        .map(|at| &stored[at])
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use crate::vault::Vault;
    use ledgerdb::{Journal, JournalStore, LayerFilter, Ledger, SegmentHeader, Why, derive};
    use nostrdb::Filter;

    const SECRET: [u8; 32] = [9u8; 32];

    /// Three raw card rows and a chequing row that pays the card.
    const BOOKS: &str = "\
2024-01-02 * Corner Grocer
    ; id: t:1
    liabilities:card  -45.50 CAD
    expenses:unknown

2024-01-03 * Warehouse
    ; id: t:2
    liabilities:card  -100.00 CAD
    expenses:unknown

2024-01-05 * Card payment
    ; id: t:3
    liabilities:card  200.00 CAD
    income:unknown

2024-01-04 * Pay card
    ; id: bank:1
    assets:chequing  -200.00 CAD
    expenses:unknown
";

    /// A raw ledger of [`BOOKS`], and the store holding it.
    struct Raw {
        ledger: Ledger,
        _tmp: tempfile::TempDir,
    }

    fn raw() -> Raw {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        store
            .write_segment(&header, &Journal::parse(BOOKS).journal)
            .unwrap();
        let ledger = Ledger::load(&store, LayerFilter::All).unwrap().ledger;
        Raw { ledger, _tmp: tmp }
    }

    /// Subscribe to every per-transaction edit kind.
    fn subscribe(ndb: &Ndb) -> nostrdb::Subscription {
        let kinds = [kinds::OVERRIDE, kinds::SPLIT, kinds::TRANSFER].map(u64::from);
        ndb.subscribe(&[Filter::new().kinds(kinds).build()])
            .unwrap()
    }

    /// The posting accounts of `record` in the derive of `raw` with `edits`.
    fn accounts_of(raw: &Ledger, edits: &Edits, record: &str) -> Vec<String> {
        let derived = derive(raw, edits);
        let journal = derived.ledger().journal();
        let txn = derived.ledger().find(record).unwrap();
        journal
            .postings_of(journal.txn(txn))
            .iter()
            .map(|p| journal.accounts.full_name(p.account).to_owned())
            .collect()
    }

    /// Each kind survives the vault and changes the derive; clearing it
    /// puts the transaction back.
    #[tokio::test]
    async fn each_edit_round_trips_and_changes_the_derive() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let raw = raw();
        let raw = &raw.ledger;

        let sub = subscribe(ndb);
        let over = OverrideNote::new(
            OverrideBody {
                account: "expenses:food",
            },
            Some("agentium:oak-flat-lamp"),
        );
        let split = SplitNote::new(
            SplitBody {
                legs: vec![
                    SplitLeg {
                        account: "expenses:food",
                        qty: 6000,
                    },
                    SplitLeg {
                        account: "expenses:home",
                        qty: 4000,
                    },
                ],
            },
            None,
        );
        let link = LinkNote::new(
            LinkBody {
                a: "bank:1",
                b: "t:3",
            },
            None,
        );
        let ov = vault.write(ndb, "t:1", &over, 0).unwrap();
        let sp = vault.write(ndb, "t:2", &split, 0).unwrap();
        let id = link_id("t:3", "bank:1").unwrap();
        assert_eq!(id, "bank:1|t:3");
        let xf = vault.write(ndb, &id, &link, 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 3, INGEST_TIMEOUT)
            .await
            .unwrap();

        {
            let txn = Transaction::new(ndb).unwrap();
            let stored = VaultEdits::load(ndb, &txn, vault.author());
            assert_eq!(stored.override_of("t:1").unwrap().body, over);
            assert_eq!(stored.split_of("t:2").unwrap().body, split);
            assert_eq!(stored.link(&id).unwrap().body, link);
            assert_eq!(stored.links_of("t:3").count(), 1);
            assert_eq!(stored.links_of("t:1").count(), 0);

            let mut edits = crate::rules::default_edits();
            stored.apply(&mut edits);
            let derived = derive(raw, &edits);
            assert!(derived.report().rejected.is_empty());
            assert_eq!(derived.explain("t:1"), Some(Why::Override(0)));
            assert_eq!(derived.explain("t:2"), Some(Why::Split(0)));
            assert_eq!(derived.explain("t:3"), Some(Why::Transfer(0)));
            assert_eq!(derived.explain("bank:1"), Some(Why::Transfer(0)));
            assert_eq!(
                accounts_of(raw, &edits, "t:2"),
                ["liabilities:card", "expenses:food", "expenses:home"]
            );
            assert_eq!(
                accounts_of(raw, &edits, "t:1"),
                ["liabilities:card", "expenses:food"]
            );
        }

        // Clear all three: each record goes back to its default.
        let sub = subscribe(ndb);
        vault
            .write(ndb, "t:1", &OverrideNote::cleared(None), ov.created_at)
            .unwrap();
        vault
            .write(ndb, "t:2", &SplitNote::cleared(None), sp.created_at)
            .unwrap();
        vault
            .write(ndb, &id, &LinkNote::cleared(None), xf.created_at)
            .unwrap();
        ndb.wait_for_all_notes_within(sub, 3, INGEST_TIMEOUT)
            .await
            .unwrap();
        let txn = Transaction::new(ndb).unwrap();
        let stored = VaultEdits::load(ndb, &txn, vault.author());
        assert_eq!(stored.override_of("t:1").unwrap().body.body, None);
        assert_eq!(stored.links_of("t:3").count(), 0);
        let mut edits = crate::rules::default_edits();
        stored.apply(&mut edits);
        assert!(edits.overrides.is_empty() && edits.splits.is_empty() && edits.links.is_empty());
        let derived = derive(raw, &edits);
        for record in ["t:1", "t:2", "t:3", "bank:1"] {
            assert_eq!(derived.explain(record), Some(Why::Default), "{record}");
        }
    }

    /// A split that doesn't sum to the counter leg is stored (it is
    /// well-formed) but derive rejects it and the row keeps its default.
    #[test]
    fn an_unbalanced_split_is_reported_not_applied() {
        let raw = raw();
        let raw = &raw.ledger;
        let mut stored = VaultEdits::empty();
        stored.splits.push(Stored {
            id: "t:2",
            created_at: 1,
            body: SplitNote::new(
                SplitBody {
                    legs: vec![SplitLeg {
                        account: "expenses:food",
                        qty: 999,
                    }],
                },
                None,
            ),
        });
        let mut edits = crate::rules::default_edits();
        stored.apply(&mut edits);
        let derived = derive(raw, &edits);
        assert_eq!(derived.explain("t:2"), Some(Why::Default));
        assert_eq!(
            derived.report().rejected[0].problem,
            ledgerdb::Problem::SplitUnbalanced {
                want: 10000,
                got: 999
            }
        );
    }

    #[test]
    fn link_ids_are_canonical() {
        assert_eq!(link_id("b:2", "a:1").unwrap(), "a:1|b:2");
        assert_eq!(link_id("a:1", "b:2").unwrap(), "a:1|b:2");
        assert_eq!(link_id("a:1", "a:1"), Err(EditError::LinkSelf));
        assert!(matches!(link_id("a|1", "b"), Err(EditError::LinkPipe(_))));
        let body = LinkBody { a: "x", b: "y" };
        assert_eq!((body.other("x"), body.other("y")), ("y", "x"));
    }

    #[test]
    fn edits_are_checked_before_they_are_written() {
        let bad = OverrideNote::new(OverrideBody { account: "a::b" }, None);
        assert!(matches!(bad.check(), Err(EditError::BadAccount { .. })));
        let none = SplitNote::new(SplitBody { legs: Vec::new() }, None);
        assert_eq!(none.check(), Err(EditError::NoLegs));
        assert_eq!(OverrideNote::cleared(None).check(), Ok(()));
        let selfish = LinkNote::new(LinkBody { a: "x", b: "x" }, None);
        assert_eq!(selfish.check(), Err(EditError::LinkSelf));
    }

    /// A note of another schema version is skipped, and a cleared revision
    /// reads as cleared whatever else it holds.
    #[test]
    fn other_versions_are_skipped_and_clears_read() {
        let note = |v: &str, cleared: bool| {
            let mut builder = nostrdb::NoteBuilder::new()
                .kind(kinds::OVERRIDE)
                .content("")
                .start_tag()
                .tag_str("d")
                .tag_str("ov:t:1")
                .start_tag()
                .tag_str("v")
                .tag_str(v)
                .start_tag()
                .tag_str("account")
                .tag_str("expenses:food");
            if cleared {
                builder = builder.start_tag().tag_str("cleared").tag_str("1");
            }
            builder.sign(&SECRET).build().unwrap()
        };
        fn read<'a>(note: &nostrdb::Note<'a>) -> Option<OverrideNote<'a>> {
            OverrideNote::from_tags(&note.tags())
        }
        let live = note("1", false);
        assert_eq!(
            read(&live),
            Some(OverrideNote::new(
                OverrideBody {
                    account: "expenses:food"
                },
                None
            ))
        );
        let newer = note("2", false);
        assert_eq!(read(&newer), None);
        let cleared = note("1", true);
        assert_eq!(read(&cleared), Some(OverrideNote::cleared(None)));
    }
}
