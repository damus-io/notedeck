//! The Goals view's figures: every goal measured against the books and
//! written out, so drawing a frame formats nothing.
//!
//! [`report`] runs [`cashflow::goals::evaluate`] once per goal. It is called
//! only when the books, the goals or today change (it allocates the chart
//! series), and the view and the Overview's strip draw from the [`Report`]
//! it returns.

use cashflow::Books;
use cashflow::goals::progress::{self, Progress, Unit};
use cashflow::goals::{Direction, Goal, Period, Status};
use cashflow::projects::Projects;
use ledgerdb::{Commodities, Date};

use crate::money::{compact, money};
use crate::views::trends::model::month_label;

/// What [`report`] reads.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    /// The derived books.
    pub books: &'a Books,
    /// The chart's project attribution, for a goal scoped to a project.
    pub projects: &'a Projects,
    /// The day the goals are measured as of.
    pub today: Date,
    /// The goals, as found on the boards.
    pub goals: &'a [Goal],
}

/// Every goal, measured and written out, and how many are in each state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// One row per goal, in the order they were found.
    pub rows: Vec<GoalRow>,
    /// The counts, for the Overview's strip.
    pub summary: Summary,
}

impl Report {
    /// The row of the goal on the card `card_ref`, if it is still a goal.
    pub fn row(&self, card_ref: &str) -> Option<&GoalRow> {
        self.rows.iter().find(|row| row.card_ref == card_ref)
    }
}

/// How many goals are in each state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    /// Goals already met.
    pub achieved: usize,
    /// Goals projected to make it.
    pub on_track: usize,
    /// Goals projected to miss by a little.
    pub at_risk: usize,
    /// Goals projected to miss by more.
    pub behind: usize,
    /// Goals whose block doesn't read or that can't be measured.
    pub errors: usize,
    /// The counts in a line, the ones that need attention first, leaving
    /// out the zeroes: `1 behind, 2 on track`. Empty with no goals.
    pub text: String,
}

/// One goal as the view shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalRow {
    /// The card, as a full `headway:<board>/<word-id>` ref: what a click
    /// opens.
    pub card_ref: String,
    /// The card's title.
    pub title: String,
    /// The column the card is in.
    pub column: String,
    /// What the block asks for, in a line; empty when it doesn't read.
    pub spec_text: String,
    /// The goal measured, or why it couldn't be.
    pub outcome: Result<Measured, String>,
}

/// A goal measured against the books.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    /// Whether it is going to make it.
    pub status: Status,
    /// How far along the current value is, `0..=1` of the target: for a
    /// ceiling, how much of it is used.
    pub fraction: f32,
    /// The value so far.
    pub current_text: String,
    /// The target.
    pub target_text: String,
    /// The span it applies to: `October 2024`, `2024`, `since Jan 2022`.
    pub window_text: String,
    /// Where the projection lands at the window's end.
    pub projected_text: String,
    /// What each month left has to add, when the goal adds up by month.
    pub required_text: Option<String>,
    /// What the trend has the next month add.
    pub pace_text: String,
    /// When the projection meets the target, or that it doesn't; `None`
    /// once achieved, and for a ceiling.
    pub eta_text: Option<String>,
    /// Postings left out for want of a price, when there were any.
    pub unconverted_text: Option<String>,
    /// The history and projection, for the detail's chart.
    pub chart: Chart,
}

/// A goal's chart: the measure per month, the projection on from the
/// current month, the target and the deadline.
#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    /// What the values are: `Per month`, `Year to date`, `Running total`.
    pub caption: &'static str,
    /// Each bucket's month, oldest first.
    pub labels: Vec<String>,
    /// The measure per month so far, from bucket 0.
    pub actual: Vec<f64>,
    /// Each of [`Self::actual`], written out.
    pub actual_texts: Vec<String>,
    /// The bucket the projection starts at: the current month's.
    pub projection_start: usize,
    /// The projected measure per month from [`Self::projection_start`];
    /// empty once the goal's window has closed.
    pub projection: Vec<f64>,
    /// Each of [`Self::projection`], written out.
    pub projection_texts: Vec<String>,
    /// The actual value the projection joins at its start.
    pub join: Option<f64>,
    /// The target, in the values' units.
    pub target: f64,
    /// The bucket of the deadline's month, when it is on the chart.
    pub deadline: Option<usize>,
    /// The deadline's label: `by 2027-01-01`, `year end`.
    pub deadline_label: String,
    /// The decimals of the values' commodity, or `None` for percentages.
    pub precision: Option<u8>,
}

