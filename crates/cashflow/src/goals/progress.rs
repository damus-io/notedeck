//! How a goal is doing: [`evaluate`] measures a [`GoalSpec`] against the
//! books and says whether it is on track.
//!
//! Everything is read from the per-month rollups ([`ledgerdb::Query::flows`]
//! with [`Bucket::Month`]), so an evaluation is a handful of index reads and
//! the same books give the same answer on every machine.
//!
//! - **The measure.** A month's sums under the goal's scope are kept as
//!   [`Parts`]: income and expenses as [`crate::net`] defines them, and a
//!   plain ledger-signed sum for a balance. The metric reads its value off
//!   them, so a goal and the dashboard total the same way. A window's value
//!   adds the parts first, so a savings rate over a year is the year's net
//!   over the year's income, not an average of monthly rates.
//! - **current** is this month's value (`per: month`), the year to date's
//!   (`per: year`), or the all-time cumulative value (`per: total`; for a
//!   balance, the balance).
//! - **projection** fits a least-squares line to the last [`TREND_MONTHS`]
//!   complete months and extends it month by month to the window's end:
//!   the end of the year, `by`, or [`HORIZON_MONTHS`] out. The rest of the
//!   current month follows the line too, except for a `per: month` goal,
//!   whose month-end is the month-to-date run rate prorated by day.
//! - **status** compares the projected value at that end with the target:
//!   meeting it is on track, within [`AT_RISK_MARGIN`] of it at risk, else
//!   behind. A target already met is achieved, except a ceiling (an
//!   expenses goal), which is only achieved once its window has closed.

use std::ops::{Add, AddAssign, Mul, Sub};

use ledgerdb::{AccountId, Bucket, CommodityId, Date, DateRange, Decimal};

use super::{Direction, GoalSpec, Metric, Period, Scope, Target};
use crate::Books;
use crate::net::{EXPENSES, INCOME};
use crate::projects::Projects;

/// How many complete months the projection's trend is fitted to.
pub const TREND_MONTHS: u32 = 3;

/// How close to the target a projection that misses it may land and still
/// be [`Status::AtRisk`] rather than [`Status::Behind`]: 15% of the target.
pub const AT_RISK_MARGIN: f64 = 0.15;

/// How many months [`Progress::history`] goes back, the current one
/// included.
pub const HISTORY_MONTHS: u32 = 12;

/// How far past the current month a goal with no end (`per: total` without
/// `by`, or the chart of a `per: month` one) is projected.
pub const HORIZON_MONTHS: u32 = 12;

/// How far ahead [`Progress::eta`] looks for the projection to meet the
/// target before it gives up and says never.
pub const ETA_SEARCH_MONTHS: u32 = 120;

/// What [`evaluate`] reads.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The derived books.
    pub books: &'a Books,
    /// The chart's project attribution, for a `project:` scope.
    pub projects: &'a Projects,
    /// The day the evaluation is as of; it counts as elapsed.
    pub today: Date,
}

/// Whether a goal is going to make it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The target is met: a floor already reached, or a ceiling whose window
    /// closed under it.
    Achieved,
    /// The projection meets the target by the window's end.
    OnTrack,
    /// The projection misses it by no more than [`AT_RISK_MARGIN`].
    AtRisk,
    /// The projection misses it by more, or a ceiling is already broken, or
    /// the window closed short of it.
    Behind,
}

impl Status {
    /// Its name for a CLI or a label: `achieved`, `on track`, `at risk` or
    /// `behind`.
    pub fn name(self) -> &'static str {
        match self {
            Status::Achieved => "achieved",
            Status::OnTrack => "on track",
            Status::AtRisk => "at risk",
            Status::Behind => "behind",
        }
    }
}

/// What a goal's values are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Smallest units of a commodity (cents, msat).
    Amount(CommodityId),
    /// Percentage points, for a savings rate: 20.0 is 20%.
    Percent,
}

/// The span a goal's target applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Its first day: the month's or the year's, or the first posting's for
    /// a `per: total` goal.
    pub start: Date,
    /// The day after it ends, or `None` for a `per: total` goal with no
    /// `by`.
    pub end: Option<Date>,
}

/// How fast the value has to move, and how fast it is projected to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// What each remaining month of the window has to add to land on the
    /// target: the rest of the gap over the months left (a ceiling's is the
    /// most it may add). `None` for a savings rate, which doesn't add up
    /// month by month, and for a window that has no end or has closed.
    pub required: Option<f64>,
    /// What the projection has the next month add: the month-to-date run
    /// rate for a `per: month` goal, else the trend's next month.
    pub projected: f64,
}

/// One month of a goal's chart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonthValue {
    /// The month's first day.
    pub month: Date,
    /// The goal's measure for it: the month's value for a `per: month`
    /// goal, else the window's value at the month's end (at the evaluation
    /// day for the current month, at `by` for `by`'s month).
    pub value: f64,
}

