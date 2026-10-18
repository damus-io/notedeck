//! The vault folded into what derive needs, kept live with change detection.
//!
//! [`DeriveInputs`] is one fold of every vault record derive depends on:
//! the rules ([`crate::vault_rules`]) compiled, then the overrides, splits
//! and transfer links ([`crate::vault_edits`]) and the account aliases
//! ([`crate::chart`]) appended, all into one owned [`Edits`], beside the
//! segments the manifests retire ([`crate::manifest::retired`]). The CLI
//! folds it once per run.
//!
//! [`EditsCache`] keeps it live for the app. It subscribes to the vault's
//! kinds and, polled from `App::update`, refolds only when the subscription
//! delivered a note, so a quiet frame costs one non-blocking poll. Two
//! counters say what moved:
//!
//! - [`EditsCache::generation`] bumps when the [`DeriveInputs`] differ from
//!   the last fold. The caller re-derives then, off the UI thread, with
//!   [`ledgerdb::rederive`], which reuses the previous derive's rule matches
//!   when only per-transaction edits changed (about 13 ms instead of 90 ms
//!   for 700k postings; see `crates/ledgerdb/tests/bench_derive.rs`).
//! - [`EditsCache::vault_generation`] bumps when the newest revision of any
//!   vault record changed, derive input or not: account metadata, settings
//!   and segment manifests too. The caller re-reads what it draws from
//!   those (the chart, the journal status) then.
//!
//! A note that arrives without changing anything, such as a re-delivered
//! revision or an older one that loses the newest-wins fold, bumps neither.
//!
//! ## Why not `notedeck::RealtimeCache`
//!
//! `RealtimeCache` seeds a reducer once and folds each later note in as a
//! delta. Every vault kind is replaceable, so a delta reducer here would be
//! a hand-rolled newest-per-`d` fold beside the [`query_replaceable`] one
//! every vault read already goes through (CLAUDE.md's rule 20 asks for the
//! latter). It also reports which notes arrived, not whether the fold's
//! output changed, which is what the generation has to mean. What this
//! keeps from it is the ordering discipline: subscribe before the first
//! fold's transaction, and hold back a delivered note until the poll's
//! transaction can see it.

use std::collections::HashSet;
use std::sync::Arc;

use ledgerdb::{Edits, SegmentHash};
use nostrdb::{Filter, Ndb, NoteKey, Subscription, Transaction};
use nostrdb_net::{Pubkey, query_replaceable};

use crate::chart::Chart;
use crate::kinds;
use crate::manifest;
use crate::vault_edits::VaultEdits;
use crate::vault_rules::{Owner, VaultRules};

/// How many delivered keys one `poll_for_notes` call takes at most. 64 is
/// what nostrdb polls into a stack buffer, so a quiet frame's poll
/// allocates nothing.
const POLL_BATCH: u32 = 64;

/// Everything derive takes from the vault, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeriveInputs {
    /// The default counter accounts, the live rules in order, then the
    /// live overrides, splits, links and aliases.
    pub edits: Edits,
    /// `owners[i]` is the vault rule `edits.rules[i]` came from, so a
    /// [`ledgerdb::Why::Rule`] can be traced back to its note.
    pub owners: Vec<Owner>,
    /// The segments the vault retires, which every journal load leaves out.
    pub retired: HashSet<SegmentHash>,
}

impl DeriveInputs {
    /// Fold `author`'s vault records in `txn`.
    #[profiling::function]
    pub fn load(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Self {
        let compiled = VaultRules::load(ndb, txn, author).compile();
        let mut edits = compiled.edits;
        VaultEdits::load(ndb, txn, author).apply(&mut edits);
        Chart::load(ndb, txn, author).apply(&mut edits);
        DeriveInputs {
            edits,
            owners: compiled.owners,
            retired: manifest::retired(&manifest::load(ndb, txn, author)),
        }
    }
}

/// What one [`EditsCache::poll`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The [`DeriveInputs`] changed: re-derive.
    pub inputs: bool,
    /// Some vault record's newest revision changed, derive input or not.
    pub vault: bool,
}

/// One account's vault, folded into [`DeriveInputs`] and kept current from
/// an ndb subscription. See the module docs.
pub struct EditsCache {
    author: Pubkey,
    /// The live subscription to the vault's kinds; `None` when nostrdb
    /// refused it, and then every poll refolds.
    sub: Option<Subscription>,
    /// Delivered keys the last poll's transaction couldn't see yet.
    pending: Vec<NoteKey>,
    /// The newest revision of every vault record, sorted: what the
    /// [`Self::vault_generation`] compares.
    newest: Vec<NoteKey>,
    /// The last fold; `None` until the first poll.
    inputs: Option<Arc<DeriveInputs>>,
    generation: u64,
    vault_generation: u64,
}

