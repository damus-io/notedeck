//! Income, expenses and what was left: the one definition every report
//! uses.
//!
//! - **Income** is −Σ `income:*`: income postings are negative in the
//!   ledger, so flipping them reads as money in.
//! - **Expenses** is Σ `expenses:*`.
//! - **Net** is income less expenses.
//! - `transfers:*` (money moving between the owner's own accounts) is in
//!   neither, and neither is `routed:*`. Routing fees are real CLN revenue,
//!   but they are earned by running a node rather than by work or
//!   spending, so they are kept as their own figure, [`NetFlow::routed`],
//!   and left out of net.
//!
//! The dashboard's Overview and Trends, goals and the Adventure's XP all
//! total through [`net_flow`] or [`net_flows`], so a figure in one is the
//! same figure in another.

use ledgerdb::{Bucket, CommodityId, DateRange, Point, Series};

use crate::Books;

/// The top-level account income is under.
pub const INCOME: &str = "income";

/// The top-level account expenses are under.
pub const EXPENSES: &str = "expenses";

/// The top-level account CLN routing is under (`routed:in`, `routed:out`).
pub const ROUTED: &str = "routed";

/// Money in, money out and routing over some days, in the home commodity's
/// smallest units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetFlow {
    /// Money in under [`INCOME`], positive.
    pub income: i128,
    /// Money out under [`EXPENSES`], positive.
    pub expenses: i128,
    /// What routing earned under [`ROUTED`], positive: fees in less fees
    /// paid. Not in [`Self::net`].
    pub routed: i128,
    /// Postings under any of the three left out because the price db had
    /// no rate into the home commodity at their date.
    pub unconverted: u64,
}

impl NetFlow {
    /// Income less expenses: negative over days that spent more than they
    /// earned.
    pub fn net(&self) -> i128 {
        self.income - self.expenses
    }

    /// How much of income was kept, `net / income`, or `None` when nothing
    /// was earned.
    pub fn savings_rate(&self) -> Option<f64> {
        (self.income > 0).then(|| self.net() as f64 / self.income as f64)
    }
}

/// [`NetFlow`] over `range`, in `home`.
#[profiling::function]
pub fn net_flow(books: &Books, home: CommodityId, range: DateRange) -> NetFlow {
    // A year a bucket keeps an all-time range to a few points.
    let mut total = NetFlow::default();
    for point in net_flows(books, home, range, Bucket::Year) {
        total.income += point.income;
        total.expenses += point.expenses;
        total.routed += point.routed;
        total.unconverted += point.unconverted;
    }
    total
}

/// [`NetFlow`] for each `bucket` of `range`, in `home`, oldest first, one
/// per bucket the range touches (as [`ledgerdb::Query::flows`] cuts it).
/// A root the journal has no account for counts as zero.
#[profiling::function]
pub fn net_flows(
    books: &Books,
    home: CommodityId,
    range: DateRange,
    bucket: Bucket,
) -> Vec<NetFlow> {
    let accounts = &books.ledger().journal().accounts;
    let query = books.query();
    let flows = |name| {
        accounts
            .lookup(name)
            .map(|account| query.flows(account, range, bucket, home))
    };
    let [income, expenses, routed] = [INCOME, EXPENSES, ROUTED].map(flows);
    let len = [&income, &expenses, &routed]
        .iter()
        .filter_map(|series| series.as_ref().map(|s| s.points.len()))
        .max()
        .unwrap_or(0);
    // The point at `i` of a root's series, if the journal has the root.
    let point =
        |series: &Option<Series>, i: usize| series.as_ref().and_then(|s| s.points.get(i)).copied();
    let total = |p: Option<Point>| p.map_or(0, |p| p.total);
    let unconverted = |p: Option<Point>| p.map_or(0, |p| p.unconverted);
    (0..len)
        .map(|i| {
            let [income, expenses, routed] = [&income, &expenses, &routed].map(|s| point(s, i));
            NetFlow {
                income: -total(income),
                expenses: total(expenses),
                routed: -total(routed),
                unconverted: unconverted(income) + unconverted(expenses) + unconverted(routed),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ledgerdb::Date;

    use super::*;
    use crate::test_util::books;

    fn range(start: &str, end: &str) -> DateRange {
        DateRange::new(Date::parse(start).unwrap(), Date::parse(end).unwrap())
    }

    /// Income reads positive, transfers count in neither, and routing is
    /// its own figure, outside net.
    #[test]
    fn transfers_and_routing_are_not_income() {
        let books = books(
            "2024-01-02 * Acme\n  income:salary  -1000.00 CAD\n  assets:checking\n\n\
             2024-01-03 * Grocer\n  expenses:food  250.00 CAD\n  assets:checking\n\n\
             2024-01-04 * To savings\n  transfers  400.00 CAD\n  assets:checking\n\n\
             2024-01-05 * Routed\n  routed:in  -3.00 CAD\n  assets:cln\n\n\
             2024-01-05 * Routed\n  routed:out  1.00 CAD\n  assets:cln\n",
        );
        let home = books.home().unwrap();
        let flow = net_flow(&books, home, range("2024-01-01", "2024-02-01"));
        assert_eq!(
            flow,
            NetFlow {
                income: 100_000,
                expenses: 25_000,
                routed: 200,
                unconverted: 0,
            }
        );
        assert_eq!(flow.net(), 75_000);
        assert_eq!(flow.savings_rate(), Some(0.75));
    }

    /// Per-bucket flows cover every bucket of the range, and their sum is
    /// the range's flow.
    #[test]
    fn buckets_sum_to_the_range() {
        let books = books(
            "2024-01-02 * Acme\n  income:salary  -1000.00 CAD\n  assets:checking\n\n\
             2024-03-03 * Grocer\n  expenses:food  250.00 CAD\n  assets:checking\n",
        );
        let home = books.home().unwrap();
        let months = net_flows(
            &books,
            home,
            range("2024-01-01", "2024-04-01"),
            Bucket::Month,
        );
        assert_eq!(months.len(), 3);
        assert_eq!(months[0].income, 100_000);
        assert_eq!(months[1], NetFlow::default());
        assert_eq!(months[2].expenses, 25_000);
        assert_eq!(months[2].net(), -25_000);
        assert_eq!(months[2].savings_rate(), None);
    }

    /// A journal with no income account earns nothing rather than failing.
    #[test]
    fn a_missing_root_is_zero() {
        let books = books("2024-01-03 * Grocer\n  expenses:food  250.00 CAD\n  assets:checking\n");
        let home = books.home().unwrap();
        let flow = net_flow(&books, home, range("2024-01-01", "2024-02-01"));
        assert_eq!(flow.income, 0);
        assert_eq!(flow.expenses, 25_000);
    }
}