/// A goal measured against the books.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// What [`Self::target`] and every value are in.
    pub unit: Unit,
    /// Which side of the target is success.
    pub direction: Direction,
    /// The target, in [`Self::unit`].
    pub target: f64,
    /// The span the target applies to.
    pub window: Window,
    /// The day the goal is measured at: the evaluation day, or the last day
    /// before `by` once `by` has passed.
    pub as_of: Date,
    /// Whether the window has closed (`by` has passed), so nothing is
    /// projected and [`Self::current`] is final.
    pub closed: bool,
    /// The value so far.
    pub current: f64,
    /// The projected value at the window's end, or [`HORIZON_MONTHS`] out
    /// when it has none; the current value once it has closed.
    pub projected: f64,
    /// How fast the value needs to move, and is projected to.
    pub pace: Pace,
    /// Whether the projection makes it.
    pub status: Status,
    /// When the projection first meets the target, if it doesn't already
    /// and does within [`ETA_SEARCH_MONTHS`] (for a `per: year` goal, within
    /// the year): the first day through which the cumulative value meets
    /// it, or for a `per: month` goal the first month (by its first day)
    /// whose projected value does, the current month by its run rate.
    pub eta: Option<Date>,
    /// The goal's measure per month, oldest first, up to [`HISTORY_MONTHS`]
    /// back (from January for a `per: year` goal), through the current
    /// month so far.
    pub history: Vec<MonthValue>,
    /// The projected measure per month, from the current month (its
    /// projected end) to the window's end or horizon; empty once closed.
    pub projection: Vec<MonthValue>,
    /// How many complete months the trend was fitted to: fewer than
    /// [`TREND_MONTHS`] when the books are younger, and zero when there
    /// is only the current month, whose run rate then stands in for it.
    pub trend_months: u32,
    /// Postings in scope left out because the price db had no rate into
    /// the goal's commodity at their date.
    pub unconverted: u64,
}

/// Why a goal can't be measured against these books.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    /// The target is in the home commodity, and the books have none.
    #[error("the books have no home commodity for a bare target")]
    NoHomeCommodity,
    /// The target's commodity never appears in the books.
    #[error("no commodity `{0}` in the books")]
    UnknownCommodity(String),
    /// The target has more decimals than the books keep for its commodity.
    #[error("target {target} is finer than {commodity}'s {precision} decimals")]
    TargetTooPrecise {
        /// The target as written.
        target: String,
        /// Its commodity.
        commodity: String,
        /// The decimals the books keep for it.
        precision: u8,
    },
    /// `account:` names no account in the books.
    #[error("no account `{0}` in the books")]
    UnknownAccount(String),
    /// `project:` names a project no account is in.
    #[error("no account is in project `{0}`")]
    UnknownProject(String),
    /// A flow goal's `account:` is outside what its metric totals.
    #[error("a {metric} goal's account must be under {wanted}, not `{account}`")]
    AccountOutsideMetric {
        /// The account.
        account: String,
        /// The metric's name.
        metric: &'static str,
        /// Where its account has to be: `income`, `expenses`, or either.
        wanted: &'static str,
    },
    /// A balance goal with neither `account:` nor `project:`.
    #[error("a balance goal needs an `account` or a `project`")]
    BalanceNeedsScope,
}

/// Measure `spec` against `inputs.books` as of `inputs.today`.
///
/// It reads a few months of rollups per account it totals and builds the
/// chart's series, so a view evaluates once when the books or the goals
/// change and keeps the [`Progress`], rather than calling this every frame.
#[profiling::function]
pub fn evaluate(inputs: &Inputs<'_>, spec: &GoalSpec) -> Result<Progress, EvalError> {
    let books = inputs.books;
    let TargetIn {
        unit,
        commodity,
        value: target,
    } = target_in(books, &spec.target)?;
    let terms = terms(books, inputs.projects, spec)?;
    let direction = spec.direction();

    let closed = spec.per == Period::Total && spec.by.is_some_and(|by| by <= inputs.today);
    let as_of = match spec.by {
        Some(by) if closed => Date::from_days(by.days().saturating_sub(1)),
        _ => inputs.today,
    };
    let now = as_of.month_index();
    let span = books.query().span();
    let first_month = span
        .map(|s| s.start.month_index())
        .filter(|m| *m <= now)
        .unwrap_or(now);
    let history_first = first_month.max(now.saturating_sub(HISTORY_MONTHS - 1));

    let tomorrow = Date::from_days(as_of.days() + 1);
    let mut fetch = Fetch {
        books,
        commodity,
        terms: &terms,
        unconverted: 0,
    };
    let months = fetch.months(DateRange::new(
        Date::first_of_month(history_first),
        tomorrow,
    ));
    // Everything before the history, for an all-time total.
    let before = match (spec.per, span) {
        (Period::Total, Some(span)) => fetch.total(DateRange::new(
            span.start,
            Date::first_of_month(history_first),
        )),
        _ => Parts::ZERO,
    };
    let month_parts = |m: u32| months[(m - history_first) as usize];

    // The fraction of the current month through `as_of`.
    let at = position(tomorrow);
    let elapsed = at - f64::from(now);
    let run_rate = month_parts(now) * (1.0 / elapsed);

    let trend_first = first_month.max(now.saturating_sub(TREND_MONTHS));
    let trend_months = now - trend_first;
    let trend = if trend_months == 0 {
        Trend::flat(run_rate, now)
    } else {
        let samples =
            &months[(trend_first - history_first) as usize..(now - history_first) as usize];
        Trend::fit(samples, trend_first)
    };

    let metric = spec.metric;
    let value = |parts: Parts| parts.value(metric);
    let meets = |v: f64| match direction {
        Direction::AtLeast => v >= target,
        Direction::AtMost => v <= target,
    };

    let window_start = match spec.per {
        Period::Month => Date::first_of_month(now),
        Period::Year => Date::first_of_month(now / 12 * 12),
        Period::Total => span.map_or(as_of, |s| s.start.min(as_of)),
    };
    let window_end = match spec.per {
        Period::Month => Some(Date::first_of_month(now + 1)),
        Period::Year => Some(Date::first_of_month((now / 12 + 1) * 12)),
        Period::Total => spec.by,
    };
    // The month the window's value starts accumulating from in `months`.
    let accrue_from = match spec.per {
        Period::Month => now,
        Period::Year => (now / 12 * 12).max(history_first),
        Period::Total => history_first,
    };

    let mut sums = before;
    let mut history = Vec::with_capacity((now - history_first + 1) as usize);
    for m in history_first..=now {
        let parts = month_parts(m);
        let shown = match spec.per {
            Period::Month => Some(value(parts)),
            _ if m < accrue_from => None,
            _ => {
                sums += parts;
                Some(value(sums))
            }
        };
        if let Some(v) = shown {
            history.push(MonthValue {
                month: Date::first_of_month(m),
                value: v,
            });
        }
    }
    let current_parts = match spec.per {
        Period::Month => month_parts(now),
        _ => sums,
    };
    let current = value(current_parts);

    let horizon = match (spec.per, window_end) {
        (Period::Month, _) => spec
            .by
            .filter(|by| *by > tomorrow)
            .unwrap_or(Date::first_of_month(now + 1 + HORIZON_MONTHS)),
        (_, Some(end)) => end,
        (_, None) => Date::first_of_month(now + 1 + HORIZON_MONTHS),
    };

    let projection_ctx = Projection {
        trend: &trend,
        metric,
        at,
        current: current_parts,
    };
    let Projected {
        value: projected,
        months: projection,
        eta,
    } = if closed {
        Projected {
            value: current,
            months: Vec::new(),
            eta: None,
        }
    } else if spec.per == Period::Month {
        projection_ctx.monthly(now, run_rate, horizon, &meets)
    } else {
        let eta_until = match spec.per {
            Period::Year => position(horizon),
            _ => f64::from(now + 1 + ETA_SEARCH_MONTHS),
        };
        projection_ctx.cumulative(now, horizon, eta_until, &meets)
    };

    let pace = Pace {
        required: window_end
            .filter(|_| !closed && metric != Metric::SavingsRate)
            .map(|end| position(end) - at)
            .filter(|left| *left > 0.0)
            .map(|left| (target - current) / left),
        projected: match spec.per {
            Period::Month => value(run_rate),
            _ => value(trend.at(now + 1)),
        },
    };

    let status = status(Judge {
        direction,
        target,
        current,
        projected,
        closed,
    });
    let eta = if status == Status::Achieved {
        None
    } else {
        eta
    };

    Ok(Progress {
        unit,
        direction,
        target,
        window: Window {
            start: window_start,
            end: window_end,
        },
        as_of,
        closed,
        current,
        projected,
        pace,
        status,
        eta,
        history,
        projection,
        trend_months,
        unconverted: fetch.unconverted,
    })
}

