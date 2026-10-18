//! What the Overview shows, worked out from the books: the period's net
//! cashflow, income, expenses and savings rate, how net compares with the
//! period before and with the trailing twelve months, net per month over
//! the last twelve, the top categories each way, and what needs attention.
//!
//! Every figure totals through [`cashflow::net`], the definition goals and
//! the Adventure's XP use too. [`overview`] runs only when the books, the
//! period or today change; the view draws the [`Overview`] it returned,
//! its text already formatted.

use cashflow::Books;
use cashflow::net::{NetFlow, net_flow, net_flows};
use ledgerdb::{Bucket, CommodityId, Date, DateRange};

use crate::Period;
use crate::money::money;
use crate::views::categories::model::{Flow, Level, Row, RowKind, breakdown};
use crate::views::trends::model::{Line, Span, Window, window};

/// How many categories each top list ranks.
pub const TOP: usize = 5;

/// What [`overview`] reads.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The loaded books.
    pub books: &'a Books,
    /// The commodity every amount is converted to.
    pub home: CommodityId,
    /// The days to report on.
    pub period: Period,
    /// The date `period` is resolved against.
    pub today: Date,
}

/// Everything the Overview draws, formatted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overview {
    /// The period's income, expenses and routing.
    pub flow: NetFlow,
    /// [`NetFlow::net`], formatted.
    pub net_text: String,
    /// [`NetFlow::income`], formatted.
    pub income_text: String,
    /// [`NetFlow::expenses`], formatted.
    pub expenses_text: String,
    /// [`NetFlow::savings_rate`] as a whole percentage, or a dash when
    /// nothing was earned.
    pub savings_text: String,
    /// [`NetFlow::routed`], formatted, when the journal routes at all.
    pub routed_text: Option<String>,
    /// Net's change from the period before, when the journal reaches back
    /// that far.
    pub vs_previous: Option<Delta>,
    /// Net's difference from what the trailing twelve months averaged over
    /// as many days, when there are such months and the period is not
    /// itself a year or more.
    pub vs_average: Option<Delta>,
    /// Net per month over the last twelve, `None` for an empty journal.
    pub trend: Option<Trend>,
    /// The largest expense categories (children of `expenses`), largest
    /// first, at most [`TOP`].
    pub top_expenses: Vec<Row>,
    /// The largest income categories, likewise.
    pub top_income: Vec<Row>,
    /// "N unpriced postings": postings in the period the price db had no
    /// rate for, when there are any.
    pub unpriced: Option<String>,
    /// "N unknown transactions": what nothing has categorized yet, in the
    /// whole journal, when there is any.
    pub unknown: Option<String>,
    /// Every layer the journal has, loaded or left out, sorted.
    pub layers: Vec<String>,
    /// The home commodity's decimals, for the chart's axis.
    pub precision: u8,
}

/// How net compares with a reference figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// Net less the reference: positive when this period did better.
    pub amount: i128,
    /// [`Self::amount`], formatted with its sign: `+619.89 CAD`.
    pub text: String,
}

/// Net per month over a [`Window`].
#[derive(Debug, Clone, PartialEq)]
pub struct Trend {
    /// The months.
    pub window: Window,
    /// Each month's net.
    pub net: Line,
}

