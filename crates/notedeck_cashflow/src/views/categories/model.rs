//! What the Categories view shows, worked out from the books: one level of
//! the chart of accounts ranked by amount, or a leaf account's payees.
//!
//! [`breakdown`] is pure and runs only when what it depends on changes (the
//! books, the period, the level drilled into); the view draws the
//! [`Breakdown`] it returned on every frame without recomputing or
//! formatting anything.

use std::collections::HashMap;

use cashflow::Books;
use ledgerdb::{AccountId, Bucket, Commodities, CommodityId, DateRange, Filter, Query, StrId};

use crate::money::money;
use crate::period::Resolved;

/// Which way money moves through a column, and so how its sums read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Money out, under `expenses`. Postings there are positive.
    Spending,
    /// Money in, under `income`. Postings there are negative, so the column
    /// flips their sign to show earnings as positive amounts.
    Earning,
}

impl Flow {
    /// Both columns, in the order the view lays them out.
    pub const ALL: [Flow; 2] = [Flow::Spending, Flow::Earning];

    /// The top-level account the column starts from.
    pub fn root_name(self) -> &'static str {
        match self {
            Flow::Spending => "expenses",
            Flow::Earning => "income",
        }
    }

    /// The column's heading.
    pub fn label(self) -> &'static str {
        match self {
            Flow::Spending => "Expenses",
            Flow::Earning => "Income",
        }
    }

    /// What a posting's sum is multiplied by to read as this column's
    /// amount.
    fn sign(self) -> i128 {
        match self {
            Flow::Spending => 1,
            Flow::Earning => -1,
        }
    }
}

/// What a row in a [`Breakdown`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// An account one level below the parent, its amount covering its
    /// subtree. Clicking it drills in.
    Account {
        /// The account.
        account: AccountId,
        /// Whether it has no sub-accounts, so drilling in lists its payees.
        leaf: bool,
    },
    /// A payee of a leaf account's postings.
    Payee,
    /// Postings to the parent account itself rather than to any account
    /// below it.
    Direct,
    /// Everything past the ranked rows, so the shares sum to 100%.
    Other,
}

/// One bar of a [`Breakdown`], with its text already formatted.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// What the row stands for.
    pub kind: RowKind,
    /// The bar's label: an account's last segment, or a payee.
    pub label: String,
    /// The tooltip's subtitle: an account's full name, or the account a
    /// payee was paid from.
    pub detail: String,
    /// The amount in the home commodity, signed so the column's usual
    /// direction is positive (see [`Flow`]).
    pub amount: i128,
    /// `amount / total`.
    pub share: f64,
    /// The amount over the comparison period, when there is one.
    pub previous: Option<i128>,
    /// [`Self::amount`], formatted.
    pub amount_text: String,
    /// [`Self::share`] as a percentage.
    pub share_text: String,
    /// The change from [`Self::previous`], formatted: `+$12.00 (+15%)`.
    pub delta_text: Option<String>,
}

/// One level of a column: its rows, largest first, and their total.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Breakdown {
    /// The ranked rows, then the parent's own postings, then "Other".
    pub rows: Vec<Row>,
    /// The sum of every row, signed like a row's amount.
    pub total: i128,
    /// [`Self::total`], formatted.
    pub total_text: String,
    /// Whether the rows are payees, because the parent is a leaf account.
    pub payees: bool,
    /// Postings left out because the price db had no rate into the home
    /// commodity at their date.
    pub unconverted: u64,
}

/// What [`breakdown`] reads.
#[derive(Clone, Copy)]
pub struct Level<'a> {
    /// The loaded books.
    pub books: &'a Books,
    /// The commodity every amount is converted to.
    pub home: CommodityId,
    /// The account whose children (or, for a leaf, payees) are ranked.
    pub parent: AccountId,
    /// Which column it is, for the sign of its amounts.
    pub flow: Flow,
    /// The days to sum over, and the days to compare with.
    pub period: Resolved,
    /// How many rows to rank before the rest go to "Other".
    pub limit: usize,
}

/// The rows of one level of a column.
#[profiling::function]
pub fn breakdown(level: Level<'_>) -> Breakdown {
    let accounts = &level.books.ledger().journal().accounts;
    if accounts.children(level.parent).is_empty() {
        return payees(level);
    }
    accounts_level(level)
}

/// One sum per row id, for the period and the one compared with it.
#[derive(Default, Clone, Copy)]
pub(crate) struct Sums {
    /// Over the period.
    pub(crate) current: i128,
    /// Over the period compared with it.
    pub(crate) previous: i128,
}

