//! Rank strings: the order keys vault rules sort by, so moving one rule
//! rewrites that rule's note and nothing else.
//!
//! A rank is a base-26 fraction in `(0, 1)` written with the digits `a`–`z`
//! (`a` = 0), compared as a plain string. No rank ends in `a`, which would
//! equal the shorter rank without it; that is what leaves room between any
//! two ranks, and before the first, forever.
//!
//! headway has a `rank_between` of its own (`crates/headway/src/event/rank.rs`),
//! but it lets ranks end in `a`: three prepends in a row reach `"a"`, and the
//! next one returns `"am"`, which sorts after it. This one is the same idea
//! without that hole.

/// The value of digit `c`, `a` = 0.
fn digit(c: u8) -> u8 {
    c - b'a'
}

/// The digit with value `d`.
fn char_of(d: u8) -> u8 {
    b'a' + d
}

/// The digit base: one past `z`.
const BASE: u8 = 26;

/// Whether `text` is a rank: non-empty, digits `a`–`z`, not ending in `a`.
pub fn is_rank(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|c| c.is_ascii_lowercase()) && !text.ends_with('a')
}

/// A rank that sorts strictly between `left` and `right`, either of which
/// may be open (`None`): `between(None, None)` is the first rank, and
/// `between(Some(last), None)` appends after `last`.
///
/// `None` when there is no room: `left` is not before `right` (two rules
/// that tied on a rank), or either is not a rank.
pub fn between(left: Option<&str>, right: Option<&str>) -> Option<String> {
    if !left.is_none_or(is_rank) || !right.is_none_or(is_rank) {
        return None;
    }
    if let (Some(left), Some(right)) = (left, right)
        && left >= right
    {
        return None;
    }
    let left = left.unwrap_or("").as_bytes();
    let mut out = Vec::with_capacity(left.len() + 1);
    midpoint(left, right.map(str::as_bytes), &mut out);
    Some(String::from_utf8(out).expect("ranks are ascii"))
}

/// Append to `out` the digits of a fraction strictly between `left` (empty
/// is 0) and `right` (`None` is 1), neither ending in `a`.
fn midpoint(left: &[u8], right: Option<&[u8]>, out: &mut Vec<u8>) {
    // Digits both share are the answer's too.
    if let Some(right) = right {
        let common = (0..right.len())
            .take_while(|&i| left.get(i).copied().unwrap_or(b'a') == right[i])
            .count();
        if common > 0 {
            out.extend_from_slice(&right[..common]);
            let rest = left.get(common..).unwrap_or(&[]);
            return midpoint(rest, Some(&right[common..]), out);
        }
    }
    let low = left.first().map_or(0, |&c| digit(c));
    let high = right.and_then(|r| r.first()).map_or(BASE, |&c| digit(c));
    if high - low > 1 {
        // A digit strictly between: never `a`, since it is above `low`.
        out.push(char_of(low.midpoint(high)));
        return;
    }
    match right {
        // `right`'s first digit alone is shorter than `right`, so before it,
        // and its first digit is above `left`'s, so after `left`.
        Some(right) if right.len() > 1 => out.push(right[0]),
        // Adjacent digits: keep `left`'s and go one deeper, after the rest
        // of `left`, where `right` no longer constrains.
        _ => {
            out.push(char_of(low));
            midpoint(left.get(1..).unwrap_or(&[]), None, out);
        }
    }
}

/// `n` ranks, in order, strictly between `left` and `right`, spread by
/// bisection so they stay short: importing 50 rules adds about six digits,
/// not one per rule.
///
/// `None` when [`between`] finds no room.
pub fn spread(left: Option<&str>, right: Option<&str>, n: usize) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(n);
    spread_into(left, right, n, &mut out)?;
    Some(out)
}

fn spread_into(
    left: Option<&str>,
    right: Option<&str>,
    n: usize,
    out: &mut Vec<String>,
) -> Option<()> {
    if n == 0 {
        return Some(());
    }
    let mid = between(left, right)?;
    let before = (n - 1) / 2;
    spread_into(left, Some(&mid), before, out)?;
    out.push(mid.clone());
    spread_into(Some(&mid), right, n - 1 - before, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(left: Option<&str>, right: Option<&str>) -> String {
        let mid = between(left, right).unwrap();
        assert!(left.is_none_or(|l| l < mid.as_str()), "{left:?} !< {mid}");
        assert!(right.is_none_or(|r| mid.as_str() < r), "{mid} !< {right:?}");
        assert!(!mid.ends_with('a'), "{mid} ends in a");
        mid
    }

    /// Prepending forever keeps finding room, where headway's rank reaches
    /// `"a"` after three and then sorts the fourth after it.
    #[test]
    fn prepends_never_run_out() {
        let mut first = check(None, None);
        for _ in 0..200 {
            first = check(None, Some(&first));
        }
    }

    #[test]
    fn appends_and_inserts_stay_ordered() {
        let mut last = check(None, None);
        for _ in 0..200 {
            last = check(Some(&last), None);
        }
        // Always between two neighbours, closing in on one side.
        let (mut l, r) = ("b".to_owned(), "bab".to_owned());
        for _ in 0..200 {
            l = check(Some(&l), Some(&r));
        }
        let (l, mut r) = ("b".to_owned(), "bab".to_owned());
        for _ in 0..200 {
            r = check(Some(&l), Some(&r));
        }
        for (l, r) in [
            ("az", "b"),
            ("y", "z"),
            ("yz", "z"),
            ("m", "mb"),
            ("zz", "zzb"),
        ] {
            check(Some(l), Some(r));
        }
    }

    #[test]
    fn spread_is_ordered_short_and_between() {
        let ranks = spread(None, None, 54).unwrap();
        assert_eq!(ranks.len(), 54);
        assert!(ranks.windows(2).all(|w| w[0] < w[1]));
        assert!(ranks.iter().all(|r| r.len() <= 6), "{ranks:?}");
        let inside = spread(Some("c"), Some("d"), 10).unwrap();
        assert!(inside.iter().all(|r| r.as_str() > "c" && r.as_str() < "d"));
        assert!(inside.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(spread(None, None, 0), Some(vec![]));
    }

    /// No room between a rank and itself (a tie), backwards bounds, or
    /// something that isn't a rank.
    #[test]
    fn no_room_is_none() {
        assert_eq!(between(Some("m"), Some("m")), None);
        assert_eq!(between(Some("m"), Some("c")), None);
        for bad in ["", "ma", "M", "m1"] {
            assert!(!is_rank(bad));
            assert_eq!(between(Some(bad), None), None);
            assert_eq!(between(None, Some(bad)), None);
        }
        assert!(is_rank("m") && is_rank("az"));
        assert_eq!(spread(Some("m"), Some("m"), 3), None);
    }
}
