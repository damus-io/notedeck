//! Financial goals, which live on Headway cards.
//!
//! A goal is a card labelled [`GOAL_LABEL`] whose description holds one
//! fenced `cashflow-goal` block:
//!
//! ````text
//! ```cashflow-goal
//! metric: net            # income | expenses | net | savings_rate | balance
//! account: income:damus  # optional subtree (default: per metric)
//! project: damus         # optional, alternative to account
//! target: 10000 CAD
//! per: month             # month | year | total (cumulative until `by`)
//! by: 2027-01-01         # optional deadline
//! ```
//! ````
//!
//! [`parse_goal`] reads that block into a [`GoalSpec`], and [`discover`]
//! finds the goals on a set of boards (see [`GoalsConfig`](crate::config::GoalsConfig)
//! for which). A goal whose block is malformed is still returned, carrying
//! its [`GoalError`], so a typo shows up beside the card instead of the goal
//! quietly vanishing. [`progress::evaluate`] measures a goal against the
//! books: how far along it is, where its trend takes it and whether that is
//! on track.
//!
//! Boards are read through the `headway` core crate ([`headway::boards`]),
//! never the headway CLI's internals, so a sealed (team-of-one or shared)
//! board folds here the same way it does in the app.

use std::fmt;

pub mod progress;

pub use progress::{EvalError, Progress, Status, evaluate};

use headway::boards::{self, Roster};
use headway::event::{BoardView, CardView};
use ledgerdb::{AmountText, Date, Decimal};
use nostrdb::Ndb;

/// The label that makes a card a goal.
pub const GOAL_LABEL: &str = "goal";

/// The fence info string of a goal block: ```` ```cashflow-goal ````.
pub const FENCE_INFO: &str = "cashflow-goal";

/// The boards goals are looked for on when `config.toml` names none.
pub const DEFAULT_BOARDS: [&str; 2] = ["cashflow", "personal"];

/// What a goal measures. Income, expenses and net are the ones [`crate::net`]
/// defines, so a goal, the dashboard and the Adventure's XP never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Money in, under `income:*`.
    Income,
    /// Money out, under `expenses:*`.
    Expenses,
    /// Income less expenses.
    Net,
    /// The share of income kept: net over income.
    SavingsRate,
    /// An account subtree's balance at a point in time.
    Balance,
}

impl Metric {
    /// Every metric, in the order the help lists them.
    pub const ALL: [Metric; 5] = [
        Metric::Income,
        Metric::Expenses,
        Metric::Net,
        Metric::SavingsRate,
        Metric::Balance,
    ];

    /// The name a goal block writes it as.
    pub fn name(self) -> &'static str {
        match self {
            Metric::Income => "income",
            Metric::Expenses => "expenses",
            Metric::Net => "net",
            Metric::SavingsRate => "savings_rate",
            Metric::Balance => "balance",
        }
    }

    /// The metric a goal block's `metric:` names.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Which side of the target is success: an expenses goal is a ceiling,
    /// every other one a floor.
    pub fn direction(self) -> Direction {
        match self {
            Metric::Expenses => Direction::AtMost,
            _ => Direction::AtLeast,
        }
    }

    /// The period a goal block that omits `per:` measures over: a balance is
    /// read at a point in time, so `total`; every flow is per `month`.
    pub fn default_period(self) -> Period {
        match self {
            Metric::Balance => Period::Total,
            _ => Period::Month,
        }
    }
}

/// Which side of its target a goal is met on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Met at the target or above it.
    AtLeast,
    /// Met at the target or below it.
    AtMost,
}

impl Direction {
    /// `≥` or `≤`.
    pub fn symbol(self) -> &'static str {
        match self {
            Direction::AtLeast => "≥",
            Direction::AtMost => "≤",
        }
    }
}

/// Which part of the chart of accounts a goal totals, when it narrows the
/// metric's default (the whole of `income`, `expenses` or both).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `account:`: an account and its subtree, such as `income:damus`.
    Account(String),
    /// `project:`: the accounts the chart of accounts attributes to a
    /// project (see [`crate::projects`]).
    Project(String),
}

/// The window a goal's target applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// Every calendar month.
    Month,
    /// Every calendar year.
    Year,
    /// Cumulatively, up to `by` if it is set.
    Total,
}

impl Period {
    /// Every period, in the order the help lists them.
    pub const ALL: [Period; 3] = [Period::Month, Period::Year, Period::Total];

    /// The name a goal block writes it as.
    pub fn name(self) -> &'static str {
        match self {
            Period::Month => "month",
            Period::Year => "year",
            Period::Total => "total",
        }
    }

    /// The period a goal block's `per:` names.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// What a goal aims for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An amount, exactly as written: `10000 CAD` is a mantissa of 10000 at
    /// scale 0. `commodity` is `None` for a bare number, which means the
    /// home commodity. It is kept as a [`Decimal`] rather than an
    /// [`ledgerdb::Amount`] because the commodity's precision is only known
    /// once the books are loaded.
    Amount {
        /// The number.
        value: Decimal,
        /// The commodity, or `None` for the home one.
        commodity: Option<String>,
    },
    /// A percentage, for `savings_rate`: `20%` is a mantissa of 20 at scale 0.
    Percent(Decimal),
}

