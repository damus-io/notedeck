//! What the Projects view shows, worked out from the books and the chart's
//! project attribution: each project's revenue over the period, its change
//! from the period before, its revenue per month over the last twelve, and
//! where it came from.
//!
//! ## What counts
//!
//! - A project's **revenue** is what the accounts in it earned under
//!   `income`, plus what the routing accounts in it earned under `routed`
//!   (fees in less fees paid). [`cashflow::net`] keeps routing out of
//!   income; a project that runs a node still earns it, so here it is one
//!   more source, `routing`.
//! - An account is in the project the chart gives it
//!   ([`Projects::project_of`]). Each account's own postings (its subtree's
//!   less its children's) go to its project, so a project tagged inside
//!   another comes out of it, as [`cashflow::chart::Chart::by_project`]
//!   rolls them up.
//! - Income no project claims is the **unattributed** row, listed last.
//! - A project that tags expense accounts too gets its **expenses** and
//!   **net**; one that doesn't shows revenue only.
//! - A project's **sources** are the accounts it earned from, one row per
//!   account with postings of its own, and one `routing` row for all its
//!   routing accounts together.
//!
//! ## The work beside it
//!
//! A project linked to Headway boards ([`crate::boards`]) also gets a
//! [`BoardRow`] from [`boards`]: the cards its boards finished in the
//! period, what sits in In Progress and In Review now, and the cards
//! finished per month over the revenue sparkline's months, so the two
//! lines can be read against each other. A card was finished when it last
//! entered a terminal column ([`cashflow::board_progress`]), dated in the
//! machine's time zone. A project with no linked board gets none.
//!
//! [`projects`] runs only when the books, the attribution, the period or
//! today change, and [`boards`] when those or the boards do; the view
//! draws the [`Report`] and the [`BoardRow`]s they returned, their text
//! already formatted.

use std::collections::HashMap;

use cashflow::Books;
use cashflow::board_progress::BoardProgress;
use cashflow::net::{EXPENSES, INCOME, ROUTED};
use cashflow::projects::Projects;
use cashflow::zone::Zone;
use ledgerdb::{AccountId, Bucket, CommodityId, Date, DateRange};

use crate::Period;
use crate::boards::ProjectLink;
use crate::money::money;
use crate::views::categories::model::{Row, RowCx, RowKind, Sums};
use crate::views::overview::model::{Delta, clip, delta};
use crate::views::trends::model::{Line, Span, Window, window};

/// How many sources a project's mix names before the rest go to "Other".
pub const MIX: usize = 4;

/// The label of a project's routing source.
pub const ROUTING: &str = "routing";

/// The label of the row for income no project claims.
pub const UNATTRIBUTED: &str = "Unattributed";

/// What [`projects`] reads.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The loaded books.
    pub books: &'a Books,
    /// The commodity every amount is converted to.
    pub home: CommodityId,
    /// Which project each account is in.
    pub projects: &'a Projects,
    /// The days to report on.
    pub period: Period,
    /// The date `period` is resolved against.
    pub today: Date,
}

/// Everything the Projects view draws, formatted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// The projects, most revenue first, then the unattributed row when
    /// there is income no project claims.
    pub rows: Vec<ProjectRow>,
    /// The months every row's [`ProjectRow::revenue_line`] covers: the last
    /// twelve the journal has.
    pub window: Option<Window>,
    /// Every row's revenue over the period, formatted.
    pub total_text: String,
    /// Whether any account sets a project at all.
    pub attributed: bool,
    /// The home commodity's decimals, for the charts' axes.
    pub precision: u8,
}

impl Report {
    /// The row for `project` (`None`: the unattributed one), if it has one.
    pub fn row(&self, project: Option<&str>) -> Option<&ProjectRow> {
        self.rows.iter().find(|r| r.project.as_deref() == project)
    }

    /// Where in [`Self::rows`] the row for `project` is, if it has one.
    pub fn position(&self, project: Option<&str>) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.project.as_deref() == project)
    }
}

