use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Computes the deterministic base delay for a given attempt number.
/// Formula: `5s * 2^attempt`, capped at `max`.
pub(crate) fn base_delay(attempt: u32, max: Duration) -> Duration {
    let secs = 5u64.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(max)
}

pub(crate) fn jitter_seed(key: &impl Hash, attempt: u32) -> u64 {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    attempt.hash(&mut hasher);
    now_nanos.hash(&mut hasher);
    hasher.finish()
}

/// Returns the backoff delay for the given attempt count.
///
/// Uses the exponential base delay as the primary component and adds up to 25%
/// additive jitter (via key/time mixed seed) to spread out simultaneous
/// retries without undermining the exponential delay itself.
pub(crate) fn next_duration(attempt: u32, jitter_seed: u64, max: Duration) -> Duration {
    let base = base_delay(attempt, max);
    let jitter_ceiling = base / 4;
    let jitter = if jitter_ceiling.is_zero() {
        Duration::ZERO
    } else {
        let jitter_ceiling_nanos = jitter_ceiling.as_nanos() as u64;
        Duration::from_nanos(jitter_seed % jitter_ceiling_nanos)
    };
    (base + jitter).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: Duration = Duration::from_secs(30 * 60); // 30 minutes

    /// Base delay doubles on each attempt until it reaches the configured cap.
    #[test]
    fn base_delay_doubles_with_cap() {
        assert_eq!(base_delay(0, MAX), Duration::from_secs(5));
        assert_eq!(base_delay(1, MAX), Duration::from_secs(10));
        assert_eq!(base_delay(2, MAX), Duration::from_secs(20));
        assert_eq!(base_delay(3, MAX), Duration::from_secs(40));
        assert_eq!(base_delay(4, MAX), Duration::from_secs(80));
        assert_eq!(base_delay(5, MAX), Duration::from_secs(160));
        assert_eq!(base_delay(6, MAX), Duration::from_secs(320));
        assert_eq!(base_delay(7, MAX), Duration::from_secs(640));
        assert_eq!(base_delay(8, MAX), Duration::from_secs(1280));
        assert_eq!(base_delay(9, MAX), MAX);
        // Saturates at cap for any large attempt count.
        assert_eq!(base_delay(100, MAX), MAX);
    }

    /// Jittered delay is always >= the base and never exceeds base * 1.25 or the cap.
    #[test]
    fn jitter_within_bounds() {
        for attempt in [0u32, 1, 3, 8, 9, 50, 100] {
            let base = base_delay(attempt, MAX);
            let max_with_jitter = (base + (base / 4)).min(MAX);
            for sample in 0u64..20 {
                let jittered = next_duration(attempt, 0xBAD5EED ^ sample, MAX);
                assert!(
                    jittered >= base,
                    "jittered {jittered:?} < base {base:?} at attempt {attempt}"
                );
                assert!(
                    jittered <= max_with_jitter,
                    "jittered {jittered:?} exceeds max-with-jitter {max_with_jitter:?} at attempt {attempt}"
                );
            }
        }
    }
}