impl EditsCache {
    /// A cache of `author`'s vault, subscribed now and folded on the first
    /// [`Self::poll`].
    ///
    /// Create it before opening the transaction that first poll is given:
    /// the fold reads that transaction and the subscription reports only
    /// what is written after it opened, so a note written between the two
    /// would otherwise be in neither.
    pub fn new(ndb: &Ndb, author: Pubkey) -> Self {
        EditsCache {
            sub: ndb.subscribe(&[vault_filter(&author)]).ok(),
            author,
            pending: Vec::new(),
            newest: Vec::new(),
            inputs: None,
            generation: 0,
            vault_generation: 0,
        }
    }

    /// Take what the subscription delivered and refold if any of it is
    /// visible in `txn`. Never blocks: a quiet poll is one queue check.
    ///
    /// The first poll folds the vault as `txn` sees it and reports both
    /// kinds of change.
    #[profiling::function]
    pub fn poll(&mut self, ndb: &Ndb, txn: &Transaction) -> Change {
        let Some(sub) = self.sub else {
            return self.refold(ndb, txn);
        };
        loop {
            let keys = ndb.poll_for_notes(sub, POLL_BATCH);
            let done = keys.len() < POLL_BATCH as usize;
            self.pending.extend(keys);
            if done {
                break;
            }
        }
        let delivered = self.pending.len();
        // A key this snapshot can't see yet stays for the next poll's.
        self.pending
            .retain(|&key| ndb.get_note_by_key(txn, key).is_err());
        let visible = self.pending.len() < delivered;
        if self.inputs.is_some() && !visible {
            return Change::default();
        }
        self.refold(ndb, txn)
    }

    /// Fold the vault in `txn` and bump whichever generations moved.
    fn refold(&mut self, ndb: &Ndb, txn: &Transaction) -> Change {
        let mut newest = query_replaceable(ndb, txn, &[vault_filter(&self.author)]);
        newest.sort_unstable();
        if self.inputs.is_some() && newest == self.newest {
            return Change::default();
        }
        self.newest = newest;
        self.vault_generation += 1;
        let inputs = DeriveInputs::load(ndb, txn, &self.author);
        if self.inputs.as_deref() == Some(&inputs) {
            return Change {
                inputs: false,
                vault: true,
            };
        }
        self.inputs = Some(Arc::new(inputs));
        self.generation += 1;
        Change {
            inputs: true,
            vault: true,
        }
    }

    /// The last fold, shared so a worker can derive from it while the UI
    /// keeps it; `None` before the first [`Self::poll`].
    pub fn inputs(&self) -> Option<&Arc<DeriveInputs>> {
        self.inputs.as_ref()
    }

    /// Bumps each time [`Self::inputs`] changes; 0 before the first fold.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bumps each time any vault record's newest revision changes; 0
    /// before the first fold.
    pub fn vault_generation(&self) -> u64 {
        self.vault_generation
    }

    /// The account whose vault this is.
    pub fn author(&self) -> &Pubkey {
        &self.author
    }
}