/// What [`status`] weighs.
struct Judge {
    direction: Direction,
    target: f64,
    current: f64,
    projected: f64,
    closed: bool,
}

/// The [`Status`] of a goal from its values.
fn status(judge: Judge) -> Status {
    let Judge {
        direction,
        target,
        current,
        projected,
        closed,
    } = judge;
    let margin = AT_RISK_MARGIN * target.abs();
    match direction {
        Direction::AtLeast if current >= target => Status::Achieved,
        Direction::AtMost if current > target => Status::Behind,
        Direction::AtMost if closed => Status::Achieved,
        _ if closed => Status::Behind,
        Direction::AtLeast if projected >= target => Status::OnTrack,
        Direction::AtLeast if projected >= target - margin => Status::AtRisk,
        Direction::AtMost if projected <= target => Status::OnTrack,
        Direction::AtMost if projected <= target + margin => Status::AtRisk,
        _ => Status::Behind,
    }
}

/// A target in the units the goal is measured in.
struct TargetIn {
    unit: Unit,
    /// The commodity flows are converted into: the target's, or the home
    /// one for a percentage.
    commodity: CommodityId,
    value: f64,
}

/// `target` in the books' units: an amount in its commodity's smallest
/// units, a percentage in points.
fn target_in(books: &Books, target: &Target) -> Result<TargetIn, EvalError> {
    let commodities = &books.ledger().journal().commodities;
    let home = || books.home().ok_or(EvalError::NoHomeCommodity);
    match target {
        Target::Percent(value) => Ok(TargetIn {
            unit: Unit::Percent,
            commodity: home()?,
            value: value.mantissa as f64 / 10f64.powi(i32::from(value.scale)),
        }),
        Target::Amount { value, commodity } => {
            let id = match commodity {
                Some(name) => commodities
                    .get(name)
                    .ok_or_else(|| EvalError::UnknownCommodity(name.clone()))?,
                None => home()?,
            };
            let precision = commodities.precision(id);
            let units = in_units(*value, precision).ok_or_else(|| EvalError::TargetTooPrecise {
                target: target.to_string(),
                commodity: commodities.name(id).to_owned(),
                precision,
            })?;
            Ok(TargetIn {
                unit: Unit::Amount(id),
                commodity: id,
                value: units as f64,
            })
        }
    }
}

/// `value` in units of `precision` decimals, or `None` when that would drop
/// a non-zero digit (or overflow).
fn in_units(value: Decimal, precision: u8) -> Option<i128> {
    let mantissa = i128::from(value.mantissa);
    if precision >= value.scale {
        return mantissa.checked_mul(10i128.checked_pow(u32::from(precision - value.scale))?);
    }
    let pow = 10i128.pow(u32::from(value.scale - precision));
    (mantissa % pow == 0).then(|| mantissa / pow)
}

/// Which of a goal's sums an account's flow goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Money in: the flow negated.
    Income,
    /// Money out: the flow as is.
    Expenses,
    /// A balance: the flow as is.
    Plain,
}

/// One account subtree a goal totals, and whether it adds or takes away.
#[derive(Debug, Clone, Copy)]
struct Term {
    account: AccountId,
    side: Side,
    /// +1, or −1 for a subtree carved back out of an enclosing one.
    sign: f64,
}

/// The side a flow account is on, by its top-level name.
fn flow_side(account: &str) -> Option<Side> {
    let top = account.split(':').next().unwrap_or(account);
    match top {
        INCOME => Some(Side::Income),
        EXPENSES => Some(Side::Expenses),
        _ => None,
    }
}

