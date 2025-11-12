# ewebsock

[<img alt="github" src="https://img.shields.io/badge/github-rerun_io/ewebsock-8da0cb?logo=github" height="20">](https://github.com/rerun-io/ewebsock)
[![Latest version](https://img.shields.io/crates/v/ewebsock.svg)](https://crates.io/crates/ewebsock)
[![Documentation](https://docs.rs/ewebsock/badge.svg)](https://docs.rs/ewebsock)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance/)
[![Build Status](https://github.com/rerun-io/ewebsock/workflows/CI/badge.svg)](https://github.com/rerun-io/ewebsock/actions?workflow=CI)
![MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Apache](https://img.shields.io/badge/license-Apache-blue.svg)

This is a simple [WebSocket](https://en.wikipedia.org/wiki/WebSocket) library for Rust which can be compiled to both native and web (WASM).

## Usage

``` rust
let options = ewebsock::Options::default();
// see documentation for more options
let (mut sender, receiver) = ewebsock::connect("ws://example.com", options).unwrap();
sender.send(ewebsock::WsMessage::Text("Hello!".into()));
while let Some(event) = receiver.try_recv() {
    println!("Received {:?}", event);
}
```

## Testing

First start the example echo server with:

```sh
cargo r -p echo_server
```

Then test the library with:

```sh
# native mode
cargo run -p example_app

# web mode
# install trunk with `cargo install trunk` - https://trunkrs.dev/
(cd example_app && trunk serve)
```
