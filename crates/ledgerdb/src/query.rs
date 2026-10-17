//! Folds over a ledger: balances, flows over time, top categories and a
//! filtered register.
//!
//! [`Rollups`] is an index built once per ledger, after every load and every
//! derive: the account tree laid out so each subtree is one contiguous run,
//! and per-account, per-month, per-commodity sums of the postings, each
//! also converted to a home commodity at its own posting's date. A
//! [`Query`] pairs it with the ledger and price db it was built from and
//! answers from it:
//!
//! - **Rollups first.** A query over whole months, natively or in the home
//!   commodity, reads one cell per account and month instead of every
//!   posting. Only the partial months at either end of a range are scanned,
//!   and each account's postings are sorted by date, so finding them is a
//!   binary search. Day and week buckets scan the postings in range, reading
//!   each one's home-commodity value, converted once at build. Conversion
//!   to anything but the home commodity converts as it scans, which is
//!   several times slower.
//! - **Exact either way.** A cell's converted sum is the sum of its
//!   postings, each converted and rounded at its own date, so the rollup and
//!   the scan give the same answer to the unit, and so does a naive fold
//!   over every posting.
//! - **Unpriced is reported, not dropped.** A posting the price db cannot
//!   convert at its date is left out of the converted total and counted in
//!   [`Unconverted`], natively.
//! - **No allocation proportional to the corpus.** Results are a handful of
//!   totals, one point per bucket, or `n` ranked accounts; the register is a
//!   lazy iterator.
//!
//! A subtree means an account and all its descendants: `expenses` covers
//! `expenses:food:coffee`. Amounts are sums of integer smallest units, kept
//! as `i128` so no realistic sum overflows.

use crate::{AccountId, CommodityId, Date, Ledger, PriceDb, Rate, StrId, Txn, TxnId};

/// The half-open span of days `start..end`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DateRange {
    /// The first day in the range.
    pub start: Date,
    /// The first day after the range.
    pub end: Date,
}

impl DateRange {
    /// The days `start..end`.
    pub fn new(start: Date, end: Date) -> Self {
        Self { start, end }
    }

    /// Every day up to and including `at`.
    pub fn through(at: Date) -> Self {
        Self {
            start: Date::EPOCH,
            end: Date::from_days(at.days().saturating_add(1)),
        }
    }

    /// Whether the range holds no days.
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Whether `date` is in the range.
    pub fn contains(self, date: Date) -> bool {
        self.start <= date && date < self.end
    }

    /// The days in both ranges.
    fn intersect(self, other: DateRange) -> DateRange {
        DateRange {
            start: self.start.max(other.start),
            end: self.end.min(other.end),
        }
    }
}

/// A sum of one commodity, in its smallest units.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Total {
    /// The commodity.
    pub commodity: CommodityId,
    /// The sum, in the ledger's precision for the commodity.
    pub qty: i128,
}

/// Postings a converting query could not convert, because the price db had
/// no path between the commodities at the posting's date.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unconverted {
    /// How many postings.
    pub count: u64,
    /// Their sums in their own commodities, sorted by commodity id.
    pub native: Vec<Total>,
}

impl Unconverted {
    /// Whether every posting was converted.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn add(&mut self, commodity: CommodityId, qty: i128, count: u32) {
        if count == 0 {
            return;
        }
        self.count += u64::from(count);
        add_total(&mut self.native, commodity, qty);
    }

    fn clear(&mut self) {
        self.count = 0;
        self.native.clear();
    }
}

/// What [`Query::balance`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Amounts {
    /// Natively: one non-zero total per commodity, sorted by commodity id.
    /// Converted: exactly one total, in the target commodity.
    pub totals: Vec<Total>,
    /// Postings left out of a converted total. Always empty natively.
    pub unconverted: Unconverted,
}

