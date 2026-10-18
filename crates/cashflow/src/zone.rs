//! The time zone a source's timestamps are dated in.
//!
//! A journal date is a day, but some sources (CLN's bookkeeper) only give a
//! unix time. Which day that is depends on the zone, and the date is part
//! of what an import compares, so two machines dating in different zones
//! rewrite each other's records near midnight forever. The zone that
//! decides it is therefore the synced [`Setting::Timezone`], the same on
//! every machine, and [`Zone::Local`] is only a per-machine fallback.
//!
//! [`Setting::Timezone`]: crate::settings::Setting::Timezone

use chrono::{Offset, TimeZone};
use ledgerdb::Date;

/// A time zone a unix time is dated in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Zone {
    /// The system's, as `$TZ` or `/etc/localtime` sets it: different on
    /// each machine, so never the synced setting.
    Local,
    /// A fixed offset east of UTC, in seconds.
    Fixed(i32),
    /// An IANA zone such as `America/Vancouver`, daylight saving included.
    /// The database is compiled in, so every machine running the same
    /// cashflow dates alike.
    Named(chrono_tz::Tz),
}

impl Zone {
    /// `local`, `utc`, `+HH:MM` / `-HH:MM`, or an IANA zone name.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "{text:?} is not `local`, `utc`, an offset like -07:00 or a zone like \
                 America/Vancouver"
            )
        };
        match text.to_ascii_lowercase().as_str() {
            "local" => return Ok(Self::Local),
            "utc" | "z" => return Ok(Self::Fixed(0)),
            _ => {}
        }
        let (sign, rest) = match text.as_bytes().first() {
            Some(b'+') => (1, &text[1..]),
            Some(b'-') => (-1, &text[1..]),
            _ => return text.parse().map(Self::Named).map_err(|_| bad()),
        };
        let (hours, minutes) = rest.split_once(':').unwrap_or((rest, "0"));
        let hours: i32 = hours.parse().map_err(|_| bad())?;
        let minutes: i32 = minutes.parse().map_err(|_| bad())?;
        if hours > 14 || minutes >= 60 {
            return Err(bad());
        }
        Ok(Self::Fixed(sign * (hours * 3600 + minutes * 60)))
    }

    /// The day unix time `secs` falls on.
    pub fn date(self, secs: u64) -> Date {
        let secs = secs.min(i64::MAX as u64) as i64;
        let offset = match self {
            Self::Fixed(offset) => offset,
            Self::Local => offset_at(&chrono::Local, secs),
            Self::Named(tz) => offset_at(&tz, secs),
        };
        let days = (secs + i64::from(offset)).div_euclid(86_400);
        Date::from_days(days.clamp(0, i64::from(u32::MAX)) as u32)
    }
}

/// `tz`'s offset east of UTC at unix time `secs`, in seconds, or 0 when the
/// time is out of chrono's range.
fn offset_at<Tz: TimeZone>(tz: &Tz, secs: i64) -> i32 {
    tz.timestamp_opt(secs, 0)
        .single()
        .map_or(0, |t| t.offset().fix().local_minus_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_follow_the_time_zone() {
        // 2024-03-02 02:30 UTC is still March 1st seven hours west.
        let late = 1_709_346_600;
        let date = |zone: &str| Zone::parse(zone).unwrap().date(late).to_string();
        assert_eq!(date("utc"), "2024-03-02");
        assert_eq!(date("-07:00"), "2024-03-01");
        assert_eq!(date("America/Vancouver"), "2024-03-01");
        assert_eq!(date("Asia/Tokyo"), "2024-03-02");
        assert_eq!(Zone::parse("+5:30").unwrap(), Zone::Fixed(5 * 3600 + 1800));
        assert_eq!(Zone::parse("LOCAL").unwrap(), Zone::Local);
        assert!(Zone::parse("-25:00").is_err());
        assert!(Zone::parse("Mars/Olympus").is_err());
    }

    #[test]
    fn a_named_zone_keeps_daylight_saving() {
        // 07:30 UTC is 23:30 the day before at UTC-8 (winter) but 00:30 at
        // UTC-7 (summer), so a fixed -08:00 gets July wrong.
        let vancouver = Zone::parse("America/Vancouver").unwrap();
        let winter = 1_704_180_600; // 2024-01-02 07:30 UTC
        let summer = 1_719_991_800; // 2024-07-03 07:30 UTC
        assert_eq!(vancouver.date(winter).to_string(), "2024-01-01");
        assert_eq!(vancouver.date(summer).to_string(), "2024-07-03");
        let fixed = Zone::parse("-08:00").unwrap();
        assert_eq!(fixed.date(summer).to_string(), "2024-07-02");
    }
}