impl Chart {
    /// A value as the axis writes it: `4.2k`, `20%`.
    pub fn axis_text(&self, value: f64) -> String {
        match self.precision {
            Some(precision) => compact(value, precision),
            None => format!("{value:.0}%"),
        }
    }
}

/// Every goal in `inputs` measured against its books.
#[profiling::function]
pub fn report(inputs: Inputs<'_>) -> Report {
    let eval = progress::Inputs {
        books: inputs.books,
        projects: inputs.projects,
        today: inputs.today,
    };
    let commodities = &inputs.books.ledger().journal().commodities;
    let rows: Vec<GoalRow> = inputs
        .goals
        .iter()
        .map(|goal| row(&eval, commodities, goal))
        .collect();
    let summary = summary(&rows);
    Report { rows, summary }
}

/// One goal's row.
fn row(eval: &progress::Inputs<'_>, commodities: &Commodities, goal: &Goal) -> GoalRow {
    let (spec_text, outcome) = match &goal.spec {
        Err(err) => (String::new(), Err(format!("goal block, {err}"))),
        Ok(spec) => {
            let measured = progress::evaluate(eval, spec)
                .map(|p| measured(commodities, spec.per, spec.by, &p))
                .map_err(|err| err.to_string());
            (spec.to_string(), measured)
        }
    };
    GoalRow {
        card_ref: goal.card_ref.clone(),
        title: goal.title.clone(),
        column: goal.column.clone(),
        spec_text,
        outcome,
    }
}

/// How values of `unit` are written.
#[derive(Clone, Copy)]
struct Units<'a> {
    unit: Unit,
    commodities: &'a Commodities,
}

impl Units<'_> {
    /// `value` in full: `1800.00 CAD`, `22.5%`.
    fn text(self, value: f64) -> String {
        match self.unit {
            Unit::Amount(id) => money(value.round() as i128, id, self.commodities),
            Unit::Percent => format!("{value:.1}%"),
        }
    }

    /// The decimals of the commodity, or `None` for percentages.
    fn precision(self) -> Option<u8> {
        match self.unit {
            Unit::Amount(id) => Some(self.commodities.precision(id)),
            Unit::Percent => None,
        }
    }
}

/// `progress` written out, for a goal `per` its window with deadline `by`.
fn measured(
    commodities: &Commodities,
    per: Period,
    by: Option<Date>,
    progress: &Progress,
) -> Measured {
    let units = Units {
        unit: progress.unit,
        commodities,
    };
    let monthly = match progress.unit {
        Unit::Amount(_) => "/mo",
        Unit::Percent => "",
    };
    let achieved = progress.status == Status::Achieved;
    let required_text = progress.pace.required.filter(|_| !achieved).map(|r| {
        let what = match progress.direction {
            Direction::AtLeast => "needs",
            Direction::AtMost => "room for",
        };
        format!("{what} {}{monthly}", units.text(r.max(0.0)))
    });
    let pace_text = match per {
        Period::Month => format!(
            "running at {}{monthly}",
            units.text(progress.pace.projected)
        ),
        _ => format!("trending {}{monthly}", units.text(progress.pace.projected)),
    };
    let eta_text = match (progress.direction, progress.status, progress.eta) {
        (Direction::AtMost, ..) | (_, Status::Achieved, _) => None,
        (_, _, Some(eta)) if per == Period::Month => {
            Some(format!("meets it {}", month_label(eta.month_index())))
        }
        (_, _, Some(eta)) => Some(format!("meets it {}", day_label(eta))),
        (_, _, None) if progress.closed => Some("closed short of it".to_owned()),
        (_, _, None) => Some("not within reach at this pace".to_owned()),
    };
    let unconverted_text = (progress.unconverted > 0).then(|| {
        let n = progress.unconverted;
        let s = if n == 1 { "" } else { "s" };
        format!("{n} posting{s} left out: no price into its commodity")
    });
    Measured {
        status: progress.status,
        fraction: fraction(progress),
        current_text: units.text(progress.current),
        target_text: units.text(progress.target),
        window_text: window_text(per, by, progress),
        projected_text: units.text(progress.projected),
        required_text,
        pace_text,
        eta_text,
        unconverted_text,
        chart: chart(units, per, by, progress),
    }
}

