//! Timezone utilities for calendar events.
//!
//! This module provides timezone detection and conversion utilities
//! for displaying calendar events in the user's local timezone.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

/// Detects the user's local timezone.
///
/// Returns the IANA timezone identifier (e.g., "America/New_York").
pub fn detect_local_timezone() -> String {
    // Get the local timezone offset and try to find a matching IANA zone
    let local_now = Local::now();
    let offset = local_now.offset();

    // Try to get timezone from environment or system
    if let Ok(tz) = std::env::var("TZ") {
        if tz.parse::<Tz>().is_ok() {
            return tz;
        }
    }

    // Fallback: use offset to make a reasonable guess
    // This is imperfect but provides a sensible default
    let offset_secs = offset.local_minus_utc();
    let offset_hours = offset_secs / 3600;

    // Common timezone mappings by offset
    match offset_hours {
        -8 => "America/Los_Angeles".to_string(),
        -7 => "America/Denver".to_string(),
        -6 => "America/Chicago".to_string(),
        -5 => "America/New_York".to_string(),
        -4 => "America/Halifax".to_string(),
        0 => "Europe/London".to_string(),
        1 => "Europe/Paris".to_string(),
        2 => "Europe/Helsinki".to_string(),
        3 => "Europe/Moscow".to_string(),
        8 => "Asia/Shanghai".to_string(),
        9 => "Asia/Tokyo".to_string(),
        _ => "UTC".to_string(),
    }
}

/// Converts a Unix timestamp to a local DateTime.
pub fn timestamp_to_local(timestamp: u64) -> DateTime<Local> {
    DateTime::from_timestamp(timestamp as i64, 0)
        .map(|dt| dt.with_timezone(&Local))
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap())
}

/// Converts a Unix timestamp with a timezone identifier to local time.
///
/// If the timezone is invalid or not provided, assumes UTC.
pub fn timestamp_to_local_with_tz(timestamp: u64, timezone: Option<&str>) -> DateTime<Local> {
    let utc_dt = DateTime::from_timestamp(timestamp as i64, 0)
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap());

    if let Some(tz_str) = timezone {
        if let Ok(tz) = tz_str.parse::<Tz>() {
            let dt_with_tz = utc_dt.with_timezone(&tz);
            return dt_with_tz.with_timezone(&Local);
        }
    }

    // Fallback to UTC interpretation
    utc_dt.with_timezone(&Local)
}

/// Formats a timestamp for display in local time.
///
/// Returns a string like "3:00 PM" for time-based events.
pub fn format_time(timestamp: u64, timezone: Option<&str>) -> String {
    let local = timestamp_to_local_with_tz(timestamp, timezone);
    local.format("%l:%M %p").to_string().trim().to_string()
}

/// Formats a timestamp with full date and time.
///
/// Returns a string like "January 25, 2026 at 3:00 PM".
pub fn format_datetime(timestamp: u64, timezone: Option<&str>) -> String {
    let local = timestamp_to_local_with_tz(timestamp, timezone);
    local.format("%B %d, %Y at %l:%M %p").to_string()
}

/// Formats a NaiveDate for display.
///
/// Returns a string like "January 25, 2026".
pub fn format_date(date: NaiveDate) -> String {
    date.format("%B %d, %Y").to_string()
}

/// Gets the date portion from a timestamp in local time.
pub fn timestamp_to_date(timestamp: u64, timezone: Option<&str>) -> NaiveDate {
    timestamp_to_local_with_tz(timestamp, timezone).date_naive()
}

/// Checks if a timezone string is valid.
pub fn is_valid_timezone(tz: &str) -> bool {
    tz.parse::<Tz>().is_ok()
}

/// Returns a list of common timezone identifiers for UI selection.
pub fn common_timezones() -> Vec<&'static str> {
    vec![
        "America/New_York",
        "America/Chicago",
        "America/Denver",
        "America/Los_Angeles",
        "America/Anchorage",
        "Pacific/Honolulu",
        "Europe/London",
        "Europe/Paris",
        "Europe/Berlin",
        "Europe/Moscow",
        "Asia/Dubai",
        "Asia/Kolkata",
        "Asia/Shanghai",
        "Asia/Tokyo",
        "Australia/Sydney",
        "Pacific/Auckland",
        "UTC",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_local_timezone() {
        let tz = detect_local_timezone();
        assert!(!tz.is_empty());
    }

    #[test]
    fn test_timestamp_to_local() {
        let ts = 1706200800; // Some timestamp
        let local = timestamp_to_local(ts);
        assert!(local.timestamp() > 0);
    }

    #[test]
    fn test_format_time() {
        let ts = 1706200800;
        let formatted = format_time(ts, Some("UTC"));
        assert!(!formatted.is_empty());
    }

    #[test]
    fn test_is_valid_timezone() {
        assert!(is_valid_timezone("America/New_York"));
        assert!(is_valid_timezone("UTC"));
        assert!(!is_valid_timezone("Invalid/Timezone"));
    }

    #[test]
    fn test_common_timezones() {
        let tzs = common_timezones();
        assert!(!tzs.is_empty());
        for tz in tzs {
            assert!(is_valid_timezone(tz), "Invalid timezone: {}", tz);
        }
    }
}
