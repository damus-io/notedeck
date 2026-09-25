//! Fractional ranking: [`rank_between`] mints the order keys columns and
//! sequences sort by.

/// Smallest rank digit value below `'a'` and above `'z'` used as open bounds.
const RANK_LOW: u8 = b'a' - 1;

const RANK_HIGH: u8 = b'z' + 1;

/// Produce a rank string that sorts strictly between `left` and `right` (each an
/// optional existing rank). `None` means "open" — i.e. `rank_between(None, None)`
/// is the first rank, `rank_between(Some(last), None)` appends after `last`, and
/// `rank_between(None, Some(first))` prepends before `first`.
///
/// Ranks are lowercase `a`–`z` strings compared lexicographically. Appending and
/// inserting-between are unbounded (ranks just grow in length), but prepending
/// repeatedly walks toward `"a"` and nothing sorts before `"a"`; exhausting the
/// low end requires a rank rebalance (future work). New boards seed from the
/// midpoint to keep headroom on both sides.
pub fn rank_between(left: Option<&str>, right: Option<&str>) -> String {
    let l = left.unwrap_or("").as_bytes();
    let r = right.unwrap_or("").as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut right_open = false;

    loop {
        let lc = l.get(i).copied().unwrap_or(RANK_LOW);
        let rc = if right_open {
            RANK_HIGH
        } else {
            r.get(i).copied().unwrap_or(RANK_HIGH)
        };

        let mid = (lc + rc) / 2;
        if mid != lc {
            out.push(mid);
            return String::from_utf8(out).expect("ascii rank");
        }

        // lc and rc are adjacent (or equal): keep this digit and descend. Once
        // we've committed a digit equal to lc while rc == lc + 1, every deeper
        // digit is already < right, so the right bound is released.
        out.push(if lc == RANK_LOW { b'a' } else { lc });
        if rc == lc + 1 {
            right_open = true;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_between_appends_in_increasing_order() {
        let mut last = rank_between(None, None);
        for _ in 0..50 {
            let next = rank_between(Some(&last), None);
            assert!(next > last, "{next:?} should be > {last:?}");
            last = next;
        }
    }

    #[test]
    fn rank_between_prepends_in_decreasing_order() {
        // Prepending repeatedly walks toward the "a" floor; a few levels are
        // always available (a real rebalance is needed to go below "a", which
        // is tracked as future work — see `rank_between` docs).
        let mut first = rank_between(None, None);
        for _ in 0..3 {
            let prev = rank_between(None, Some(&first));
            assert!(prev < first, "{prev:?} should be < {first:?}");
            assert!(prev.bytes().all(|b| b.is_ascii_lowercase()));
            first = prev;
        }
    }

    #[test]
    fn rank_between_inserts_strictly_between() {
        let a = rank_between(None, None);
        let b = rank_between(Some(&a), None);
        for _ in 0..50 {
            let mid = rank_between(Some(&a), Some(&b));
            assert!(
                mid > a && mid < b,
                "{mid:?} not strictly between {a:?},{b:?}"
            );
        }
        // Adjacent ranks still admit an in-between value by growing length.
        let lo = "m".to_string();
        let hi = "n".to_string();
        let mid = rank_between(Some(&lo), Some(&hi));
        assert!(mid > lo && mid < hi, "{mid:?} not between {lo:?},{hi:?}");
    }
}
