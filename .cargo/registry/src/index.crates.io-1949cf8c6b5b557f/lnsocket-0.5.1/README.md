# lnsocket-rs

A Rust library for establishing connections to Lightning Network nodes. This library provides low-level primitives for establishing encrypted communication channels with Lightning Network nodes using the Noise_XK protocol as specified in [BOLT #8](https://github.com/lightning/bolts/blob/master/08-transport.md).

lnsocket also comes batteries included with Commando support, allowing you to call RPCs on core-lightning nodes remotely!

## Features

- [x] Establish encrypted connections to Lightning Network nodes with Noise_XK handshake protocol
- [x] Send and receive Lightning Network messages
- [x] Support for Commando CLN RPC messages

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
lnsocket = "0.1.0"
```

## Commando over LNSocket

This crate includes a small [Commando][commando] client that runs **over the same encrypted Lightning transport**.

```rust
use bitcoin::secp256k1::{SecretKey, PublicKey, rand};
use lnsocket::{LNSocket, CommandoClient};
use serde_json::json;
use lnsocket::commando::CallOpts;

async fn commando_rpc_demo(pk: PublicKey, rune: &str) -> Result<(), lnsocket::Error> {
    let key = SecretKey::new(&mut rand::thread_rng());
    let sock = LNSocket::connect_and_init(key, pk, "ln.example.com:9735").await?;
    let client = CommandoClient::spawn(sock, rune);

    // Inherit client defaults (30s timeout, auto-reconnect with backoff,
    // and retry up to 3 times). Override per call if needed:
    let res = client.call("getinfo", json!({})).await?;
    println!("{}", res);

    let opts = CallOpts::new().timeout(std::time::Duration::from_secs(5)).retry(5);
    let channels = client.call_with_opts("listchannels", json!({}), &opts).await?;
    Ok(())
}

```

## Status

This library is experimental and under active development. APIs may change significantly between versions.

## License

This library contains code derived from LDK, which is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](http://www.apache.org/licenses/LICENSE-2.0))
- MIT License ([LICENSE-MIT](http://opensource.org/licenses/MIT))

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

[commando]: https://docs.corelightning.org/reference/commando