/// One project's figures.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow {
    /// The project, `None` for the unattributed row.
    pub project: Option<String>,
    /// The project's name, or [`UNATTRIBUTED`].
    pub label: String,
    /// Revenue over the period, in the home commodity's smallest units.
    pub revenue: i128,
    /// [`Self::revenue`], formatted.
    pub revenue_text: String,
    /// [`Self::revenue`]'s share of every row's, as a whole percentage.
    pub share_text: String,
    /// Revenue's change from the period before, when there is one.
    pub vs_previous: Option<Delta>,
    /// Expenses and net, when the project tags expense accounts.
    pub net: Option<ProjectNet>,
    /// Revenue per month over [`Report::window`].
    pub revenue_line: Line,
    /// [`Self::revenue_line`]'s values, for the sparkline.
    pub spark: Vec<f32>,
    /// Expenses per month over [`Report::window`], when the project tags
    /// expense accounts.
    pub expenses_line: Option<Line>,
    /// Where the revenue came from over the period, largest first: a row
    /// per income account ([`RowKind::Account`], which drills into the
    /// Categories view) and one for routing (drawn as [`RowKind::Direct`],
    /// which doesn't).
    pub sources: Vec<Row>,
    /// The sources that earned, as shares of a bar: the [`MIX`] largest,
    /// then "Other" for the rest.
    pub mix: Vec<MixPart>,
}

/// A project's spending and what it kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNet {
    /// Expenses over the period, formatted.
    pub expenses_text: String,
    /// Revenue less expenses.
    pub net: i128,
    /// [`Self::net`], formatted.
    pub net_text: String,
}

/// One slice of a project's revenue mix.
#[derive(Debug, Clone, PartialEq)]
pub struct MixPart {
    /// The source, or "Other".
    pub label: String,
    /// Its share of what the project's sources earned, 0 to 1.
    pub share: f32,
    /// [`Self::share`] as a whole percentage.
    pub share_text: String,
}

/// The Projects report on `inputs.books` over `inputs.period`.
#[profiling::function]
pub fn projects(inputs: Inputs<'_>) -> Report {
    let Inputs {
        books,
        home,
        projects,
        period,
        today,
    } = inputs;
    let journal = books.ledger().journal();
    let commodities = &journal.commodities;
    let mut report = Report {
        attributed: !projects.is_empty(),
        precision: commodities.precision(home),
        total_text: money(0, home, commodities),
        ..Report::default()
    };
    let Some(span) = books.query().span() else {
        return report;
    };
    let resolved = period.resolve(today);
    let window = window(Span::Year, today, Some(span));
    let ranges = Ranges {
        period: clip(resolved.range, span),
        previous: resolved
            .previous
            .filter(|previous| previous.end > span.start)
            .map(|previous| clip(previous, span)),
        months: window.as_ref().map(|w| w.range),
        month_count: window.as_ref().map_or(0, Window::len),
    };

    let groups = group(books, home, projects, &ranges);
    let total: i128 = groups.iter().map(|g| g.revenue.period).sum();
    let percent = |part: i128, whole: i128| {
        let share = if whole == 0 {
            0.0
        } else {
            part as f64 / whole as f64
        };
        format!("{:.0}%", share * 100.0)
    };
    report.rows = groups
        .into_iter()
        .map(|g| {
            let revenue = g.revenue.period;
            let row_cx = RowCx {
                total: revenue,
                compared: ranges.previous.is_some(),
                home,
                commodities,
            };
            let sources = g.sources(books, projects, &row_cx);
            ProjectRow {
                label: g.project.unwrap_or(UNATTRIBUTED).to_owned(),
                project: g.project.map(str::to_owned),
                revenue,
                revenue_text: money(revenue, home, commodities),
                share_text: percent(revenue, total),
                vs_previous: ranges
                    .previous
                    .map(|_| delta(revenue - g.revenue.previous, home, books)),
                net: g.tags_expenses.then(|| {
                    let net = revenue - g.expenses.period;
                    ProjectNet {
                        expenses_text: money(g.expenses.period, home, commodities),
                        net,
                        net_text: money(net, home, commodities),
                    }
                }),
                spark: g.revenue.months.iter().map(|&v| v as f32).collect(),
                revenue_line: Line::new(g.revenue.months, home, books),
                expenses_line: g
                    .tags_expenses
                    .then(|| Line::new(g.expenses.months, home, books)),
                mix: mix(&sources),
                sources,
            }
        })
        .collect();
    report.total_text = money(total, home, commodities);
    report.window = window;
    report
}

