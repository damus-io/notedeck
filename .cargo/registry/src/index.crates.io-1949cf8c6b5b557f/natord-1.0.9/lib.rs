// Natord: Natural ordering for Rust.
// Copyright (c) 2014-2015, Kang Seonghoon.
// See README.md and LICENSE.txt for details.

/*!

# Natord 1.0.9

Natural ordering for Rust. (also known as `rust-natord`)
This allows for the comparison like this:

~~~~ {.rust}
let mut files = vec!("rfc2086.txt", "rfc822.txt", "rfc1.txt");
files.sort_by(|&a, &b| natord::compare(a, b));
assert_eq!(files, ["rfc1.txt", "rfc822.txt", "rfc2086.txt"]);
~~~~

There are multiple natural ordering algorithms available.
This version of natural ordering is inspired by
[Martin Pool's `strnatcmp.c`](http://sourcefrog.net/projects/natsort/).

*/

#![crate_name = "natord"]
#![crate_type = "lib"]

use std::cmp::Ordering;
use std::cmp::Ordering::{Less, Equal, Greater};

/// Compares two iterators of "characters" possibly containing "digits".
/// The natural ordering can be customized with the following parameters:
///
/// * `skip` returns true if the "character" does not affect the comparison,
///   other than splitting two consecutive digits.
/// * `cmp` compares two "characters", assuming that they are not "digits".
/// * `to_digit` converts a "character" into a "digit" if possible. The digit of zero is special.
pub fn compare_iter<T, L, R, Skip, Cmp, ToDigit>(left: L, right: R, mut skip: Skip, mut cmp: Cmp,
                                                 mut to_digit: ToDigit) -> Ordering
        where L: Iterator<Item=T>,
              R: Iterator<Item=T>,
              Skip: for<'a> FnMut(&'a T) -> bool,
              Cmp: for<'a> FnMut(&'a T, &'a T) -> Ordering,
              ToDigit: for<'a> FnMut(&'a T) -> Option<isize> {
    let mut left = left.fuse();
    let mut right = right.fuse();

    let mut l;
    let mut r;
    let mut ll;
    let mut rr;

    macro_rules! read_left {
        () => ({
            l = left.next();
            ll = l.as_ref().and_then(|v| to_digit(v));
        })
    }

    macro_rules! read_right {
        () => ({
            r = right.next();
            rr = r.as_ref().and_then(|v| to_digit(v));
        })
    }

    macro_rules! return_unless_equal {
        ($ord:expr) => (
            match $ord {
                Equal => {}
                lastcmp => return lastcmp,
            }
        )
    }

    read_left!();
    read_right!();
    'nondigits: loop {
        // skip preceding whitespaces
        while l.as_ref().map_or(false, |c| skip(c)) { read_left!(); }
        while r.as_ref().map_or(false, |c| skip(c)) { read_right!(); }

        match (l, r) {
            (Some(l_), Some(r_)) => match (ll, rr) {
                (Some(ll_), Some(rr_)) => {
                    if ll_ == 0 || rr_ == 0 {
                        // left-aligned matching. (`015` < `12`)
                        return_unless_equal!(ll_.cmp(&rr_));
                        'digits_left: loop {
                            read_left!();
                            read_right!();
                            match (ll, rr) {
                                (Some(ll_), Some(rr_)) => return_unless_equal!(ll_.cmp(&rr_)),
                                (Some(_), None) => return Greater,
                                (None, Some(_)) => return Less,
                                (None, None) => break 'digits_left,
                            }
                        }
                    } else {
                        // right-aligned matching. (`15` < `123`)
                        let mut lastcmp = ll_.cmp(&rr_);
                        'digits_right: loop {
                            read_left!();
                            read_right!();
                            match (ll, rr) {
                                (Some(ll_), Some(rr_)) => {
                                    // `lastcmp` is only used when there are the same number of
                                    // digits, so we only update it.
                                    if lastcmp == Equal { lastcmp = ll_.cmp(&rr_); }
                                }
                                (Some(_), None) => return Greater,
                                (None, Some(_)) => return Less,
                                (None, None) => break 'digits_right,
                            }
                        }
                        return_unless_equal!(lastcmp);
                    }
                    continue 'nondigits; // do not read from the iterators again
                },
                (_, _) => return_unless_equal!(cmp(&l_, &r_)),
            },
            (Some(_), None) => return Greater,
            (None, Some(_)) => return Less,
            (None, None) => return Equal,
        }

        read_left!();
        read_right!();
    }
}

/// Compares two strings case-sensitively.
/// It skips any Unicode whitespaces and handles a series of decimal digits.
pub fn compare(left: &str, right: &str) -> Ordering {
    compare_iter(left.chars(), right.chars(),
                 |&c| c.is_whitespace(),
                 |&l, &r| l.cmp(&r),
                 |&c| c.to_digit(10).map(|v| v as isize))
}

/// Compares two strings case-insensitively.
/// It skips any Unicode whitespaces and handles a series of decimal digits.
pub fn compare_ignore_case(left: &str, right: &str) -> Ordering {
    // XXX what we really want is a case folding!
    // Unicode case folding can be done iteratively, but currently we don't have them in stdlib.

    let left_iter  =  left.chars().flat_map(|c| c.to_lowercase());
    let right_iter = right.chars().flat_map(|c| c.to_lowercase());

    compare_iter(left_iter, right_iter,
                 |&c| c.is_whitespace(),
                 |&l, &r| l.cmp(&r),
                 |&c| c.to_digit(10).map(|v| v as isize))
}

#[cfg(test)]
mod tests {
    use super::compare;
    use std::cmp::Ordering;
    use std::cmp::Ordering::{Less, Equal, Greater};

    fn check_total_order(strs: &[&str]) {
        fn ordering_to_op(ord: Ordering) -> &'static str {
            match ord {
                Greater => ">",
                Equal => "=",
                Less => "<",
            }
        }

        for (i, &x) in strs.iter().enumerate() {
            for (j, &y) in strs.iter().enumerate() {
                assert!(compare(x, y) == i.cmp(&j),
                        "expected x {} y, returned x {} y (where x = `{}`, y = `{}`)",
                        ordering_to_op(i.cmp(&j)), ordering_to_op(compare(x, y)), x, y);
            }
        }
    }

    #[test]
    fn test_numeric() {
        check_total_order(&["a", "a0", "a1", "a1a", "a1b", "a2", "a10", "a20"]);
    }

    #[test]
    fn test_multiple_parts() {
        check_total_order(&["x2-g8", "x2-y7", "x2-y8", "x8-y8"]);
    }

    #[test]
    fn test_leading_zeroes() {
        check_total_order(&["1.001", "1.002", "1.010", "1.02", "1.1", "1.3"]);
    }

    #[test]
    fn test_longer() {
        check_total_order(&[
            "1-02",
            "1-2",
            "1-20",
            "10-20",
            "fred",
            "jane",
            "pic1",
            "pic2",
            "pic2a",
            "pic3",
            "pic4",
            "pic4   alpha",
            "pic 4 else",
            "pic4  last",
            "pic5",
            "pic5.07",
            "pic5.08",
            "pic5.13",
            "pic5.113",
            "pic 5 something",
            "pic 6",
            "pic   7",
            "pic100",
            "pic100a",
            "pic120",
            "pic121",
            "pic2000",
            "tom",
            "x2-g8",
            "x2-y7",
            "x2-y8",
            "x8-y8",
        ]);
    }
}
