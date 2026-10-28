//! RBC's monthly e-statement PDFs, read for the `rbc` importer, which
//! files their rows beside its CSV downloads' (see [`super::rbc`]).
//!
//! RBC's "Download Transactions" CSV only reaches back a few months; the
//! statements go back years. There are two layouts, told apart by their
//! text, and both were read off real statements, not guessed.
//!
//! **Bank accounts** (chequing and savings) print one activity table:
//!
//! ```text
//! From March 2, 2026 to April 1, 2026          Your account number: 00000-0001234
//! Your opening balance on March 2, 2026            $1,000.00
//! Total deposits into your account              + 3,000.00
//! Total withdrawals from your account           -   500.00
//! Your closing balance on April 1, 2026        = $3,500.00
//! Date    Description                      Withdrawals ($)  Deposits ($)  Balance ($)
//!         Opening Balance                                                    1,000.00
//! 6 Mar   e-Transfer - Autodeposit Shop
//!           C1A2B3C4D5E6                                       3,000.00      4,000.00
//!         Monthly fee                               500.00
//!         ...
//!         Closing Balance                                                   $3,500.00
//! ```
//!
//! A date is printed only on a day's first row, and without a year, which
//! comes from the period; a balance only on a day's last. A description can
//! take two lines, its amount on the last. Money out and money in are the
//! same kind of number, told apart only by their column, so the table is
//! read from [`pdf_text`](super::pdf_text)'s positioned cells: each figure
//! goes in the header column its right edge is nearest to. The table
//! starts again under a repeated header on each page.
//!
//! **Visa cards** list a row per charge with its transaction and posting
//! dates, `APR 04  APR 06  SOME SHOP  $58.00`, a foreign charge's original
//! amount and rate on the line below it (`Foreign Currency-USD 40.00` and
//! `Exchange rate-1.450000`), and a payment as a negative amount. The
//! previous and new balance are what's owed, a credit balance negative.
//!
//! Every amount here is the owner's side, as in the CSV: money out of the
//! account and a charge on the card are negative.
//!
//! **The balance check.** A statement prints where its balance started and
//! ended. A statement whose rows don't take the one to the other is an
//! error naming both, as is a bank table whose day-end balance doesn't
//! follow from its rows, or whose deposits and withdrawals don't add up to
//! the summary's totals. This is what proves the columns were read right.

use ledgerdb::{Date, Ymd};

use super::csv_rows::{parse_cents, squeeze};
use super::pdf_text::{Cell, Line};

/// One statement, read and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Statement {
    /// The account or card number's last four digits.
    pub last4: String,
    /// The period's first day.
    pub from: Date,
    /// The period's last day, the statement's date.
    pub to: Date,
    /// The rows, in the statement's order.
    pub rows: Vec<StatementRow>,
}

/// One transaction of a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StatementRow {
    /// The date the row is filed under: a bank row's date, a card row's
    /// transaction date.
    pub date: Date,
    /// A card row's posting date; `None` on a bank statement.
    pub posted: Option<Date>,
    /// Signed cents, the change to the account from its owner's side.
    pub cents: i64,
    /// The description, its lines joined by a space.
    pub description: String,
    /// A card row's foreign charge as `<qty> <currency> @ <rate>`, the shape
    /// the CSV writes it in; empty otherwise.
    pub foreign: String,
}

/// Read the statement in `lines`, checking its balances.
pub(super) fn parse(lines: &[Line]) -> Result<Statement, String> {
    if lines
        .iter()
        .any(|l| l.text().starts_with("STATEMENT FROM "))
    {
        return parse_card(lines);
    }
    if lines
        .iter()
        .any(|l| l.text().starts_with("Details of your account activity"))
    {
        return parse_bank(lines);
    }
    Err(
        "not an RBC bank or Visa statement: it has neither an account activity table nor a \
         card statement period"
            .to_owned(),
    )
}

/// Where a bank table's columns are on one page, from its header row.
struct Columns {
    /// The page the header row is on; the table ends with it.
    page: u32,
    /// Text starting left of this is a date.
    description: f64,
    /// Text starting left of this, and at or right of `description`, is a
    /// description; right of it, a figure. Figures are set flush right
    /// under their header, so even a seven-digit one starts right of it.
    figures: f64,
    withdrawals: f64,
    deposits: f64,
    balance: f64,
}

