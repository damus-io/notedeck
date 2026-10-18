//! The span of days a view reports on, and the span it is compared with.
//!
//! One [`Period`] lives in [`crate::CashflowApp`]'s state and is shared by
//! every view that reports over time (Overview, Categories), so picking "Last
//! month" in one shows last month in the other. A period is relative: it is
//! resolved against today's date, which the app holds too, so a test pins the
//! date and gets the same ranges on every run.

use ledgerdb::{Date, DateRange, Ymd};

/// A span of days relative to today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Period {
    /// The first of this month through today.
    #[default]
    ThisMonth,
    /// The whole of last month.
    LastMonth,
    /// January 1st through today.
    ThisYear,
    /// The twelve whole months before this one.
    Last12Months,
    /// Every day the journal has.
    AllTime,
}

/// A period resolved against a date: the days it covers, and the days it is
/// compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// The days the period covers.
    pub range: DateRange,
    /// The days just before it that a delta is measured against, `None` for
    /// [`Period::AllTime`], which has nothing before it.
    pub previous: Option<DateRange>,
}

impl Period {
    /// Every period, in picker order.
    pub const ALL: [Period; 5] = [
        Period::ThisMonth,
        Period::LastMonth,
        Period::ThisYear,
        Period::Last12Months,
        Period::AllTime,
    ];

    /// The period's name, as the picker shows it.
    pub fn label(self) -> &'static str {
        match self {
            Period::ThisMonth => "This month",
            Period::LastMonth => "Last month",
            Period::ThisYear => "This year",
            Period::Last12Months => "Last 12 months",
            Period::AllTime => "All time",
        }
    }

    /// What a delta is measured against, as a tooltip says it: "vs …".
    pub fn previous_label(self) -> Option<&'static str> {
        match self {
            Period::ThisMonth => Some("vs the same days last month"),
            Period::LastMonth => Some("vs the month before"),
            Period::ThisYear => Some("vs the same days last year"),
            Period::Last12Months => Some("vs the 12 months before"),
            Period::AllTime => None,
        }
    }

    /// The days this period covers on `today`, and the days it is compared
    /// with.
    ///
    /// A period that runs "through today" is compared with the same number
    /// of days at the start of the period before it, so the middle of a
    /// month is not measured against a whole one.
    pub fn resolve(self, today: Date) -> Resolved {
        let month = today.month_index();
        let tomorrow = Date::from_days(today.days().saturating_add(1));
        match self {
            Period::ThisMonth => to_date(
                Date::first_of_month(month),
                Date::first_of_month(month.saturating_sub(1)),
                tomorrow,
            ),
            Period::LastMonth => whole(month.saturating_sub(1), 1),
            Period::ThisYear => {
                let year = today.ymd().year;
                to_date(jan_first(year), jan_first(year.saturating_sub(1)), tomorrow)
            }
            Period::Last12Months => whole(month.saturating_sub(12), 12),
            Period::AllTime => Resolved {
                range: DateRange::new(Date::EPOCH, Date::from_days(u32::MAX)),
                previous: None,
            },
        }
    }
}

/// `months` whole months starting at month index `first`, compared with the
/// `months` before them.
fn whole(first: u32, months: u32) -> Resolved {
    Resolved {
        range: DateRange::new(
            Date::first_of_month(first),
            Date::first_of_month(first + months),
        ),
        previous: Some(DateRange::new(
            Date::first_of_month(first.saturating_sub(months)),
            Date::first_of_month(first),
        )),
    }
}

/// `start..end`, compared with as many days from `previous_start`, cut off
/// where the current period begins.
fn to_date(start: Date, previous_start: Date, end: Date) -> Resolved {
    let days = end.days() - start.days();
    let previous_end = Date::from_days((previous_start.days() + days).min(start.days()));
    Resolved {
        range: DateRange::new(start, end),
        previous: Some(DateRange::new(previous_start, previous_end)),
    }
}

/// January 1st of `year`.
fn jan_first(year: u16) -> Date {
    Date::from_ymd(Ymd {
        year,
        month: 1,
        day: 1,
    })
    .unwrap_or(Date::EPOCH)
}

/// Today's date on this machine's clock, in its local time zone.
pub fn today() -> Date {
    let now = chrono::Local::now().date_naive();
    let ymd = Ymd {
        year: chrono::Datelike::year(&now).clamp(1970, 9999) as u16,
        month: chrono::Datelike::month(&now) as u8,
        day: chrono::Datelike::day(&now) as u8,
    };
    Date::from_ymd(ymd).unwrap_or(Date::EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap()
    }

    fn range(start: &str, end: &str) -> DateRange {
        DateRange::new(date(start), date(end))
    }

    /// Mid-month, this month runs through today and is compared with the
    /// same seventeen days of last month.
    #[test]
    fn this_month_is_to_date_against_the_same_days() {
        let r = Period::ThisMonth.resolve(date("2024-10-17"));
        assert_eq!(r.range, range("2024-10-01", "2024-10-18"));
        assert_eq!(r.previous, Some(range("2024-09-01", "2024-09-18")));
    }

    /// On the 31st, the comparison stops where this month starts rather than
    /// running into it from a 30-day month.
    #[test]
    fn this_month_comparison_never_overlaps_this_month() {
        let r = Period::ThisMonth.resolve(date("2024-10-31"));
        assert_eq!(r.previous, Some(range("2024-09-01", "2024-10-01")));
    }

    /// Last month and the last twelve are whole months, ending before this
    /// one, compared with as many months before them.
    #[test]
    fn whole_month_periods_end_before_this_month() {
        let today = date("2024-01-15");
        let last = Period::LastMonth.resolve(today);
        assert_eq!(last.range, range("2023-12-01", "2024-01-01"));
        assert_eq!(last.previous, Some(range("2023-11-01", "2023-12-01")));

        let year = Period::Last12Months.resolve(today);
        assert_eq!(year.range, range("2023-01-01", "2024-01-01"));
        assert_eq!(year.previous, Some(range("2022-01-01", "2023-01-01")));
    }

    /// This year runs from January 1st, compared with the same days of the
    /// year before.
    #[test]
    fn this_year_is_to_date() {
        let r = Period::ThisYear.resolve(date("2023-03-01"));
        assert_eq!(r.range, range("2023-01-01", "2023-03-02"));
        assert_eq!(r.previous, Some(range("2022-01-01", "2022-03-02")));
    }

    /// All time covers every date and has nothing to compare with.
    #[test]
    fn all_time_has_no_previous() {
        let r = Period::AllTime.resolve(date("2024-03-01"));
        assert!(r.range.contains(date("1970-01-01")));
        assert!(r.range.contains(date("9999-12-31")));
        assert_eq!(r.previous, None);
        assert_eq!(Period::AllTime.previous_label(), None);
    }
}
