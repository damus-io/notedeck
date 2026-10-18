//! The in-memory ledger: every loaded segment merged into one set of sorted,
//! contiguous arrays, which is what every query reads.
//!
//! Loading reads each segment in the [`JournalStore`], skips the ones whose
//! layer the [`LayerFilter`] leaves out, and merges the rest into one
//! [`Journal`] with shared account, commodity and string tables:
//!
//! - **Order.** Transactions are sorted by date, then by segment hash, then
//!   by position in the segment, and postings follow their transactions, so
//!   the posting array is sorted by `(date, txn)` and a date range is a
//!   contiguous run. The order depends only on which segments are loaded,
//!   never on the order they were loaded in.
//! - **Prices** sort by date, then by their segment's `imported` header,
//!   then by segment hash and position, so among prices on one date the last
//!   is the latest import's. [`crate::PriceDb`] keeps the last price of a
//!   date for a pair, which makes a re-fetched rate replace the one it
//!   revises, the way a re-imported transaction replaces its record id. A
//!   `P` directive carries no `; id:`; its date and pair are its identity.
//! - **Record ids.** When two transactions carry the same `; id:`, the one
//!   from the segment with the later `imported` header wins and the other is
//!   left out; equal imports fall back to the larger segment hash, then the
//!   later position in the segment. This is how a re-export that turns a
//!   pending row into a posted one replaces it while edits keyed on the id
//!   keep applying.
//! - **Precision.** Segments are parsed with tables of their own, so the
//!   same commodity can arrive with different precisions. The ledger keeps
//!   the finest one seen and multiplies coarser quantities up to it, which is
//!   exact; it never rounds.
//! - **Style.** A commodity is written in the style of the segment with the
//!   smallest hash that has it, so export does not depend on load order
//!   either.
//!
//! Interned ids ([`crate::AccountId`], [`crate::CommodityId`],
//! [`crate::StrId`]) are assigned in load order and so can differ between
//! two ledgers holding the same segments; compare names, not ids, across
//! ledgers. Strings and accounts used only by a transaction that lost to a
//! later import stay interned, unreferenced.
//!
//! The ledger is never synced and never written to disk. It is rebuilt from
//! the store at startup and grown with [`Ledger::refresh`] or
//! [`Ledger::add_segment`] as segments arrive.

use std::collections::{HashMap, HashSet};

use crate::{
    AccountId, AmountError, CommodityId, Journal, JournalStore, ParseError, Posting, Price,
    Segment, SegmentHash, SegmentHeader, StoreError, StrId, Txn, TxnId,
};

/// Which segment layers a ledger includes, like choosing which files to
/// pass `ledger -f`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LayerFilter {
    /// Every layer.
    #[default]
    All,
    /// Only these layers.
    Only(Vec<String>),
    /// Every layer but these.
    Except(Vec<String>),
}

impl LayerFilter {
    /// Whether segments in `layer` are included.
    pub fn includes(&self, layer: &str) -> bool {
        match self {
            LayerFilter::All => true,
            LayerFilter::Only(layers) => layers.iter().any(|l| l == layer),
            LayerFilter::Except(layers) => !layers.iter().any(|l| l == layer),
        }
    }
}

/// A segment the ledger has seen: its name and header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentInfo {
    /// The segment's hash.
    pub hash: SegmentHash,
    /// The segment's header.
    pub header: SegmentHeader,
}

/// Where a transaction or price came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    /// The segment, as an index into [`Ledger::segments`].
    pub segment: u32,
    /// The row's position in that segment's journal: an index into its
    /// transactions, or into its prices.
    pub index: u32,
}

/// What [`Ledger::add_segment`] did with a segment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Added {
    /// The segment was merged in.
    Loaded,
    /// The segment's layer is filtered out; it is remembered as seen, so a
    /// refresh does not read it again.
    Excluded,
    /// The ledger had already seen the segment, so nothing changed.
    AlreadySeen,
}

