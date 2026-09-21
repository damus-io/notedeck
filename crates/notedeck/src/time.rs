use crate::{tr, Localization};
use chrono::DateTime;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

// Time duration constants in seconds
const ONE_MINUTE_IN_SECONDS: u64 = 60;
const ONE_HOUR_IN_SECONDS: u64 = 3600;
const ONE_DAY_IN_SECONDS: u64 = 86_400;
const ONE_WEEK_IN_SECONDS: u64 = 604_800;
const ONE_MONTH_IN_SECONDS: u64 = 2_592_000; // 30 days
const ONE_YEAR_IN_SECONDS: u64 = 31_536_000; // 365 days

/// Maximum tolerated skew for note timestamps in the future (2 minutes / 120 seconds).
pub const MAX_FUTURE_NOTE_SKEW_SECS: u64 = 2 * ONE_MINUTE_IN_SECONDS;

/// Returns the current UNIX timestamp in seconds.
pub fn unix_time_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

/// Whether `timestamp` is further in the future than the allowed skew.
pub fn is_future_timestamp(timestamp: u64, now: u64) -> bool {
    timestamp > now + MAX_FUTURE_NOTE_SKEW_SECS
}

/// One component of a relative time, e.g. the `4h` of `"3d 4h"`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TimePart {
    pub unit: TimeUnit,
    pub count: u64,
}

/// The unit a [`TimePart`] is counted in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TimeUnit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
}

/// What a relative time renders as, before it is turned into text.
///
/// This is everything [`render_relative_time`] reads, and nothing else — which
/// is what makes it usable as a cache key. See [`RelativeTimeCache`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RelativeTime {
    /// The leading component, or `None` for a gap of under three seconds,
    /// which renders as "now".
    pub first: Option<TimePart>,

    /// The trailing component, e.g. the `4h` of `"3d 4h"`. Only ever present
    /// alongside a `first`, and only at day scale and coarser.
    pub second: Option<TimePart>,

    /// The timestamp is in the future; renders with a leading `+`.
    pub future: bool,
}

