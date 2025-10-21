# blurhash-rs

![CI Build](https://github.com/whisperfish/blurhash-rs/workflows/Build/badge.svg)
[![Crates.io](https://img.shields.io/crates/v/blurhash.svg)](https://crates.io/crates/blurhash)
[![Crates.io](https://img.shields.io/crates/l/blurhash.svg)](https://crates.io/crates/blurhash)

> A pure Rust implementation of [Blurhash](https://github.com/woltapp/blurhash).

Blurhash is an algorithm written by [Dag Ågren](https://github.com/DagAgren) for [Wolt (woltapp/blurhash)](https://github.com/woltapp/blurhash) that encodes an image into a short (~20-30 byte) ASCII string. When you decode the string back into an image, you get a gradient of colors that represent the original image. This can be useful for scenarios where you want an image placeholder before loading, or even to censor the contents of an image [a la Mastodon](https://blog.joinmastodon.org/2019/05/improving-support-for-adult-content-on-mastodon/).

## 🚴 Usage

Add `blurhash` to your `Cargo.toml`:

```toml
[dependencies]
blurhash = "0.2.3"
```

By default, the `fast-linear-to-srgb` is enabled.
This improves decoding performance by about 60%, but has a memory overhead of 8KB.
If this overhead is problematic, you can disable it by instead specifying the following to your `Cargo.toml`:

```toml
[dependencies]
blurhash = { version = "0.2.3", default-features = false }
```

### Encoding
```rust
use blurhash::encode;
use image::GenericImageView;

fn main() {
  // Add image to your Cargo.toml
  let img = image::open("octocat.png").unwrap();
  let (width, height) = img.dimensions();
  let blurhash = encode(4, 3, width, height, &img.to_rgba().into_vec());
}
```

### Decoding
```rust
use blurhash::decode;

let pixels = decode("LBAdAqof00WCqZj[PDay0.WB}pof", 50, 50, 1.0);
```

## Licence

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
