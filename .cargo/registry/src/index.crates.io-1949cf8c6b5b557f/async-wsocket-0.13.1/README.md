# Async WebSocket

A convenience library for using websockets both in native and WASM environments! Include embedded tor client support.

```rust
use std::time::Duration;

use async_wsocket::{ConnectionMode, Url, WsMessage};
use futures_util::{SinkExt, StreamExt};

const NONCE: u64 = 123456789;

#[tokio::main]
async fn main() {
    let url =
        Url::parse("ws://oxtrdevav64z64yb7x6rjg4ntzqjhedm5b5zjqulugknhzr46ny2qbad.onion").unwrap();
    let (mut tx, mut rx) =
        async_wsocket::connect(&url, ConnectionMode::tor(), Duration::from_secs(120))
            .await
            .unwrap();

    // Send ping
    let nonce = NONCE.to_be_bytes().to_vec();
    tx.send(WsMessage::Ping(nonce.clone())).await.unwrap();

    // Listen for messages
    while let Some(msg) = rx.next().await {
        if let Ok(WsMessage::Pong(bytes)) = msg {
            assert_eq!(nonce, bytes);
            println!("Pong match!");
            break;
        }
    }
}
```

## Crate Feature Flags

The following crate feature flags are available:

| Feature               | Default | Description                                                             |
|-----------------------|:-------:|-------------------------------------------------------------------------|
| `socks`               |   No    | Enable `socks` proxy support                                            |
| `tor`                 |   No    | Enable embedded tor client support                                      |
| `tor-launch-service ` |   No    | Enable embedded tor client with support to launch hidden onion services |

## Minimum Supported Rust Version (MSRV)

The MSRV for this project when compiled with `default` features and on `native` targets is `1.63.0`. 
When using `tor` feature, MSRV is `1.70.0`. MSRV for WASM targets is `1.73.0`

## License

This project is distributed under the MIT software license - see the [LICENSE](LICENSE) file for details
