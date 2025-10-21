# hex_color

A simple, lightweight library for working with RGB(A) hexadecimal colors.

[![Build Status]][actions]
[![Latest Version]][crates.io]

[Build Status]: https://img.shields.io/github/actions/workflow/status/seancroach/hex_color/ci.yml?logo=github
[actions]: https://github.com/seancroach/hex_color/actions/workflows/ci.yml
[Latest Version]: https://img.shields.io/crates/v/hex_color?logo=rust
[crates.io]: https://crates.io/crates/hex_color

## Documentation

[Module documentation with examples](https://docs.rs/hex_color). The module documentation also
includes a comprehensive description of the syntax supported for parsing hex colors.

## Usage

This crate is [on crates.io][crates] and can be used by adding `hex_color`
to your dependencies in your project's `Cargo.toml`:

```toml
[dependencies]
hex_color = "3"
```

[crates]: https://crates.io/crates/hex_color

## Examples

Basic parsing:

```rust
use hex_color::HexColor;

let cyan = HexColor::parse("#0FF")?;
assert_eq!(cyan, HexColor::CYAN);

let transparent_plum = HexColor::parse("#DDA0DD80")?;
assert_eq!(transparent_plum, HexColor::rgba(221, 160, 221, 128));

// Strictly enforce only an RGB color through parse_rgb:
let pink = HexColor::parse_rgb("#FFC0CB")?;
assert_eq!(pink, HexColor::rgb(255, 192, 203));

// Strictly enforce an alpha component through parse_rgba:
let opaque_white = HexColor::parse_rgba("#FFFF")?;
assert_eq!(opaque_white, HexColor::WHITE);
```

Flexible constructors:

```rust
use hex_color::HexColor;

let violet = HexColor::rgb(238, 130, 238);
let transparent_maroon= HexColor::rgba(128, 0, 0, 128);
let transparent_gray = HexColor::GRAY.with_a(128);
let lavender = HexColor::from_u24(0x00E6_E6FA);
let transparent_lavender = HexColor::from_u32(0xE6E6_FA80);
let floral_white = HexColor::WHITE
    .with_g(250)
    .with_b(240);
```

Comprehensive arithmetic:

```rust
use hex_color::HexColor;

assert_eq!(HexColor::BLUE + HexColor::RED, HexColor::MAGENTA);
assert_eq!(
    HexColor::CYAN.saturating_add(HexColor::GRAY),
    HexColor::rgb(128, 255, 255),
);
assert_eq!(
    HexColor::BLACK.wrapping_sub(HexColor::achromatic(1)),
    HexColor::WHITE,
);
```

### With [`rand`]

Using `rand` + `std` features to generate random colors via [`rand`][`rand`]
out of the box:

[`rand`]: https://docs.rs/rand

```rust
use hex_color::HexColor;

let random_rgb: HexColor = rand::random();
```

To specify whether an RGB or RGBA color is randomly created, use
`HexColor::random_rgb` or `HexColor::random_rgba` respectively:

```rust
use hex_color::HexColor;

let random_rgb = HexColor::random_rgb();
let random_rgba = HexColor::random_rgba();
```

### With [`serde`]

Use [`serde`] to serialize and deserialize colors in multiple
formats: `u24`, `u32`, `rgb`, or `rgba`:

[`serde`]: https://docs.rs/serde

```rust
use hex_color::HexColor;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Color {
    name: String,
    value: HexColor,
}

let json_input = json!({
    "name": "Light Coral",
    "value": "#F08080",
});
assert_eq!(
    serde_json::from_value::<Color>(json_input)?,
    Color {
        name: String::from("Light Coral"),
        value: HexColor::rgb(240, 128, 128),
    },
);

let my_color = Color {
    name: String::from("Dark Salmon"),
    value: HexColor::rgb(233, 150, 122),
};
assert_eq!(
    serde_json::to_value(my_color)?,
    json!({
        "name": "Dark Salmon",
        "value": "#E9967A",
    }),
);

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct NumericColor {
    name: String,
    #[serde(with = "hex_color::u24")]
    value: HexColor,
}

let json_input = json!({
    "name": "Light Coral",
    "value": 0x00F0_8080_u32,
});
assert_eq!(
    serde_json::from_value::<NumericColor>(json_input)?,
    NumericColor {
        name: String::from("Light Coral"),
        value: HexColor::rgb(240, 128, 128),
    },
);

let my_color = NumericColor {
    name: String::from("Dark Salmon"),
    value: HexColor::rgb(233, 150, 122),
};
assert_eq!(
    serde_json::to_value(my_color)?,
    json!({
        "name": "Dark Salmon",
        "value": 0x00E9_967A_u32,
    }),
);
```

## Features

* `rand` enables out-of-the-box compatability with the [`rand`]
  crate.
* `serde` enables serialization and deserialization with the
  [`serde`] crate.
* `std` enables `std::error::Error` on `ParseHexColorError`. Otherwise,
  it's needed with `rand` for `HexColor::random_rgb`, `HexColor::random_rgba`,
  and, of course, `rand::random`.

*Note*: Only the `std` feature is enabled by default.

## License

Licensed under either of

-   Apache License, Version 2.0
    ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license
    ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