impl Amounts {
    /// The total in `commodity`, zero when there is none.
    pub fn get(&self, commodity: CommodityId) -> i128 {
        self.totals
            .iter()
            .find(|t| t.commodity == commodity)
            .map_or(0, |t| t.qty)
    }
}

/// How [`Query::flows`] cuts a range into points.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// One point per day.
    Day,
    /// One point per week, weeks starting on Monday.
    Week,
    /// One point per calendar month.
    Month,
    /// One point per calendar year.
    Year,
}

impl Bucket {
    /// The bucket holding `date`, counted from a fixed origin.
    fn index_of_date(self, date: Date) -> u32 {
        match self {
            Bucket::Day => date.days(),
            // 1970-01-01 was a Thursday, three days after a Monday.
            Bucket::Week => (date.days() + 3) / 7,
            Bucket::Month => date.month_index(),
            Bucket::Year => date.month_index() / 12,
        }
    }

    /// The bucket holding month `month` (a [`Date::month_index`]); only
    /// for month and year buckets.
    fn index_of_month(self, month: u32) -> u32 {
        match self {
            Bucket::Month => month,
            Bucket::Year => month / 12,
            Bucket::Day | Bucket::Week => unreachable!("cells only feed month and year buckets"),
        }
    }

    /// The first day of bucket `index`.
    fn start_of(self, index: u32) -> Date {
        match self {
            Bucket::Day => Date::from_days(index),
            Bucket::Week => Date::from_days((index * 7).saturating_sub(3)),
            Bucket::Month => Date::first_of_month(index),
            Bucket::Year => Date::first_of_month(index * 12),
        }
    }

    /// Whether whole months fit in this bucket, so rollup cells can feed it.
    fn takes_months(self) -> bool {
        matches!(self, Bucket::Month | Bucket::Year)
    }
}

/// One bucket of a [`Series`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// The bucket's first day, clipped to the query's range.
    pub start: Date,
    /// The day after the bucket, clipped to the query's range.
    pub end: Date,
    /// The converted sum of the postings in the bucket.
    pub total: i128,
    /// How many postings in the bucket could not be converted.
    pub unconverted: u64,
}

/// What [`Query::flows`] returns: a subtree's per-bucket sums.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Series {
    /// The commodity the totals are in.
    pub commodity: CommodityId,
    /// How the range was cut.
    pub bucket: Bucket,
    /// One point per bucket the range touches, in date order, empty buckets
    /// included.
    pub points: Vec<Point>,
    /// Every posting left out of a point's total.
    pub unconverted: Unconverted,
}

impl Series {
    /// An empty series, to pass to [`Query::flows_into`] and reuse.
    pub fn new(commodity: CommodityId, bucket: Bucket) -> Self {
        Self {
            commodity,
            bucket,
            points: Vec::new(),
            unconverted: Unconverted::default(),
        }
    }

    /// The sum of every point.
    pub fn total(&self) -> i128 {
        self.points.iter().map(|p| p.total).sum()
    }
}

/// One account in a [`TopN`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ranked {
    /// The account; its total covers its whole subtree.
    pub account: AccountId,
    /// Its converted sum over the range.
    pub amount: i128,
    /// `amount / total`: the shares of every account at the depth sum to 1.
    pub share: f64,
}

/// What [`Query::top_n`] returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopN {
    /// The largest accounts by magnitude, largest first, ties by name.
    pub ranked: Vec<Ranked>,
    /// The sum over every account at the depth, ranked or not.
    pub total: i128,
    /// Postings left out of the totals.
    pub unconverted: Unconverted,
}