impl fmt::Display for Target {
    /// `10000 CAD`, `10000` (home commodity) or `12.5%`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Amount { value, commodity } => {
                write_decimal(f, *value)?;
                match commodity {
                    Some(commodity) => write!(f, " {commodity}"),
                    None => Ok(()),
                }
            }
            Target::Percent(value) => {
                write_decimal(f, *value)?;
                f.write_str("%")
            }
        }
    }
}

/// Write `value` at its own scale, as in `-12.50`.
fn write_decimal(f: &mut fmt::Formatter<'_>, value: Decimal) -> fmt::Result {
    let magnitude = value.mantissa.unsigned_abs();
    let sign = if value.mantissa < 0 { "-" } else { "" };
    if value.scale == 0 {
        return write!(f, "{sign}{magnitude}");
    }
    // A target's scale is at most 18 (see `AmountText::qty_at`), so the power
    // fits a u64.
    let pow = 10u64.pow(u32::from(value.scale));
    let width = usize::from(value.scale);
    write!(f, "{sign}{}.{:0>width$}", magnitude / pow, magnitude % pow)
}

/// One goal, as its card's `cashflow-goal` block states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSpec {
    /// What is measured.
    pub metric: Metric,
    /// Which accounts, or `None` for the metric's default.
    pub scope: Option<Scope>,
    /// What it aims for.
    pub target: Target,
    /// The window the target applies to (see [`Metric::default_period`]).
    pub per: Period,
    /// The deadline, if any.
    pub by: Option<Date>,
}

impl GoalSpec {
    /// Which side of the target is success.
    pub fn direction(&self) -> Direction {
        self.metric.direction()
    }
}

impl fmt::Display for GoalSpec {
    /// One line: `net ≥ 10000 CAD per month in income:damus by 2027-01-01`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.metric.name(),
            self.direction().symbol(),
            self.target
        )?;
        match self.per {
            Period::Total => f.write_str(" in total")?,
            per => write!(f, " per {}", per.name())?,
        }
        match &self.scope {
            Some(Scope::Account(account)) => write!(f, " in {account}")?,
            Some(Scope::Project(project)) => write!(f, " for project {project}")?,
            None => {}
        }
        if let Some(by) = self.by {
            write!(f, " by {by}")?;
        }
        Ok(())
    }
}

/// A `cashflow-goal` block that couldn't be read, and the line of the card's
/// description it went wrong on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {kind}")]
pub struct GoalError {
    /// 1-based, counted from the top of the card's description.
    pub line: usize,
    /// What was wrong.
    pub kind: GoalErrorKind,
}

/// What was wrong with a goal block.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GoalErrorKind {
    /// The card is labelled a goal but its description has no block.
    #[error("labelled `{GOAL_LABEL}` but has no ```{FENCE_INFO} block")]
    NoBlock,
    /// The block's fence is never closed.
    #[error("the ```{FENCE_INFO} block is never closed")]
    Unterminated,
    /// A second block; a card is one goal.
    #[error("a second ```{FENCE_INFO} block (the first is on line {first}); a card holds one goal")]
    SecondBlock {
        /// The line the first block opens on.
        first: usize,
    },
    /// A line that isn't `key: value`.
    #[error("expected `key: value`")]
    NotKeyValue,
    /// A key the block doesn't have.
    #[error("unknown key `{0}` (keys: metric, account, project, target, per, by)")]
    UnknownKey(String),
    /// A key given twice.
    #[error("`{key}` is already set on line {first}")]
    DuplicateKey {
        /// The key.
        key: String,
        /// The line it was first set on.
        first: usize,
    },
    /// A key with nothing after the colon.
    #[error("`{0}` needs a value")]
    EmptyValue(&'static str),
    /// A required key is missing.
    #[error("missing `{0}`")]
    Missing(&'static str),
    /// `metric:` names no metric.
    #[error("unknown metric `{0}` (income, expenses, net, savings_rate or balance)")]
    UnknownMetric(String),
    /// `per:` names no period.
    #[error("unknown period `{0}` (month, year or total)")]
    UnknownPeriod(String),
    /// Both `account:` and `project:`.
    #[error("`account` and `project` are alternatives; set one")]
    AccountAndProject,
    /// `target:` isn't an amount or a percentage.
    #[error("bad target `{text}`: {reason}")]
    BadTarget {
        /// The value as written.
        text: String,
        /// Why it didn't parse.
        reason: String,
    },
    /// A savings rate given as an amount, or a flow given as a percentage.
    #[error("a {metric} target is {wanted}")]
    TargetKind {
        /// The metric's name.
        metric: &'static str,
        /// What it takes: "a percentage, like 20%" or "an amount, like 10000 CAD".
        wanted: &'static str,
    },
    /// A balance is a point in time, so it has no `per`.
    #[error("a balance is read at a point in time, so it takes no `per`")]
    BalancePer,
    /// `by:` isn't a date.
    #[error("bad date `{0}` (YYYY-MM-DD)")]
    BadDate(String),
}

/// The goal in `desc`, a card's description: `None` when it has no
/// `cashflow-goal` block, else the block read into a [`GoalSpec`] or why it
/// couldn't be.
///
/// Fences are followed the way Markdown does, so a ```` ```cashflow-goal ````
/// quoted inside a longer fence (an example in a design note) is not a goal.
pub fn parse_goal(desc: &str) -> Option<Result<GoalSpec, GoalError>> {
    let block = find_block(desc)?;
    Some(block.and_then(|block| parse_block(&block)))
}

/// A goal block's body lines, numbered from the top of the description.
struct Block<'a> {
    /// The line the opening fence is on.
    open: usize,
    /// `(line number, text)` of each line between the fences.
    lines: Vec<NumberedLine<'a>>,
}

