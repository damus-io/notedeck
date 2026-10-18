//! What the Trends view shows, worked out from the books: per-month income,
//! expenses and net, any account's monthly flow, and net worth at the end of
//! each month.
//!
//! Every series is one value per month of a [`Window`], oldest first, in the
//! home commodity's smallest units as `f64` (exact for any sum under 2^53
//! cents), which is what `notedeck_ui::charts` draws. Each value's text is
//! formatted here too, once, so the view's tooltips format nothing.
//!
//! These run only when what they depend on changes (the books, the span,
//! today, the accounts picked); the view draws what they returned.

use cashflow::Books;
use cashflow::net::{NetFlow, net_flows};
use ledgerdb::{AccountId, Amount, Bucket, CommodityId, Date, DateRange};

use crate::money::money;

/// How far back the Trends view looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Span {
    /// This month and the eleven before it.
    #[default]
    Year,
    /// This month and the thirty-five before it.
    ThreeYears,
    /// Every month since the journal's first posting.
    AllTime,
}

impl Span {
    /// Every span, in picker order.
    pub const ALL: [Span; 3] = [Span::Year, Span::ThreeYears, Span::AllTime];

    /// The span's name, as the picker shows it.
    pub fn label(self) -> &'static str {
        match self {
            Span::Year => "12 months",
            Span::ThreeYears => "3 years",
            Span::AllTime => "All time",
        }
    }

    /// How many months the span covers, `None` for all of them.
    fn months(self) -> Option<u32> {
        match self {
            Span::Year => Some(12),
            Span::ThreeYears => Some(36),
            Span::AllTime => None,
        }
    }
}

/// The months a [`Span`] covers on a date: whole months, the last one
/// through today, never starting before the journal does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// From the first month's first day through today.
    pub range: DateRange,
    /// The first month, as a [`Date::month_index`].
    pub first_month: u32,
    /// Each month's name, `Oct 2024`, oldest first: one per point of every
    /// series over this window.
    pub labels: Vec<String>,
}

impl Window {
    /// How many months the window covers.
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Whether the window covers no months.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
}

/// The months `span` covers on `today`, cut to start no earlier than
/// `journal` (the days the ledger has), or `None` when the journal is empty
/// or starts after today.
pub fn window(span: Span, today: Date, journal: Option<DateRange>) -> Option<Window> {
    let journal = journal?;
    let last = today.month_index();
    let earliest = journal.start.month_index();
    let first = match span.months() {
        Some(months) => last.saturating_sub(months - 1).max(earliest),
        None => earliest,
    };
    if first > last {
        return None;
    }
    Some(Window {
        range: DateRange::new(
            Date::first_of_month(first),
            Date::from_days(today.days().saturating_add(1)),
        ),
        first_month: first,
        labels: (first..=last).map(month_label).collect(),
    })
}

/// `Oct 2024` for month index `month`.
fn month_label(month: u32) -> String {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let ymd = Date::first_of_month(month).ymd();
    let name = NAMES[usize::from(ymd.month.clamp(1, 12) - 1)];
    format!("{name} {}", ymd.year)
}

/// One series over a [`Window`]: a value per month and its text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    /// One value per month, in the home commodity's smallest units.
    pub values: Vec<f64>,
    /// Each value, formatted.
    pub texts: Vec<String>,
}

impl Line {
    /// A line of `values` (smallest units of `home`), each formatted.
    pub(crate) fn new(values: Vec<i128>, home: CommodityId, books: &Books) -> Self {
        let commodities = &books.ledger().journal().commodities;
        Line {
            texts: values
                .iter()
                .map(|&v| money(v, home, commodities))
                .collect(),
            values: values.into_iter().map(|v| v as f64).collect(),
        }
    }
}

/// Each month's income, expenses and what was left.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Monthly {
    /// Money in under `income`, positive.
    pub income: Line,
    /// Money out under `expenses`, positive.
    pub expenses: Line,
    /// Income less expenses: negative in a month that spent more than it
    /// earned.
    pub net: Line,
    /// The window's income, formatted.
    pub income_total: String,
    /// The window's expenses, formatted.
    pub expenses_total: String,
    /// The window's net, formatted.
    pub net_total: String,
    /// A note about postings left out because the price db had no rate
    /// into the home commodity at their date, when there were any.
    pub unconverted: Option<String>,
}