/// What [`Query::register`] matches. Every field set must hold; the default
/// matches every transaction.
#[derive(Copy, Clone, Debug, Default)]
pub struct Filter<'f> {
    /// A posting to this account's subtree.
    pub account: Option<AccountId>,
    /// The transaction's date is in the range.
    pub range: Option<DateRange>,
    /// The payee contains this text, ignoring ASCII case.
    pub payee: Option<&'f str>,
    /// A `; key: value` entry with this key, on the transaction or a
    /// posting.
    pub meta_key: Option<&'f str>,
    /// The transaction's `; id:` is exactly this.
    pub record_id: Option<&'f str>,
    /// A posting to one of these accounts exactly, such as the derive
    /// step's defaults (`expenses:unknown`), to list what is still
    /// uncategorized.
    pub unknown: Option<&'f [AccountId]>,
}

/// A transaction [`Query::register`] matched.
#[derive(Copy, Clone, Debug)]
pub struct TxnRef<'a> {
    /// Its position in the ledger.
    pub id: TxnId,
    /// The transaction; its postings are in [`Ledger::journal`].
    pub txn: &'a Txn,
}

/// The per-account, per-month sums every [`Query`] reads, and the account
/// tree in subtree order. Build it with [`Rollups::new`] whenever the
/// ledger changes; it is checked against the ledger it is queried with.
#[derive(Debug, Default)]
pub struct Rollups {
    home: Option<CommodityId>,
    /// Every account, each followed by its descendants.
    preorder: Vec<AccountId>,
    /// Each account's position in `preorder`, by account id.
    pre: Vec<u32>,
    /// The end of each account's subtree in `preorder`, by account id.
    subtree_end: Vec<u32>,
    /// `cells[cell_starts[a]..cell_starts[a + 1]]` are account `a`'s own
    /// postings summed by month, then commodity, in that order.
    cell_starts: Vec<u32>,
    cells: Vec<Cell>,
    /// Every posting converted to `home`; empty without one.
    converted: Converted,
    /// The ledger the rollups were built from, by size.
    postings: usize,
    accounts: usize,
}

/// The sum of one account's postings in one commodity in one month.
#[derive(Copy, Clone, Debug)]
struct Cell {
    /// Every posting, natively.
    qty: i128,
    /// The converted sum of the postings the price db could convert to the
    /// home commodity.
    home: i128,
    /// The native sum of the ones it could not.
    unpriced_qty: i128,
    /// A [`Date::month_index`].
    month: u32,
    /// How many could not be converted.
    unpriced: u32,
    commodity: CommodityId,
}

/// Every posting of a ledger converted to one commodity, by posting index,
/// so a scan in the home commodity reads a number instead of converting.
#[derive(Debug, Default)]
struct Converted {
    /// The converted quantity; zero where `unpriced` is set.
    qty: Vec<i64>,
    /// One bit per posting, set when the price db could not convert it.
    unpriced: Vec<u64>,
}

/// The rate a [`Converted`] build last looked up for one commodity.
#[derive(Copy, Clone)]
struct DayRate {
    date: Option<Date>,
    rate: Option<Rate>,
}

impl Converted {
    /// Convert every posting in `ledger` to `home` at its date. Postings
    /// are sorted by date, so each commodity's rate is looked up once a
    /// day rather than once a posting.
    fn new(ledger: &Ledger, prices: &PriceDb, home: CommodityId) -> Converted {
        let postings = ledger.postings();
        let mut out = Converted {
            qty: vec![0; postings.len()],
            unpriced: vec![0; postings.len().div_ceil(64)],
        };
        let none = DayRate {
            date: None,
            rate: None,
        };
        let mut rates = vec![none; ledger.journal().commodities.len()];
        for (at, p) in postings.iter().enumerate() {
            let day = &mut rates[p.commodity.index()];
            if day.date != Some(p.date) {
                day.date = Some(p.date);
                day.rate = prices.rate(p.commodity, home, p.date);
            }
            let converted = day
                .rate
                .and_then(|r| prices.convert_at(p.amount(), home, r));
            match converted {
                Some(c) => out.qty[at] = c.qty,
                None => out.unpriced[at / 64] |= 1 << (at % 64),
            }
        }
        out
    }