/// The parent's children, ranked, plus its own postings and the tail.
fn accounts_level(level: Level<'_>) -> Breakdown {
    let Level {
        books,
        home,
        parent,
        flow,
        period,
        limit,
    } = level;
    let journal = books.ledger().journal();
    let accounts = &journal.accounts;
    let query = books.query();
    let depth = accounts.depth(parent) + 1;

    let top = query.top_n(parent, depth, period.range, limit, home);
    let subtree = subtree_sum(&query, parent, period.range, home);

    // Every account at the depth over the comparison period, so each ranked
    // row finds its earlier amount whatever its earlier rank.
    let previous = period.previous.map(|range| {
        let all = query.top_n(parent, depth, range, accounts.len(), home);
        let subtree = subtree_sum(&query, parent, range, home);
        let by_account: HashMap<AccountId, i128> =
            all.ranked.iter().map(|r| (r.account, r.amount)).collect();
        Previous {
            total: all.total,
            subtree: subtree.total,
            by_account,
        }
    });

    let sign = flow.sign();
    let total = subtree.total * sign;
    let cx = RowCx {
        total,
        compared: previous.is_some(),
        home,
        commodities: &journal.commodities,
    };
    let mut rows = Vec::with_capacity(top.ranked.len() + 2);
    let mut ranked_sum = 0;
    let mut ranked_previous = 0;
    for ranked in &top.ranked {
        ranked_sum += ranked.amount;
        let earlier = previous.as_ref().map(|p| {
            let amount = p.by_account.get(&ranked.account).copied().unwrap_or(0);
            ranked_previous += amount;
            amount * sign
        });
        let account = ranked.account;
        rows.push(cx.row(
            RowKind::Account {
                account,
                leaf: accounts.children(account).is_empty(),
            },
            accounts.leaf_name(account).to_owned(),
            accounts.full_name(account).to_owned(),
            Sums {
                current: ranked.amount * sign,
                previous: earlier.unwrap_or(0),
            },
        ));
    }

    let direct = subtree.total - top.total;
    if direct != 0 {
        let full = accounts.full_name(parent);
        rows.push(
            cx.row(
                RowKind::Direct,
                format!("{} (direct)", accounts.leaf_name(parent)),
                full.to_owned(),
                Sums {
                    current: direct * sign,
                    previous: previous
                        .as_ref()
                        .map_or(0, |p| (p.subtree - p.total) * sign),
                },
            ),
        );
    }

    let other = top.total - ranked_sum;
    if other != 0 {
        rows.push(
            cx.row(
                RowKind::Other,
                "Other".to_owned(),
                format!("the rest of {}", accounts.full_name(parent)),
                Sums {
                    current: other * sign,
                    previous: previous
                        .as_ref()
                        .map_or(0, |p| (p.total - ranked_previous) * sign),
                },
            ),
        );
    }

    Breakdown {
        rows,
        total,
        total_text: money(total, home, &journal.commodities),
        payees: false,
        unconverted: subtree.unconverted,
    }
}

/// A leaf account's postings, grouped by the transaction's payee.
fn payees(level: Level<'_>) -> Breakdown {
    let Level {
        books,
        home,
        parent,
        flow,
        period,
        limit,
    } = level;
    let journal = books.ledger().journal();
    let mut sums: HashMap<StrId, Sums> = HashMap::new();
    let unconverted = sum_by_payee(books, home, parent, period.range, &mut |payee, qty| {
        sums.entry(payee).or_default().current += qty;
    });
    if let Some(range) = period.previous {
        sum_by_payee(books, home, parent, range, &mut |payee, qty| {
            sums.entry(payee).or_default().previous += qty;
        });
    }

    let sign = flow.sign();
    let mut ranked: Vec<(StrId, Sums)> = sums.into_iter().filter(|(_, s)| s.current != 0).collect();
    let name = |id: StrId| journal.strings.get(id);
    ranked.sort_by(|(a, x), (b, y)| {
        y.current
            .unsigned_abs()
            .cmp(&x.current.unsigned_abs())
            .then_with(|| name(*a).cmp(name(*b)))
    });

    let total: i128 = ranked.iter().map(|(_, s)| s.current).sum::<i128>() * sign;
    let cx = RowCx {
        total,
        compared: period.previous.is_some(),
        home,
        commodities: &journal.commodities,
    };
    let leaf = journal.accounts.full_name(parent);
    let mut rows = Vec::with_capacity(ranked.len().min(limit) + 1);
    let mut rest = Sums::default();
    for (at, (payee, s)) in ranked.into_iter().enumerate() {
        if at >= limit {
            rest.current += s.current;
            rest.previous += s.previous;
            continue;
        }
        let label = match name(payee) {
            "" => "(no payee)".to_owned(),
            payee => payee.to_owned(),
        };
        rows.push(cx.row(
            RowKind::Payee,
            label,
            leaf.to_owned(),
            Sums {
                current: s.current * sign,
                previous: s.previous * sign,
            },
        ));
    }
    if rest.current != 0 {
        rows.push(cx.row(
            RowKind::Other,
            "Other".to_owned(),
            format!("the rest of {leaf}'s payees"),
            Sums {
                current: rest.current * sign,
                previous: rest.previous * sign,
            },
        ));
    }

    Breakdown {
        rows,
        total,
        total_text: money(total, home, &journal.commodities),
        payees: true,
        unconverted,
    }
}