impl Columns {
    /// The columns of the header row `line`, if it is one.
    fn of(line: &Line) -> Option<Self> {
        let find = |name: &str| line.cells.iter().find(|c| c.text == name);
        find("Date")?;
        let description = find("Description")?;
        let withdrawals = find("Withdrawals ($)")?;
        let deposits = find("Deposits ($)")?;
        let balance = find("Balance ($)")?;
        Some(Self {
            page: line.page,
            description: description.left - 2.0,
            figures: withdrawals.left - 30.0,
            withdrawals: withdrawals.right,
            deposits: deposits.right,
            balance: balance.right,
        })
    }

    /// The figure column `cell`'s right edge is nearest.
    fn figure(&self, cell: &Cell) -> Figure {
        let off = |figure: &Figure| {
            let edge = match figure {
                Figure::Withdrawal => self.withdrawals,
                Figure::Deposit => self.deposits,
                Figure::Balance => self.balance,
            };
            (cell.right - edge).abs()
        };
        [Figure::Withdrawal, Figure::Deposit, Figure::Balance]
            .into_iter()
            .min_by(|a, b| off(a).total_cmp(&off(b)))
            .unwrap_or(Figure::Balance)
    }
}

/// Which figure column a number is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Figure {
    Withdrawal,
    Deposit,
    Balance,
}

/// One table line, its cells sorted into the columns.
#[derive(Default)]
struct TableLine {
    date: Option<String>,
    description: Option<String>,
    withdrawal: Option<i64>,
    deposit: Option<i64>,
    balance: Option<i64>,
}

impl TableLine {
    /// `line`'s cells in `columns`, or `None` when the line isn't part of
    /// the table: something in a figure column that isn't a figure, or two
    /// figures in one column.
    fn of(line: &Line, columns: &Columns) -> Option<Self> {
        let mut out = Self::default();
        for cell in &line.cells {
            if cell.left < columns.description {
                set(&mut out.date, cell.text.clone())?;
            } else if cell.left < columns.figures {
                set(&mut out.description, cell.text.clone())?;
            } else {
                let cents = amount(&cell.text)?;
                let slot = match columns.figure(cell) {
                    Figure::Withdrawal => &mut out.withdrawal,
                    Figure::Deposit => &mut out.deposit,
                    Figure::Balance => &mut out.balance,
                };
                set(slot, cents)?;
            }
        }
        Some(out)
    }
}

/// Fill `slot` with `value`, or `None` when it was already full.
fn set<T>(slot: &mut Option<T>, value: T) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value);
    Some(())
}

/// The period a statement covers.
struct Period {
    from: Date,
    to: Date,
}

/// What a bank statement's summary says.
#[derive(Default)]
struct Summary {
    opening: Option<i64>,
    closing: Option<i64>,
    deposits: Option<i64>,
    withdrawals: Option<i64>,
}