    /// Posting `at` converted, or `None` when it could not be.
    fn get(&self, at: usize) -> Option<i64> {
        let unpriced = self.unpriced[at / 64] & (1 << (at % 64)) != 0;
        (!unpriced).then(|| self.qty[at])
    }
}

impl Rollups {
    /// Index `ledger`, converting each posting to `home` at its date with
    /// `prices` (no conversion when `None`). Converted queries in `home`
    /// read the index; queries in any other commodity scan.
    #[profiling::function]
    pub fn new(ledger: &Ledger, prices: &PriceDb, home: Option<CommodityId>) -> Rollups {
        let accounts = &ledger.journal().accounts;
        let n = accounts.len();
        let mut rollups = Rollups {
            home,
            preorder: Vec::with_capacity(n),
            pre: vec![0; n],
            subtree_end: vec![0; n],
            cell_starts: Vec::with_capacity(n + 1),
            cells: Vec::new(),
            converted: match home {
                Some(home) => Converted::new(ledger, prices, home),
                None => Converted::default(),
            },
            postings: ledger.postings().len(),
            accounts: n,
        };
        for &root in accounts.roots() {
            rollups.visit(ledger, root);
        }

        let postings = ledger.postings();
        let mut month: Vec<Cell> = Vec::new();
        for account in accounts.ids() {
            rollups.cell_starts.push(rollups.cells.len() as u32);
            let mut current = 0;
            let mut month_end = Date::EPOCH;
            for &at in ledger.account_postings(account) {
                let p = postings[at as usize];
                if p.date >= month_end {
                    rollups.flush(&mut month);
                    current = p.date.month_index();
                    month_end = Date::first_of_month(current + 1);
                }
                let slot = match month.iter().position(|c| c.commodity == p.commodity) {
                    Some(at) => at,
                    None => {
                        month.push(Cell {
                            qty: 0,
                            home: 0,
                            unpriced_qty: 0,
                            month: current,
                            unpriced: 0,
                            commodity: p.commodity,
                        });
                        month.len() - 1
                    }
                };
                let cell = &mut month[slot];
                cell.qty += i128::from(p.qty);
                if home.is_none() {
                    continue;
                }
                match rollups.converted.get(at as usize) {
                    Some(qty) => cell.home += i128::from(qty),
                    None => {
                        cell.unpriced += 1;
                        cell.unpriced_qty += i128::from(p.qty);
                    }
                }
            }
            rollups.flush(&mut month);
        }
        rollups.cell_starts.push(rollups.cells.len() as u32);
        rollups
    }

    /// Lay out `account`'s subtree in preorder.
    fn visit(&mut self, ledger: &Ledger, account: AccountId) {
        self.pre[account.index()] = self.preorder.len() as u32;
        self.preorder.push(account);
        for &child in ledger.journal().accounts.children(account) {
            self.visit(ledger, child);
        }
        self.subtree_end[account.index()] = self.preorder.len() as u32;
    }

    /// Move one account-month's cells into the index, by commodity.
    fn flush(&mut self, month: &mut Vec<Cell>) {
        month.sort_unstable_by_key(|c| c.commodity);
        self.cells.append(month);
    }

    /// The commodity converted sums are kept in.
    pub fn home(&self) -> Option<CommodityId> {
        self.home
    }