/// Feed the converted sum of each posting to `account` in `range`, keyed by
/// its transaction's payee, to `sink`. Returns how many postings had no rate
/// into `home`.
fn sum_by_payee(
    books: &Books,
    home: CommodityId,
    account: AccountId,
    range: DateRange,
    sink: &mut impl FnMut(StrId, i128),
) -> u64 {
    let postings = books.ledger().postings();
    let filter = Filter {
        account: Some(account),
        range: Some(range),
        ..Filter::default()
    };
    let mut unconverted = 0;
    for found in books.query().register(filter) {
        for posting in &postings[found.txn.postings.range()] {
            if posting.account != account {
                continue;
            }
            let qty = if posting.commodity == home {
                Some(posting.qty)
            } else {
                books
                    .prices
                    .convert(posting.amount(), home, posting.date)
                    .map(|a| a.qty)
            };
            match qty {
                Some(qty) => sink(found.txn.payee, i128::from(qty)),
                None => unconverted += 1,
            }
        }
    }
    unconverted
}

/// The comparison period's sums an accounts level reads.
struct Previous {
    /// Every account at the depth.
    total: i128,
    /// The parent's whole subtree.
    subtree: i128,
    /// Each account at the depth.
    by_account: HashMap<AccountId, i128>,
}

/// A subtree's converted sum over a range.
struct SubtreeSum {
    total: i128,
    unconverted: u64,
}

/// The converted sum of `account`'s subtree over `range`, clamped to the
/// days the ledger has (which [`Query::flows`] needs).
fn subtree_sum(
    query: &Query<'_>,
    account: AccountId,
    range: DateRange,
    home: CommodityId,
) -> SubtreeSum {
    let none = SubtreeSum {
        total: 0,
        unconverted: 0,
    };
    let Some(span) = query.span() else {
        return none;
    };
    let range = DateRange::new(range.start.max(span.start), range.end.min(span.end));
    if range.is_empty() {
        return none;
    }
    let series = query.flows(account, range, Bucket::Year, home);
    SubtreeSum {
        total: series.total(),
        unconverted: series.unconverted.count,
    }
}

/// What every row of one level shares: the level's total, whether it has a
/// comparison period, and how its amounts are written.
pub(crate) struct RowCx<'a> {
    /// The level's total, which each row's share is of.
    pub(crate) total: i128,
    /// Whether the period has one to compare with, so rows carry a delta.
    pub(crate) compared: bool,
    /// The commodity the amounts are in.
    pub(crate) home: CommodityId,
    /// How to write them.
    pub(crate) commodities: &'a Commodities,
}

impl RowCx<'_> {
    /// A row, with its share of the level's total and its text.
    pub(crate) fn row(&self, kind: RowKind, label: String, detail: String, sums: Sums) -> Row {
        let share = if self.total == 0 {
            0.0
        } else {
            sums.current as f64 / self.total as f64
        };
        let previous = self.compared.then_some(sums.previous);
        Row {
            kind,
            label,
            detail,
            amount: sums.current,
            share,
            previous,
            amount_text: money(sums.current, self.home, self.commodities),
            share_text: format!("{:.0}%", share * 100.0),
            delta_text: previous.map(|p| delta(sums.current, p, self.home, self.commodities)),
        }
    }
}

/// The change from `previous` to `current`: `+$12.00 (+15%)`, or `new` when
/// there was nothing before.
fn delta(current: i128, previous: i128, home: CommodityId, commodities: &Commodities) -> String {
    let change = current - previous;
    let sign = if change > 0 { "+" } else { "" };
    let amount = money(change, home, commodities);
    if previous == 0 {
        return format!("{sign}{amount} (new)");
    }
    let percent = change as f64 / previous.unsigned_abs() as f64 * 100.0;
    format!("{sign}{amount} ({sign}{percent:.0}%)")
}
