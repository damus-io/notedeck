[Natord][doc] 1.0.9
===================

[![Natord on Travis CI][travis-image]][travis]

[travis-image]: https://travis-ci.org/lifthrasiir/rust-natord.png
[travis]: https://travis-ci.org/lifthrasiir/rust-natord

Natural ordering for Rust. (also known as `rust-natord`)
This allows for the comparison like this:

~~~~ {.rust}
let mut files = vec!("rfc2086.txt", "rfc822.txt", "rfc1.txt");
files.sort_by(|&a, &b| natord::compare(a, b));
assert_eq!(files, ["rfc1.txt", "rfc822.txt", "rfc2086.txt"]);
~~~~

It provides a `compare` and `compare_ignore_case` function for comparing strings,
and also a `compare_iter` function for the customizable algorithm.

There are multiple natural ordering algorithms available.
This version of natural ordering is inspired by
[Martin Pool's `strnatcmp.c`](http://sourcefrog.net/projects/natsort/).
See the test cases in the source code to see what it can do and it cannot.

Natord is written by Kang Seonghoon and licensed under the MIT/X11 license.

[doc]: https://lifthrasiir.github.io/rust-natord/
