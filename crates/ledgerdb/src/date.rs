//! Calendar dates as a day count.

use std::fmt;

/// A calendar date, stored as the number of days since 1970-01-01.
///
/// Four bytes, `Copy`, and ordered chronologically, so postings sort and
/// bucket by date with integer compares. Only dates from 1970-01-01 to
/// 9999-12-31 are representable.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Date(u32);

/// A date split into its calendar fields.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Ymd {
    /// The year, 1970 to 9999.
    pub year: u16,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, starting at 1.
    pub day: u8,
}

/// Why a date could not be parsed or built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DateError {
    /// The text is not `YYYY-MM-DD` or `YYYY/MM/DD`.
    #[error("expected a date like YYYY-MM-DD or YYYY/MM/DD")]
    Syntax,
    /// The fields are well-formed but name no real day, such as February 30.
    #[error("{year:04}-{month:02}-{day:02} is not a calendar date")]
    NoSuchDay {
        /// The year as written.
        year: u16,
        /// The month as written.
        month: u8,
        /// The day as written.
        day: u8,
    },
    /// The date is before 1970-01-01 or after 9999-12-31.
    #[error("year {0} is outside 1970 to 9999")]
    OutOfRange(u16),
}

impl Date {
    /// The first representable date, 1970-01-01.
    pub const EPOCH: Date = Date(0);

    /// The date `days` days after 1970-01-01.
    pub fn from_days(days: u32) -> Self {
        Date(days)
    }

    /// The number of days since 1970-01-01.
    pub fn days(self) -> u32 {
        self.0
    }

    /// The date with the given calendar fields, if it exists.
    pub fn from_ymd(ymd: Ymd) -> Result<Self, DateError> {
        let Ymd { year, month, day } = ymd;
        if !(1970..=9999).contains(&year) {
            return Err(DateError::OutOfRange(year));
        }
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return Err(DateError::NoSuchDay { year, month, day });
        }
        Ok(Date(days_from_civil(ymd)))
    }

    /// Parse `YYYY-MM-DD` or `YYYY/MM/DD`.
    ///
    /// The year is four digits; the month and day may be one or two digits,
    /// as ledger-cli accepts. Both separators must be the same character.
    pub fn parse(text: &str) -> Result<Self, DateError> {
        let bytes = text.as_bytes();
        if bytes.len() < 8 {
            return Err(DateError::Syntax);
        }
        let sep = bytes[4];
        if sep != b'-' && sep != b'/' {
            return Err(DateError::Syntax);
        }
        let year = digits(&bytes[..4], 4).ok_or(DateError::Syntax)?;
        let rest = &bytes[5..];
        let mid = rest
            .iter()
            .position(|b| *b == sep)
            .ok_or(DateError::Syntax)?;
        let month = digits(&rest[..mid], 2).ok_or(DateError::Syntax)?;
        let day = digits(&rest[mid + 1..], 2).ok_or(DateError::Syntax)?;
        Self::from_ymd(Ymd {
            year,
            month: month as u8,
            day: day as u8,
        })
    }

    /// The date's calendar fields.
    pub fn ymd(self) -> Ymd {
        civil_from_days(self.0)
    }
}

/// Formats as `YYYY-MM-DD`.
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Ymd { year, month, day } = self.ymd();
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

/// The value of 1 to `max` ASCII digits, or `None` for anything else.
fn digits(bytes: &[u8], max: usize) -> Option<u16> {
    if bytes.is_empty() || bytes.len() > max {
        return None;
    }
    bytes.iter().try_fold(0u16, |acc, b| {
        b.is_ascii_digit().then(|| acc * 10 + u16::from(b - b'0'))
    })
}

/// Whether `year` is a Gregorian leap year.
fn is_leap(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// The number of days in `month` of `year`.
fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a valid date (Howard Hinnant's algorithm,
/// specialised to years from 1970, so no negative eras).
fn days_from_civil(ymd: Ymd) -> u32 {
    let month = u32::from(ymd.month);
    let year = u32::from(ymd.year) - u32::from(month <= 2);
    let era = year / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + u32::from(ymd.day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The calendar fields of a day count; the inverse of [`days_from_civil`].
fn civil_from_days(days: u32) -> Ymd {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u32::from(month <= 2);
    Ymd {
        year: year as u16,
        month: month as u8,
        day: day as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(year: u16, month: u8, day: u8) -> Ymd {
        Ymd { year, month, day }
    }

    #[test]
    fn epoch_and_known_days() {
        assert_eq!(Date::parse("1970-01-01"), Ok(Date::EPOCH));
        assert_eq!(Date::parse("1970-01-02").unwrap().days(), 1);
        assert_eq!(Date::parse("2000-03-01").unwrap().days(), 11_017);
        assert_eq!(Date::parse("2023-07-14").unwrap().days(), 19_552);
    }

    #[test]
    fn both_separators_and_short_fields() {
        let dashed = Date::parse("2024-02-09").unwrap();
        assert_eq!(Date::parse("2024/02/09"), Ok(dashed));
        assert_eq!(Date::parse("2024/2/9"), Ok(dashed));
        assert_eq!(dashed.to_string(), "2024-02-09");
    }

    #[test]
    fn rejects_bad_text() {
        for bad in [
            "",
            "2024",
            "2024-02",
            "2024-02-",
            "2024-02/09",
            "2024.02.09",
            "24-02-09",
            "2024-002-09",
            "2024-02-09x",
            "2024-0a-09",
            "2024--09",
        ] {
            assert_eq!(Date::parse(bad), Err(DateError::Syntax), "{bad:?}");
        }
    }

    #[test]
    fn rejects_days_that_do_not_exist() {
        assert!(matches!(
            Date::parse("2023-02-29"),
            Err(DateError::NoSuchDay { .. })
        ));
        assert!(matches!(
            Date::parse("2023-13-01"),
            Err(DateError::NoSuchDay { .. })
        ));
        assert!(matches!(
            Date::parse("2023-04-31"),
            Err(DateError::NoSuchDay { .. })
        ));
        assert!(Date::parse("2024-02-29").is_ok());
        assert!(Date::parse("2000-02-29").is_ok());
        assert!(Date::parse("1900-02-28").is_err());
        assert_eq!(Date::parse("1969-12-31"), Err(DateError::OutOfRange(1969)));
    }

    #[test]
    fn every_day_round_trips() {
        let last = Date::from_ymd(ymd(9999, 12, 31)).unwrap().days();
        let mut prev: Option<Ymd> = None;
        for days in 0..=last {
            let date = Date::from_days(days);
            let fields = date.ymd();
            assert_eq!(Date::from_ymd(fields), Ok(date));
            if let Some(prev) = prev {
                let next_day = fields.day == prev.day + 1 && fields.month == prev.month;
                let next_month = fields.day == 1 && fields.month == prev.month % 12 + 1;
                assert!(next_day || next_month, "{prev:?} -> {fields:?}");
            }
            prev = Some(fields);
        }
        assert_eq!(Date::from_days(last).to_string(), "9999-12-31");
    }
}
