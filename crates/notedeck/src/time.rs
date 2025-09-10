use crate::{tr, Localization};
use chrono::DateTime;
use std::time::{SystemTime, UNIX_EPOCH};

// Time duration constants in seconds
const ONE_MINUTE_IN_SECONDS: u64 = 60;
const ONE_HOUR_IN_SECONDS: u64 = 3600;
const ONE_DAY_IN_SECONDS: u64 = 86_400;
const ONE_WEEK_IN_SECONDS: u64 = 604_800;
const ONE_MONTH_IN_SECONDS: u64 = 2_592_000; // 30 days
const ONE_YEAR_IN_SECONDS: u64 = 31_536_000; // 365 days

/// Calculate relative time between two timestamps, with two units only
/// when the scale is large enough (e.g., "1y 6m", "5d 4h"),
/// but not for hours/minutes/seconds.
fn time_ago_between(i18n: &mut Localization, timestamp: u64, now: u64) -> String {
    let duration = if now >= timestamp {
        now.saturating_sub(timestamp)
    } else {
        timestamp.saturating_sub(now)
    };

    // Special-case: "now" for < 3 seconds
    if duration <= 2 {
        let s = tr!(
            i18n,
            "now",
            "Relative time for very recent events (less than 3 seconds)"
        );
        return if timestamp > now { format!("+{s}") } else { s };
    }

    // Break into buckets
    let years = duration / ONE_YEAR_IN_SECONDS;
    let rem_y = duration % ONE_YEAR_IN_SECONDS;

    let months = rem_y / ONE_MONTH_IN_SECONDS;
    let rem_m = rem_y % ONE_MONTH_IN_SECONDS;

    let weeks = rem_m / ONE_WEEK_IN_SECONDS;
    let rem_w = rem_m % ONE_WEEK_IN_SECONDS;

    let days = rem_w / ONE_DAY_IN_SECONDS;
    let rem_d = rem_w % ONE_DAY_IN_SECONDS;

    let hours = rem_d / ONE_HOUR_IN_SECONDS;
    let rem_h = rem_d % ONE_HOUR_IN_SECONDS;

    let mins = rem_h / ONE_MINUTE_IN_SECONDS;
    let secs = rem_h % ONE_MINUTE_IN_SECONDS;

    let mut parts: Vec<String> = Vec::with_capacity(2);

    let mut push_part = |count: u64, key: &str, desc: &str| {
        if count > 0 && parts.len() < 2 {
            parts.push(tr!(i18n, key, desc, count = count));
        }
    };

    if years > 0 {
        push_part(years, "{count}y", "Relative time in years");
        push_part(months, "{count}mo", "Relative time in months");
    } else if months > 0 {
        push_part(months, "{count}mo", "Relative time in months");
        push_part(weeks, "{count}w", "Relative time in weeks");
    } else if weeks > 0 {
        push_part(weeks, "{count}w", "Relative time in weeks");
        push_part(days, "{count}d", "Relative time in days");
    } else if days > 0 {
        push_part(days, "{count}d", "Relative time in days");
        push_part(hours, "{count}h", "Relative time in hours");
    } else if hours > 0 {
        push_part(hours, "{count}h", "Relative time in hours");
    } else if mins > 0 {
        push_part(mins, "{count}m", "Relative time in minutes");
    } else {
        push_part(secs.max(1), "{count}s", "Relative time in seconds");
    }

    let time_str = parts.join(" ");

    if timestamp > now {
        format!("+{time_str}")
    } else {
        time_str
    }
}

pub fn time_format(_i18n: &mut Localization, timestamp: u64) -> String {
    // TODO: format this using the selected locale
    DateTime::from_timestamp(timestamp as i64, 0)
        .unwrap()
        .format("%l:%M %p %b %d, %Y")
        .to_string()
}