/// Read a bank statement: see the module docs.
fn parse_bank(lines: &[Line]) -> Result<Statement, String> {
    let period = lines
        .iter()
        .find_map(|l| bank_period(&l.text()))
        .ok_or("no \"From <date> to <date>\" period")?;
    let last4 = lines
        .iter()
        .find_map(|l| {
            let at = l
                .cells
                .iter()
                .position(|c| c.text == "Your account number:")?;
            last4(&l.cells.get(at + 1)?.text)
        })
        .ok_or("no \"Your account number:\"")?;

    let mut summary = Summary::default();
    for line in lines {
        let (Some(first), Some(last)) = (line.cells.first(), line.cells.last()) else {
            continue;
        };
        let slot = if first.text.starts_with("Your opening balance on") {
            &mut summary.opening
        } else if first.text.starts_with("Your closing balance on") {
            &mut summary.closing
        } else if first.text == "Total deposits into your account" {
            &mut summary.deposits
        } else if first.text == "Total withdrawals from your account" {
            &mut summary.withdrawals
        } else {
            continue;
        };
        if slot.is_none() {
            *slot = amount(&last.text);
        }
    }
    let opening = summary.opening.ok_or("no \"Your opening balance\"")?;
    let closing = summary.closing.ok_or("no \"Your closing balance\"")?;

    let mut rows = Vec::new();
    let mut table: Option<Columns> = None;
    let mut balance = opening;
    let mut date: Option<Date> = None;
    let mut pending: Vec<String> = Vec::new();
    let mut deposits = 0i64;
    let mut withdrawals = 0i64;
    let mut closed = false;
    for line in lines {
        if let Some(columns) = Columns::of(line) {
            table = Some(columns);
            continue;
        }
        let Some(columns) = &table else {
            continue;
        };
        let read = (line.page == columns.page)
            .then(|| TableLine::of(line, columns))
            .flatten();
        let Some(read) = read else {
            table = None;
            continue;
        };
        let description = read.description.as_deref().unwrap_or("");
        if description == "Opening Balance" {
            if read.balance != Some(opening) {
                return Err(format!(
                    "the table opens at {} but the summary at {}",
                    show(read.balance),
                    cents(opening)
                ));
            }
            continue;
        }
        if description == "Closing Balance" {
            if read.balance != Some(closing) {
                return Err(format!(
                    "the table closes at {} but the summary at {}",
                    show(read.balance),
                    cents(closing)
                ));
            }
            closed = true;
            break;
        }
        if let Some(text) = &read.date {
            date = Some(day_in(text, &period).ok_or_else(|| {
                format!(
                    "bad date {text:?} for a statement from {} to {}",
                    period.from, period.to
                )
            })?);
        }
        if !description.is_empty() {
            pending.push(description.to_owned());
        }
        let change = match (read.withdrawal, read.deposit) {
            (Some(out), None) => Some(-out),
            (None, Some(into)) => Some(into),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(format!(
                    "a row with both a withdrawal and a deposit: {:?}",
                    line.text()
                ));
            }
        };
        if let Some(change) = change {
            let Some(date) = date else {
                return Err(format!("a row before any date: {:?}", line.text()));
            };
            if pending.is_empty() {
                return Err(format!("an amount with no description: {:?}", line.text()));
            }
            if change < 0 {
                withdrawals -= change;
            } else {
                deposits += change;
            }
            balance += change;
            rows.push(StatementRow {
                date,
                posted: None,
                cents: change,
                description: squeeze(&pending.join(" ")),
                foreign: String::new(),
            });
            pending.clear();
        }
        if let Some(stated) = read.balance
            && stated != balance
        {
            return Err(format!(
                "the balance after {} reads {} but its rows come to {}",
                date.map_or("the opening".to_owned(), |d| d.to_string()),
                cents(stated),
                cents(balance)
            ));
        }
    }
    if !closed {
        return Err("the activity table has no \"Closing Balance\" row".to_owned());
    }
    if !pending.is_empty() {
        return Err(format!(
            "a description with no amount: {:?}",
            pending.join(" ")
        ));
    }
    if balance != closing {
        return Err(format!(
            "the rows take the opening balance {} to {}, but the closing balance is {}",
            cents(opening),
            cents(balance),
            cents(closing)
        ));
    }
    let totals = [
        ("deposits", summary.deposits, deposits),
        ("withdrawals", summary.withdrawals, withdrawals),
    ];
    for (what, stated, read) in totals {
        if stated.is_some_and(|stated| stated.abs() != read) {
            return Err(format!(
                "the rows' {what} come to {} but the summary's total is {}",
                cents(read),
                show(stated.map(i64::abs))
            ));
        }
    }
    Ok(Statement {
        last4,
        from: period.from,
        to: period.to,
        rows,
    })
}

/// A bank statement's period, from text holding `From March 2, 2026 to
/// April 1, 2026`.
fn bank_period(text: &str) -> Option<Period> {
    let rest = &text[text.find("From ")? + "From ".len()..];
    let (from, to) = rest.split_once(" to ")?;
    Some(Period {
        from: long_date(from)?,
        to: long_date(to)?,
    })
}

/// `March 2, 2026` (or `MAR 2, 2026`), ignoring anything after the year.
fn long_date(text: &str) -> Option<Date> {
    let mut words = text.split_whitespace();
    let month = month(words.next()?)?;
    let day: u8 = words.next()?.strip_suffix(',')?.parse().ok()?;
    let year: u16 = words.next()?.parse().ok()?;
    Date::from_ymd(Ymd { year, month, day }).ok()
}