/// Every vault record `author` wrote, of every kind cashflow allocates.
///
/// One newest-wins fold over all of them is sound because each kind's
/// records carry their own `d` prefix ([`crate::vault::Record::PREFIX`]),
/// so two kinds never share a `d`.
fn vault_filter(author: &Pubkey) -> Filter {
    Filter::new()
        .authors([author.bytes()])
        .kinds(kinds::ALL.map(u64::from))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    use ledgerdb::{
        Journal, JournalStore, LayerFilter, Ledger, SegmentHeader, Why, derive, rederive,
    };

    use crate::Vault;
    use crate::chart::{AccountMeta, AliasBody, AliasNote};
    use crate::manifest::Manifest;
    use crate::rank;
    use crate::settings::{self, Setting};
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};
    use crate::vault_edits::{
        LinkBody, LinkNote, OverrideBody, OverrideNote, SplitBody, SplitLeg, SplitNote, link_id,
    };
    use crate::vault_rules::RuleNote;

    const SECRET: [u8; 32] = [13u8; 32];
    const OTHER_SECRET: [u8; 32] = [14u8; 32];

    /// Run `write`, wait until `notes` vault records of anybody's have
    /// landed, then poll `cache` once.
    ///
    /// The wait subscribes after `cache` did, and nostrdb fills
    /// subscriptions in the order they were made, under one lock, so once
    /// this one has its notes the cache's queue holds every one of them it
    /// matches.
    async fn poll_after(
        ndb: &Ndb,
        cache: &mut EditsCache,
        notes: u32,
        write: impl FnOnce(),
    ) -> Change {
        let all = Filter::new().kinds(kinds::ALL.map(u64::from)).build();
        let landed = ndb.subscribe(&[all]).unwrap();
        write();
        ndb.wait_for_all_notes_within(landed, notes, INGEST_TIMEOUT)
            .await
            .unwrap();
        let txn = Transaction::new(ndb).unwrap();
        cache.poll(ndb, &txn)
    }

    const BOTH: Change = Change {
        inputs: true,
        vault: true,
    };
    const VAULT_ONLY: Change = Change {
        inputs: false,
        vault: true,
    };

    fn a_rule(rank: &str) -> RuleNote<'_> {
        let mut rule = RuleNote::new("expenses:food", rank);
        rule.patterns.push("grocer".into());
        rule
    }

    fn a_manifest(retired: bool) -> Manifest<'static> {
        Manifest {
            source: "bank",
            layer: "banks",
            imported: 1,
            first: None,
            last: None,
            txns: 0,
            prices: 0,
            bytes: 1,
            host: "alpha",
            retired,
        }
    }

    /// Every kind of derive input bumps the generation once, and the fold
    /// reflects it; metadata and settings bump only the vault's.
    #[tokio::test]
    async fn each_edit_kind_bumps_the_generation() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let mut cache = EditsCache::new(ndb, *vault.author());
        assert_eq!(cache.generation(), 0);
        assert!(cache.inputs().is_none());

        // The first poll folds the empty vault.
        {
            let txn = Transaction::new(ndb).unwrap();
            assert_eq!(cache.poll(ndb, &txn), BOTH);
            assert_eq!(cache.poll(ndb, &txn), Change::default());
        }
        let empty = cache.inputs().unwrap().clone();
        assert_eq!(empty.edits, crate::rules::default_edits());
        assert_eq!(cache.generation(), 1);

        let rank = rank::between(None, None).unwrap();
        let change = poll_after(ndb, &mut cache, 1, || {
            vault
                .write(ndb, "oak-flat-lamp", &a_rule(&rank), 0)
                .unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert_eq!(cache.inputs().unwrap().edits.rules.len(), 1);
        assert_eq!(cache.inputs().unwrap().owners.len(), 1);

        let over = OverrideNote::new(
            OverrideBody {
                account: "expenses:home",
            },
            None,
        );
        let change = poll_after(ndb, &mut cache, 1, || {
            vault.write(ndb, "t:1", &over, 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert_eq!(cache.inputs().unwrap().edits.overrides.len(), 1);

        let split = SplitNote::new(
            SplitBody {
                legs: vec![SplitLeg {
                    account: "expenses:food",
                    qty: 450,
                }],
            },
            None,
        );
        let change = poll_after(ndb, &mut cache, 1, || {
            vault.write(ndb, "t:2", &split, 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert_eq!(cache.inputs().unwrap().edits.splits.len(), 1);

        let link = LinkNote::new(LinkBody { a: "t:3", b: "t:4" }, None);
        let change = poll_after(ndb, &mut cache, 1, || {
            let id = link_id("t:3", "t:4").unwrap();
            vault.write(ndb, &id, &link, 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert_eq!(cache.inputs().unwrap().edits.links.len(), 1);

        let alias = AliasNote::new(
            AliasBody {
                to: "expenses:groceries",
            },
            None,
        );
        let change = poll_after(ndb, &mut cache, 1, || {
            vault.write(ndb, "expenses:food", &alias, 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert_eq!(cache.inputs().unwrap().edits.aliases.len(), 1);

        let hash = SegmentHash::of(b"a segment");
        let change = poll_after(ndb, &mut cache, 1, || {
            manifest::write(&vault, ndb, hash, &a_manifest(true), 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        assert!(cache.inputs().unwrap().retired.contains(&hash));
        assert_eq!(cache.generation(), 7);
        assert_eq!(cache.vault_generation(), 7);

        // Not derive inputs: the vault generation alone moves.
        let meta = AccountMeta {
            icon: Some("🍔"),
            ..AccountMeta::default()
        };
        let change = poll_after(ndb, &mut cache, 1, || {
            vault.write(ndb, "expenses:food", &meta, 0).unwrap();
        })
        .await;
        assert_eq!(change, VAULT_ONLY);
        let change = poll_after(ndb, &mut cache, 1, || {
            settings::set(&vault, ndb, Setting::Home, "USD").unwrap();
        })
        .await;
        assert_eq!(change, VAULT_ONLY);
        let other = SegmentHash::of(b"another segment");
        let change = poll_after(ndb, &mut cache, 1, || {
            manifest::write(&vault, ndb, other, &a_manifest(false), 0).unwrap();
        })
        .await;
        assert_eq!(change, VAULT_ONLY, "a live manifest retires nothing");
        assert_eq!(cache.generation(), 7);
        assert_eq!(cache.vault_generation(), 10);
    }

    /// Another account's vault notes never reach the cache, and a re-save
    /// that changes nothing moves only the vault generation.
    #[tokio::test]
    async fn an_unrelated_or_unchanged_note_does_not_bump() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let other = Vault::new(&OTHER_SECRET).unwrap();
        other.register(ndb);
        let mut cache = EditsCache::new(ndb, *vault.author());
        {
            let txn = Transaction::new(ndb).unwrap();
            cache.poll(ndb, &txn);
        }

        let rank = rank::between(None, None).unwrap();
        let change = poll_after(ndb, &mut cache, 1, || {
            other
                .write(ndb, "oak-flat-lamp", &a_rule(&rank), 0)
                .unwrap();
        })
        .await;
        assert_eq!(change, Change::default());
        assert_eq!((cache.generation(), cache.vault_generation()), (1, 1));

        let mut first = None;
        let change = poll_after(ndb, &mut cache, 1, || {
            first = Some(
                vault
                    .write(ndb, "oak-flat-lamp", &a_rule(&rank), 0)
                    .unwrap(),
            );
        })
        .await;
        assert_eq!(change, BOTH);
        let after = first.unwrap().created_at;
        let change = poll_after(ndb, &mut cache, 1, || {
            vault
                .write(ndb, "oak-flat-lamp", &a_rule(&rank), after)
                .unwrap();
        })
        .await;
        assert_eq!(change, VAULT_ONLY);
        assert_eq!((cache.generation(), cache.vault_generation()), (2, 3));
    }

    /// A derive after the generation moves reflects the edit, and an
    /// override re-derives from the previous result's rule matches.
    #[tokio::test]
    async fn a_derive_after_a_change_reflects_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JournalStore::new(tmp.path());
        let header = SegmentHeader {
            source: "t".to_owned(),
            imported: 1,
            layer: "t".to_owned(),
        };
        let text = "2024-01-02 * Corner Grocer\n  ; id: t:1\n  liabilities:card  -4.50 CAD\n  expenses:unknown\n\n\
                    2024-01-03 * Corner Grocer\n  ; id: t:2\n  liabilities:card  -9.00 CAD\n  expenses:unknown\n";
        store
            .write_segment(&header, &Journal::parse(text).journal)
            .unwrap();
        let raw = Ledger::load(&store, LayerFilter::All).unwrap().ledger;

        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);
        let mut cache = EditsCache::new(ndb, *vault.author());

        let rank = rank::between(None, None).unwrap();
        let change = poll_after(ndb, &mut cache, 1, || {
            vault
                .write(ndb, "oak-flat-lamp", &a_rule(&rank), 0)
                .unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        let ruled = derive(&raw, &cache.inputs().unwrap().edits);
        assert_eq!(ruled.explain("t:1"), Some(Why::Rule(0)));

        let over = OverrideNote::new(
            OverrideBody {
                account: "expenses:home",
            },
            None,
        );
        let change = poll_after(ndb, &mut cache, 1, || {
            vault.write(ndb, "t:1", &over, 0).unwrap();
        })
        .await;
        assert_eq!(change, BOTH);
        let overridden = rederive(&raw, &cache.inputs().unwrap().edits, &ruled);
        assert!(overridden.reused_rule_matches());
        assert_eq!(overridden.explain("t:1"), Some(Why::Override(0)));
        assert_eq!(overridden.explain("t:2"), Some(Why::Rule(0)));
        let home = overridden
            .ledger()
            .journal()
            .accounts
            .lookup("expenses:home");
        assert_eq!(overridden.ledger().postings_to(home.unwrap()).count(), 1);
    }
}