/// One line of a description and its 1-based number.
struct NumberedLine<'a> {
    number: usize,
    text: &'a str,
}

/// An open fence: its character and length, which the closing fence must
/// match.
struct Fence {
    ch: char,
    len: usize,
}

/// A line that opens a fence: the fence, and the info string after it.
struct Opening<'a> {
    fence: Fence,
    info: &'a str,
}

impl Fence {
    /// The fence `line` opens, with its info string, if it is one.
    fn open(line: &str) -> Option<Opening<'_>> {
        let line = line.trim_start();
        let ch = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let len = line.chars().take_while(|c| *c == ch).count();
        (len >= 3).then(|| Opening {
            fence: Self { ch, len },
            info: line[len..].trim(),
        })
    }

    /// Whether `line` closes this fence: the same character, at least as
    /// many, and nothing after.
    fn closes(&self, line: &str) -> bool {
        let line = line.trim();
        let len = line.chars().take_while(|c| *c == self.ch).count();
        len >= self.len && len == line.chars().count()
    }
}

/// The description's goal block, `None` when there is none, or an error for
/// an unterminated one or a second one.
fn find_block(desc: &str) -> Option<Result<Block<'_>, GoalError>> {
    let mut lines = desc.lines().enumerate().map(|(i, text)| NumberedLine {
        number: i + 1,
        text,
    });
    let mut found: Option<Block<'_>> = None;
    while let Some(line) = lines.next() {
        let Some(Opening { fence, info }) = Fence::open(line.text) else {
            continue;
        };
        let is_goal = info.split_whitespace().next() == Some(FENCE_INFO);
        if is_goal && let Some(first) = &found {
            return Some(Err(GoalError {
                line: line.number,
                kind: GoalErrorKind::SecondBlock { first: first.open },
            }));
        }
        let mut body = Vec::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if fence.closes(inner.text) {
                closed = true;
                break;
            }
            if is_goal {
                body.push(inner);
            }
        }
        if !is_goal {
            continue;
        }
        if !closed {
            return Some(Err(GoalError {
                line: line.number,
                kind: GoalErrorKind::Unterminated,
            }));
        }
        found = Some(Block {
            open: line.number,
            lines: body,
        });
    }
    found.map(Ok)
}

/// A value from the block and the line it was on.
struct Field<'a> {
    line: usize,
    value: &'a str,
}

/// Every key a block may set, each at most once.
#[derive(Default)]
struct Fields<'a> {
    metric: Option<Field<'a>>,
    account: Option<Field<'a>>,
    project: Option<Field<'a>>,
    target: Option<Field<'a>>,
    per: Option<Field<'a>>,
    by: Option<Field<'a>>,
}

/// Where one key's value goes, and the key's name as a `'static` str for
/// errors.
struct Slot<'s, 'a> {
    field: &'s mut Option<Field<'a>>,
    name: &'static str,
}

impl<'a> Fields<'a> {
    /// The slot for `key`, or `None` for a key the block doesn't have.
    fn slot(&mut self, key: &str) -> Option<Slot<'_, 'a>> {
        let (field, name) = match key {
            "metric" => (&mut self.metric, "metric"),
            "account" => (&mut self.account, "account"),
            "project" => (&mut self.project, "project"),
            "target" => (&mut self.target, "target"),
            "per" => (&mut self.per, "per"),
            "by" => (&mut self.by, "by"),
            _ => return None,
        };
        Some(Slot { field, name })
    }
}

/// `text` without a trailing `# comment`. A `#` only starts one at the start
/// of the line or after whitespace, so it can't eat into a value.
fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let hash = (0..bytes.len())
        .find(|&i| bytes[i] == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t'));
    match hash {
        Some(i) => &text[..i],
        None => text,
    }
}