    /// How many account-month-commodity cells the index holds.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Query `ledger`, the one these rollups were built from, converting
    /// with `prices`, the price db they were built with.
    ///
    /// # Panics
    ///
    /// If `ledger` has a different number of postings or accounts than the
    /// one the rollups were built from: rebuild them after every change.
    pub fn query<'a>(&'a self, ledger: &'a Ledger, prices: &'a PriceDb) -> Query<'a> {
        assert!(
            ledger.postings().len() == self.postings
                && ledger.journal().accounts.len() == self.accounts,
            "rollups were built from a different ledger; rebuild them after every load or derive"
        );
        Query {
            rollups: self,
            ledger,
            prices,
        }
    }

    /// `root` and its descendants.
    fn subtree(&self, root: AccountId) -> &[AccountId] {
        let Some(&start) = self.pre.get(root.index()) else {
            return &[];
        };
        &self.preorder[start as usize..self.subtree_end[root.index()] as usize]
    }

    /// Whether `account` is `root` or one of its descendants.
    fn in_subtree(&self, root: AccountId, account: AccountId) -> bool {
        let at = self.pre[account.index()];
        self.pre[root.index()] <= at && at < self.subtree_end[root.index()]
    }

    /// Account `account`'s cells in months `months.start..months.end`.
    fn cells(&self, account: AccountId, months: MonthRange) -> &[Cell] {
        let at = account.index();
        let all = &self.cells[self.cell_starts[at] as usize..self.cell_starts[at + 1] as usize];
        let lo = all.partition_point(|c| c.month < months.start);
        let hi = all.partition_point(|c| c.month < months.end);
        &all[lo..hi]
    }
}

/// Months `start..end`, as [`Date::month_index`] values.
#[derive(Copy, Clone)]
struct MonthRange {
    start: u32,
    end: u32,
}

/// What a fold sums.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Measure {
    /// Native quantities.
    Native,
    /// Quantities converted to this commodity.
    In(CommodityId),
}

/// When a [`Piece`] happened.
#[derive(Copy, Clone)]
enum When {
    /// Some time in a month, a [`Date::month_index`].
    Month(u32),
    /// On a day.
    Day(Date),
}

/// One contribution to a fold: a rollup cell or a single posting.
#[derive(Copy, Clone)]
struct Piece {
    when: When,
    commodity: CommodityId,
    /// Natively.
    qty: i128,
    /// Converted, of the part that could be; zero natively.
    converted: i128,
    /// Natively, of the part that could not be converted.
    unpriced_qty: i128,
    unpriced: u32,
}

/// [`Rollups`] paired with the ledger and price db they index. Cheap to
/// make; make one per frame or per batch of queries.
#[derive(Copy, Clone)]
pub struct Query<'a> {
    rollups: &'a Rollups,
    ledger: &'a Ledger,
    prices: &'a PriceDb,
}