/// A bank row's `6 Jul`, dated within `period`.
fn day_in(text: &str, period: &Period) -> Option<Date> {
    let (day, month_name) = text.split_once(' ')?;
    let day: u8 = day.parse().ok()?;
    let month = month(month_name)?;
    let [first, last] = [period.from, period.to].map(|d| d.ymd().year);
    (first..=last)
        .filter_map(|year| Date::from_ymd(Ymd { year, month, day }).ok())
        .find(|date| (period.from..=period.to).contains(date))
}

/// A card row's `APR 04`, the latest such date no later than `on`.
fn day_by(text: &str, on: Date) -> Option<Date> {
    let (month_name, day) = text.split_once(' ')?;
    let day: u8 = day.parse().ok()?;
    let month = month(month_name)?;
    let year = on.ymd().year;
    [year, year.checked_sub(1)?]
        .into_iter()
        .filter_map(|year| Date::from_ymd(Ymd { year, month, day }).ok())
        .find(|date| *date <= on)
}

/// The month `name` abbreviates or spells out, in any case: `Jul`, `JULY`,
/// `Sept`.
fn month(name: &str) -> Option<u8> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let name = name.to_lowercase();
    if name.len() < 3 {
        return None;
    }
    let at = MONTHS.iter().position(|m| m.starts_with(&name))?;
    Some(at as u8 + 1)
}

/// `$1,234.56`, `= $833.11`, `+ 44,836.53` or `-$56.65` in cents; `None`
/// for anything else, including text with a figure in it.
fn amount(text: &str) -> Option<i64> {
    let text: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '$' | '=' | '+'))
        .collect();
    if !text.contains('.') {
        return None;
    }
    parse_cents(&text)
}

/// The last four digits of an account number, `None` with fewer than four.
fn last4(number: &str) -> Option<String> {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    let start = digits.len().checked_sub(4)?;
    Some(digits[start..].iter().collect())
}

/// Cents as dollars, for an error message: `-1,234.56` as `-1234.56`.
fn cents(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let value = value.unsigned_abs();
    format!("{sign}{}.{:02}", value / 100, value % 100)
}

/// [`cents`], or `nothing` for no figure.
fn show(value: Option<i64>) -> String {
    value.map_or("nothing".to_owned(), cents)
}

/// Read a Visa statement: see the module docs.
fn parse_card(lines: &[Line]) -> Result<Statement, String> {
    let period = lines
        .iter()
        .find_map(|l| card_period(&l.text()))
        .ok_or("no \"STATEMENT FROM <date> TO <date>\" period")?;
    let last4 = lines
        .iter()
        .find_map(|l| card_last4(&l.text()))
        .ok_or("no masked card number")?;
    let figure_after = |label: &dyn Fn(&str) -> bool| {
        lines.iter().find_map(|l| {
            let at = l.cells.iter().position(|c| label(&c.text))?;
            amount(&l.cells.get(at + 1)?.text)
        })
    };
    let previous = figure_after(&|t| {
        t.eq_ignore_ascii_case("Previous Statement Balance")
            || t.eq_ignore_ascii_case("Previous Account Balance")
    })
    .ok_or("no \"Previous Statement Balance\"")?;
    let new = figure_after(&|t| t == "NEW BALANCE" || t == "CREDIT BALANCE")
        .ok_or("no \"NEW BALANCE\"")?;

    let mut rows: Vec<StatementRow> = Vec::new();
    for line in lines {
        if let Some(foreign) = foreign(line) {
            let Some(row) = rows.last_mut().filter(|r| r.foreign.is_empty()) else {
                return Err(format!(
                    "a foreign currency line under no charge: {:?}",
                    line.text()
                ));
            };
            row.foreign = foreign;
            continue;
        }
        let Some(row) = card_row(line, &period)? else {
            continue;
        };
        rows.push(row);
    }
    // What's owed rises with a charge; the owner's side is its negation.
    let read = rows.iter().map(|r| r.cents).sum::<i64>();
    if -previous + read != -new {
        return Err(format!(
            "the rows take the previous balance {} to {}, but the new balance is {}",
            cents(previous),
            cents(previous - read),
            cents(new)
        ));
    }
    Ok(Statement {
        last4,
        from: period.from,
        to: period.to,
        rows,
    })
}