/// Read a block's `key: value` lines into a [`GoalSpec`].
fn parse_block(block: &Block<'_>) -> Result<GoalSpec, GoalError> {
    let mut fields = Fields::default();
    for line in &block.lines {
        let text = strip_comment(line.text).trim();
        if text.is_empty() {
            continue;
        }
        let err = |kind| GoalError {
            line: line.number,
            kind,
        };
        let (key, value) = text
            .split_once(':')
            .ok_or_else(|| err(GoalErrorKind::NotKeyValue))?;
        let key = key.trim();
        let Slot { field: slot, name } = fields
            .slot(key)
            .ok_or_else(|| err(GoalErrorKind::UnknownKey(key.to_owned())))?;
        if let Some(first) = slot {
            return Err(err(GoalErrorKind::DuplicateKey {
                key: name.to_owned(),
                first: first.line,
            }));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(err(GoalErrorKind::EmptyValue(name)));
        }
        *slot = Some(Field {
            line: line.number,
            value,
        });
    }
    spec_of(block.open, &fields)
}

/// Check and convert the fields a block set.
fn spec_of(open: usize, fields: &Fields<'_>) -> Result<GoalSpec, GoalError> {
    let at = |line, kind| GoalError { line, kind };
    let missing = |key| at(open, GoalErrorKind::Missing(key));

    let metric_field = fields.metric.as_ref().ok_or_else(|| missing("metric"))?;
    let metric = Metric::from_name(metric_field.value).ok_or_else(|| {
        at(
            metric_field.line,
            GoalErrorKind::UnknownMetric(metric_field.value.to_owned()),
        )
    })?;

    let scope = match (&fields.account, &fields.project) {
        (Some(_), Some(project)) => {
            return Err(at(project.line, GoalErrorKind::AccountAndProject));
        }
        (Some(account), None) => Some(Scope::Account(account.value.to_owned())),
        (None, Some(project)) => Some(Scope::Project(project.value.to_owned())),
        (None, None) => None,
    };

    let target_field = fields.target.as_ref().ok_or_else(|| missing("target"))?;
    let target = parse_target(target_field.value).map_err(|reason| {
        at(
            target_field.line,
            GoalErrorKind::BadTarget {
                text: target_field.value.to_owned(),
                reason,
            },
        )
    })?;
    let wanted = match (metric, &target) {
        (Metric::SavingsRate, Target::Amount { .. }) => Some("a percentage, like 20%"),
        (Metric::SavingsRate, Target::Percent(_)) => None,
        (_, Target::Percent(_)) => Some("an amount, like 10000 CAD"),
        (_, Target::Amount { .. }) => None,
    };
    if let Some(wanted) = wanted {
        return Err(at(
            target_field.line,
            GoalErrorKind::TargetKind {
                metric: metric.name(),
                wanted,
            },
        ));
    }

    let per = match &fields.per {
        None => metric.default_period(),
        Some(field) => {
            let per = Period::from_name(field.value).ok_or_else(|| {
                at(
                    field.line,
                    GoalErrorKind::UnknownPeriod(field.value.to_owned()),
                )
            })?;
            if metric == Metric::Balance && per != Period::Total {
                return Err(at(field.line, GoalErrorKind::BalancePer));
            }
            per
        }
    };

    let by = match &fields.by {
        None => None,
        Some(field) => Some(
            Date::parse(field.value)
                .map_err(|_| at(field.line, GoalErrorKind::BadDate(field.value.to_owned())))?,
        ),
    };

    Ok(GoalSpec {
        metric,
        scope,
        target,
        per,
        by,
    })
}

/// `20%` as a percentage, else an amount such as `10000 CAD` or `-500`.
fn parse_target(text: &str) -> Result<Target, String> {
    if let Some(number) = text.strip_suffix('%') {
        let amount = AmountText::parse(number).map_err(|e| e.to_string())?;
        if !amount.commodity.is_empty() {
            return Err("a percentage takes no commodity".to_owned());
        }
        return decimal_of(&amount).map(Target::Percent);
    }
    let amount = AmountText::parse(text).map_err(|e| e.to_string())?;
    let commodity = (!amount.commodity.is_empty()).then(|| amount.commodity.to_owned());
    Ok(Target::Amount {
        value: decimal_of(&amount)?,
        commodity,
    })
}

/// A written amount as a [`Decimal`] at the scale it was written with.
fn decimal_of(amount: &AmountText<'_>) -> Result<Decimal, String> {
    let scale = amount.decimals();
    let mantissa = amount.qty_at(scale).map_err(|e| e.to_string())?;
    Ok(Decimal { mantissa, scale })
}

/// A goal found on a board: its card, and its spec or why the spec couldn't
/// be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    /// The card, as a full `headway:<board>/<word-id>` ref.
    pub card_ref: String,
    /// The board's slug.
    pub board: String,
    /// The board's owner: the account itself, or whoever shared the board.
    /// A slug is only unique per owner, so this is what tells two boards of
    /// one slug apart.
    pub owner: [u8; 32],
    /// The card's id.
    pub card: [u8; 32],
    /// The card's title.
    pub title: String,
    /// The name of the column the card is in, such as `In Progress`.
    pub column: String,
    /// Whether that column is terminal (the goal's card is done).
    pub done: bool,
    /// The goal, or why its block couldn't be read.
    pub spec: Result<GoalSpec, GoalError>,
}