/// How far along the current value is, `0..=1` of the target.
fn fraction(progress: &Progress) -> f32 {
    if progress.target > 0.0 {
        return (progress.current / progress.target).clamp(0.0, 1.0) as f32;
    }
    let met = match progress.direction {
        Direction::AtLeast => progress.current >= progress.target,
        Direction::AtMost => progress.current <= progress.target,
    };
    if met { 1.0 } else { 0.0 }
}

/// The span a goal's target applies to, in words.
fn window_text(per: Period, by: Option<Date>, progress: &Progress) -> String {
    let start = progress.window.start;
    match (per, by) {
        (Period::Month, _) => month_label(start.month_index()),
        (Period::Year, _) => start.ymd().year.to_string(),
        (Period::Total, Some(by)) => {
            format!("since {}, by {by}", month_label(start.month_index()))
        }
        (Period::Total, None) => format!("since {}", month_label(start.month_index())),
    }
}

/// `Mar 14, 2025`.
pub(crate) fn day_label(date: Date) -> String {
    let month = month_label(date.month_index());
    let (name, year) = month.split_once(' ').unwrap_or((&month, ""));
    format!("{name} {}, {year}", date.ymd().day)
}

/// The detail's chart of `progress`.
fn chart(units: Units<'_>, per: Period, by: Option<Date>, progress: &Progress) -> Chart {
    let history = &progress.history;
    let projection = &progress.projection;
    let first = history
        .first()
        .or(projection.first())
        .map_or(0, |m| m.month.month_index());
    let last = history
        .iter()
        .chain(projection)
        .map(|m| m.month.month_index())
        .max()
        .unwrap_or(first);
    let bucket = |date: Date| date.month_index().checked_sub(first).map(|b| b as usize);
    let projection_start = projection
        .first()
        .and_then(|m| bucket(m.month))
        .unwrap_or(history.len());
    let join = projection_start
        .checked_add(1)
        .filter(|end| *end == history.len())
        .and_then(|_| history.last())
        .map(|m| m.value);
    let deadline = match per {
        Period::Year => progress.window.end,
        Period::Month | Period::Total => by,
    };
    let deadline_label = match (per, by) {
        (Period::Year, _) => "year end".to_owned(),
        (_, Some(by)) => format!("by {by}"),
        (_, None) => String::new(),
    };
    let deadline = deadline
        .map(|end| Date::from_days(end.days().saturating_sub(1)))
        .and_then(bucket)
        .filter(|b| *b <= (last - first) as usize);
    Chart {
        caption: match per {
            Period::Month => "Per month",
            Period::Year => "Year to date",
            Period::Total => "Running total",
        },
        labels: (first..=last).map(month_label).collect(),
        actual: history.iter().map(|m| m.value).collect(),
        actual_texts: history.iter().map(|m| units.text(m.value)).collect(),
        projection_start,
        projection: projection.iter().map(|m| m.value).collect(),
        projection_texts: projection.iter().map(|m| units.text(m.value)).collect(),
        join,
        target: progress.target,
        deadline,
        deadline_label,
        precision: units.precision(),
    }
}

/// The counts of `rows`.
fn summary(rows: &[GoalRow]) -> Summary {
    let mut summary = Summary::default();
    for row in rows {
        match row.outcome.as_ref().map(|m| m.status) {
            Ok(Status::Achieved) => summary.achieved += 1,
            Ok(Status::OnTrack) => summary.on_track += 1,
            Ok(Status::AtRisk) => summary.at_risk += 1,
            Ok(Status::Behind) => summary.behind += 1,
            Err(_) => summary.errors += 1,
        }
    }
    let parts = [
        (summary.errors, "to fix"),
        (summary.behind, Status::Behind.name()),
        (summary.at_risk, Status::AtRisk.name()),
        (summary.on_track, Status::OnTrack.name()),
        (summary.achieved, Status::Achieved.name()),
    ];
    let texts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{count} {name}"))
        .collect();
    summary.text = texts.join(", ");
    summary
}
