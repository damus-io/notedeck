vec1 [![Crates.io](https://img.shields.io/crates/v/vec1.svg)](https://crates.io/crates/vec1) [![vec1](https://docs.rs/vec1/badge.svg)](https://docs.rs/vec1) [![License](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT) [![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
=============

This crate provides a rust `std::vec::Vec` wrapper with type
guarantees to contain at least 1 element. This is useful if
you build a API which sometimes has such constraints e.g. you
need at least one target server address but there can be more.

Example
--------

```rust
#[macro_use]
extern crate vec1;

use vec1::Vec1;

fn main() {
    // vec1![] makes sure there is at least one element
    // at compiler time
    //let names = vec1! [ ];
    let names = vec1! [ "Liz" ];
    greet(names);
}

fn greet(names: Vec1<&str>) {
    // methods like first/last which return a Option on Vec do
    // directly return the value, we know it's possible
    let first = names.first();
    println!("hallo {}", first);
    for name in names.iter().skip(1) {
        println!("  who is also know as {}", name)
    }
}

```

Support for `serde::{Serialize, Deserialize}`
-------------

The `Vec1` type supports both of [`serde`](https://serde.rs/)'s `Serialize` and
`Deserialize` traits, but this feature is only enabled when the `"serde"` feature
flag is specified in your project's `Cargo.toml` file:

```toml
# Cargo.toml

[dependencies]
vec1 = { version = "...", features = ["serde"] }
```

Building docs like on docs.rs
-------------

To build docs which document all features and contains hints
which functions require which features use following command:

```sh
RUSTDOCFLAGS="--cfg docs" cargo +nightly doc --all-features
```

This will document all features and enable the unstable
nightly only `doc_auto_cfg` feature.


License
--------

Licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

Contribution
------------

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

Contributors: [./CONTRIBUTORS.md](./CONTRIBUTORS.md)

Change Log
-----------

See [./CHANGELOG.md](./CHANGELOG.md)