/// The goals on `boards`, in the order the boards are given, then the
/// board's column and card order.
///
/// Each slug names the roster owner's own board and every board of that
/// slug shared with them, folded by whichever transport it uses (see
/// [`headway::boards::load_board`]). A deleted board, or one not in the
/// cache, contributes nothing. A card is a goal when it is labelled
/// [`GOAL_LABEL`]; one labelled so without a block is returned with
/// [`GoalErrorKind::NoBlock`] rather than skipped. A block on an unlabelled
/// card is not a goal.
///
/// The roster must have been [loaded](Roster::load) against `ndb`, which is
/// what registers the sealed boards' roots: without it they fold empty.
#[profiling::function]
pub fn discover(ndb: &Ndb, roster: &Roster, boards: &[impl AsRef<str>]) -> Vec<Goal> {
    let shared = boards::list_shared_with_me(ndb, roster);
    let mut goals = Vec::new();
    for slug in boards {
        let slug = slug.as_ref();
        let own = boards::load_board(ndb, roster, slug).filter(|view| !view.deleted);
        let theirs = shared.iter().filter(|view| view.id == slug);
        for view in own.iter().chain(theirs) {
            goals.extend(board_goals(view));
        }
    }
    goals
}

/// The goals on one folded board, in column then card order.
pub fn board_goals(view: &BoardView) -> impl Iterator<Item = Goal> + '_ {
    view.columns.iter().flat_map(move |column| {
        let done = view.column_is_terminal(&column.id);
        column
            .cards
            .iter()
            .filter(|card| card.labels.iter().any(|label| label == GOAL_LABEL))
            .map(move |card| goal_of(view, &column.name, done, card))
    })
}

