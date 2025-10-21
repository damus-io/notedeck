# log-once

[![Tests](https://github.com/Luthaf/log-once/actions/workflows/tests.yml/badge.svg?event=push)](https://github.com/Luthaf/log-once/actions/workflows/tests.yml)
[![Documentation](https://img.shields.io/badge/doc-docs.rs-green.svg)](https://docs.rs/log-once/)
[![Crates.io version](https://img.shields.io/crates/v/log-once.svg)](https://crates.io/crates/log-once)

Collection of helper macros for logging some events only once.

This crate provide macro in the `log_once` family (`warn_once!`,
`trace_once!`, ...); that only send a logging event once for every message.
It rely and uses the logging infrastructure in the [log][log] crate; and
is fully compatible with any logger implementation.

These macro will store the already seen messages in a `BTreeSet`, and check
if a message is in the set before sending the log event.

[log]: https://crates.io/crates/log

## Usage

1) Add it to your `Cargo.toml` file:

```toml
[dependencies]
log-once = "0.4"
```

2) import the macros:

```rust
use log_once::{warn_once, log_once};
```

3) Enjoy!

## Examples

```rust
use log::info;
use log_once::{info_once, warn_once};

pub fn shave_the_yak(yaks: &[Yak]) {
    for yak in yaks {
        info!(target: "yak_events", "Commencing yak shaving for {yak:?}");

        loop {
            match find_a_razor() {
                Ok(razor) => {
                    // This will only appear once in the logger output for each razor
                    info_once!("Razor located: {razor}");
                    yak.shave(razor);
                    break;
                }
                Err(err) => {
                    // This will only appear once in the logger output for each error
                    warn_once!("Unable to locate a razor: {err}, retrying");
                }
            }
        }
    }
}
```

## License

log-once is distributed under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.