/// The days each figure covers.
struct Ranges {
    /// The period, from the journal's first day.
    period: DateRange,
    /// The period before it, when the journal reaches back that far.
    previous: Option<DateRange>,
    /// The months of the per-month lines, when the journal has any.
    months: Option<DateRange>,
    /// How many months [`Self::months`] covers.
    month_count: usize,
}

/// One subtree's (or one account's own) sums over each of [`Ranges`],
/// signed so money in is positive for revenue and money out for expenses.
#[derive(Debug, Clone, Default)]
struct Amounts {
    period: i128,
    previous: i128,
    months: Vec<i128>,
}

impl Amounts {
    /// `account`'s subtree over `ranges`, as the ledger signs it.
    fn subtree(books: &Books, home: CommodityId, account: AccountId, ranges: &Ranges) -> Self {
        let query = books.query();
        let total = |range: DateRange| query.flows(account, range, Bucket::Year, home).total();
        let mut months: Vec<i128> = match ranges.months {
            Some(range) => query
                .flows(account, range, Bucket::Month, home)
                .points
                .iter()
                .map(|p| p.total)
                .collect(),
            None => Vec::new(),
        };
        months.resize(ranges.month_count, 0);
        Amounts {
            period: total(ranges.period),
            previous: ranges.previous.map_or(0, total),
            months,
        }
    }

    /// Add `other` times `sign` to these sums.
    fn add(&mut self, other: &Amounts, sign: i128) {
        self.period += other.period * sign;
        self.previous += other.previous * sign;
        if self.months.len() < other.months.len() {
            self.months.resize(other.months.len(), 0);
        }
        for (into, from) in self.months.iter_mut().zip(&other.months) {
            *into += from * sign;
        }
    }

    /// Whether every sum is zero.
    fn is_zero(&self) -> bool {
        self.period == 0 && self.previous == 0 && self.months.iter().all(|&m| m == 0)
    }
}

/// Which root an account is under, and so how its sums read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// `income`: postings are negative, revenue is their negation.
    Income,
    /// `routed`: likewise, and every account is the one `routing` source.
    Routed,
    /// `expenses`: postings are positive, and count only for a project
    /// that tags expense accounts.
    Expenses,
}

impl Root {
    /// Every root, with its top-level account name.
    const ALL: [(Root, &'static str); 3] = [
        (Root::Income, INCOME),
        (Root::Routed, ROUTED),
        (Root::Expenses, EXPENSES),
    ];

    /// What a posting's sum is multiplied by to read as revenue or
    /// spending.
    fn sign(self) -> i128 {
        match self {
            Root::Income | Root::Routed => -1,
            Root::Expenses => 1,
        }
    }
}

/// A source of one project's revenue, before it is formatted into a
/// [`Row`].
struct Source {
    /// The income account, or `None` for the routing source.
    account: Option<AccountId>,
    /// Its own sums.
    amounts: Amounts,
}

/// One project's sums (or the unattributed income's), before formatting.
struct Group<'p> {
    project: Option<&'p str>,
    revenue: Amounts,
    expenses: Amounts,
    /// Whether any expense account is in the project.
    tags_expenses: bool,
    sources: Vec<Source>,
}

impl Group<'_> {
    /// This group's sources as rows, largest first, the ones that earned
    /// nothing in either period left out.
    fn sources(&self, books: &Books, projects: &Projects, cx: &RowCx<'_>) -> Vec<Row> {
        let accounts = &books.ledger().journal().accounts;
        let mut sources: Vec<&Source> = self
            .sources
            .iter()
            .filter(|s| s.amounts.period != 0 || s.amounts.previous != 0)
            .collect();
        // An account's name below the account that put it in the project
        // (below `income` for the unattributed), or its last segment when
        // it is that account.
        let label = |s: &Source| {
            let Some(account) = s.account else {
                return ROUTING;
            };
            let full = accounts.full_name(account);
            let root = projects
                .root_of(full)
                .map_or(INCOME, |r| r.account.as_str());
            full.strip_prefix(root)
                .and_then(|rest| rest.strip_prefix(':'))
                .unwrap_or_else(|| accounts.leaf_name(account))
        };
        sources.sort_by(|a, b| {
            b.amounts
                .period
                .cmp(&a.amounts.period)
                .then_with(|| label(a).cmp(label(b)))
        });
        sources
            .into_iter()
            .map(|s| {
                let (kind, detail) = match s.account {
                    Some(account) => (
                        RowKind::Account {
                            account,
                            leaf: accounts.children(account).is_empty(),
                        },
                        accounts.full_name(account),
                    ),
                    None => (RowKind::Direct, ROUTED),
                };
                cx.row(
                    kind,
                    label(s).to_owned(),
                    detail.to_owned(),
                    Sums {
                        current: s.amounts.period,
                        previous: s.amounts.previous,
                    },
                )
            })
            .collect()
    }
}