/// One goal-labelled card as a [`Goal`].
fn goal_of(view: &BoardView, column: &str, done: bool, card: &CardView) -> Goal {
    let spec = parse_goal(&card.description).unwrap_or(Err(GoalError {
        line: 1,
        kind: GoalErrorKind::NoBlock,
    }));
    Goal {
        card_ref: headway::wordid::card_ref(&view.id, card.id.bytes()),
        board: view.id.clone(),
        owner: view.author,
        card: *card.id.bytes(),
        title: card.title.clone(),
        column: column.to_owned(),
        done,
        spec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The description with its goal block, for the parse tests.
    fn goal(body: &str) -> String {
        format!("Save up.\n\n```cashflow-goal\n{body}```\n\nmore notes\n")
    }

    /// The spec of a block that must parse.
    fn spec(body: &str) -> GoalSpec {
        parse_goal(&goal(body))
            .expect("a block")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// The error of a block that must not parse.
    fn error(desc: &str) -> GoalError {
        parse_goal(desc).expect("a block").unwrap_err()
    }

    fn decimal(mantissa: i64, scale: u8) -> Decimal {
        Decimal { mantissa, scale }
    }

    /// Every field, with the epic's own trailing comments.
    #[test]
    fn every_field_parses() {
        let spec = spec(
            "metric: net            # income | expenses | net | savings_rate | balance\n\
             account: income:damus  # optional subtree\n\
             target: 10000.50 CAD\n\
             per: year\n\
             by: 2027-01-01         # optional deadline\n",
        );
        assert_eq!(
            spec,
            GoalSpec {
                metric: Metric::Net,
                scope: Some(Scope::Account("income:damus".to_owned())),
                target: Target::Amount {
                    value: decimal(1000050, 2),
                    commodity: Some("CAD".to_owned()),
                },
                per: Period::Year,
                by: Some(Date::parse("2027-01-01").unwrap()),
            }
        );
        assert_eq!(spec.direction(), Direction::AtLeast);
        assert_eq!(
            spec.to_string(),
            "net ≥ 10000.50 CAD per year in income:damus by 2027-01-01"
        );
    }

    /// Only metric and target are required; the rest default by metric.
    #[test]
    fn defaults() {
        let income = spec("metric: income\ntarget: 5000\n");
        assert_eq!(income.scope, None);
        assert_eq!(income.per, Period::Month);
        assert_eq!(income.by, None);
        assert_eq!(
            income.target,
            Target::Amount {
                value: decimal(5000, 0),
                commodity: None,
            },
            "a bare number is in the home commodity"
        );

        let balance = spec("metric: balance\naccount: assets:savings\ntarget: 1.5 BTC\n");
        assert_eq!(balance.per, Period::Total);
        assert_eq!(
            balance.to_string(),
            "balance ≥ 1.5 BTC in total in assets:savings"
        );

        let rate = spec("metric: savings_rate\ntarget: 12.5%\n");
        assert_eq!(rate.target, Target::Percent(decimal(125, 1)));
        assert_eq!(rate.to_string(), "savings_rate ≥ 12.5% per month");

        let project = spec("metric: income\nproject: damus\ntarget: 1 CAD\nper: total\n");
        assert_eq!(project.scope, Some(Scope::Project("damus".to_owned())));
        assert_eq!(project.per, Period::Total);
    }

    /// Expenses goals are ceilings, every other metric a floor.
    #[test]
    fn expenses_are_at_most() {
        assert_eq!(
            spec("metric: expenses\ntarget: 3000 CAD\n").direction(),
            Direction::AtMost
        );
        for metric in [
            Metric::Income,
            Metric::Net,
            Metric::SavingsRate,
            Metric::Balance,
        ] {
            assert_eq!(metric.direction(), Direction::AtLeast, "{metric:?}");
        }
    }

    /// No block is `None`, and so is a goal block quoted inside a longer
    /// fence, like the epic's own design note.
    #[test]
    fn no_block_and_quoted_block_are_none() {
        assert_eq!(parse_goal("just a card"), None);
        assert_eq!(parse_goal("```rust\nlet x = 1;\n```\n"), None);
        let quoted = "Spec:\n````\n```cashflow-goal\nmetric: net\ntarget: 1\n```\n````\n";
        assert_eq!(parse_goal(quoted), None);
    }

    /// A tilde fence and a longer backtick fence both work, closed by at
    /// least as many of the same character.
    #[test]
    fn fence_variants() {
        let tilde = "~~~cashflow-goal\nmetric: net\ntarget: 1\n~~~\n";
        assert!(parse_goal(tilde).unwrap().is_ok());
        let long = "````cashflow-goal\nmetric: net\n```\ntarget: 1\n````\n";
        assert_eq!(
            error(long).kind,
            GoalErrorKind::NotKeyValue,
            "a shorter fence inside is body, not the close"
        );
    }

    /// Each malformed block names its error and the description line it is
    /// on (the block in [`goal`] opens on line 3, so its first body line is 4).
    #[test]
    fn malformed_blocks_name_the_line() {
        let cases: &[(&str, usize, GoalErrorKind)] = &[
            ("metric: net\n", 3, GoalErrorKind::Missing("target")),
            ("target: 1\n", 3, GoalErrorKind::Missing("metric")),
            (
                "metric: net\ntargt: 1\n",
                5,
                GoalErrorKind::UnknownKey("targt".to_owned()),
            ),
            ("metric: net\nnonsense\n", 5, GoalErrorKind::NotKeyValue),
            (
                "metric: net\nmetric: income\n",
                5,
                GoalErrorKind::DuplicateKey {
                    key: "metric".to_owned(),
                    first: 4,
                },
            ),
            ("metric:\n", 4, GoalErrorKind::EmptyValue("metric")),
            (
                "metric: profit\ntarget: 1\n",
                4,
                GoalErrorKind::UnknownMetric("profit".to_owned()),
            ),
            (
                "metric: net\ntarget: 1\nper: week\n",
                6,
                GoalErrorKind::UnknownPeriod("week".to_owned()),
            ),
            (
                "metric: net\naccount: income\nproject: damus\ntarget: 1\n",
                6,
                GoalErrorKind::AccountAndProject,
            ),
            (
                "metric: savings_rate\ntarget: 100 CAD\n",
                5,
                GoalErrorKind::TargetKind {
                    metric: "savings_rate",
                    wanted: "a percentage, like 20%",
                },
            ),
            (
                "metric: net\ntarget: 20%\n",
                5,
                GoalErrorKind::TargetKind {
                    metric: "net",
                    wanted: "an amount, like 10000 CAD",
                },
            ),
            (
                "metric: balance\ntarget: 1\nper: month\n",
                6,
                GoalErrorKind::BalancePer,
            ),
            (
                "metric: net\ntarget: 1\nby: next year\n",
                6,
                GoalErrorKind::BadDate("next year".to_owned()),
            ),
        ];
        for (body, line, kind) in cases {
            let err = error(&goal(body));
            assert_eq!((err.line, &err.kind), (*line, kind), "{body}");
        }

        let bad = error(&goal("metric: net\ntarget: lots\n"));
        assert_eq!(bad.line, 5);
        assert!(matches!(bad.kind, GoalErrorKind::BadTarget { .. }), "{bad}");
        assert!(
            bad.to_string().starts_with("line 5: bad target `lots`"),
            "{bad}"
        );
    }

    /// An unclosed block and a second block are errors at their fence.
    #[test]
    fn unterminated_and_second_blocks() {
        let open = "intro\n```cashflow-goal\nmetric: net\n";
        assert_eq!(
            error(open),
            GoalError {
                line: 2,
                kind: GoalErrorKind::Unterminated,
            }
        );
        let two = format!(
            "{}\n```cashflow-goal\nmetric: net\n```\n",
            goal("metric: net\ntarget: 1\n")
        );
        let err = error(&two);
        assert_eq!(err.kind, GoalErrorKind::SecondBlock { first: 3 });
    }

    /// A `#` inside a value is kept; only a whitespace-led one is a comment.
    #[test]
    fn comments_need_leading_whitespace() {
        assert_eq!(strip_comment("account: a#b # note"), "account: a#b ");
        assert_eq!(strip_comment("# whole line"), "");
    }
}

#[cfg(test)]
mod discovery_tests {
    use headway::boards::{Identity, Roster};
    use headway::event::{self, BoardView};
    use headway::store::{self, BoardAction, NewCard, NoPublish, Signer};
    use headway::teams::RootRegistry;
    use nostrdb::{Filter, Ndb, SubscriptionStream, Transaction};
    use nostrdb_net::FullKeypair;

    use super::*;
    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    /// A valid goal description.
    const GOAL: &str = "```cashflow-goal\nmetric: income\ntarget: 100 CAD\n```\n";

    /// Re-run `check` after each batch of ingested notes until it returns
    /// something. Subscribed before the first check, so a note that lands
    /// between the write and the subscription is seen by that check, and one
    /// after it wakes the next.
    async fn settle<T>(ndb: &Ndb, mut check: impl FnMut() -> Option<T>) -> T {
        let kinds = event::HEADWAY_KINDS
            .iter()
            .map(|k| *k as u64)
            .chain([u64::from(nostrdb_net::sns::KEYSHARE_KIND)]);
        let sub = ndb
            .subscribe(&[Filter::new().kinds(kinds).build()])
            .unwrap();
        let mut stream = SubscriptionStream::new(ndb.clone(), sub);
        loop {
            if let Some(found) = check() {
                return found;
            }
            stream
                .wait_for_notes(1, INGEST_TIMEOUT)
                .await
                .expect("a note ingested before the deadline");
        }
    }

    /// `kp`'s roster, with its sealed boards' roots registered.
    fn roster(ndb: &Ndb, kp: &FullKeypair, registry: &mut RootRegistry) -> Roster {
        Roster::load(ndb, Identity::own(kp.pubkey), registry)
    }

    /// `kp`'s own board `slug`, once it folds.
    async fn board(
        ndb: &Ndb,
        kp: &FullKeypair,
        registry: &mut RootRegistry,
        slug: &str,
    ) -> BoardView {
        settle(ndb, || {
            boards::load_board(ndb, &roster(ndb, kp, registry), slug)
        })
        .await
    }

    /// Add a card to `kp`'s own board `slug`, sealed when the board is.
    async fn add(
        ndb: &Ndb,
        kp: &FullKeypair,
        registry: &mut RootRegistry,
        slug: &str,
        title: &str,
        description: &str,
        labels: &[&str],
    ) {
        let view = board(ndb, kp, registry, slug).await;
        let channel = roster(ndb, kp, registry).channel(slug);
        let secret = kp.secret_key.secret_bytes();
        let signer = Signer::new(&secret, channel.as_ref());
        let declined = store::apply(
            ndb,
            slug,
            &view,
            &kp.pubkey,
            &signer,
            BoardAction::AddCard(NewCard {
                col: 0,
                title: title.to_owned(),
                description: description.to_owned(),
                labels: labels.iter().map(|l| (*l).to_owned()).collect(),
                ..NewCard::default()
            }),
            &mut NoPublish,
        );
        assert!(declined.is_none(), "{declined:?}");
        settle(ndb, || {
            boards::load_board(ndb, &roster(ndb, kp, registry), slug)
                .filter(|v| v.columns[0].cards.iter().any(|c| c.title == title))
        })
        .await;
    }

    /// Add a goal card to Alice's sealed `cashflow` board, as Alice, through
    /// the channel her `root` derives (her roster isn't in this nostrdb; see
    /// the test).
    async fn add_alices(ndb: &Ndb, alice: &FullKeypair, root: &[u8; 32], title: &str) {
        let channel = store::SnsChannel {
            keys: nostrdb_net::sns::derive_sns_keys(root).unwrap(),
        };
        let addr = event::board_address(&alice.pubkey, "cashflow");
        let team = [channel.keys.team_keypair.pubkey];
        let fold = || {
            let txn = Transaction::new(ndb).ok()?;
            event::load_shared_board(ndb, &txn, &addr, &team)
        };
        let view = settle(ndb, fold).await;
        let secret = alice.secret_key.secret_bytes();
        let declined = store::apply(
            ndb,
            "cashflow",
            &view,
            &alice.pubkey,
            &Signer::shared(&secret, &channel),
            BoardAction::AddCard(NewCard {
                col: 0,
                title: title.to_owned(),
                description: GOAL.to_owned(),
                labels: vec![GOAL_LABEL.to_owned()],
                ..NewCard::default()
            }),
            &mut NoPublish,
        );
        assert!(declined.is_none(), "{declined:?}");
    }

    /// Discovery finds the goal cards on the named boards — a plaintext one,
    /// a sealed team-of-one one, and one another owner shared in — in board
    /// order, surfaces a labelled card without a block or with a broken one,
    /// and ignores unlabelled cards and unlisted boards.
    #[tokio::test]
    async fn discovers_goals_on_plain_sealed_and_shared_boards() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let me = FullKeypair::generate();
        let alice = FullKeypair::generate();
        let secret = me.secret_key.secret_bytes();
        let alice_secret = alice.secret_key.secret_bytes();
        // Register my key so the key-share gift-wraps to me unwrap into the
        // kind-1082s my roster is built from. Not Alice's: her self-share and
        // her share to me are one rumor id, so nostrdb would keep whichever
        // it unwrapped first, and hers would hide mine. She writes through
        // her channel directly instead.
        ndb.add_key(&secret);
        let mut registry = RootRegistry::default();

        // A plaintext board, a sealed one (how boards are born today) and an
        // unlisted one, all mine.
        store::seed_board(
            ndb,
            &me.pubkey,
            &secret,
            "personal",
            "Personal",
            &mut NoPublish,
        );
        store::seed_board(ndb, &me.pubkey, &secret, "work", "Work", &mut NoPublish);
        let root = nostrdb_net::sns::derive_board_root(&secret, "cashflow");
        assert!(store::create_shared_board(
            ndb,
            &me.pubkey,
            &secret,
            "cashflow",
            "Cashflow",
            &root,
            &mut NoPublish,
        ));
        // Alice's own sealed `cashflow`, shared with me.
        let alice_root = nostrdb_net::sns::derive_board_root(&alice_secret, "cashflow");
        assert!(store::create_shared_board(
            ndb,
            &alice.pubkey,
            &alice_secret,
            "cashflow",
            "Ours",
            &alice_root,
            &mut NoPublish,
        ));
        let alice_addr = event::board_address(&alice.pubkey, "cashflow");
        assert!(store::share_board(
            ndb,
            &alice_secret,
            &me.pubkey,
            &alice_addr,
            &alice_root,
            &mut NoPublish,
        ));

        let mut add_mine = async |slug, title, desc, labels: &[&str]| {
            add(ndb, &me, &mut registry, slug, title, desc, labels).await
        };
        add_mine("personal", "Earn 100", GOAL, &[GOAL_LABEL]).await;
        add_mine("personal", "Not a goal", GOAL, &["money"]).await;
        add_mine("personal", "Forgot the block", "", &[GOAL_LABEL]).await;
        add_mine("cashflow", "Sealed goal", GOAL, &[GOAL_LABEL, "q4"]).await;
        add_mine(
            "cashflow",
            "Broken",
            "```cashflow-goal\nmetric: net\n```\n",
            &[GOAL_LABEL],
        )
        .await;
        add_mine("work", "Unlisted", GOAL, &[GOAL_LABEL]).await;
        add_alices(ndb, &alice, &alice_root, "Alice's goal").await;

        let goals = settle(ndb, || {
            let roster = roster(ndb, &me, &mut registry);
            let goals = discover(ndb, &roster, &["cashflow", "personal"]);
            (goals.len() == 5).then_some(goals)
        })
        .await;

        // My board of each slug, then the ones shared with me, in the order
        // the boards are given. Within a board the column order holds, but
        // headway's fold can order two cards placed in one second either way,
        // so each board's titles are compared as a set.
        let boards: Vec<(&str, bool)> = goals
            .iter()
            .map(|g| (g.board.as_str(), &g.owner == alice.pubkey.bytes()))
            .collect();
        assert_eq!(
            boards,
            [
                ("cashflow", false),
                ("cashflow", false),
                ("cashflow", true),
                ("personal", false),
                ("personal", false),
            ]
        );
        let titles = |range: std::ops::Range<usize>| {
            let mut titles: Vec<&str> = goals[range].iter().map(|g| g.title.as_str()).collect();
            titles.sort_unstable();
            titles
        };
        assert_eq!(titles(0..2), ["Broken", "Sealed goal"]);
        assert_eq!(titles(2..3), ["Alice's goal"]);
        assert_eq!(titles(3..5), ["Earn 100", "Forgot the block"]);

        let by_title = |title: &str| goals.iter().find(|g| g.title == title).unwrap();
        let earn = by_title("Earn 100");
        assert_eq!(earn.board, "personal");
        assert!(
            earn.card_ref.starts_with("headway:personal/"),
            "{}",
            earn.card_ref
        );
        assert_eq!(
            earn.card_ref,
            headway::wordid::card_ref("personal", &earn.card)
        );
        assert!(!earn.column.is_empty());
        let spec = earn.spec.as_ref().unwrap();
        assert_eq!(spec.metric, Metric::Income);
        assert_eq!(spec.to_string(), "income ≥ 100 CAD per month");

        assert_eq!(&earn.owner, me.pubkey.bytes());
        let sealed = by_title("Sealed goal");
        assert!(sealed.spec.is_ok(), "a sealed board's goal folds");
        assert!(sealed.card_ref.starts_with("headway:cashflow/"));
        assert!(
            by_title("Alice's goal").spec.is_ok(),
            "a shared board's goal folds"
        );
        assert_eq!(
            by_title("Broken").spec.as_ref().unwrap_err().kind,
            GoalErrorKind::Missing("target")
        );
        assert_eq!(
            by_title("Forgot the block").spec.as_ref().unwrap_err().kind,
            GoalErrorKind::NoBlock
        );
    }
}
