use std::time::{SystemTime, UNIX_EPOCH};

pub fn time_ago_since(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    // Determine if the timestamp is in the future or the past
    let duration = if now >= timestamp {
        now.saturating_sub(timestamp)
    } else {
        timestamp.saturating_sub(now)
    };

    let future = timestamp > now;
    let relstr = if future { "+" } else { "" };

    let years = duration / 31_536_000; // seconds in a year
    if years >= 1 {
        return format!("{}{}yr", relstr, years);
    }

    let months = duration / 2_592_000; // seconds in a month (30.44 days)
    if months >= 1 {
        return format!("{}{}mth", relstr, months);
    }

    let weeks = duration / 604_800; // seconds in a week
    if weeks >= 1 {
        return format!("{}{}wk", relstr, weeks);
    }

    let days = duration / 86_400; // seconds in a day
    if days >= 1 {
        return format!("{}{}d", relstr, days);
    }

    let hours = duration / 3600; // seconds in an hour
    if hours >= 1 {
        return format!("{}{}h", relstr, hours);
    }

    let minutes = duration / 60; // seconds in a minute
    if minutes >= 1 {
        return format!("{}{}m", relstr, minutes);
    }

    let seconds = duration;
    if seconds >= 3 {
        return format!("{}{}s", relstr, seconds);
    }

    "now".to_string()
}