/// A card statement's period, from `STATEMENT FROM NOV 27 TO DEC 26, 2023`
/// or `STATEMENT FROM DEC 26, 2018 TO JAN 25, 2019`.
fn card_period(text: &str) -> Option<Period> {
    let rest = text.strip_prefix("STATEMENT FROM ")?;
    let (from, to) = rest.split_once(" TO ")?;
    let to = long_date(to)?;
    let from = match long_date(from) {
        Some(from) => from,
        None => day_by(from.trim(), to)?,
    };
    Some(Period { from, to })
}

/// The last four of a masked card number such as `4500 12** **** 9876`
/// anywhere in `text`.
fn card_last4(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "****")?;
    let last = words.get(at + 1)?;
    (last.len() == 4 && last.bytes().all(|b| b.is_ascii_digit())).then(|| (*last).to_owned())
}

/// A card row's `<qty> <currency> @ <rate>` from its foreign currency line,
/// `Foreign Currency-USD 40.00` then `Exchange rate-1.450000`.
fn foreign(line: &Line) -> Option<String> {
    let mut cells = line.cells.iter();
    let charge = cells.next()?.text.strip_prefix("Foreign Currency-")?;
    let (currency, qty) = charge.split_once(' ')?;
    let rate = cells.next()?.text.strip_prefix("Exchange rate-")?;
    Some(format!("{} {currency} @ {}", qty.trim(), rate.trim()))
}

/// The charge or payment on `line`, if it is one: a transaction date, a
/// posting date, the description, then the amount. The first two can share
/// a cell with what follows them, and cells after the amount are the
/// statement's other column.
fn card_row(line: &Line, period: &Period) -> Result<Option<StatementRow>, String> {
    let mut words: Vec<&str> = Vec::new();
    let mut amount_at = None;
    for cell in &line.cells {
        if !words.is_empty() && is_dollars(&cell.text) {
            amount_at = amount(&cell.text);
            break;
        }
        words.extend(cell.text.split_whitespace());
    }
    let [m1, d1, m2, d2, description @ ..] = words.as_slice() else {
        return Ok(None);
    };
    let short = |m: &str, d: &str| {
        m.len() == 3
            && m.bytes().all(|b| b.is_ascii_uppercase())
            && month(m).is_some()
            && d.len() == 2
            && d.bytes().all(|b| b.is_ascii_digit())
    };
    if !short(m1, d1) || !short(m2, d2) {
        return Ok(None);
    }
    let text = line.text();
    let Some(charge) = amount_at else {
        return Err(format!("a card row with no amount: {text:?}"));
    };
    let posted = day_by(&format!("{m2} {d2}"), period.to)
        .filter(|d| *d >= period.from)
        .ok_or_else(|| format!("a posting date outside the statement: {text:?}"))?;
    let date = day_by(&format!("{m1} {d1}"), posted)
        .ok_or_else(|| format!("a bad transaction date: {text:?}"))?;
    if description.is_empty() {
        return Err(format!("a card row with no description: {text:?}"));
    }
    Ok(Some(StatementRow {
        date,
        posted: Some(posted),
        cents: -charge,
        description: description.join(" "),
        foreign: String::new(),
    }))
}

