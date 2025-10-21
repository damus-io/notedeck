<!-- Allow this file to not have a first line heading -->
<!-- markdownlint-disable-file MD041 -->

<!-- inline html -->
<!-- markdownlint-disable-file MD033 -->

<div align="center">

# ⌛ `poll-promise`

**A Rust promise for games and immediate mode GUIs**

[![Embark](https://img.shields.io/badge/embark-open%20source-blueviolet.svg)](https://embark.dev)
[![Embark](https://img.shields.io/badge/discord-ark-%237289da.svg?logo=discord)](https://discord.gg/dAuKfZS)
[![Crates.io](https://img.shields.io/crates/v/poll-promise.svg)](https://crates.io/crates/poll-promise)
[![Docs](https://docs.rs/poll-promise/badge.svg)](https://docs.rs/poll-promise)
[![dependency status](https://deps.rs/repo/github/EmbarkStudios/poll-promise/status.svg)](https://deps.rs/repo/github/EmbarkStudios/poll-promise)
[![Build status](https://github.com/EmbarkStudios/physx-rs/workflows/CI/badge.svg)](https://github.com/EmbarkStudios/physx-rs/actions)
</div>

## Description

`poll-promise` is a Rust crate for polling the result of a concurrent (e.g. `async`) operation. This is in particular useful in games and immediate mode GUI:s, where one often wants to start a background operation and then ask "are we there yet?" on each subsequent frame until the operation completes.

Example:

``` rust
let promise = poll_promise::Promise::spawn_thread("slow_operation", something_slow);

// Then in the game loop or immediate mode GUI code:
if let Some(result) = promise.ready() {
    // Use/show result
} else {
    // Show a loading icon
}
```

If you enable the `tokio` feature you can use `poll-promise` with the [tokio](https://github.com/tokio-rs/tokio) runtime.

### Caveat
The crate is primarily useful as a high-level building block in apps.

This crate provides convenience methods to spawn threads and tokio tasks, and methods that block on waiting for a result.
This is gererally a bad idea to do in a library, as decisions about execution environments and thread blocking should be left to the app.
So we do not recommend using this crate for libraries in its current state.

## See also
Similar functionality is provided by:

* [`eventuals::Eventual`](https://docs.rs/eventuals/latest/eventuals/struct.Eventual.html)
* [`tokio::sync::watch::channel`](https://docs.rs/tokio/latest/tokio/sync/watch/fn.channel.html)

## Contribution

[![Contributor Covenant](https://img.shields.io/badge/contributor%20covenant-v1.4-ff69b4.svg)](../main/CODE_OF_CONDUCT.md)

We welcome community contributions to this project.

Please read our [Contributor Guide](CONTRIBUTING.md) for more information on how to get started.
Please also read our [Contributor Terms](CONTRIBUTING.md#contributor-terms) before you make any contributions.

Any contribution intentionally submitted for inclusion in an Embark Studios project, shall comply with the Rust standard licensing model (MIT OR Apache 2.0) and therefore be dual licensed as described below, without any additional terms or conditions:

### License

This contribution is dual licensed under EITHER OF

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

For clarity, "your" refers to Embark or any other licensee/user of the contribution.