/// The Overview of `inputs.books` over `inputs.period`.
#[profiling::function]
pub fn overview(inputs: Inputs<'_>) -> Overview {
    let Inputs {
        books,
        home,
        period,
        today,
    } = inputs;
    let journal = books.ledger().journal();
    let commodities = &journal.commodities;
    let text = |qty: i128| money(qty, home, commodities);
    let span = books.query().span();
    let resolved = period.resolve(today);
    let flow = match span {
        Some(span) => net_flow(books, home, clip(resolved.range, span)),
        None => NetFlow::default(),
    };
    let net = flow.net();

    let vs_previous = resolved
        .previous
        .zip(span)
        .filter(|(previous, span)| previous.end > span.start)
        .map(|(previous, span)| net - net_flow(books, home, clip(previous, span)).net());
    let vs_average = span.and_then(|span| average(books, home, period, today, span));

    let top = |flow: Flow| {
        let Some(parent) = journal.accounts.lookup(flow.root_name()) else {
            return Vec::new();
        };
        let mut rows = breakdown(Level {
            books,
            home,
            parent,
            flow,
            period: resolved,
            limit: TOP,
        })
        .rows;
        rows.retain(|row| matches!(row.kind, RowKind::Account { .. }));
        rows.truncate(TOP);
        rows
    };

    let layers = crate::layers::known(books);

    let unknown = cashflow::unknown::count(books);
    Overview {
        flow,
        net_text: text(net),
        income_text: text(flow.income),
        expenses_text: text(flow.expenses),
        savings_text: flow
            .savings_rate()
            .map_or_else(|| "—".to_owned(), |rate| format!("{:.0}%", rate * 100.0)),
        routed_text: journal
            .accounts
            .lookup(cashflow::net::ROUTED)
            .map(|_| text(flow.routed)),
        vs_previous: vs_previous.map(|amount| delta(amount, home, books)),
        vs_average: vs_average.map(|reference| delta(net - reference, home, books)),
        trend: window(Span::Year, today, span).map(|window| trend(books, home, window)),
        top_expenses: top(Flow::Spending),
        top_income: top(Flow::Earning),
        unpriced: (flow.unconverted > 0).then(|| plural(flow.unconverted, "unpriced posting")),
        unknown: (unknown > 0).then(|| plural(unknown as u64, "unknown transaction")),
        layers,
        precision: commodities.precision(home),
    }
}

/// What net over `period` would be at the trailing twelve months' daily
/// average: the twelve whole months before this one (cut to start no
/// earlier than the journal's first month), scaled to as many days as the
/// period covers.
///
/// `None` for a period of a year or more, which the average would only
/// restate, and when the journal has no whole month before this one.
fn average(
    books: &Books,
    home: CommodityId,
    period: Period,
    today: Date,
    span: DateRange,
) -> Option<i128> {
    if matches!(period, Period::Last12Months | Period::AllTime) {
        return None;
    }
    let year = Period::Last12Months.resolve(today).range;
    let start = year
        .start
        .max(Date::first_of_month(span.start.month_index()));
    if start >= year.end {
        return None;
    }
    let base = net_flow(books, home, DateRange::new(start, year.end)).net();
    let base_days = i128::from(year.end.days() - start.days());
    let range = clip(period.resolve(today).range, span);
    let days = i128::from(range.end.days().saturating_sub(range.start.days()));
    Some(base * days / base_days)
}

/// The days of `range` from the journal's first day on, so a period that
/// reaches back before the journal (or all time) totals only real days.
/// The end is left alone, so "this month" stays through today.
pub(crate) fn clip(range: DateRange, span: DateRange) -> DateRange {
    let start = range.start.max(span.start);
    let end = if range.end.days() == u32::MAX {
        span.end
    } else {
        range.end
    };
    DateRange::new(start, end.max(start))
}

/// Net per month of `window`.
fn trend(books: &Books, home: CommodityId, window: Window) -> Trend {
    let mut flows = net_flows(books, home, window.range, Bucket::Month);
    flows.resize(window.len(), NetFlow::default());
    let net = flows.iter().map(NetFlow::net).collect();
    Trend {
        net: Line::new(net, home, books),
        window,
    }
}

/// A [`Delta`] of `amount`, signed.
pub(crate) fn delta(amount: i128, home: CommodityId, books: &Books) -> Delta {
    let text = money(amount, home, &books.ledger().journal().commodities);
    Delta {
        amount,
        text: if amount > 0 { format!("+{text}") } else { text },
    }
}

/// `1 unknown transaction`, `12 unknown transactions`.
fn plural(count: u64, noun: &str) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{s}")
}