/// Every account's own sums under each [`Root`], grouped by project:
/// projects by revenue (largest first, then by name), the unattributed
/// income last, groups with nothing to show left out.
fn group<'p>(
    books: &Books,
    home: CommodityId,
    projects: &'p Projects,
    ranges: &Ranges,
) -> Vec<Group<'p>> {
    let accounts = &books.ledger().journal().accounts;
    let mut groups: Vec<Group<'p>> = Vec::new();
    for (root, name) in Root::ALL {
        let Some(top) = accounts.lookup(name) else {
            continue;
        };
        // Every account under the root, with its subtree's sums, so each
        // one's own sums are its subtree's less its children's.
        let mut subtree: HashMap<AccountId, Amounts> = HashMap::new();
        let mut stack = vec![top];
        while let Some(account) = stack.pop() {
            stack.extend_from_slice(accounts.children(account));
            subtree.insert(account, Amounts::subtree(books, home, account, ranges));
        }
        let mut order: Vec<AccountId> = subtree.keys().copied().collect();
        order.sort_by(|a, b| accounts.full_name(*a).cmp(accounts.full_name(*b)));
        for account in order {
            let mut own = Amounts::default();
            own.add(&subtree[&account], root.sign());
            for child in accounts.children(account) {
                own.add(&subtree[child], -root.sign());
            }
            let project = projects.project_of(accounts.full_name(account));
            if root == Root::Expenses && project.is_none() {
                continue;
            }
            let at = match groups.iter().position(|g| g.project == project) {
                Some(at) => at,
                None => {
                    groups.push(Group {
                        project,
                        revenue: Amounts::default(),
                        expenses: Amounts::default(),
                        tags_expenses: false,
                        sources: Vec::new(),
                    });
                    groups.len() - 1
                }
            };
            let group = &mut groups[at];
            match root {
                Root::Expenses => {
                    group.tags_expenses = true;
                    group.expenses.add(&own, 1);
                }
                Root::Income => {
                    group.revenue.add(&own, 1);
                    if !own.is_zero() {
                        group.sources.push(Source {
                            account: Some(account),
                            amounts: own,
                        });
                    }
                }
                Root::Routed => {
                    group.revenue.add(&own, 1);
                    match group.sources.iter_mut().find(|s| s.account.is_none()) {
                        Some(routing) => routing.amounts.add(&own, 1),
                        None => group.sources.push(Source {
                            account: None,
                            amounts: own,
                        }),
                    }
                }
            }
        }
    }
    groups.retain(|g| !g.revenue.is_zero() || (g.project.is_some() && !g.expenses.is_zero()));
    groups.sort_by(|a, b| {
        a.project
            .is_none()
            .cmp(&b.project.is_none())
            .then(b.revenue.period.cmp(&a.revenue.period))
            .then(a.project.cmp(&b.project))
    });
    groups
}

/// `sources`' shares of what they earned: the [`MIX`] largest that earned
/// anything, then "Other" for the rest. Empty when nothing earned.
fn mix(sources: &[Row]) -> Vec<MixPart> {
    let earned: i128 = sources.iter().map(|r| r.amount.max(0)).sum();
    if earned <= 0 {
        return Vec::new();
    }
    let part = |label: &str, amount: i128| {
        let share = amount as f64 / earned as f64;
        MixPart {
            label: label.to_owned(),
            share: share as f32,
            share_text: format!("{:.0}%", share * 100.0),
        }
    };
    let mut positive = sources.iter().filter(|r| r.amount > 0);
    let mut parts: Vec<MixPart> = positive
        .by_ref()
        .take(MIX)
        .map(|r| part(&r.label, r.amount))
        .collect();
    let rest: i128 = positive.map(|r| r.amount).sum();
    if rest > 0 {
        parts.push(part("Other", rest));
    }
    parts
}