/// Bucket the gap between `timestamp` and `now` into what it will be rendered
/// as, with two units only when the scale is large enough (e.g. "1y 6m",
/// "5d 4h"), but not for hours/minutes/seconds.
///
/// Pure arithmetic: no locale, no allocation. [`render_relative_time`] turns
/// the result into text.
pub fn relative_time(timestamp: u64, now: u64) -> RelativeTime {
    let future = timestamp > now;
    let duration = if now >= timestamp {
        now.saturating_sub(timestamp)
    } else {
        timestamp.saturating_sub(now)
    };

    // Special-case: "now" for < 3 seconds
    if duration <= 2 {
        return RelativeTime {
            first: None,
            second: None,
            future,
        };
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

    let part = |unit, count| TimePart { unit, count };
    let opt_part = |unit, count| (count > 0).then(|| part(unit, count));

    let (first, second) = if years > 0 {
        (
            part(TimeUnit::Year, years),
            opt_part(TimeUnit::Month, months),
        )
    } else if months > 0 {
        (
            part(TimeUnit::Month, months),
            opt_part(TimeUnit::Week, weeks),
        )
    } else if weeks > 0 {
        (part(TimeUnit::Week, weeks), opt_part(TimeUnit::Day, days))
    } else if days > 0 {
        (part(TimeUnit::Day, days), opt_part(TimeUnit::Hour, hours))
    } else if hours > 0 {
        (part(TimeUnit::Hour, hours), None)
    } else if mins > 0 {
        (part(TimeUnit::Minute, mins), None)
    } else {
        (part(TimeUnit::Second, secs.max(1)), None)
    };

    RelativeTime {
        first: Some(first),
        second,
        future,
    }
}

/// Localize one component, e.g. `"4h"`.
fn render_part(i18n: &mut Localization, part: TimePart) -> String {
    let count = part.count;
    match part.unit {
        TimeUnit::Year => tr!(i18n, "{count}y", "Relative time in years", count = count),
        TimeUnit::Month => tr!(i18n, "{count}mo", "Relative time in months", count = count),
        TimeUnit::Week => tr!(i18n, "{count}w", "Relative time in weeks", count = count),
        TimeUnit::Day => tr!(i18n, "{count}d", "Relative time in days", count = count),
        TimeUnit::Hour => tr!(i18n, "{count}h", "Relative time in hours", count = count),
        TimeUnit::Minute => tr!(i18n, "{count}m", "Relative time in minutes", count = count),
        TimeUnit::Second => tr!(i18n, "{count}s", "Relative time in seconds", count = count),
    }
}

/// Localize a bucketed relative time, e.g. `"3d 4h"` or `"+2m"`.
pub fn render_relative_time(i18n: &mut Localization, relative: RelativeTime) -> String {
    let Some(first) = relative.first else {
        let s = tr!(
            i18n,
            "now",
            "Relative time for very recent events (less than 3 seconds)"
        );
        return if relative.future { format!("+{s}") } else { s };
    };

    let first = render_part(i18n, first);
    let second = relative.second.map(|part| render_part(i18n, part));

    match (relative.future, second) {
        (false, None) => first,
        (true, None) => format!("+{first}"),
        (false, Some(second)) => format!("{first} {second}"),
        (true, Some(second)) => format!("+{first} {second}"),
    }
}

/// Calculate relative time between two timestamps, with two units only
/// when the scale is large enough (e.g., "1y 6m", "5d 4h"),
/// but not for hours/minutes/seconds. Takes `now` explicitly (unlike
/// [`time_ago_since`], which reads the wall clock) so callers can drive it off a
/// captured timestamp and test it deterministically.
///
/// Allocates. Per-frame callers should go through [`RelativeTimeCache`]
/// instead.
pub fn time_ago_between(i18n: &mut Localization, timestamp: u64, now: u64) -> String {
    render_relative_time(i18n, relative_time(timestamp, now))
}

pub fn time_format(_i18n: &mut Localization, timestamp: u64) -> String {
    // TODO: format this using the selected locale
    DateTime::from_timestamp(timestamp as i64, 0)
        .unwrap()
        .format("%l:%M %p %b %d, %Y")
        .to_string()
}

/// Allocates. Per-frame callers should go through
/// [`RelativeTimeCache::time_ago_since`] instead.
pub fn time_ago_since(i18n: &mut Localization, timestamp: u64) -> String {
    let now = unix_time_secs();

    time_ago_between(i18n, timestamp, now)
}

/// How many rendered relative times to keep before starting over.
///
/// A timeline needs one entry per distinct bucket on screen, which is a handful.
/// The cap is here because the keys drift as notes age — a session left open for
/// a day accumulates one entry per minute it spent showing a note that was
/// minutes old. It clears rather than evicting one entry, because a rebuild is a
/// few `tr!` calls and a correct LRU is not worth the code.
const MAX_RENDERED_TIMES: usize = 64;

/// Memoises localized relative timestamps, so the note header's "3d 4h" is
/// formatted when it changes rather than sixty times a second.
///
/// # Why this exists
///
/// `NoteView` renders a relative timestamp per visible note, every frame.
/// Formatting one runs two `tr!` calls with arguments, and an argument-bearing
/// `tr!` is the most expensive kind: `FluentArgs` cannot be cached (see
/// [`Localization::translate`](crate::Localization::translate)) so every call
/// builds one, formats through the bundle, and allocates the result. Measured by
/// `crates/notedeck_columns/tests/frame_alloc.rs`, that was **42 allocations and
/// ~8.4 KB per frame** on a seven-note timeline — the second largest byte figure
/// in the frame — for a string whose resolution is the unit it is displayed in:
/// it ticks once a second only while a note is seconds old, once a minute for
/// the first hour, and once an hour from a day old onwards.
///
/// # The key
///
/// Entries are keyed on [`RelativeTime`], the bucketed form, not on the
/// timestamp and not on a coarse clock. That is deliberate: the rendered text is
/// a pure function of the bucket, so an entry cannot go stale as the clock moves
/// — when the bucket changes, so does the key. It also means notes that are the
/// same age share one entry.
///
/// The one thing the key does not cover is the locale, so the cache clears when
/// [`Localization::cache_generation`](crate::Localization::cache_generation)
/// moves.
///
/// # Lifetime
///
/// One instance lives on the [`Notedeck`](crate::Notedeck) host and is reached
/// through [`AppContext::time_cache`](crate::AppContext::time_cache) and
/// [`NoteContext::time_cache`](crate::NoteContext::time_cache). It is state
/// passed in by reference rather than a global, per CLAUDE.md.
#[derive(Default)]
pub struct RelativeTimeCache {
    rendered: HashMap<RelativeTime, String>,

    /// The `Localization` cache generation `rendered` was built against;
    /// anything else means the locale changed under us.
    generation: u64,
}

impl RelativeTimeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Localized relative time between `timestamp` and `now`, formatted on the
    /// first frame its bucket is seen and borrowed on every frame after.
    pub fn time_ago_between(&mut self, i18n: &mut Localization, timestamp: u64, now: u64) -> &str {
        let key = relative_time(timestamp, now);

        if self.generation != i18n.cache_generation() {
            self.rendered.clear();
            self.generation = i18n.cache_generation();
        }

        if self.rendered.len() >= MAX_RENDERED_TIMES && !self.rendered.contains_key(&key) {
            self.rendered.clear();
        }

        self.rendered
            .entry(key)
            .or_insert_with(|| render_relative_time(i18n, key))
    }

    /// [`time_ago_between`](Self::time_ago_between) against the wall clock.
    pub fn time_ago_since(&mut self, i18n: &mut Localization, timestamp: u64) -> &str {
        let now = unix_time_secs();

        self.time_ago_between(i18n, timestamp, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn i18n() -> Localization {
        Localization::no_bidi()
    }

    fn ago(i18n: &mut Localization, secs_ago: u64) -> String {
        time_ago_between(i18n, NOW - secs_ago, NOW)
    }

    #[test]
    fn renders_one_unit_below_a_day_and_two_above() {
        let i18n = &mut i18n();

        assert_eq!(ago(i18n, 0), "now");
        assert_eq!(ago(i18n, 2), "now");
        assert_eq!(ago(i18n, 3), "3s");
        assert_eq!(ago(i18n, 59), "59s");
        assert_eq!(ago(i18n, ONE_MINUTE_IN_SECONDS), "1m");
        assert_eq!(ago(i18n, 90 * ONE_MINUTE_IN_SECONDS), "1h");
        assert_eq!(
            ago(i18n, 3 * ONE_DAY_IN_SECONDS + 4 * ONE_HOUR_IN_SECONDS),
            "3d 4h"
        );
        assert_eq!(ago(i18n, 3 * ONE_DAY_IN_SECONDS), "3d");
        assert_eq!(
            ago(i18n, ONE_YEAR_IN_SECONDS + 6 * ONE_MONTH_IN_SECONDS),
            "1y 6mo"
        );
    }

    #[test]
    fn a_future_timestamp_is_prefixed() {
        let i18n = &mut i18n();

        assert_eq!(time_ago_between(i18n, NOW + 1, NOW), "+now");
        assert_eq!(
            time_ago_between(i18n, NOW + 5 * ONE_MINUTE_IN_SECONDS, NOW),
            "+5m"
        );
        assert_eq!(
            time_ago_between(
                i18n,
                NOW + 3 * ONE_DAY_IN_SECONDS + 4 * ONE_HOUR_IN_SECONDS,
                NOW
            ),
            "+3d 4h"
        );
    }

    /// The cache key is the bucket, so it must agree with the uncached path for
    /// every gap, and must not hand back a stale string when the bucket moves.
    #[test]
    fn the_cache_agrees_with_the_uncached_path_as_the_clock_moves() {
        let i18n = &mut i18n();
        let cache = &mut RelativeTimeCache::new();

        // Walk a gap that crosses every branch, including the boundaries where
        // the rendered unit changes.
        let gaps = (0..200).chain((0..400).map(|n| n * 997)).chain([
            ONE_MINUTE_IN_SECONDS - 1,
            ONE_MINUTE_IN_SECONDS,
            ONE_HOUR_IN_SECONDS - 1,
            ONE_HOUR_IN_SECONDS,
            ONE_DAY_IN_SECONDS - 1,
            ONE_DAY_IN_SECONDS,
            ONE_WEEK_IN_SECONDS - 1,
            ONE_WEEK_IN_SECONDS,
            ONE_MONTH_IN_SECONDS - 1,
            ONE_MONTH_IN_SECONDS,
            ONE_YEAR_IN_SECONDS - 1,
            ONE_YEAR_IN_SECONDS,
            40 * ONE_YEAR_IN_SECONDS,
        ]);

        for gap in gaps {
            let timestamp = NOW - gap;
            let expected = time_ago_between(i18n, timestamp, NOW);
            assert_eq!(
                cache.time_ago_between(i18n, timestamp, NOW),
                expected,
                "gap of {gap}s"
            );

            // And the same gap in the other direction.
            let expected = time_ago_between(i18n, NOW + gap, NOW);
            assert_eq!(
                cache.time_ago_between(i18n, NOW + gap, NOW),
                expected,
                "gap of {gap}s into the future"
            );
        }
    }

    /// The relative-time messages have no FTL entries today, so both locales
    /// render them through `tr!`'s fallback and a round-trip through
    /// `set_locale` cannot be observed from the outside. Check the mechanism
    /// instead: a locale change must drop what was rendered against the old one.
    #[test]
    fn the_cache_notices_a_locale_change() {
        use unic_langid::langid;

        let i18n = &mut i18n();
        let cache = &mut RelativeTimeCache::new();

        cache.time_ago_between(i18n, NOW - ONE_HOUR_IN_SECONDS, NOW);
        cache.time_ago_between(i18n, NOW - ONE_MINUTE_IN_SECONDS, NOW);
        assert_eq!(cache.rendered.len(), 2);

        i18n.set_locale(langid!("en-XA")).unwrap();
        cache.time_ago_between(i18n, NOW - ONE_HOUR_IN_SECONDS, NOW);

        assert_eq!(
            cache.generation,
            i18n.cache_generation(),
            "cache did not rebase onto the new locale"
        );
        assert_eq!(
            cache.rendered.len(),
            1,
            "cache kept the old locale's text across set_locale"
        );
    }

    #[test]
    fn the_cache_does_not_grow_without_bound() {
        let i18n = &mut i18n();
        let cache = &mut RelativeTimeCache::new();

        // Every second under a minute is its own bucket, so this is more
        // distinct keys than the cap allows.
        for gap in 3..(3 * MAX_RENDERED_TIMES as u64) {
            cache.time_ago_between(i18n, NOW - gap, NOW);
        }

        assert!(cache.rendered.len() <= MAX_RENDERED_TIMES);
    }
}