/// The account subtrees `spec` totals.
fn terms(books: &Books, projects: &Projects, spec: &GoalSpec) -> Result<Vec<Term>, EvalError> {
    let accounts = &books.ledger().journal().accounts;
    let metric = spec.metric;
    let term = |account, side| Term {
        account,
        side,
        sign: 1.0,
    };
    match &spec.scope {
        None if metric == Metric::Balance => Err(EvalError::BalanceNeedsScope),
        // The whole of income and expenses, as `crate::net` totals them.
        None => Ok([(INCOME, Side::Income), (EXPENSES, Side::Expenses)]
            .into_iter()
            .filter_map(|(name, side)| accounts.lookup(name).map(|a| term(a, side)))
            .collect()),
        Some(Scope::Account(name)) => {
            let account = accounts
                .lookup(name)
                .ok_or_else(|| EvalError::UnknownAccount(name.clone()))?;
            if metric == Metric::Balance {
                return Ok(vec![term(account, Side::Plain)]);
            }
            let side = flow_side(name);
            let wanted = match metric {
                Metric::Income => Side::Income,
                Metric::Expenses => Side::Expenses,
                _ => side.unwrap_or(Side::Plain),
            };
            match side {
                Some(side) if side == wanted => Ok(vec![term(account, side)]),
                _ => Err(EvalError::AccountOutsideMetric {
                    account: name.clone(),
                    metric: metric.name(),
                    wanted: match metric {
                        Metric::Income => INCOME,
                        Metric::Expenses => EXPENSES,
                        _ => "income or expenses",
                    },
                }),
            }
        }
        Some(Scope::Project(project)) => project_terms(books, projects, metric, project),
    }
}

/// The subtrees that make up `project`: each account that puts its subtree
/// in it, less each account below one of those that moves its own subtree
/// to another project. Telescoped this way every account is counted once,
/// in the project [`Projects::project_of`] gives it.
fn project_terms(
    books: &Books,
    projects: &Projects,
    metric: Metric,
    project: &str,
) -> Result<Vec<Term>, EvalError> {
    let accounts = &books.ledger().journal().accounts;
    let in_project = |name: Option<&str>| name == Some(project);
    if !projects.roots().iter().any(|r| r.project == project) {
        return Err(EvalError::UnknownProject(project.to_owned()));
    }
    let mut terms = Vec::new();
    for root in projects.roots() {
        let parent = root
            .account
            .rfind(':')
            .and_then(|at| projects.project_of(&root.account[..at]));
        let sign = match (in_project(Some(&root.project)), in_project(parent)) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => continue,
        };
        let Some(account) = accounts.lookup(&root.account) else {
            continue;
        };
        let side = match metric {
            Metric::Balance => Side::Plain,
            _ => match flow_side(&root.account) {
                Some(side) => side,
                None => continue,
            },
        };
        terms.push(Term {
            account,
            side,
            sign,
        });
    }
    Ok(terms)
}

/// One stretch of time's sums under a goal's scope, in the goal's
/// commodity's smallest units. Kept apart so a ratio is taken of sums, and
/// as floats so the trend can scale them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Parts {
    /// Money in, positive.
    income: f64,
    /// Money out, positive.
    expenses: f64,
    /// A balance's change, ledger-signed.
    plain: f64,
}

impl Parts {
    const ZERO: Parts = Parts {
        income: 0.0,
        expenses: 0.0,
        plain: 0.0,
    };

    /// What `metric` reads off these sums. A savings rate with nothing
    /// earned is 0%: nothing was kept.
    fn value(self, metric: Metric) -> f64 {
        match metric {
            Metric::Income => self.income,
            Metric::Expenses => self.expenses,
            Metric::Net => self.income - self.expenses,
            Metric::Balance => self.plain,
            Metric::SavingsRate if self.income > 0.0 => {
                (self.income - self.expenses) / self.income * 100.0
            }
            Metric::SavingsRate => 0.0,
        }
    }
}

impl Add for Parts {
    type Output = Parts;

    fn add(self, other: Parts) -> Parts {
        Parts {
            income: self.income + other.income,
            expenses: self.expenses + other.expenses,
            plain: self.plain + other.plain,
        }
    }
}

impl AddAssign for Parts {
    fn add_assign(&mut self, other: Parts) {
        *self = *self + other;
    }
}

impl Sub for Parts {
    type Output = Parts;

    fn sub(self, other: Parts) -> Parts {
        self + other * -1.0
    }
}

impl Mul<f64> for Parts {
    type Output = Parts;

    fn mul(self, k: f64) -> Parts {
        Parts {
            income: self.income * k,
            expenses: self.expenses * k,
            plain: self.plain * k,
        }
    }
}

/// Reads a goal's terms out of the rollups, counting what couldn't be
/// converted.
struct Fetch<'a> {
    books: &'a Books,
    commodity: CommodityId,
    terms: &'a [Term],
    unconverted: u64,
}