/// A segment that could not be merged into the ledger. The ledger is left as
/// it was.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    /// Raising a commodity to a finer precision would overflow an `i64`
    /// quantity, in the ledger or in the segment.
    #[error("segment {segment}: {commodity} at {precision} decimal places overflows")]
    Overflow {
        /// The segment being merged.
        segment: SegmentHash,
        /// The commodity's name.
        commodity: String,
        /// The precision it would have needed.
        precision: u8,
    },
    /// The segment's commodities do not fit in the ledger's table.
    #[error("segment {segment}: {source}")]
    Amount {
        /// The segment being merged.
        segment: SegmentHash,
        /// What the commodity table refused.
        source: AmountError,
    },
}

/// A segment that was loaded although some of its transactions did not
/// parse; those transactions are left out. A segment this crate wrote never
/// has any, so these came from a different writer.
#[derive(Debug)]
pub struct DamagedSegment {
    /// The segment.
    pub hash: SegmentHash,
    /// What did not parse.
    pub errors: Vec<ParseError>,
}

/// What [`Ledger::load`] or [`Ledger::refresh`] did with the store's
/// segments. Loading never stops at a bad segment: it is reported here and
/// tried again on the next refresh.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Segments merged in.
    pub loaded: Vec<SegmentHash>,
    /// Segments whose layer is filtered out.
    pub excluded: Vec<SegmentHash>,
    /// Segments the store has that the ledger was told to leave out
    /// ([`Ledger::load_without`]); none of them was read.
    pub skipped: Vec<SegmentHash>,
    /// Loaded segments that had transactions which did not parse.
    pub damaged: Vec<DamagedSegment>,
    /// Segments that could not be read: corrupt, not text, or with a bad
    /// header.
    pub unreadable: Vec<StoreError>,
    /// Segments that were read but could not be merged.
    pub unmergeable: Vec<MergeError>,
}

/// A ledger loaded from a store, with what loading it found.
#[derive(Debug)]
pub struct Loaded {
    /// The ledger.
    pub ledger: Ledger,
    /// What happened to each segment.
    pub report: LoadReport,
}

/// Every included segment merged into sorted, contiguous arrays.
///
/// The arrays can only be changed by loading; everything else reads them
/// through the accessors.
#[derive(Debug, Default)]
pub struct Ledger {
    /// Transactions sorted by `(date, segment hash, position)`, postings in
    /// the same order, prices sorted by `(date, imported, segment hash,
    /// position)`, and the shared tables.
    /// No file comments.
    journal: Journal,
    /// Where each transaction came from, parallel to `journal.txns`.
    origins: Vec<Origin>,
    /// Where each price came from, parallel to `journal.prices`.
    price_origins: Vec<Origin>,
    /// Loaded segments, sorted by hash.
    segments: Vec<SegmentInfo>,
    /// Segments left out by the layer filter, in the order they were seen.
    excluded: Vec<SegmentInfo>,
    /// Every segment loaded or excluded.
    seen: HashSet<SegmentHash>,
    filter: LayerFilter,
    /// Segments never to read, though the store has them.
    skip: HashSet<SegmentHash>,
    /// The live transaction carrying each record id.
    by_id: HashMap<StrId, TxnId>,
    /// `account_postings[account_starts[a]..account_starts[a + 1]]` are the
    /// postings to account `a`, as indices into `journal.postings`, in
    /// posting order.
    account_starts: Vec<u32>,
    account_postings: Vec<u32>,
    /// The segment each commodity's style was taken from, by commodity id.
    style_from: Vec<Option<SegmentHash>>,
    /// Transactions appended since the last rebuild that lost to another
    /// with the same record id.
    dead: Vec<TxnId>,
    /// Whether rows were appended since the last rebuild.
    dirty: bool,
}

/// How a transaction ranks against another with the same record id: the
/// greater one wins. Field order is the comparison order.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    imported: u64,
    hash: SegmentHash,
    index: u32,
}

