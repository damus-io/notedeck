use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Show a relative time string based on some timestamp
pub fn time_ago_since(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();
    let duration = now.checked_sub(timestamp).unwrap_or(0);

    let years = duration / 31_536_000; // seconds in a year
    if years >= 1 {
        return format!("{}yr", years);
    }

    let months = duration / 2_592_000; // seconds in a month (30.44 days)
    if months >= 1 {
        return format!("{}mth", months);
    }

    let weeks = duration / 604_800; // seconds in a week
    if weeks >= 1 {
        return format!("{}wk", weeks);
    }

    let days = duration / 86_400; // seconds in a day
    if days >= 1 {
        return format!("{}d", days);
    }

    let hours = duration / 3600; // seconds in an hour
    if hours >= 1 {
        return format!("{}h", hours);
    }

    let minutes = duration / 60; // seconds in a minute
    if minutes >= 1 {
        return format!("{}m", minutes);
    }

    let seconds = duration;
    if seconds >= 3 {
        return format!("{}s", seconds);
    }

    "now".to_string()
}