impl Fetch<'_> {
    /// One [`Parts`] per month `range` touches.
    fn months(&mut self, range: DateRange) -> Vec<Parts> {
        self.bucketed(range, Bucket::Month)
    }

    /// The sums over the whole of `range`.
    fn total(&mut self, range: DateRange) -> Parts {
        if range.is_empty() {
            return Parts::ZERO;
        }
        self.bucketed(range, Bucket::Year)
            .into_iter()
            .fold(Parts::ZERO, Add::add)
    }

    fn bucketed(&mut self, range: DateRange, bucket: Bucket) -> Vec<Parts> {
        let query = self.books.query();
        let mut out: Vec<Parts> = Vec::new();
        for term in self.terms {
            let series = query.flows(term.account, range, bucket, self.commodity);
            out.resize(series.points.len(), Parts::ZERO);
            self.unconverted += series.unconverted.count;
            for (into, point) in out.iter_mut().zip(&series.points) {
                let flow = point.total as f64 * term.sign;
                match term.side {
                    Side::Income => into.income -= flow,
                    Side::Expenses => into.expenses += flow,
                    Side::Plain => into.plain += flow,
                }
            }
        }
        if out.is_empty() && !range.is_empty() {
            // No terms: still one (empty) month per month of the range.
            let first = range.start.month_index();
            let last = Date::from_days(range.end.days() - 1).month_index();
            let len = match bucket {
                Bucket::Year => (last / 12 - first / 12 + 1) as usize,
                _ => (last - first + 1) as usize,
            };
            out.resize(len, Parts::ZERO);
        }
        out
    }
}

/// A least-squares line through monthly [`Parts`], each field fitted on its
/// own.
struct Trend {
    /// The fitted value at `origin`.
    intercept: Parts,
    /// The change per month.
    slope: Parts,
    /// The month index of the first sample.
    origin: u32,
}

impl Trend {
    /// The line through `samples`, months `origin`, `origin + 1`, …; one
    /// sample is a flat line.
    fn fit(samples: &[Parts], origin: u32) -> Self {
        let n = samples.len() as f64;
        let mean_x = (n - 1.0) / 2.0;
        let mean = samples.iter().fold(Parts::ZERO, |sum, s| sum + *s) * (1.0 / n);
        let mut sxy = Parts::ZERO;
        let mut sxx = 0.0;
        for (x, sample) in samples.iter().enumerate() {
            let dx = x as f64 - mean_x;
            sxy += (*sample - mean) * dx;
            sxx += dx * dx;
        }
        let slope = if sxx == 0.0 {
            Parts::ZERO
        } else {
            sxy * (1.0 / sxx)
        };
        Trend {
            intercept: mean - slope * mean_x,
            slope,
            origin,
        }
    }

    /// A flat line at `parts` a month.
    fn flat(parts: Parts, origin: u32) -> Self {
        Trend {
            intercept: parts,
            slope: Parts::ZERO,
            origin,
        }
    }

    /// The line's value for month `month`.
    fn at(&self, month: u32) -> Parts {
        let x = f64::from(month) - f64::from(self.origin);
        self.intercept + self.slope * x
    }
}

/// Where `date` starts on a continuous month axis: its month index plus the
/// fraction of the month before it.
fn position(date: Date) -> f64 {
    let month = date.month_index();
    let first = Date::first_of_month(month);
    f64::from(month) + f64::from(date.days() - first.days()) / f64::from(days_in(month))
}

/// How many days month `month` (a [`Date::month_index`]) has.
fn days_in(month: u32) -> u32 {
    Date::first_of_month(month + 1).days() - Date::first_of_month(month).days()
}

/// A projection's value at the window's end, its chart, and its eta.
struct Projected {
    value: f64,
    months: Vec<MonthValue>,
    eta: Option<Date>,
}

/// What both kinds of projection extend.
struct Projection<'a> {
    trend: &'a Trend,
    metric: Metric,
    /// Where the evaluation day ends on the month axis.
    at: f64,
    /// The window's sums so far.
    current: Parts,
}