impl<'a> Query<'a> {
    /// The ledger being queried.
    pub fn ledger(&self) -> &'a Ledger {
        self.ledger
    }

    /// From the first posting's day through the last's, or `None` for an
    /// empty ledger. A natural range for an all-time [`Self::flows`].
    pub fn span(&self) -> Option<DateRange> {
        let postings = self.ledger.postings();
        Some(DateRange {
            start: postings.first()?.date,
            end: Date::from_days(postings.last()?.date.days() + 1),
        })
    }

    /// The balance of `account`'s subtree at the end of day `at`: native
    /// totals per commodity, or with `commodity` set, one total with each
    /// posting converted at its own date.
    #[profiling::function]
    pub fn balance(&self, account: AccountId, at: Date, commodity: Option<CommodityId>) -> Amounts {
        let measure = commodity.map_or(Measure::Native, Measure::In);
        let mut out = Amounts::default();
        let mut converted = 0;
        self.fold(
            account,
            DateRange::through(at),
            measure,
            true,
            &mut |p| match measure {
                Measure::Native => add_total(&mut out.totals, p.commodity, p.qty),
                Measure::In(_) => {
                    converted += p.converted;
                    out.unconverted.add(p.commodity, p.unpriced_qty, p.unpriced);
                }
            },
        );
        match commodity {
            Some(commodity) => out.totals.push(Total {
                commodity,
                qty: converted,
            }),
            None => out.totals.retain(|t| t.qty != 0),
        }
        out
    }

    /// The sum of `account`'s subtree in each `bucket` of `range`, converted
    /// to `commodity`: the data of a cashflow-over-time chart. `range`
    /// sets the points, empty ones included, so keep it to real dates;
    /// [`Self::span`] is all time.
    pub fn flows(
        &self,
        account: AccountId,
        range: DateRange,
        bucket: Bucket,
        commodity: CommodityId,
    ) -> Series {
        let mut out = Series::new(commodity, bucket);
        self.flows_into(account, range, &mut out);
        out
    }

    /// [`Self::flows`] into `out`, reusing its buffers; the commodity and
    /// bucket are `out`'s.
    #[profiling::function]
    pub fn flows_into(&self, account: AccountId, range: DateRange, out: &mut Series) {
        out.points.clear();
        out.unconverted.clear();
        if range.is_empty() {
            return;
        }
        let bucket = out.bucket;
        let first = bucket.index_of_date(range.start);
        let last = bucket.index_of_date(Date::from_days(range.end.days() - 1));
        out.points.extend((first..=last).map(|index| Point {
            start: bucket.start_of(index).max(range.start),
            end: bucket.start_of(index + 1).min(range.end),
            total: 0,
            unconverted: 0,
        }));
        let Series {
            commodity,
            points,
            unconverted,
            ..
        } = out;
        let measure = Measure::In(*commodity);
        self.fold(account, range, measure, bucket.takes_months(), &mut |p| {
            let index = match p.when {
                When::Month(month) => bucket.index_of_month(month),
                When::Day(date) => bucket.index_of_date(date),
            };
            let point = &mut points[(index - first) as usize];
            point.total += p.converted;
            point.unconverted += u64::from(p.unpriced);
            unconverted.add(p.commodity, p.unpriced_qty, p.unpriced);
        });
    }

    /// The `n` largest accounts at tree depth `depth` under `parent` over
    /// `range`, converted to `commodity`. Depth counts from the top:
    /// `expenses` at depth 2 ranks `expenses:food`, at depth 3
    /// `expenses:food:coffee`. Each account's amount covers its subtree;
    /// postings to accounts above `depth` are in neither the ranking nor
    /// the total.
    #[profiling::function]
    pub fn top_n(
        &self,
        parent: AccountId,
        depth: u16,
        range: DateRange,
        n: usize,
        commodity: CommodityId,
    ) -> TopN {
        let accounts = &self.ledger.journal().accounts;
        let mut out = TopN {
            ranked: Vec::with_capacity(n + 1),
            ..TopN::default()
        };
        let subtree = self.rollups.subtree(parent);
        let mut at = 0;
        while let Some(&account) = subtree.get(at) {
            if accounts.depth(account) != depth {
                at += 1;
                continue;
            }
            let mut amount = 0;
            let unconverted = &mut out.unconverted;
            self.fold(account, range, Measure::In(commodity), true, &mut |p| {
                amount += p.converted;
                unconverted.add(p.commodity, p.unpriced_qty, p.unpriced);
            });
            // Skip the rest of this account's subtree: its amount covers it.
            at += self.rollups.subtree(account).len();
            out.total += amount;
            if amount == 0 || n == 0 {
                continue;
            }
            let ranked = Ranked {
                account,
                amount,
                share: 0.0,
            };
            let before = |r: &Ranked| {
                r.amount.unsigned_abs() > amount.unsigned_abs()
                    || (r.amount.unsigned_abs() == amount.unsigned_abs()
                        && accounts.full_name(r.account) < accounts.full_name(account))
            };
            let slot = out.ranked.partition_point(before);
            if slot < n {
                out.ranked.insert(slot, ranked);
                out.ranked.truncate(n);
            }
        }
        for r in &mut out.ranked {
            r.share = if out.total == 0 {
                0.0
            } else {
                r.amount as f64 / out.total as f64
            };
        }
        out
    }

    /// The transactions `filter` matches, in date order, found lazily.
    pub fn register<'f>(&self, filter: Filter<'f>) -> Register<'a, 'f> {
        let journal = self.ledger.journal();
        let txns = &journal.txns;
        let mut at = 0;
        let mut end = txns.len();
        if let Some(id) = filter.record_id {
            match self.ledger.find(id) {
                Some(txn) => {
                    at = txn.index();
                    end = at + 1;
                }
                None => end = 0,
            }
        }
        if let Some(range) = filter.range {
            at = at.max(txns.partition_point(|t| t.date < range.start));
            end = end.min(txns.partition_point(|t| t.date < range.end));
        }
        let meta_key = match filter.meta_key {
            Some(key) => match journal.strings.lookup(key) {
                Some(key) => Some(key),
                None => {
                    end = 0;
                    None
                }
            },
            None => None,
        };
        Register {
            query: *self,
            filter,
            meta_key,
            at,
            end,
        }
    }

    /// Feed every posting to `root`'s subtree in `range`, measured by
    /// `measure`, to `sink`: as rollup cells for the whole months when
    /// `by_month` allows it and the cells hold the measure, as single
    /// postings otherwise.
    fn fold(
        &self,
        root: AccountId,
        range: DateRange,
        measure: Measure,
        by_month: bool,
        sink: &mut impl FnMut(Piece),
    ) {
        let Some(span) = self.span() else { return };
        // Clamp to real dates, so month arithmetic never meets a sentinel.
        let range = range.intersect(span);
        if range.is_empty() {
            return;
        }
        let subtree = self.rollups.subtree(root);
        let cells_hold = match measure {
            Measure::Native => true,
            Measure::In(c) => self.rollups.home == Some(c),
        };
        if by_month && cells_hold {
            let start = range.start.month_index();
            let months = MonthRange {
                start: start + u32::from(Date::first_of_month(start) != range.start),
                end: range.end.month_index(),
            };
            if months.start < months.end {
                let whole = DateRange {
                    start: Date::first_of_month(months.start),
                    end: Date::first_of_month(months.end),
                };
                for &account in subtree {
                    self.scan(
                        account,
                        DateRange::new(range.start, whole.start),
                        measure,
                        sink,
                    );
                    self.read_cells(account, months, measure, sink);
                    self.scan(account, DateRange::new(whole.end, range.end), measure, sink);
                }
                return;
            }
        }
        // A subtree holding a good share of the ledger is cheaper to find
        // in one pass over the date-sorted postings than account by account.
        let own: usize = subtree
            .iter()
            .map(|&a| self.ledger.account_postings(a).len())
            .sum();
        if own * 4 >= self.ledger.postings().len() {
            self.scan_all(root, range, measure, sink);
            return;
        }
        for &account in subtree {
            self.scan(account, range, measure, sink);
        }
    }

    /// Feed `account`'s own cells in `months` to `sink`.
    fn read_cells(
        &self,
        account: AccountId,
        months: MonthRange,
        measure: Measure,
        sink: &mut impl FnMut(Piece),
    ) {
        for cell in self.rollups.cells(account, months) {
            let converted = measure != Measure::Native;
            sink(Piece {
                when: When::Month(cell.month),
                commodity: cell.commodity,
                qty: cell.qty,
                converted: if converted { cell.home } else { 0 },
                unpriced_qty: if converted { cell.unpriced_qty } else { 0 },
                unpriced: if converted { cell.unpriced } else { 0 },
            });
        }
    }

    /// Feed `account`'s own postings in `range` to `sink`, converting each.
    fn scan(
        &self,
        account: AccountId,
        range: DateRange,
        measure: Measure,
        sink: &mut impl FnMut(Piece),
    ) {
        if range.is_empty() {
            return;
        }
        let postings = self.ledger.postings();
        let own = self.ledger.account_postings(account);
        let lo = own.partition_point(|&at| postings[at as usize].date < range.start);
        let hi = own.partition_point(|&at| postings[at as usize].date < range.end);
        for &at in &own[lo..hi] {
            sink(self.piece(at as usize, measure));
        }
    }

    /// Feed every posting to `root`'s subtree in `range` to `sink`, in one
    /// pass over the postings in range.
    fn scan_all(
        &self,
        root: AccountId,
        range: DateRange,
        measure: Measure,
        sink: &mut impl FnMut(Piece),
    ) {
        let postings = self.ledger.postings();
        let lo = postings.partition_point(|p| p.date < range.start);
        let hi = postings.partition_point(|p| p.date < range.end);
        for (at, p) in postings[lo..hi].iter().enumerate() {
            if self.rollups.in_subtree(root, p.account) {
                sink(self.piece(lo + at, measure));
            }
        }
    }

    /// Posting `at` as a piece, measured by `measure`.
    fn piece(&self, at: usize, measure: Measure) -> Piece {
        let p = self.ledger.postings()[at];
        let qty = i128::from(p.qty);
        let mut piece = Piece {
            when: When::Day(p.date),
            commodity: p.commodity,
            qty,
            converted: 0,
            unpriced_qty: 0,
            unpriced: 0,
        };
        let Measure::In(to) = measure else {
            return piece;
        };
        let converted = if self.rollups.home == Some(to) {
            self.rollups.converted.get(at)
        } else {
            self.prices.convert(p.amount(), to, p.date).map(|c| c.qty)
        };
        match converted {
            Some(qty) => piece.converted = i128::from(qty),
            None => {
                piece.unpriced_qty = qty;
                piece.unpriced = 1;
            }
        }
        piece
    }
}