/// The sort key of a transaction or price in the rebuilt arrays. Field
/// order is the comparison order.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OrderKey {
    date: crate::Date,
    /// Index into the hash-sorted segments, so it orders like the hash.
    segment: u32,
    index: u32,
}

/// The sort key of a price in the rebuilt arrays: [`OrderKey`] with the
/// segment's import time before its hash, so the latest import of a date
/// comes last. Field order is the comparison order.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PriceKey {
    date: crate::Date,
    imported: u64,
    /// Index into the hash-sorted segments, so it orders like the hash.
    segment: u32,
    index: u32,
}

/// How one of a segment's commodities maps into the ledger's table.
#[derive(Copy, Clone)]
struct ScaledCommodity {
    /// The commodity's id in the ledger.
    id: CommodityId,
    /// The ledger's precision for it once the segment is merged.
    precision: u8,
    /// What to multiply the segment's quantities by to reach the ledger's
    /// precision.
    factor: i64,
}

/// A commodity whose ledger precision a segment raises.
#[derive(Copy, Clone)]
struct Raise {
    id: CommodityId,
    precision: u8,
    /// What to multiply the ledger's existing quantities by.
    factor: i64,
}

impl Ledger {
    /// An empty ledger that will include the layers `filter` allows.
    pub fn new(filter: LayerFilter) -> Self {
        Self {
            filter,
            ..Self::default()
        }
    }

    /// Build a ledger from every segment in `store` whose layer `filter`
    /// includes. Only listing the store can fail; bad segments are reported.
    #[profiling::function]
    pub fn load(store: &JournalStore, filter: LayerFilter) -> Result<Loaded, StoreError> {
        Self::load_without(store, filter, HashSet::new())
    }

    /// [`Self::load`], leaving out the segments in `skip` as if the store
    /// didn't have them: they are never read, here or on a later
    /// [`Self::refresh`], and are listed in [`LoadReport::skipped`].
    ///
    /// A store is write-once, so this is how a reader retires a segment
    /// whose file is still there, such as one another machine retired after
    /// it had been synced here.
    #[profiling::function]
    pub fn load_without(
        store: &JournalStore,
        filter: LayerFilter,
        skip: HashSet<SegmentHash>,
    ) -> Result<Loaded, StoreError> {
        let mut ledger = Ledger {
            skip,
            ..Ledger::new(filter)
        };
        let report = ledger.refresh(store)?;
        Ok(Loaded { ledger, report })
    }

    /// Merge in the segments in `store` that the ledger has not seen yet,
    /// without reading the ones it has, and re-sort once at the end.
    #[profiling::function]
    pub fn refresh(&mut self, store: &JournalStore) -> Result<LoadReport, StoreError> {
        let mut report = LoadReport::default();
        for hash in store.list()? {
            if self.seen.contains(&hash) {
                continue;
            }
            if self.skip.contains(&hash) {
                report.skipped.push(hash);
                continue;
            }
            let mut segment = match store.read(hash) {
                Ok(segment) => segment,
                Err(err) => {
                    report.unreadable.push(err);
                    continue;
                }
            };
            let errors = std::mem::take(&mut segment.errors);
            match self.ingest(segment) {
                Ok(Added::Loaded) => {
                    report.loaded.push(hash);
                    if !errors.is_empty() {
                        report.damaged.push(DamagedSegment { hash, errors });
                    }
                }
                Ok(Added::Excluded) => report.excluded.push(hash),
                Ok(Added::AlreadySeen) => {}
                Err(err) => report.unmergeable.push(err),
            }
        }
        self.rebuild();
        Ok(report)
    }

    /// Merge in one segment, already read, and re-sort. Its
    /// [`Segment::errors`] are not looked at; check them first if they
    /// matter. On an error the ledger is unchanged.
    #[profiling::function]
    pub fn add_segment(&mut self, segment: Segment) -> Result<Added, MergeError> {
        let added = self.ingest(segment)?;
        self.rebuild();
        Ok(added)
    }