impl Projection<'_> {
    /// A `per: month` goal: the current month at `run_rate`, each later
    /// month on the trend, through `horizon`'s month (exclusive when it is
    /// a month's first day).
    fn monthly(
        &self,
        now: u32,
        run_rate: Parts,
        horizon: Date,
        meets: &dyn Fn(f64) -> bool,
    ) -> Projected {
        let month_value = |m: u32| {
            let parts = if m == now { run_rate } else { self.trend.at(m) };
            parts.value(self.metric)
        };
        let last = Date::from_days(horizon.days() - 1).month_index().max(now);
        let months = (now..=last)
            .map(|m| MonthValue {
                month: Date::first_of_month(m),
                value: month_value(m),
            })
            .collect();
        let eta = (now..=now + ETA_SEARCH_MONTHS)
            .find(|m| meets(month_value(*m)))
            .map(Date::first_of_month);
        Projected {
            value: month_value(now),
            months,
            eta,
        }
    }

    /// A cumulative goal: the sums so far plus the trend accrued day by
    /// day from the evaluation day, through `horizon`; the eta is looked
    /// for up to `eta_until` on the month axis.
    fn cumulative(
        &self,
        now: u32,
        horizon: Date,
        eta_until: f64,
        meets: &dyn Fn(f64) -> bool,
    ) -> Projected {
        let value_at = |to: f64| (self.current + self.accrue(to)).value(self.metric);
        let end = position(horizon);
        let last = Date::from_days(horizon.days() - 1).month_index().max(now);
        let months = (now..=last)
            .map(|m| MonthValue {
                month: Date::first_of_month(m),
                value: value_at(f64::from(m + 1).min(end)),
            })
            .collect();
        let eta = if meets(self.current.value(self.metric)) {
            None
        } else {
            self.eta(now, eta_until, &value_at, meets)
        };
        Projected {
            value: value_at(end),
            months,
            eta,
        }
    }

    /// The first day through which the cumulative value meets the target,
    /// up to `until` on the month axis.
    fn eta(
        &self,
        now: u32,
        until: f64,
        value_at: &dyn Fn(f64) -> f64,
        meets: &dyn Fn(f64) -> bool,
    ) -> Option<Date> {
        let month = (now..)
            .take_while(|m| f64::from(*m) < until)
            .find(|m| meets(value_at(f64::from(m + 1).min(until))))?;
        let first = Date::first_of_month(month).days();
        (first..first + days_in(month))
            .map(Date::from_days)
            .filter(|day| position(Date::from_days(day.days() + 1)) > self.at)
            .find(|day| {
                meets(value_at(
                    position(Date::from_days(day.days() + 1)).min(until),
                ))
            })
    }

    /// The trend's sums from the evaluation day to `to` on the month axis.
    fn accrue(&self, to: f64) -> Parts {
        let mut sum = Parts::ZERO;
        let mut month = self.at.floor() as u32;
        while f64::from(month) < to {
            let lo = self.at.max(f64::from(month));
            let hi = to.min(f64::from(month + 1));
            if hi > lo {
                sum += self.trend.at(month) * (hi - lo);
            }
            month += 1;
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net;
    use crate::projects::ProjectRoot;
    use crate::test_util::books as ledger;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// The spec of a goal block body.
    fn spec(body: &str) -> GoalSpec {
        let desc = format!("```cashflow-goal\n{body}```\n");
        super::super::parse_goal(&desc).unwrap().unwrap()
    }

    fn eval_with(books: &Books, projects: &Projects, body: &str, today: &str) -> Progress {
        evaluate(
            &Inputs {
                books,
                projects,
                today: date(today),
            },
            &spec(body),
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn eval(books: &Books, body: &str, today: &str) -> Progress {
        eval_with(books, &Projects::default(), body, today)
    }

    fn eval_err(books: &Books, projects: &Projects, body: &str) -> EvalError {
        evaluate(
            &Inputs {
                books,
                projects,
                today: date("2024-06-15"),
            },
            &spec(body),
        )
        .unwrap_err()
    }

    /// One posting of `amount` CAD to `account` on `date`, balanced against
    /// `assets:checking`, as journal text.
    fn post(date: &str, account: &str, amount: &str) -> String {
        format!("{date} * t\n  {account}  {amount} CAD\n  assets:checking\n\n")
    }

    /// `amount` CAD of income on the first of `month` (`YYYY-MM`).
    fn earn(month: &str, amount: &str) -> String {
        post(
            &format!("{month}-01"),
            "income:salary",
            &format!("-{amount}.00"),
        )
    }

    /// `per: month` judges the month by its run rate, prorated by day:
    /// June 15th is half of June, so the month-to-date doubles.
    #[test]
    fn per_month_prorates_the_run_rate() {
        let goal = "metric: income\ntarget: 1000 CAD\n";
        let cases = [
            ("600.00", Status::OnTrack, 120_000.0),
            ("450.00", Status::AtRisk, 90_000.0),
            ("300.00", Status::Behind, 60_000.0),
            ("1100.00", Status::Achieved, 220_000.0),
        ];
        for (earned, status, projected) in cases {
            let books =
                ledger(&[post("2024-06-03", "income:salary", &format!("-{earned}"))].concat());
            let p = eval(&books, goal, "2024-06-15");
            assert_eq!(p.status, status, "earned {earned}");
            assert_eq!(p.projected, projected, "earned {earned}");
            assert_eq!(p.pace.projected, projected);
            assert_eq!(p.target, 100_000.0);
            assert_eq!(p.window.start, date("2024-06-01"));
            assert_eq!(p.window.end, Some(date("2024-07-01")));
        }
        let books = ledger(&[post("2024-06-03", "income:salary", "-600.00")].concat());
        let p = eval(&books, goal, "2024-06-15");
        assert_eq!(p.current, 60_000.0);
        // 400 more over the half month left is 800 a month.
        assert_eq!(p.pace.required, Some(80_000.0));
        assert_eq!(
            p.eta,
            Some(date("2024-06-01")),
            "this month, by its run rate"
        );
        assert_eq!(p.trend_months, 0, "the run rate stands in for a trend");
    }

    /// An expenses goal is a ceiling: under it by the run rate is on
    /// track, already over it is behind whatever the projection says.
    #[test]
    fn expenses_are_a_ceiling() {
        let goal = "metric: expenses\ntarget: 3000 CAD\n";
        let cases = [
            ("1000.00", Status::OnTrack),
            ("1600.00", Status::AtRisk),
            ("2000.00", Status::Behind),
        ];
        for (spent, status) in cases {
            let books = ledger(&[post("2024-06-03", "expenses:rent", spent)].concat());
            assert_eq!(
                eval(&books, goal, "2024-06-15").status,
                status,
                "spent {spent}"
            );
        }
        // Over the ceiling by the 30th: behind, even though the 30th's run
        // rate is the month itself.
        let books = ledger(&[post("2024-06-03", "expenses:rent", "3100.00")].concat());
        assert_eq!(eval(&books, goal, "2024-06-30").status, Status::Behind);
        let p = eval(&books, goal, "2024-06-30");
        assert_eq!(p.direction, Direction::AtMost);
    }

    /// Five months of 1000: a flat trend carries a total goal to its
    /// deadline, and the eta is the day the running total crosses.
    #[test]
    fn total_by_a_deadline() {
        let books = ledger(
            &[
                earn("2024-01", "1000"),
                earn("2024-02", "1000"),
                earn("2024-03", "1000"),
                earn("2024-04", "1000"),
                earn("2024-05", "1000"),
            ]
            .concat(),
        );
        let goal =
            |target| format!("metric: income\ntarget: {target} CAD\nper: total\nby: 2024-11-01\n");

        let p = eval(&books, &goal(9500), "2024-05-31");
        assert_eq!(p.current, 500_000.0);
        assert_eq!(p.trend_months, 3);
        assert_eq!(p.status, Status::OnTrack);
        assert!((p.projected - 1_000_000.0).abs() < 1e-6, "{}", p.projected);
        // 4500 more over June to October, five months.
        assert_eq!(p.pace.required, Some(90_000.0));
        assert!((p.pace.projected - 100_000.0).abs() < 1e-6);
        // 4000 by the end of September, then 500 of October's 1000 is
        // half of its 31 days: through the 16th.
        assert_eq!(p.eta, Some(date("2024-10-16")));
        assert_eq!(p.window.start, date("2024-01-01"));
        assert_eq!(p.window.end, Some(date("2024-11-01")));
        // May (its end), then June to October.
        assert_eq!(p.projection.len(), 6);
        assert_eq!(p.projection[0].month, date("2024-05-01"));
        assert_eq!(p.projection[0].value, 500_000.0);
        assert!((p.projection[5].value - 1_000_000.0).abs() < 1e-6);
        // The history is cumulative.
        let history: Vec<f64> = p.history.iter().map(|m| m.value).collect();
        assert_eq!(
            history,
            [100_000.0, 200_000.0, 300_000.0, 400_000.0, 500_000.0]
        );

        let p = eval(&books, &goal(11000), "2024-05-31");
        assert_eq!(p.status, Status::AtRisk, "10000 is within 15% of 11000");
        assert_eq!(p.eta, Some(date("2024-11-30")), "past the deadline");
        assert_eq!(
            eval(&books, &goal(13000), "2024-05-31").status,
            Status::Behind
        );
        assert_eq!(
            eval(&books, &goal(4000), "2024-05-31").status,
            Status::Achieved
        );
        assert_eq!(eval(&books, &goal(4000), "2024-05-31").eta, None);
    }

    /// Once `by` has passed the window is closed: the value at `by` is
    /// final, and nothing is projected.
    #[test]
    fn a_passed_deadline_is_final() {
        let books = ledger(&[earn("2024-01", "1000"), earn("2024-02", "1000")].concat());
        let at_least = "metric: income\ntarget: 1500 CAD\nper: total\nby: 2024-02-01\n";
        let p = eval(&books, at_least, "2024-06-15");
        assert!(p.closed);
        assert_eq!(p.as_of, date("2024-01-31"));
        assert_eq!(p.current, 100_000.0);
        assert_eq!(p.status, Status::Behind);
        assert!(p.projection.is_empty());
        assert_eq!(p.pace.required, None);

        let books = ledger(&[post("2024-01-05", "expenses:rent", "1000.00")].concat());
        let ceiling = "metric: expenses\ntarget: 1500 CAD\nper: total\nby: 2024-02-01\n";
        assert_eq!(eval(&books, ceiling, "2024-06-15").status, Status::Achieved);
    }

    /// A rising trend: the next months follow the line, and a monthly
    /// goal's eta is the first month the line meets.
    #[test]
    fn a_rising_trend_extends() {
        let books = ledger(
            &[
                earn("2024-03", "1000"),
                earn("2024-04", "2000"),
                earn("2024-05", "3000"),
                earn("2024-06", "1000"),
            ]
            .concat(),
        );
        let p = eval(&books, "metric: income\ntarget: 5500 CAD\n", "2024-06-15");
        assert_eq!(p.trend_months, 3);
        // June so far doubles to 2000; the line has July at 5000 and
        // August at 6000.
        assert_eq!(p.status, Status::Behind);
        assert_eq!(p.projection[0].value, 200_000.0);
        assert!((p.projection[1].value - 500_000.0).abs() < 1e-6);
        assert!((p.projection[2].value - 600_000.0).abs() < 1e-6);
        assert_eq!(p.eta, Some(date("2024-08-01")));
        assert_eq!(p.projection.len(), 13, "June and 12 months out");
        let history: Vec<f64> = p.history.iter().map(|m| m.value).collect();
        assert_eq!(history, [100_000.0, 200_000.0, 300_000.0, 100_000.0]);
    }

    /// A falling or flat-zero trend never reaches the target: behind, and
    /// no eta.
    #[test]
    fn degenerate_trends_have_no_eta() {
        let goal = "metric: income\ntarget: 10000 CAD\nper: total\nby: 2025-01-01\n";
        let falling = ledger(
            &[
                earn("2024-03", "3000"),
                earn("2024-04", "2000"),
                earn("2024-05", "1000"),
            ]
            .concat(),
        );
        let p = eval(&falling, goal, "2024-06-15");
        assert_eq!(p.current, 600_000.0);
        assert_eq!(p.status, Status::Behind);
        assert!(p.projected < p.current, "income going negative");
        assert_eq!(p.eta, None);

        let flat = ledger(
            &[
                post("2024-01-03", "income:salary", "-1000.00"),
                post("2024-05-03", "expenses:rent", "10.00"),
            ]
            .concat(),
        );
        let p = eval(&flat, goal, "2024-06-15");
        assert_eq!(p.current, 100_000.0);
        assert_eq!(p.projected, 100_000.0);
        assert_eq!(p.status, Status::Behind);
        assert_eq!(p.eta, None);
    }

    /// Nothing in scope reads as zero rather than failing; empty books
    /// can't price a target at all.
    #[test]
    fn no_data() {
        let books = ledger(&[post("2024-06-03", "expenses:rent", "10.00")].concat());
        let p = eval(&books, "metric: income\ntarget: 1000 CAD\n", "2024-06-15");
        assert_eq!(p.current, 0.0);
        assert_eq!(p.projected, 0.0);
        assert_eq!(p.status, Status::Behind);
        assert_eq!(p.eta, None);
        assert_eq!(p.history.len(), 1);

        let empty = ledger("");
        assert_eq!(
            eval_err(
                &empty,
                &Projects::default(),
                "metric: income\ntarget: 1000\n"
            ),
            EvalError::NoHomeCommodity
        );
        assert_eq!(
            eval_err(
                &empty,
                &Projects::default(),
                "metric: income\ntarget: 1000 CAD\n"
            ),
            EvalError::UnknownCommodity("CAD".to_owned())
        );
    }

    /// A savings rate is the window's net over its income, in percent, and
    /// has no pace to add up.
    #[test]
    fn savings_rate() {
        let books = ledger(
            &[
                post("2024-06-03", "income:salary", "-1000.00"),
                post("2024-06-04", "expenses:rent", "800.00"),
            ]
            .concat(),
        );
        let p = eval(&books, "metric: savings_rate\ntarget: 22%\n", "2024-06-15");
        assert_eq!(p.unit, Unit::Percent);
        assert!((p.current - 20.0).abs() < 1e-9);
        assert_eq!(p.status, Status::AtRisk);
        assert_eq!(p.pace.required, None);
        let p = eval(&books, "metric: savings_rate\ntarget: 25%\n", "2024-06-15");
        assert_eq!(p.status, Status::Behind);
        let p = eval(&books, "metric: savings_rate\ntarget: 15%\n", "2024-06-15");
        assert_eq!(p.status, Status::Achieved);
    }

    /// The default scope is `crate::net`'s: a net goal's month is
    /// `net_flow`'s.
    #[test]
    fn default_scope_is_net() {
        let books = ledger(
            &[
                post("2024-06-03", "income:salary", "-1000.00"),
                post("2024-06-04", "expenses:rent", "300.00"),
                post("2024-06-05", "transfers", "50.00"),
                post("2024-06-05", "routed:in", "-7.00"),
            ]
            .concat(),
        );
        let p = eval(&books, "metric: net\ntarget: 1 CAD\n", "2024-06-15");
        let flow = net::net_flow(
            &books,
            books.home().unwrap(),
            DateRange::new(date("2024-06-01"), date("2024-06-16")),
        );
        assert_eq!(p.current, flow.net() as f64);
    }

    /// A per-year goal totals the year to date, and its history starts in
    /// January.
    #[test]
    fn per_year_is_year_to_date() {
        let books = ledger(
            &[
                earn("2023-11", "500"),
                earn("2024-01", "1000"),
                earn("2024-02", "1000"),
            ]
            .concat(),
        );
        let p = eval(
            &books,
            "metric: income\ntarget: 12000 CAD\nper: year\n",
            "2024-02-29",
        );
        assert_eq!(p.current, 200_000.0);
        assert_eq!(p.window.end, Some(date("2025-01-01")));
        assert_eq!(p.history.len(), 2);
        assert_eq!(p.history[0].month, date("2024-01-01"));
        assert_eq!(p.projection.last().unwrap().month, date("2024-12-01"));
    }

    /// A balance is the account's running total, read in its own
    /// commodity; it needs a scope.
    #[test]
    fn balance_of_an_account() {
        let books = ledger(
            &[
                post("2024-01-03", "assets:savings", "5000.00"),
                post("2024-02-03", "assets:savings", "1000.00"),
            ]
            .concat(),
        );
        let p = eval(
            &books,
            "metric: balance\naccount: assets:savings\ntarget: 10000 CAD\n",
            "2024-06-15",
        );
        assert_eq!(p.current, 600_000.0);
        assert_eq!(p.window.end, None);
        assert_eq!(p.status, Status::Behind);
        assert_eq!(
            eval_err(
                &books,
                &Projects::default(),
                "metric: balance\ntarget: 1 CAD\n"
            ),
            EvalError::BalanceNeedsScope
        );
    }

    /// A scope that matches nothing, or sits outside its metric, is an
    /// error naming it; so is a target finer than the books keep.
    #[test]
    fn scope_and_target_errors() {
        let books = ledger(&[post("2024-06-03", "expenses:rent", "10.00")].concat());
        let none = Projects::default();
        assert_eq!(
            eval_err(
                &books,
                &none,
                "metric: income\naccount: income:nope\ntarget: 1\n"
            ),
            EvalError::UnknownAccount("income:nope".to_owned())
        );
        assert_eq!(
            eval_err(&books, &none, "metric: income\nproject: damus\ntarget: 1\n"),
            EvalError::UnknownProject("damus".to_owned())
        );
        assert!(matches!(
            eval_err(
                &books,
                &none,
                "metric: income\naccount: expenses:rent\ntarget: 1\n"
            ),
            EvalError::AccountOutsideMetric { .. }
        ));
        assert!(matches!(
            eval_err(&books, &none, "metric: income\ntarget: 10.005 CAD\n"),
            EvalError::TargetTooPrecise { precision: 2, .. }
        ));
        let p = eval(
            &books,
            "metric: expenses\naccount: expenses:rent\ntarget: 10.50 CAD\n",
            "2024-06-15",
        );
        assert_eq!(p.target, 1050.0);
    }

    /// A project is the accounts it attributes, nested reassignments
    /// carved out and back in.
    #[test]
    fn project_scope_follows_attribution() {
        let books = ledger(
            &[
                post("2024-06-03", "income:a:x", "-100.00"),
                post("2024-06-03", "income:a:b", "-10.00"),
                post("2024-06-03", "income:a:b:c", "-1.00"),
                post("2024-06-03", "expenses:a", "5.00"),
            ]
            .concat(),
        );
        let root = |account: &str, project: &str| ProjectRoot {
            account: account.to_owned(),
            project: project.to_owned(),
        };
        let projects = Projects::new([
            root("income:a", "p"),
            root("income:a:b", "q"),
            root("income:a:b:c", "p"),
        ]);
        let p = eval_with(
            &books,
            &projects,
            "metric: income\nproject: p\ntarget: 1\n",
            "2024-06-15",
        );
        assert_eq!(p.current, 10_100.0);
        let q = eval_with(
            &books,
            &projects,
            "metric: net\nproject: q\ntarget: 1\n",
            "2024-06-15",
        );
        assert_eq!(q.current, 1_000.0);
    }
}