/// The lazy iterator [`Query::register`] returns.
pub struct Register<'a, 'f> {
    query: Query<'a>,
    filter: Filter<'f>,
    /// The filter's metadata key, interned.
    meta_key: Option<StrId>,
    /// The next transaction to look at.
    at: usize,
    /// One past the last transaction that can match.
    end: usize,
}

impl Register<'_, '_> {
    /// Whether `txn` passes every check the bounds did not already make.
    fn matches(&self, txn: &Txn) -> bool {
        let journal = self.query.ledger.journal();
        let filter = &self.filter;
        let postings = journal.postings_of(txn);
        if let Some(root) = filter.account {
            let rollups = self.query.rollups;
            if !postings.iter().any(|p| rollups.in_subtree(root, p.account)) {
                return false;
            }
        }
        if let Some(unknown) = filter.unknown
            && !postings.iter().any(|p| unknown.contains(&p.account))
        {
            return false;
        }
        if let Some(needle) = filter.payee
            && !contains_ignore_ascii_case(journal.strings.get(txn.payee), needle)
        {
            return false;
        }
        if let Some(key) = self.meta_key
            && !journal.meta_of(txn).iter().any(|m| m.key == key)
        {
            return false;
        }
        true
    }
}

impl<'a> Iterator for Register<'a, '_> {
    type Item = TxnRef<'a>;

    fn next(&mut self) -> Option<TxnRef<'a>> {
        let txns = &self.query.ledger.journal().txns;
        while self.at < self.end {
            let at = self.at;
            self.at += 1;
            let txn = &txns[at];
            if self.matches(txn) {
                return Some(TxnRef {
                    id: TxnId(at as u32),
                    txn,
                });
            }
        }
        None
    }
}

/// Add `qty` of `commodity` to `totals`, kept sorted by commodity.
fn add_total(totals: &mut Vec<Total>, commodity: CommodityId, qty: i128) {
    match totals.binary_search_by_key(&commodity, |t| t.commodity) {
        Ok(at) => totals[at].qty += qty,
        Err(at) => totals.insert(at, Total { commodity, qty }),
    }
}

/// Whether `hay` contains `needle`, ignoring ASCII case, without folding
/// either into a new string.
fn contains_ignore_ascii_case(hay: &str, needle: &str) -> bool {
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    needle.is_empty()
        || hay
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}
