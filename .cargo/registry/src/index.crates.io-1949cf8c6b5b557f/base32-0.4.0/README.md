# base32 [![Build Status](https://travis-ci.org/andreasots/base32.svg?branch=master)](https://travis-ci.org/andreasots/base32)

This library lets you encode and decode in either [RFC4648 Base32](http://en.wikipedia.org/wiki/Base32#RFC_4648_Base32_alphabet) or in [Crockford Base32](http://en.wikipedia.org/wiki/Base32#Crockford.27s_Base32).

# Usage

```rust
// crockford base32
assert_eq!(encode(CrockfordBase32, [0xF8, 0x3E, 0x0F, 0x83, 0xE0]), Vec::from_slice("Z0Z0Z0Z0".to_ascii()));
assert_eq!(decode(CrockfordBase32, "Z0Z0Z0Z0".to_ascii()).unwrap(), vec![0xF8, 0x3E, 0x0F, 0x83, 0xE0]);

// rfc4648 base32
assert_eq!(encode(RFC4648Base32, [0xF8, 0x3E, 0x7F, 0x83, 0xE7]), Vec::from_slice("7A7H7A7H".to_ascii()));
assert_eq!(decode(RFC4648Base32, "7A7H7A7H".to_ascii()).unwrap(), vec![0xF8, 0x3E, 0x7F, 0x83, 0xE7]);

// rfc4648 base32 without padding characters (=)
assert_eq!(encode(UnpaddedRFC4648Base32, [0xF8, 0x3E, 0x7F, 0x83, 0xE7]), Vec::from_slice("7A7H7A7H".to_ascii()));
assert_eq!(decode(UnpaddedRFC4648Base32, "7A7H7A7H".to_ascii()).unwrap(), vec![0xF8, 0x3E, 0x7F, 0x83, 0xE7]);
```

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