/// Whether `text` is a dollar figure: `$58.00` or `-$2.16`.
fn is_dollars(text: &str) -> bool {
    let text = text.trim();
    let digits = text.strip_prefix('-').unwrap_or(text);
    digits.starts_with('$') && amount(text).is_some()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A cell of `text` from `left`, about 4.5 points a character.
    pub fn cell(left: f64, text: &str) -> Cell {
        Cell {
            left,
            right: left + 4.5 * text.chars().count() as f64,
            text: text.to_owned(),
        }
    }

    /// A cell of `text` ending at `right`, as a figure is set.
    pub fn figure(right: f64, text: &str) -> Cell {
        Cell {
            left: right - 4.5 * text.chars().count() as f64,
            right,
            text: text.to_owned(),
        }
    }

    pub fn line(page: u32, cells: Vec<Cell>) -> Line {
        Line {
            page,
            y: 0.0,
            cells,
        }
    }

    /// A PDF drawing `lines`: each cell's text from its left edge, the
    /// lines of a page twelve points apart from the top.
    pub fn pdf_of(lines: &[Line]) -> Vec<u8> {
        use crate::import::pdf_text::synthetic::{Placed, pdf};
        let last = lines.iter().map(|l| l.page).max().unwrap_or(1);
        let pages: Vec<Vec<Placed>> = (1..=last)
            .map(|page| {
                lines
                    .iter()
                    .filter(|l| l.page == page)
                    .enumerate()
                    .flat_map(|(at, l)| {
                        let y = 750.0 - 12.0 * at as f64;
                        l.cells.iter().map(move |c| Placed {
                            x: c.left,
                            y,
                            text: &c.text,
                        })
                    })
                    .collect()
            })
            .collect();
        pdf(&pages)
    }

    /// The right edges of a synthetic bank table's figure columns.
    pub const OUT: f64 = 370.0;
    pub const IN: f64 = 465.0;
    pub const BAL: f64 = 590.0;

    /// A bank table row: `date` and `description` (either may be empty),
    /// then its `figures`.
    pub fn row(page: u32, date: &str, description: &str, figures: Vec<Cell>) -> Line {
        let mut cells = Vec::new();
        if !date.is_empty() {
            cells.push(cell(45.0, date));
        }
        if !description.is_empty() {
            cells.push(cell(90.0, description));
        }
        cells.extend(figures);
        line(page, cells)
    }

    pub fn header(page: u32) -> Line {
        line(
            page,
            vec![
                cell(45.0, "Date"),
                cell(90.0, "Description"),
                figure(OUT, "Withdrawals ($)"),
                figure(IN, "Deposits ($)"),
                figure(BAL, "Balance ($)"),
            ],
        )
    }

    /// The front matter of a synthetic bank statement for `…1234` from
    /// `from` to `to`, opening at `opening` and closing at `closing`.
    pub fn front(from: &str, to: &str, opening: &str, closing: &str) -> Vec<Line> {
        vec![
            line(1, vec![cell(452.0, &format!("From {from} to {to}"))]),
            line(
                1,
                vec![
                    cell(394.0, "Your account number:"),
                    cell(536.0, "00000-0001234"),
                ],
            ),
            line(
                1,
                vec![
                    cell(45.0, &format!("Your opening balance on {from}")),
                    cell(323.0, &format!("${opening}")),
                ],
            ),
            line(
                1,
                vec![
                    cell(45.0, &format!("Your closing balance on {to}")),
                    cell(328.0, &format!("= ${closing}")),
                ],
            ),
            line(1, vec![cell(45.0, "Details of your account activity")]),
        ]
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    /// A two-page statement over a new year: a two-line description, a
    /// day's rows sharing one date and one balance, and the table picking
    /// up again under the second page's header.
    #[test]
    fn a_bank_table_reads_across_pages_and_years() {
        let mut lines = front(
            "December 6, 2025",
            "January 7, 2026",
            "1,000.00",
            "3,487.05",
        );
        lines.extend([
            line(
                1,
                vec![
                    cell(45.0, "Total deposits into your account"),
                    cell(321.0, "+ 3,000.00"),
                ],
            ),
            line(
                1,
                vec![
                    cell(45.0, "Total withdrawals from your account"),
                    cell(324.0, "- 512.95"),
                ],
            ),
            header(1),
            row(1, "", "Opening Balance", vec![figure(BAL, "1,000.00")]),
            row(1, "30 Dec", "e-Transfer - Autodeposit Shop", vec![]),
            row(
                1,
                "",
                "  C1A2B3C4D5E6",
                vec![figure(IN, "3,000.00"), figure(BAL, "4,000.00")],
            ),
            line(1, vec![cell(570.0, "1 of 2")]),
            line(
                2,
                vec![cell(17.0, "Details of your account activity - continued")],
            ),
            header(2),
            row(
                2,
                "2 Jan",
                "ATM withdrawal - X1",
                vec![figure(OUT, "500.00")],
            ),
            row(
                2,
                "",
                "Monthly fee",
                vec![figure(OUT, "12.95"), figure(BAL, "3,487.05")],
            ),
            row(2, "", "Closing Balance", vec![figure(BAL, "$3,487.05")]),
            line(
                2,
                vec![cell(18.0, "Important information about your account")],
            ),
        ]);
        let statement = parse(&lines).unwrap();
        assert_eq!(statement.last4, "1234");
        assert_eq!(statement.from, date("2025-12-06"));
        assert_eq!(statement.to, date("2026-01-07"));
        let rows: Vec<(Date, i64, &str)> = statement
            .rows
            .iter()
            .map(|r| (r.date, r.cents, r.description.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    date("2025-12-30"),
                    300000,
                    "e-Transfer - Autodeposit Shop C1A2B3C4D5E6"
                ),
                (date("2026-01-02"), -50000, "ATM withdrawal - X1"),
                (date("2026-01-02"), -1295, "Monthly fee"),
            ]
        );
    }

    /// The same table with the fee read as a deposit, as a misplaced column
    /// would: the day's balance and the closing balance both catch it.
    #[test]
    fn a_bank_table_that_does_not_balance_is_an_error() {
        let mut lines = front("December 6, 2025", "January 7, 2026", "1,000.00", "974.10");
        lines.extend([
            header(1),
            row(1, "", "Opening Balance", vec![figure(BAL, "1,000.00")]),
            row(
                1,
                "2 Jan",
                "Monthly fee",
                vec![figure(IN, "12.95"), figure(BAL, "974.10")],
            ),
            row(1, "", "Closing Balance", vec![figure(BAL, "$974.10")]),
        ]);
        let err = parse(&lines).unwrap_err();
        assert_eq!(
            err,
            "the balance after 2026-01-02 reads 974.10 but its rows come to 1012.95"
        );

        // With no day-end balance to catch it, the closing balance does.
        let mut lines = front("December 6, 2025", "January 7, 2026", "1,000.00", "987.05");
        lines.extend([
            header(1),
            row(1, "2 Jan", "Monthly fee", vec![figure(IN, "12.95")]),
            row(1, "", "Closing Balance", vec![figure(BAL, "$987.05")]),
        ]);
        assert_eq!(
            parse(&lines).unwrap_err(),
            "the rows take the opening balance 1000.00 to 1012.95, but the closing balance is \
             987.05"
        );
    }

    #[test]
    fn totals_that_do_not_add_up_are_an_error() {
        let mut lines = front("December 6, 2025", "January 7, 2026", "1,000.00", "987.05");
        lines.extend([
            line(
                1,
                vec![
                    cell(45.0, "Total withdrawals from your account"),
                    cell(324.0, "- 20.00"),
                ],
            ),
            header(1),
            row(
                1,
                "2 Jan",
                "Monthly fee",
                vec![figure(OUT, "12.95"), figure(BAL, "987.05")],
            ),
            row(1, "", "Closing Balance", vec![figure(BAL, "$987.05")]),
        ]);
        assert_eq!(
            parse(&lines).unwrap_err(),
            "the rows' withdrawals come to 12.95 but the summary's total is 20.00"
        );
    }

    #[test]
    fn a_bank_row_outside_the_period_is_an_error() {
        let mut lines = front("December 6, 2025", "January 7, 2026", "1,000.00", "987.05");
        lines.extend([
            header(1),
            row(1, "2 Mar", "Monthly fee", vec![figure(OUT, "12.95")]),
            row(1, "", "Closing Balance", vec![figure(BAL, "$987.05")]),
        ]);
        assert!(parse(&lines).unwrap_err().starts_with("bad date \"2 Mar\""),);
    }

    #[test]
    fn a_table_with_no_closing_row_is_an_error() {
        let mut lines = front("December 6, 2025", "January 7, 2026", "1,000.00", "987.05");
        lines.extend([
            header(1),
            row(1, "2 Jan", "Monthly fee", vec![figure(OUT, "12.95")]),
        ]);
        assert!(
            parse(&lines)
                .unwrap_err()
                .contains("no \"Closing Balance\"")
        );
    }

    /// A synthetic Visa statement's lines: a foreign charge, a payment, and
    /// a second column of text beside the rows.
    pub fn card(new_balance: &str) -> Vec<Line> {
        vec![
            line(1, vec![cell(10.0, "SOME CARD Gold")]),
            line(1, vec![cell(10.0, "A PERSON 4500 12** **** 4321")]),
            line(
                1,
                vec![cell(10.0, "STATEMENT FROM DEC 26, 2018 TO JAN 25, 2019")],
            ),
            line(
                1,
                vec![
                    cell(10.0, "TRANSACTION POSTING"),
                    cell(120.0, "ACTIVITY DESCRIPTION"),
                    cell(280.0, "AMOUNT ($)"),
                    cell(330.0, "New points balance"),
                ],
            ),
            line(
                1,
                vec![
                    cell(10.0, "PREVIOUS STATEMENT BALANCE"),
                    cell(280.0, "$10.00"),
                ],
            ),
            line(
                1,
                vec![
                    cell(10.0, "DEC 23"),
                    cell(50.0, "DEC 27"),
                    cell(90.0, "SOFTWARE CO"),
                    cell(280.0, "$58.00"),
                    cell(330.0, "CONTACT US"),
                ],
            ),
            line(
                1,
                vec![
                    cell(90.0, "Foreign Currency-USD 40.00"),
                    cell(220.0, "Exchange rate-1.450000"),
                ],
            ),
            line(
                1,
                vec![
                    cell(10.0, "JAN 14"),
                    cell(50.0, "JAN 14 PAYMENT - THANK YOU"),
                    cell(280.0, "-$10.00"),
                ],
            ),
            line(
                1,
                vec![
                    cell(330.0, "Previous Statement Balance"),
                    cell(450.0, "$10.00"),
                ],
            ),
            line(
                1,
                vec![cell(330.0, "NEW BALANCE"), cell(450.0, new_balance)],
            ),
            line(
                1,
                vec![cell(10.0, "Payment due date"), cell(100.0, "FEB 15, 2019")],
            ),
        ]
    }

    #[test]
    fn a_card_statement_reads_its_rows() {
        let statement = parse(&card("$58.00")).unwrap();
        assert_eq!(statement.last4, "4321");
        assert_eq!(statement.from, date("2018-12-26"));
        assert_eq!(statement.to, date("2019-01-25"));
        assert_eq!(
            statement.rows,
            [
                StatementRow {
                    date: date("2018-12-23"),
                    posted: Some(date("2018-12-27")),
                    cents: -5800,
                    description: "SOFTWARE CO".to_owned(),
                    foreign: "40.00 USD @ 1.450000".to_owned(),
                },
                StatementRow {
                    date: date("2019-01-14"),
                    posted: Some(date("2019-01-14")),
                    cents: 1000,
                    description: "PAYMENT - THANK YOU".to_owned(),
                    foreign: String::new(),
                },
            ]
        );
    }

    #[test]
    fn a_card_statement_that_does_not_balance_is_an_error() {
        assert_eq!(
            parse(&card("$68.00")).unwrap_err(),
            "the rows take the previous balance 10.00 to 58.00, but the new balance is 68.00"
        );
    }

    #[test]
    fn periods_dates_and_amounts() {
        let p = bank_period("From March 2, 2026 to April 1, 2026").unwrap();
        assert_eq!((p.from, p.to), (date("2026-03-02"), date("2026-04-01")));
        let p = card_period("STATEMENT FROM NOV 27 TO DEC 26, 2023").unwrap();
        assert_eq!((p.from, p.to), (date("2023-11-27"), date("2023-12-26")));
        let p = card_period("STATEMENT FROM DEC 26, 2018 TO JAN 25, 2019").unwrap();
        assert_eq!((p.from, p.to), (date("2018-12-26"), date("2019-01-25")));
        assert_eq!(month("Sept"), Some(9));
        assert_eq!(month("ma"), None);
        assert_eq!(month("Mayo"), None);
        assert_eq!(amount("= $833.11"), Some(83311));
        assert_eq!(amount("-$56.65"), Some(-5665));
        assert_eq!(amount("+ 44,836.53"), Some(4483653));
        assert_eq!(amount("1 of 2"), None);
        assert_eq!(amount("2026"), None);
        assert!(is_dollars("-$2.16"));
        assert!(!is_dollars("2.16"));
        assert_eq!(
            card_last4("A PERSON 4500 12** **** 4321").as_deref(),
            Some("4321")
        );
    }

    #[test]
    fn neither_layout_is_an_error() {
        let lines = [line(1, vec![cell(10.0, "Hello")])];
        assert!(parse(&lines).unwrap_err().starts_with("not an RBC"));
    }
}