    /// The merged journal: the shared tables and the sorted arrays. Its
    /// writer exports the whole ledger as ledger-cli text.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Transactions, sorted by date.
    pub fn txns(&self) -> &[Txn] {
        &self.journal.txns
    }

    /// Postings, sorted by `(date, txn)`.
    pub fn postings(&self) -> &[Posting] {
        &self.journal.postings
    }

    /// Prices, sorted by date, then by import so that on one date the
    /// latest import's comes last.
    pub fn prices(&self) -> &[Price] {
        &self.journal.prices
    }

    /// The indices into [`Self::postings`] of the postings to `account`
    /// itself (not its descendants), in posting order.
    pub fn account_postings(&self, account: AccountId) -> &[u32] {
        let at = account.index();
        let Some(&end) = self.account_starts.get(at + 1) else {
            return &[];
        };
        &self.account_postings[self.account_starts[at] as usize..end as usize]
    }

    /// The postings to `account` itself, in posting order.
    pub fn postings_to(&self, account: AccountId) -> impl Iterator<Item = &Posting> + '_ {
        self.account_postings(account)
            .iter()
            .map(|&at| &self.journal.postings[at as usize])
    }

    /// The transaction carrying record id `id`, if one is loaded.
    pub fn find(&self, id: &str) -> Option<TxnId> {
        let id = self.journal.strings.lookup(id)?;
        self.by_id.get(&id).copied()
    }

    /// Where transaction `txn` came from.
    pub fn origin(&self, txn: TxnId) -> Origin {
        self.origins[txn.index()]
    }

    /// Where the price at `index` in [`Self::prices`] came from.
    pub fn price_origin(&self, index: usize) -> Origin {
        self.price_origins[index]
    }

    /// The segment transaction `txn` came from.
    pub fn segment_of(&self, txn: TxnId) -> &SegmentInfo {
        &self.segments[self.origin(txn).segment as usize]
    }

    /// The loaded segments, sorted by hash.
    pub fn segments(&self) -> &[SegmentInfo] {
        &self.segments
    }

    /// The segments the layer filter left out, so a caller can list the
    /// layers there are to switch on.
    pub fn excluded(&self) -> &[SegmentInfo] {
        &self.excluded
    }

    /// Whether the ledger has loaded or excluded segment `hash`.
    pub fn has_seen(&self, hash: SegmentHash) -> bool {
        self.seen.contains(&hash)
    }

    /// The layers the ledger includes.
    pub fn filter(&self) -> &LayerFilter {
        &self.filter
    }

    /// A ledger with this one's segments, origins and record ids but
    /// `journal`'s rows, for [`crate::derive`]. `journal` must hold this
    /// ledger's transactions in the same order with the same string table;
    /// its postings may differ, and the account index is rebuilt for them.
    pub(crate) fn with_journal(&self, journal: Journal) -> Ledger {
        debug_assert_eq!(journal.txns.len(), self.journal.txns.len());
        let mut ledger = Ledger {
            journal,
            origins: self.origins.clone(),
            price_origins: self.price_origins.clone(),
            segments: self.segments.clone(),
            excluded: self.excluded.clone(),
            seen: self.seen.clone(),
            filter: self.filter.clone(),
            skip: self.skip.clone(),
            by_id: self.by_id.clone(),
            account_starts: Vec::new(),
            account_postings: Vec::new(),
            style_from: self.style_from.clone(),
            dead: Vec::new(),
            dirty: false,
        };
        ledger.index_accounts();
        ledger
    }

    /// Append `segment`'s rows to the unsorted tail of the arrays. Nothing
    /// is changed until every check has passed, apart from interning names
    /// that end up unused.
    fn ingest(&mut self, segment: Segment) -> Result<Added, MergeError> {
        let Segment {
            hash,
            header,
            journal: seg,
            errors: _,
        } = segment;
        if self.seen.contains(&hash) {
            return Ok(Added::AlreadySeen);
        }
        if !self.filter.includes(&header.layer) {
            self.seen.insert(hash);
            self.excluded.push(SegmentInfo { hash, header });
            return Ok(Added::Excluded);
        }

        let mut raises = Vec::new();
        let mut scaled = Vec::with_capacity(seg.commodities.len());
        for c in seg.commodities.ids() {
            let id = self
                .journal
                .commodities
                .intern(seg.commodities.name(c))
                .map_err(|source| MergeError::Amount {
                    segment: hash,
                    source,
                })?;
            // A commodity new to the ledger starts at precision 0 with no
            // postings, so raising it rescales nothing.
            let have = self.journal.commodities.precision(id);
            let want = seg.commodities.precision(c);
            let target = have.max(want);
            if target > have {
                raises.push(Raise {
                    id,
                    precision: target,
                    factor: pow10(target - have),
                });
            }
            scaled.push(ScaledCommodity {
                id,
                precision: target,
                factor: pow10(target - want),
            });
        }

        let overflow = |id: CommodityId, precision: u8| MergeError::Overflow {
            segment: hash,
            commodity: self.journal.commodities.name(id).to_owned(),
            precision,
        };
        let qtys = seg
            .postings
            .iter()
            .map(|p| {
                let to = scaled[p.commodity.index()];
                p.qty
                    .checked_mul(to.factor)
                    .ok_or_else(|| overflow(to.id, to.precision))
            })
            .collect::<Result<Vec<i64>, MergeError>>()?;
        let factors = self
            .check_raises(&raises)
            .map_err(|r| overflow(r.id, r.precision))?;

        // Every check passed: from here on nothing fails.
        if let Some(factors) = factors {
            for posting in &mut self.journal.postings {
                posting.qty *= factors[posting.commodity.index()];
            }
        }
        for raise in &raises {
            self.journal
                .commodities
                .set_precision(raise.id, raise.precision);
        }
        self.style_from.resize(self.journal.commodities.len(), None);
        for (c, to) in seg.commodities.ids().zip(&scaled) {
            let from = &mut self.style_from[to.id.index()];
            if from.is_none_or(|from| hash < from) {
                *from = Some(hash);
                self.journal
                    .commodities
                    .set_style(to.id, seg.commodities.style(c));
            }
        }

        let strings: Vec<StrId> = seg
            .strings
            .ids()
            .map(|s| self.journal.strings.intern(seg.strings.get(s)))
            .collect();
        let accounts: Vec<AccountId> = seg
            .accounts
            .ids()
            .map(|a| {
                self.journal
                    .accounts
                    .intern(seg.accounts.full_name(a))
                    .expect("a segment's account names are valid")
            })
            .collect();
        let str_of = |s: StrId| strings[s.index()];

        let segment_at = self.segments.len() as u32;
        self.segments.push(SegmentInfo { hash, header });
        self.seen.insert(hash);

        let journal = &mut self.journal;
        let txn_base = journal.txns.len() as u32;
        let posting_base = journal.postings.len() as u32;
        let meta_base = journal.meta.len() as u32;
        let comment_base = journal.comments.len() as u32;
        for (at, txn) in seg.txns.iter().enumerate() {
            journal.txns.push(Txn {
                code: txn.code.map(str_of),
                payee: str_of(txn.payee),
                id: txn.id.map(str_of),
                postings: crate::Span {
                    start: txn.postings.start + posting_base,
                    ..txn.postings
                },
                meta: crate::Span {
                    start: txn.meta.start + meta_base,
                    ..txn.meta
                },
                comments: crate::Span {
                    start: txn.comments.start + comment_base,
                    ..txn.comments
                },
                ..*txn
            });
            self.origins.push(Origin {
                segment: segment_at,
                index: at as u32,
            });
        }
        journal
            .postings
            .extend(seg.postings.iter().zip(qtys).map(|(p, qty)| Posting {
                account: accounts[p.account.index()],
                commodity: scaled[p.commodity.index()].id,
                txn: TxnId(p.txn.0 + txn_base),
                qty,
                ..*p
            }));
        journal.meta.extend(seg.meta.iter().map(|m| crate::Meta {
            key: str_of(m.key),
            value: str_of(m.value),
            ..*m
        }));
        journal
            .comments
            .extend(seg.comments.iter().map(|c| crate::Comment {
                text: str_of(c.text),
                ..*c
            }));
        journal.costs.extend(seg.costs.iter().map(|c| crate::Cost {
            posting: c.posting + posting_base,
            commodity: scaled[c.commodity.index()].id,
            ..*c
        }));
        for (at, price) in seg.prices.iter().enumerate() {
            journal.prices.push(Price {
                commodity: scaled[price.commodity.index()].id,
                quote: scaled[price.quote.index()].id,
                ..*price
            });
            self.price_origins.push(Origin {
                segment: segment_at,
                index: at as u32,
            });
        }

        for at in txn_base..self.journal.txns.len() as u32 {
            self.dedup(TxnId(at));
        }
        self.dirty = true;
        Ok(Added::Loaded)
    }

    /// Check that every existing quantity of a raised commodity still fits
    /// after rescaling, and return the per-commodity factors, or `None` when
    /// nothing is raised.
    fn check_raises(&self, raises: &[Raise]) -> Result<Option<Vec<i64>>, Raise> {
        if raises.is_empty() {
            return Ok(None);
        }
        let mut factors = vec![1i64; self.journal.commodities.len()];
        for raise in raises {
            factors[raise.id.index()] = raise.factor;
        }
        for posting in &self.journal.postings {
            let factor = factors[posting.commodity.index()];
            if posting.qty.checked_mul(factor).is_none() {
                let raise = raises.iter().find(|r| r.id == posting.commodity);
                return Err(*raise.expect("only raised commodities have a factor"));
            }
        }
        Ok(Some(factors))
    }

    /// Settle `txn`'s record id against the transaction already holding it,
    /// marking the loser dead.
    fn dedup(&mut self, txn: TxnId) {
        let Some(id) = self.journal.txns[txn.index()].id else {
            return;
        };
        let Some(&held) = self.by_id.get(&id) else {
            self.by_id.insert(id, txn);
            return;
        };
        if self.rank(txn) > self.rank(held) {
            self.by_id.insert(id, txn);
            self.dead.push(held);
        } else {
            self.dead.push(txn);
        }
    }

    /// How `txn` ranks against another with the same record id.
    fn rank(&self, txn: TxnId) -> Rank {
        let origin = self.origins[txn.index()];
        let segment = &self.segments[origin.segment as usize];
        Rank {
            imported: segment.header.imported,
            hash: segment.hash,
            index: origin.index,
        }
    }

    /// Sort the segments by hash, drop dead transactions, sort the rest and
    /// their rows into place, and rebuild the indexes.
    #[profiling::function]
    fn rebuild(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.sort_segments();

        let mut old = std::mem::take(&mut self.journal);
        let mut dead = vec![false; old.txns.len()];
        for txn in self.dead.drain(..) {
            dead[txn.index()] = true;
        }
        let key = |origin: Origin, date| OrderKey {
            date,
            segment: origin.segment,
            index: origin.index,
        };
        let mut order: Vec<u32> = (0..old.txns.len() as u32)
            .filter(|&at| !dead[at as usize])
            .collect();
        order.sort_unstable_by_key(|&at| {
            let at = at as usize;
            key(self.origins[at], old.txns[at].date)
        });

        let mut journal = Journal {
            accounts: std::mem::take(&mut old.accounts),
            commodities: std::mem::take(&mut old.commodities),
            strings: std::mem::take(&mut old.strings),
            txns: Vec::with_capacity(order.len()),
            postings: Vec::with_capacity(old.postings.len()),
            meta: Vec::with_capacity(old.meta.len()),
            comments: Vec::with_capacity(old.comments.len()),
            costs: Vec::with_capacity(old.costs.len()),
            prices: Vec::with_capacity(old.prices.len()),
            file_comments: Vec::new(),
        };
        let mut origins = Vec::with_capacity(order.len());
        for (new, &at) in order.iter().enumerate() {
            let txn = old.txns[at as usize];
            let postings = crate::Span {
                start: journal.postings.len() as u32,
                len: txn.postings.len,
            };
            for from in txn.postings.range() {
                if let Some(cost) = old.cost_of(from as u32) {
                    journal.costs.push(crate::Cost {
                        posting: journal.postings.len() as u32,
                        ..*cost
                    });
                }
                journal.postings.push(Posting {
                    txn: TxnId(new as u32),
                    ..old.postings[from]
                });
            }
            let meta = crate::Span {
                start: journal.meta.len() as u32,
                len: txn.meta.len,
            };
            journal.meta.extend_from_slice(&old.meta[txn.meta.range()]);
            let comments = crate::Span {
                start: journal.comments.len() as u32,
                len: txn.comments.len,
            };
            journal
                .comments
                .extend_from_slice(&old.comments[txn.comments.range()]);
            journal.txns.push(Txn {
                postings,
                meta,
                comments,
                ..txn
            });
            origins.push(self.origins[at as usize]);
        }

        let mut prices: Vec<u32> = (0..old.prices.len() as u32).collect();
        prices.sort_unstable_by_key(|&at| {
            let at = at as usize;
            let origin = self.price_origins[at];
            PriceKey {
                date: old.prices[at].date,
                imported: self.segments[origin.segment as usize].header.imported,
                segment: origin.segment,
                index: origin.index,
            }
        });
        journal
            .prices
            .extend(prices.iter().map(|&at| old.prices[at as usize]));
        self.price_origins = prices
            .iter()
            .map(|&at| self.price_origins[at as usize])
            .collect();

        self.journal = journal;
        self.origins = origins;
        self.index_ids();
        self.index_accounts();
    }

    /// Sort the loaded segments by hash and repoint every origin at their new
    /// positions.
    fn sort_segments(&mut self) {
        let mut order: Vec<usize> = (0..self.segments.len()).collect();
        order.sort_unstable_by_key(|&at| self.segments[at].hash);
        let mut moved_to = vec![0u32; order.len()];
        for (new, &at) in order.iter().enumerate() {
            moved_to[at] = new as u32;
        }
        let mut slots: Vec<Option<SegmentInfo>> = std::mem::take(&mut self.segments)
            .into_iter()
            .map(Some)
            .collect();
        self.segments = order
            .iter()
            .map(|&at| slots[at].take().expect("each segment moves once"))
            .collect();
        for origin in self.origins.iter_mut().chain(&mut self.price_origins) {
            origin.segment = moved_to[origin.segment as usize];
        }
    }

    /// Point each record id at its transaction's new position.
    fn index_ids(&mut self) {
        self.by_id.clear();
        for (at, txn) in self.journal.txns.iter().enumerate() {
            if let Some(id) = txn.id {
                self.by_id.insert(id, TxnId(at as u32));
            }
        }
    }

    /// Group posting indices by account, in posting order.
    fn index_accounts(&mut self) {
        let accounts = self.journal.accounts.len();
        let mut starts = vec![0u32; accounts + 1];
        for posting in &self.journal.postings {
            starts[posting.account.index() + 1] += 1;
        }
        for at in 0..accounts {
            starts[at + 1] += starts[at];
        }
        let mut next = starts.clone();
        let mut grouped = vec![0u32; self.journal.postings.len()];
        for (at, posting) in self.journal.postings.iter().enumerate() {
            let slot = &mut next[posting.account.index()];
            grouped[*slot as usize] = at as u32;
            *slot += 1;
        }
        self.account_starts = starts;
        self.account_postings = grouped;
    }
}

/// `10^exp`; `exp` is at most [`crate::MAX_PRECISION`], so it fits.
fn pow10(exp: u8) -> i64 {
    10i64.pow(u32::from(exp))
}