/// What [`boards`] reads.
#[derive(Clone, Copy)]
pub struct BoardInputs<'a> {
    /// The report the rows line up with.
    pub report: &'a Report,
    /// Which boards each project links.
    pub links: &'a [ProjectLink],
    /// The linked boards' progress.
    pub progress: &'a [BoardProgress],
    /// The days "done" counts over.
    pub period: Period,
    /// The date `period` is resolved against.
    pub today: Date,
}

/// One project's work on its Headway boards, formatted.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardRow {
    /// The linked boards' slugs, comma separated.
    pub boards_text: String,
    /// Cards finished in the period, across the boards.
    pub done: usize,
    /// Cards in an In Progress column now.
    pub in_progress: usize,
    /// Cards in an In Review column now.
    pub in_review: usize,
    /// `12 done · 3 in progress · 2 in review`.
    pub stats_text: String,
    /// Cards finished per month over [`Report::window`], for the sparkline.
    pub spark: Vec<f32>,
    /// The linked slugs no board folded for, when there are any:
    /// `not found: damus-android`.
    pub missing_text: Option<String>,
    /// Each board's own figures, in link order, for the detail.
    pub lines: Vec<BoardLine>,
}

/// One board's figures, formatted.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardLine {
    /// The board's slug.
    pub board: String,
    /// `4 done · 1 in progress · 0 in review`.
    pub stats_text: String,
}

/// What a tile's sparkline tooltip says the board line counts.
pub const BOARD_HELP: &str = "Cards finished per month on the linked Headway boards, last 12 months. \
A card is finished when it enters a terminal column (In Review or Done, as the board marks them), \
dated by its move history.";

/// The board figures for each of `inputs.report`'s rows, in its order:
/// `None` for a row whose project links no board (and for the
/// unattributed row).
#[profiling::function]
pub fn boards(inputs: BoardInputs<'_>) -> Vec<Option<BoardRow>> {
    let BoardInputs {
        report,
        links,
        progress,
        period,
        today,
    } = inputs;
    let range = period.resolve(today).range;
    report
        .rows
        .iter()
        .map(|row| {
            let project = row.project.as_deref()?;
            let link = links.iter().find(|l| l.project == project)?;
            Some(board_row(link, progress, range, report.window.as_ref()))
        })
        .collect()
}

/// `link`'s boards' figures from `progress`, done counted over `range`
/// and per month over `window`.
fn board_row(
    link: &ProjectLink,
    progress: &[BoardProgress],
    range: DateRange,
    window: Option<&Window>,
) -> BoardRow {
    let mut row = BoardRow {
        boards_text: link.boards.join(", "),
        done: 0,
        in_progress: 0,
        in_review: 0,
        stats_text: String::new(),
        spark: vec![0.0; window.map_or(0, Window::len)],
        missing_text: None,
        lines: Vec::new(),
    };
    let mut missing: Vec<&str> = Vec::new();
    for slug in &link.boards {
        let mut found = false;
        // A slug names the account's own board and any shared with it.
        for board in progress.iter().filter(|b| &b.board == slug) {
            found = true;
            let dates = board.completed.iter().map(|&at| Zone::Local.date(at));
            let mut done = 0;
            for date in dates {
                done += usize::from(range.contains(date));
                let Some(window) = window else {
                    continue;
                };
                let month = date.month_index().checked_sub(window.first_month);
                let slot = month.and_then(|m| row.spark.get_mut(m as usize));
                if let Some(slot) = slot {
                    *slot += 1.0;
                }
            }
            row.done += done;
            row.in_progress += board.in_progress;
            row.in_review += board.in_review;
            row.lines.push(BoardLine {
                board: board.board.clone(),
                stats_text: stats(done, board.in_progress, board.in_review),
            });
        }
        if !found {
            missing.push(slug);
        }
    }
    row.stats_text = stats(row.done, row.in_progress, row.in_review);
    if !missing.is_empty() {
        row.missing_text = Some(format!("not found: {}", missing.join(", ")));
    }
    row
}

/// `12 done · 3 in progress · 2 in review`.
fn stats(done: usize, in_progress: usize, in_review: usize) -> String {
    format!("{done} done · {in_progress} in progress · {in_review} in review")
}