/// Income, expenses and net for each month of `window`, in `home`, as
/// [`cashflow::net`] defines them.
#[profiling::function]
pub fn monthly(books: &Books, home: CommodityId, window: &Window) -> Monthly {
    let mut flows = net_flows(books, home, window.range, Bucket::Month);
    flows.resize(window.len(), NetFlow::default());
    let income: Vec<i128> = flows.iter().map(|f| f.income).collect();
    let expenses: Vec<i128> = flows.iter().map(|f| f.expenses).collect();
    let net: Vec<i128> = flows.iter().map(NetFlow::net).collect();

    let commodities = &books.ledger().journal().commodities;
    let total = |values: &[i128]| money(values.iter().sum(), home, commodities);
    Monthly {
        income_total: total(&income),
        expenses_total: total(&expenses),
        net_total: total(&net),
        unconverted: unconverted_note(flows.iter().map(|f| f.unconverted).sum()),
        income: Line::new(income, home, books),
        expenses: Line::new(expenses, home, books),
        net: Line::new(net, home, books),
    }
}

/// `account`'s subtree summed per month of `window`, converted to `home` at
/// each posting's date.
fn flow(books: &Books, home: CommodityId, account: AccountId, window: &Window) -> Vec<i128> {
    let series = books
        .query()
        .flows(account, window.range, Bucket::Month, home);
    let mut values: Vec<i128> = series.points.iter().map(|p| p.total).collect();
    values.resize(window.len(), 0);
    values
}

/// One account's monthly flow, for comparing accounts on one chart.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Trend {
    /// The flow per month, signed so that the account's usual direction
    /// reads as positive: an income account's earnings, a card's spending.
    pub line: Line,
    /// The window's total, signed like [`Self::line`] and formatted.
    pub total: String,
    /// Whether the ledger's signs were flipped to get there.
    pub flipped: bool,
}

/// `account`'s subtree summed per month of `window`, in `home`, flipped when
/// it mostly ran negative (income, a liability being paid down) so its line
/// reads above the baseline.
#[profiling::function]
pub fn trend(books: &Books, home: CommodityId, account: AccountId, window: &Window) -> Trend {
    let mut values = flow(books, home, account, window);
    let sum: i128 = values.iter().sum();
    let flipped = sum < 0;
    if flipped {
        values.iter_mut().for_each(|v| *v = -*v);
    }
    let commodities = &books.ledger().journal().commodities;
    Trend {
        total: money(sum.abs(), home, commodities),
        line: Line::new(values, home, books),
        flipped,
    }
}

/// What everything under `assets` and `liabilities` was worth at the end of
/// each month.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetWorth {
    /// The worth at each month's end (today, for this month).
    pub line: Line,
    /// The latest month's worth, formatted.
    pub latest: String,
    /// The commodities besides the home one held at some point, by name.
    /// Each is valued at its price on each month's last day, so the line
    /// moves with its price as well as with what was bought and spent.
    pub others: Vec<String>,
    /// A note about holdings left out because the price db had no rate for
    /// them on a month's last day, when there were any.
    pub unconverted: Option<String>,
}

/// [`NetWorth`] over `window`, in `home`. With `home_only` set, holdings in
/// any other commodity are left out of the line (but still named in
/// [`NetWorth::others`]).
#[profiling::function]
pub fn net_worth(books: &Books, home: CommodityId, window: &Window, home_only: bool) -> NetWorth {
    let journal = books.ledger().journal();
    let roots: Vec<AccountId> = ["assets", "liabilities"]
        .into_iter()
        .filter_map(|name| journal.accounts.lookup(name))
        .collect();
    let query = books.query();
    let today = Date::from_days(window.range.end.days().saturating_sub(1));

    let mut others: Vec<CommodityId> = Vec::new();
    let mut unconverted = 0;
    let mut values = Vec::with_capacity(window.len());
    for month in window.first_month..window.first_month + window.len() as u32 {
        let month_end = Date::from_days(Date::first_of_month(month + 1).days() - 1);
        let at = month_end.min(today);
        let mut worth: i128 = 0;
        for &root in &roots {
            for total in query.balance(root, at, None).totals {
                if total.commodity == home {
                    worth += total.qty;
                    continue;
                }
                if !others.contains(&total.commodity) {
                    others.push(total.commodity);
                }
                if home_only {
                    continue;
                }
                let converted = i64::try_from(total.qty).ok().and_then(|qty| {
                    let amount = Amount {
                        qty,
                        commodity: total.commodity,
                    };
                    books.prices.convert(amount, home, at)
                });
                match converted {
                    Some(amount) => worth += i128::from(amount.qty),
                    None => unconverted += 1,
                }
            }
        }
        values.push(worth);
    }

    NetWorth {
        latest: money(
            values.last().copied().unwrap_or(0),
            home,
            &journal.commodities,
        ),
        others: others
            .into_iter()
            .map(|c| journal.commodities.name(c).to_owned())
            .collect(),
        unconverted: unconverted_note(unconverted),
        line: Line::new(values, home, books),
    }
}

/// What a chart says under it when `count` amounts had no price into the
/// home commodity, or `None` when none were left out.
fn unconverted_note(count: u64) -> Option<String> {
    (count > 0)
        .then(|| format!("{count} amounts had no price into the home commodity and are left out."))
}