pub fn time_ago_since(i18n: &mut Localization, timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    time_ago_between(i18n, timestamp, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn get_current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs()
    }

    #[test]
    fn test_now_condition() {
        let now = get_current_timestamp();
        let mut intl = Localization::no_bidi();

        // Test 0 seconds ago
        let result = time_ago_between(&mut intl, now, now);
        assert_eq!(
            result, "now",
            "Expected 'now' for 0 seconds, got: {}",
            result
        );

        // Test 1 second ago
        let result = time_ago_between(&mut intl, now - 1, now);
        assert_eq!(
            result, "now",
            "Expected 'now' for 1 second, got: {}",
            result
        );

        // Test 2 seconds ago
        let result = time_ago_between(&mut intl, now - 2, now);
        assert_eq!(
            result, "now",
            "Expected 'now' for 2 seconds, got: {}",
            result
        );
    }

    #[test]
    fn test_seconds_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 3 seconds ago
        let result = time_ago_between(&mut i18n, now - 3, now);
        assert_eq!(result, "3s", "Expected '3s' for 3 seconds, got: {}", result);

        // Test 30 seconds ago
        let result = time_ago_between(&mut i18n, now - 30, now);
        assert_eq!(
            result, "30s",
            "Expected '30s' for 30 seconds, got: {}",
            result
        );

        // Test 59 seconds ago (max for seconds)
        let result = time_ago_between(&mut i18n, now - 59, now);
        assert_eq!(
            result, "59s",
            "Expected '59s' for 59 seconds, got: {}",
            result
        );
    }

    #[test]
    fn test_minutes_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 minute ago
        let result = time_ago_between(&mut i18n, now - ONE_MINUTE_IN_SECONDS, now);
        assert_eq!(result, "1m", "Expected '1m' for 1 minute, got: {}", result);

        // Test 30 minutes ago
        let result = time_ago_between(&mut i18n, now - 30 * ONE_MINUTE_IN_SECONDS, now);
        assert_eq!(
            result, "30m",
            "Expected '30m' for 30 minutes, got: {}",
            result
        );

        // Test 59 minutes ago (max for minutes)
        let result = time_ago_between(&mut i18n, now - 59 * ONE_MINUTE_IN_SECONDS, now);
        assert_eq!(
            result, "59m",
            "Expected '59m' for 59 minutes, got: {}",
            result
        );
    }

    #[test]
    fn test_hours_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 hour ago
        let result = time_ago_between(&mut i18n, now - ONE_HOUR_IN_SECONDS, now);
        assert_eq!(result, "1h", "Expected '1h' for 1 hour, got: {}", result);

        // Test 12 hours ago
        let result = time_ago_between(&mut i18n, now - 12 * ONE_HOUR_IN_SECONDS, now);
        assert_eq!(
            result, "12h",
            "Expected '12h' for 12 hours, got: {}",
            result
        );

        // Test 23 hours ago (max for hours)
        let result = time_ago_between(&mut i18n, now - 23 * ONE_HOUR_IN_SECONDS, now);
        assert_eq!(
            result, "23h",
            "Expected '23h' for 23 hours, got: {}",
            result
        );
    }

    #[test]
    fn test_days_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 day ago
        let result = time_ago_between(&mut i18n, now - ONE_DAY_IN_SECONDS, now);
        assert_eq!(result, "1d", "Expected '1d' for 1 day, got: {}", result);

        // Test 3 days ago
        let result = time_ago_between(&mut i18n, now - 3 * ONE_DAY_IN_SECONDS, now);
        assert_eq!(result, "3d", "Expected '3d' for 3 days, got: {}", result);

        // Test 6 days ago (max for days, before weeks)
        let result = time_ago_between(&mut i18n, now - 6 * ONE_DAY_IN_SECONDS, now);
        assert_eq!(result, "6d", "Expected '6d' for 6 days, got: {}", result);
    }

    #[test]
    fn test_weeks_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 week ago
        let result = time_ago_between(&mut i18n, now - ONE_WEEK_IN_SECONDS, now);
        assert_eq!(result, "1w", "Expected '1w' for 1 week, got: {}", result);

        // Test 4 weeks ago
        let result = time_ago_between(&mut i18n, now - 4 * ONE_WEEK_IN_SECONDS, now);
        assert_eq!(result, "4w", "Expected '4w' for 4 weeks, got: {}", result);
    }

    #[test]
    fn test_months_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 month ago
        let result = time_ago_between(&mut i18n, now - ONE_MONTH_IN_SECONDS, now);
        assert_eq!(result, "1mo", "Expected '1mo' for 1 month, got: {}", result);

        // Test 11 months ago (max for months, before years)
        let result = time_ago_between(&mut i18n, now - 11 * ONE_MONTH_IN_SECONDS, now);
        assert_eq!(
            result, "11mo",
            "Expected '11mo' for 11 months, got: {}",
            result
        );
    }

    #[test]
    fn test_years_condition() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 year ago
        let result = time_ago_between(&mut i18n, now - ONE_YEAR_IN_SECONDS, now);
        assert_eq!(result, "1y", "Expected '1y' for 1 year, got: {}", result);

        // Test 5 years ago
        let result = time_ago_between(&mut i18n, now - 5 * ONE_YEAR_IN_SECONDS, now);
        assert_eq!(result, "5y", "Expected '5y' for 5 years, got: {}", result);

        // Test 10 years ago (reduced from 100 to avoid overflow)
        let result = time_ago_between(&mut i18n, now - 10 * ONE_YEAR_IN_SECONDS, now);
        assert_eq!(
            result, "10y",
            "Expected '10y' for 10 years, got: {}",
            result
        );
    }

    #[test]
    fn test_future_timestamps() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test 1 minute in the future
        let result = time_ago_between(&mut i18n, now + ONE_MINUTE_IN_SECONDS, now);
        assert_eq!(
            result, "+1m",
            "Expected '+1m' for 1 minute in future, got: {}",
            result
        );

        // Test 1 hour in the future
        let result = time_ago_between(&mut i18n, now + ONE_HOUR_IN_SECONDS, now);
        assert_eq!(
            result, "+1h",
            "Expected '+1h' for 1 hour in future, got: {}",
            result
        );

        // Test 1 day in the future
        let result = time_ago_between(&mut i18n, now + ONE_DAY_IN_SECONDS, now);
        assert_eq!(
            result, "+1d",
            "Expected '+1d' for 1 day in future, got: {}",
            result
        );
    }

    #[test]
    fn test_boundary_conditions() {
        let now = get_current_timestamp();
        let mut i18n = Localization::no_bidi();

        // Test boundary between seconds and minutes
        let result = time_ago_between(&mut i18n, now - 60, now);
        assert_eq!(
            result, "1m",
            "Expected '1m' for exactly 60 seconds, got: {}",
            result
        );

        // Test boundary between minutes and hours
        let result = time_ago_between(&mut i18n, now - 3600, now);
        assert_eq!(
            result, "1h",
            "Expected '1h' for exactly 3600 seconds, got: {}",
            result
        );

        // Test boundary between hours and days
        let result = time_ago_between(&mut i18n, now - 86400, now);
        assert_eq!(
            result, "1d",
            "Expected '1d' for exactly 86400 seconds, got: {}",
            result
        );
    }
}
