//! A simple, lightweight library for working with RGB(A) hexadecimal colors.
//!
//! # Usage
//!
//! This crate is [on crates.io][crates] and can be used by adding `hex_color`
//! to your dependencies in your project's `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! hex_color = "3"
//! ```
//!
//! [crates]: https://crates.io/crates/hex_color
//!
//! # Examples
//!
//! Basic parsing:
//!
//! ```
//! use hex_color::HexColor;
//!
//! # fn main() -> Result<(), hex_color::ParseHexColorError> {
//! let cyan = HexColor::parse("#0FF")?;
//! assert_eq!(cyan, HexColor::CYAN);
//!
//! let transparent_plum = HexColor::parse("#DDA0DD80")?;
//! assert_eq!(transparent_plum, HexColor::rgba(221, 160, 221, 128));
//!
//! // Strictly enforce only an RGB color through parse_rgb:
//! let pink = HexColor::parse_rgb("#FFC0CB")?;
//! assert_eq!(pink, HexColor::rgb(255, 192, 203));
//!
//! // Strictly enforce an alpha component through parse_rgba:
//! let opaque_white = HexColor::parse_rgba("#FFFF")?;
//! assert_eq!(opaque_white, HexColor::WHITE);
//! # Ok(())
//! # }
//! ```
//!
//! Flexible constructors:
//!
//! ```
//! use hex_color::HexColor;
//!
//! let violet = HexColor::rgb(238, 130, 238);
//! let transparent_maroon = HexColor::rgba(128, 0, 0, 128);
//! let transparent_gray = HexColor::GRAY.with_a(128);
//! let lavender = HexColor::from_u24(0x00E6_E6FA);
//! let transparent_lavender = HexColor::from_u32(0xE6E6_FA80);
//! let floral_white = HexColor::WHITE.with_g(250).with_b(240);
//! ```
//!
//! Comprehensive arithmetic:
//!
//! ```
//! use hex_color::HexColor;
//!
//! assert_eq!(HexColor::BLUE + HexColor::RED, HexColor::MAGENTA);
//! assert_eq!(
//!     HexColor::CYAN.saturating_add(HexColor::GRAY),
//!     HexColor::rgb(128, 255, 255),
//! );
//! assert_eq!(
//!     HexColor::BLACK.wrapping_sub(HexColor::achromatic(1)),
//!     HexColor::WHITE,
//! );
//! ```
//!
//! ## With [`rand`](::rand)
//!
//! Using `rand` + `std` features to generate random colors via [`rand`](::rand)
//! out of the box:
//!
//! ```
//! use hex_color::HexColor;
//!
//! let random_rgb: HexColor = rand::random();
//! ```
//!
//! To specify whether an RGB or RGBA color is randomly created, use
//! [`HexColor::random_rgb`] or [`HexColor::random_rgba`] respectively:
//!
//! ```
//! use hex_color::HexColor;
//!
//! let random_rgb = HexColor::random_rgb();
//! let random_rgba = HexColor::random_rgba();
//! ```
//!
//! ## With [`serde`](::serde)
//!
//! Use [`serde`](::serde) to serialize and deserialize colors in multiple
//! formats: [`u24`], [`mod@u32`], [`rgb`], or [`rgba`]:
//!
//! ```
//! use hex_color::HexColor;
//! use serde::{Deserialize, Serialize};
//! use serde_json::json;
//!
//! #[derive(Debug, PartialEq, Deserialize, Serialize)]
//! struct Color {
//!     name: String,
//!     value: HexColor,
//! }
//!
//! # fn main() -> serde_json::Result<()> {
//! let json_input = json!({
//!     "name": "Light Coral",
//!     "value": "#F08080",
//! });
//! assert_eq!(
//!     serde_json::from_value::<Color>(json_input)?,
//!     Color {
//!         name: String::from("Light Coral"),
//!         value: HexColor::rgb(240, 128, 128),
//!     },
//! );
//!
//! let my_color = Color {
//!     name: String::from("Dark Salmon"),
//!     value: HexColor::rgb(233, 150, 122),
//! };
//! assert_eq!(
//!     serde_json::to_value(my_color)?,
//!     json!({
//!         "name": "Dark Salmon",
//!         "value": "#E9967A",
//!     }),
//! );
//!
//! #[derive(Debug, PartialEq, Deserialize, Serialize)]
//! struct NumericColor {
//!     name: String,
//!     #[serde(with = "hex_color::u24")]
//!     value: HexColor,
//! }
//!
//! let json_input = json!({
//!     "name": "Light Coral",
//!     "value": 0x00F0_8080_u32,
//! });
//! assert_eq!(
//!     serde_json::from_value::<NumericColor>(json_input)?,
//!     NumericColor {
//!         name: String::from("Light Coral"),
//!         value: HexColor::rgb(240, 128, 128),
//!     },
//! );
//!
//! let my_color = NumericColor {
//!     name: String::from("Dark Salmon"),
//!     value: HexColor::rgb(233, 150, 122),
//! };
//! assert_eq!(
//!     serde_json::to_value(my_color)?,
//!     json!({
//!         "name": "Dark Salmon",
//!         "value": 0x00E9_967A_u32,
//!     }),
//! );
//! # Ok(())
//! # }
//! ```
//!
//! # Features
//!
//! * `rand` enables out-of-the-box compatability with the [`rand`](::rand)
//!   crate.
//! * `serde` enables serialization and deserialization with the
//!   [`serde`](::serde) crate.
//! * `std` enables [`std::error::Error`] on [`ParseHexColorError`]. Otherwise,
//!   it's needed with `rand` for [`HexColor::random_rgb`],
//!   [`HexColor::random_rgba`], and, of course,
//!   [`rand::random`](::rand::random).
//!
//! *Note*: Only the `std` feature is enabled by default.

#![cfg_attr(not(feature = "std"), no_std)]
// hex_color types in rustdoc of other crates get linked to here
#![doc(html_root_url = "https://docs.rs/hex_color/3.0.0")]
#![cfg_attr(doc_cfg, feature(doc_cfg))]
#![deny(missing_docs)]
#![deny(clippy::pedantic)]
// This is a necessary evil for "r", "g", "b", "a", and more:
#![allow(clippy::many_single_char_names, clippy::similar_names)]

#[cfg(feature = "rand")]
mod rand;
#[cfg(feature = "serde")]
mod serde;

use core::fmt;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign};
use core::str::{Bytes, FromStr};

#[cfg(feature = "serde")]
#[doc(inline)]
pub use self::serde::{rgb, rgba, u24, u32};

/// An RGBA color.
///
/// See the [module documentation](self) for details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct HexColor {
    /// The red component of the color.
    pub r: u8,
    /// The green component of the color.
    pub g: u8,
    /// The blue component of the color.
    pub b: u8,
    /// The alpha component of the color (`0` is transparent, `255` is opaque).
    pub a: u8,
}

impl HexColor {
    /// Solid black. RGBA is `(0, 0, 0, 255)`.
    pub const BLACK: HexColor = HexColor::rgb(0, 0, 0);
    /// Solid blue. RGBA is `(0, 0, 255, 255)`.
    pub const BLUE: HexColor = HexColor::rgb(0, 0, 255);
    /// Completely transparent. RGBA is `(0, 0, 0, 0)`.
    pub const CLEAR: HexColor = HexColor::rgba(0, 0, 0, 0);
    /// Solid cyan. RGBA is `(0, 255, 255, 255)`.
    pub const CYAN: HexColor = HexColor::rgb(0, 255, 255);
    /// Solid gray; American spelling of grey. RGBA is `(128, 128, 128, 255)`.
    pub const GRAY: HexColor = HexColor::achromatic(128);
    /// Solid green. RGBA is `(0, 0, 255, 255)`.
    pub const GREEN: HexColor = HexColor::rgb(0, 255, 0);
    /// Solid grey; British spelling of gray. RGBA is `(128, 128, 128, 255)`.
    pub const GREY: HexColor = HexColor::achromatic(128);
    /// Solid magenta. RGBA is `(255, 0, 255, 255)`.
    pub const MAGENTA: HexColor = HexColor::rgb(255, 0, 255);
    /// The maximum possible value. RGBA is `(255, 255, 255, 255)`.
    pub const MAX: HexColor = HexColor::from_u32(0xFFFF_FFFF);
    ////////////////////////////////////////////////////////////////////////////
    // Basic colors
    ////////////////////////////////////////////////////////////////////////////

    /// The minimum possible value. RGBA is `(0, 0, 0, 0)`.
    pub const MIN: HexColor = HexColor::from_u32(0x0000_0000);
    /// Solid red. RGBA is `(255, 0, 0, 255)`.
    pub const RED: HexColor = HexColor::rgb(255, 0, 0);
    /// Solid white. RGBA is `(255, 255, 255, 255)`.
    pub const WHITE: HexColor = HexColor::rgb(255, 255, 255);
    /// Solid yellow. RGBA is `(255, 255, 0, 255)`.
    pub const YELLOW: HexColor = HexColor::rgb(255, 255, 0);

    ////////////////////////////////////////////////////////////////////////////
    // Constructors
    ////////////////////////////////////////////////////////////////////////////

    /// Constructs a new RGBA value.
    ///
    /// For creating just an RGB value instead, use [`HexColor::rgb`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let red = HexColor::rgba(255, 0, 0, 255);
    /// let translucent_red = HexColor::rgba(255, 0, 0, 128);
    /// ```
    #[must_use]
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> HexColor {
        HexColor { r, g, b, a }
    }

    /// Constructs a new RGB value. (The alpha channel defaults to [`u8::MAX`].)
    ///
    /// For creating an RGBA value instead, use [`HexColor::rgba`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let aqua = HexColor::rgb(0, 255, 255);
    /// ```
    #[must_use]
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> HexColor {
        HexColor { r, g, b, a: 255 }
    }

    /// Constructs a new achromatic RGB value. (The alpha channel defaults to
    /// [`u8::MAX`].)
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::achromatic(128), HexColor::rgb(128, 128, 128));
    /// ```
    ///
    /// *Note*: There is no "`achromatic_alpha`" constructor or similar method.
    /// Instead, it's advised to chain [`HexColor::achromatic`] with
    /// [`HexColor::with_a`]:
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let transparent_dark_gray = HexColor::achromatic(64).with_a(128);
    /// assert_eq!(transparent_dark_gray, HexColor::rgba(64, 64, 64, 128));
    /// ```
    #[must_use]
    #[inline]
    pub const fn achromatic(v: u8) -> HexColor {
        HexColor::rgb(v, v, v)
    }

    ////////////////////////////////////////////////////////////////////////////
    // Random color generation
    ////////////////////////////////////////////////////////////////////////////

    /// Constructs a new random RGB value through the [`rand`](::rand) crate.
    ///
    /// To generate a random RGBA value, use [`HexColor::random_rgba`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// println!("{}", HexColor::random_rgb().display_rgb());
    /// ```
    #[cfg(all(feature = "rand", feature = "std"))]
    #[cfg_attr(doc_cfg, doc(cfg(all(feature = "rand", feature = "std"))))]
    #[must_use]
    #[inline]
    pub fn random_rgb() -> Self {
        ::rand::random()
    }

    /// Constructs a new random RGBA value through the [`rand`](::rand) crate.
    ///
    /// To generate a random RGB value, use [`HexColor::random_rgb`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// println!("{}", HexColor::random_rgba().display_rgba());
    /// ```
    #[cfg(all(feature = "rand", feature = "std"))]
    #[cfg_attr(doc_cfg, doc(cfg(all(feature = "rand", feature = "std"))))]
    #[must_use]
    #[inline]
    pub fn random_rgba() -> Self {
        HexColor::random_rgb().with_a(::rand::random())
    }

    ////////////////////////////////////////////////////////////////////////////
    // Parsing
    ////////////////////////////////////////////////////////////////////////////

    #[must_use]
    unsafe fn parse_shorthand(bytes: &mut Bytes, has_alpha: bool) -> Option<HexColor> {
        unsafe fn parse_single_hex_value(bytes: &mut Bytes) -> Option<u8> {
            match bytes.next().unwrap_unchecked() {
                b'0' => Some(0x00),
                b'1' => Some(0x11),
                b'2' => Some(0x22),
                b'3' => Some(0x33),
                b'4' => Some(0x44),
                b'5' => Some(0x55),
                b'6' => Some(0x66),
                b'7' => Some(0x77),
                b'8' => Some(0x88),
                b'9' => Some(0x99),
                b'a' | b'A' => Some(0xAA),
                b'b' | b'B' => Some(0xBB),
                b'c' | b'C' => Some(0xCC),
                b'd' | b'D' => Some(0xDD),
                b'e' | b'E' => Some(0xEE),
                b'f' | b'F' => Some(0xFF),
                _ => None,
            }
        }

        let r = parse_single_hex_value(bytes)?;
        let g = parse_single_hex_value(bytes)?;
        let b = parse_single_hex_value(bytes)?;
        let a = if has_alpha {
            parse_single_hex_value(bytes)?
        } else {
            u8::MAX
        };
        Some(HexColor::rgba(r, g, b, a))
    }

    #[must_use]
    unsafe fn parse_full(bytes: &mut Bytes, has_alpha: bool) -> Option<HexColor> {
        const HEX_RADIX: u32 = 16;

        unsafe fn parse_double_hex_value(bytes: &mut Bytes) -> Option<u8> {
            let buf = [
                bytes.next().unwrap_unchecked(),
                bytes.next().unwrap_unchecked(),
            ];
            let s = core::str::from_utf8_unchecked(&buf);
            u8::from_str_radix(s, HEX_RADIX).ok()
        }

        let r = parse_double_hex_value(bytes)?;
        let g = parse_double_hex_value(bytes)?;
        let b = parse_double_hex_value(bytes)?;
        let a = if has_alpha {
            parse_double_hex_value(bytes)?
        } else {
            u8::MAX
        };
        Some(HexColor::rgba(r, g, b, a))
    }

    fn parse_internals(s: &str, mode: ParseMode) -> Result<HexColor, ParseHexColorError> {
        macro_rules! err {
            ($variant:ident) => {{
                return Err(ParseHexColorError::$variant);
            }};
        }

        let mut bytes = s.bytes();
        match bytes.next() {
            Some(b'#') => {}
            Some(_) => err!(InvalidFormat),
            None => err!(Empty),
        }
        let has_alpha = matches!(s.len(), 5 | 9);
        let opt = match (s.len(), mode) {
            (4, ParseMode::Rgb | ParseMode::Any) | (5, ParseMode::Rgba | ParseMode::Any) => {
                // SAFETY: `bytes` will have either `3` or `4` bytes left and
                // `has_alpha` is synchronized.
                unsafe { HexColor::parse_shorthand(&mut bytes, has_alpha) }
            }
            (7, ParseMode::Rgb | ParseMode::Any) | (9, ParseMode::Rgba | ParseMode::Any) => {
                // SAFETY: `bytes` will have either `6` or `8` bytes left and
                // `has_alpha` is synchronized.
                unsafe { HexColor::parse_full(&mut bytes, has_alpha) }
            }
            _ => err!(InvalidFormat),
        };
        opt.ok_or(ParseHexColorError::InvalidDigit)
    }

    /// Parses an RGB(A) hex code.
    ///
    /// **All parsing is case-insensitive**. There are currently four parseable
    /// formats:
    ///
    /// * `#RGB`
    /// * `#RRGGBB`
    /// * `#RGBA`
    /// * `#RRGGBBAA`
    ///
    /// To parse *only* a hexadecimal triplet, use [`parse_rgb`]. Otherwise,
    /// to parse *only* a hexadecimal quadruplet, use [`parse_rgba`].
    ///
    /// [`parse_rgb`]: HexColor::parse_rgb
    /// [`parse_rgba`]: HexColor::parse_rgba
    ///
    /// # Errors
    ///
    /// - [`Empty`] when the input is empty.
    /// - [`InvalidFormat`] when the input is a malformed length or lacks a
    ///   leading `#`. If you suspect there might be whitespace in the input,
    ///   consider calling [`str::trim`] first.
    /// - [`InvalidDigit`] when the format seems correct but one of the
    ///   characters is an invalid hexadecimal digit.
    ///
    /// [`Empty`]: ParseHexColorError::Empty
    /// [`InvalidFormat`]: ParseHexColorError::InvalidFormat
    /// [`InvalidDigit`]: ParseHexColorError::InvalidDigit
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// # fn main() -> Result<(), hex_color::ParseHexColorError> {
    /// let red = HexColor::parse("#F00")?;
    /// assert_eq!(red, HexColor::rgb(0xFF, 0x00, 0x00));
    ///
    /// let plum = HexColor::parse("#DDA0DD")?;
    /// assert_eq!(plum, HexColor::rgb(0xDD, 0xA0, 0xDD));
    ///
    /// let opaque_cyan = HexColor::parse("#0FFF")?;
    /// assert_eq!(opaque_cyan, HexColor::rgba(0x00, 0xFF, 0xFF, 0xFF));
    ///
    /// let translucent_azure = HexColor::parse("#F0FFFF80")?;
    /// assert_eq!(translucent_azure, HexColor::rgba(0xF0, 0xFF, 0xFF, 0x80));
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn parse(s: &str) -> Result<HexColor, ParseHexColorError> {
        HexColor::parse_internals(s, ParseMode::Any)
    }

    /// Parses an RGB hex code.
    ///
    /// **All parsing is case-insensitive**. There are currently two parseable
    /// formats:
    ///
    /// * `#RGB`
    /// * `#RRGGBB`
    ///
    /// To parse *only* a hexadecimal quadruplet, use [`parse_rgba`]. Otherwise,
    /// to parse *both* hexadecimal triplets and quadruplets, use [`parse`].
    ///
    /// [`parse_rgba`]: HexColor::parse_rgba
    /// [`parse`]: HexColor::parse
    ///
    /// # Errors
    ///
    /// - [`Empty`] when the input is empty.
    /// - [`InvalidFormat`] when the input is a malformed length or lacks a
    ///   leading `#`. If you suspect there might be whitespace in the input,
    ///   consider calling [`str::trim`] first.
    /// - [`InvalidDigit`] when the format seems correct but one of the
    ///   characters is an invalid hexadecimal digit.
    ///
    /// *Note*: a valid RGBA input will return an [`InvalidFormat`]. Use
    /// [`parse_rgba`] or [`parse`] instead if that behavior is not desired.
    ///
    /// [`Empty`]: ParseHexColorError::Empty
    /// [`InvalidFormat`]: ParseHexColorError::InvalidFormat
    /// [`InvalidDigit`]: ParseHexColorError::InvalidDigit
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// # fn main() -> Result<(), hex_color::ParseHexColorError> {
    /// let yellow = HexColor::parse_rgb("#FF0")?;
    /// assert_eq!(yellow, HexColor::rgb(0xFF, 0xFF, 0x00));
    ///
    /// let hot_pink = HexColor::parse_rgb("#FF69B4")?;
    /// assert_eq!(hot_pink, HexColor::rgb(0xFF, 0x69, 0xB4));
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn parse_rgb(s: &str) -> Result<HexColor, ParseHexColorError> {
        HexColor::parse_internals(s, ParseMode::Rgb)
    }

    /// Parses an RGBA hex code.
    ///
    /// **All parsing is case-insensitive**. There are currently two parseable
    /// formats:
    ///
    /// * `#RGBA`
    /// * `#RRGGBBAA`
    ///
    /// To parse *only* a hexadecimal triplet, use [`parse_rgb`]. Otherwise,
    /// to parse *both* hexadecimal triplets and quadruplets, use [`parse`].
    ///
    /// [`parse_rgb`]: HexColor::parse_rgb
    /// [`parse`]: HexColor::parse
    ///
    /// # Errors
    ///
    /// - [`Empty`] when the input is empty.
    /// - [`InvalidFormat`] when the input is a malformed length or lacks a
    ///   leading `#`. If you suspect there might be whitespace in the input,
    ///   consider calling [`str::trim`] first.
    /// - [`InvalidDigit`] when the format seems correct but one of the
    ///   characters is an invalid hexadecimal digit.
    ///
    /// **Note**: a valid RGB input (without an alpha value) will return an
    /// [`InvalidFormat`]. Use [`parse_rgb`] or [`parse`] instead if that
    /// behavior is not desired.
    ///
    /// [`Empty`]: ParseHexColorError::Empty
    /// [`InvalidFormat`]: ParseHexColorError::InvalidFormat
    /// [`InvalidDigit`]: ParseHexColorError::InvalidDigit
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// # fn main() -> Result<(), hex_color::ParseHexColorError> {
    /// let transparent = HexColor::parse_rgba("#FFFF")?;
    /// assert_eq!(transparent, HexColor::rgba(0xFF, 0xFF, 0xFF, 0xFF));
    ///
    /// let translucent_gold = HexColor::parse_rgba("#FFD70080")?;
    /// assert_eq!(translucent_gold, HexColor::rgba(0xFF, 0xD7, 0x00, 0x80));
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn parse_rgba(s: &str) -> Result<HexColor, ParseHexColorError> {
        HexColor::parse_internals(s, ParseMode::Rgba)
    }

    ////////////////////////////////////////////////////////////////////////////
    // Other Conversions
    ////////////////////////////////////////////////////////////////////////////

    /// Converts any `u32` in the range `0x0000_0000..=0x00FF_FFFF` to an RGB
    /// value.
    ///
    /// To convert any `u32` to an RGBA value, use [`from_u32`] instead.
    ///
    /// [`from_u32`]: HexColor::from_u32
    ///
    /// # Panics
    ///
    /// Panics if `debug_assertions` are enabled and the given value exceeds
    /// `0x00FF_FFFF`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let pale_green = HexColor::from_u24(0x98FB98);
    /// assert_eq!(pale_green, HexColor::rgb(0x98, 0xFB, 0x98));
    /// ```
    #[must_use]
    #[inline]
    #[track_caller]
    pub const fn from_u24(n: u32) -> HexColor {
        debug_assert!(n <= 0x00FF_FFFF);

        let [_, r, g, b] = n.to_be_bytes();
        HexColor::rgb(r, g, b)
    }

    /// Converts any `u32` to an RGBA value.
    ///
    /// For converting a `u32` in the range of `0x0000_0000..=0x00FF_FFFF` to
    /// an RGB value, use [`from_u24`] instead.
    ///
    /// [`from_u24`]: HexColor::from_u24
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let navajo_white = HexColor::from_u32(0xFFDEADFF);
    /// assert_eq!(navajo_white, HexColor::rgba(0xFF, 0xDE, 0xAD, 0xFF));
    ///
    /// let translucent_violet = HexColor::from_u32(0xEE82EE80);
    /// assert_eq!(translucent_violet, HexColor::rgba(0xEE, 0x82, 0xEE, 0x80));
    /// ```
    #[must_use]
    #[inline]
    pub const fn from_u32(v: u32) -> HexColor {
        let [r, g, b, a] = v.to_be_bytes();
        HexColor::rgba(r, g, b, a)
    }

    /// Converts a `HexColor` into a `u32` in the range `0x000000..=0xFFFFFF`,
    /// discarding any possibly significant alpha value.
    ///
    /// To convert this `HexColor` into a `u32` containing the alpha value as
    /// well, use [`to_u32`].
    ///
    /// [`to_u32`]: HexColor::to_u32
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let misty_rose = HexColor::rgb(0xFF, 0xE4, 0xE1);
    /// assert_eq!(misty_rose.to_u24(), 0xFFE4E1);
    ///
    /// // Again, note that the alpha value is lost in this conversion:
    /// let translucent_navy = HexColor::rgba(0x00, 0x00, 0x80, 0x80);
    /// assert_eq!(translucent_navy.to_u24(), 0x000080);
    /// ```
    #[inline]
    #[must_use]
    pub const fn to_u24(self) -> u32 {
        let (r, g, b) = self.split_rgb();
        u32::from_be_bytes([0x00, r, g, b])
    }

    /// Converts a `HexColor` into a `u32`.
    ///
    /// To convert the `HexColor` into a `u32` with only the red, green, and
    /// blue channels packed, use [`to_u24`].
    ///
    /// [`to_u24`]: HexColor::to_u24
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let sea_shell = HexColor::rgb(0xFF, 0xF5, 0xEE);
    /// assert_eq!(sea_shell.to_u32(), 0xFFF5EEFF);
    ///
    /// // Unlike `to_u24` the alpha value is preserved
    /// let translucent_navy = HexColor::rgba(0x00, 0x00, 0x80, 0x80);
    /// assert_eq!(translucent_navy.to_u32(), 0x00008080);
    /// ```
    #[must_use]
    #[inline]
    pub const fn to_u32(self) -> u32 {
        let (r, g, b, a) = self.split_rgba();
        u32::from_be_bytes([r, g, b, a])
    }

    /// Converts a `HexColor` into `[r, g, b, a]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let color = HexColor::from_u32(0x89AB_CDEF);
    /// assert_eq!(color.to_be_bytes(), [0x89, 0xAB, 0xCD, 0xEF]);
    /// ```
    #[must_use]
    #[inline]
    pub const fn to_be_bytes(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// Converts a `HexColor` into `[a, b, g, r]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let color = HexColor::from_u32(0x89AB_CDEF);
    /// assert_eq!(color.to_le_bytes(), [0xEF, 0xCD, 0xAB, 0x89]);
    /// ```
    #[must_use]
    #[inline]
    pub const fn to_le_bytes(self) -> [u8; 4] {
        [self.a, self.b, self.g, self.r]
    }

    ////////////////////////////////////////////////////////////////////////////
    // Utility methods
    ////////////////////////////////////////////////////////////////////////////

    /// Constructs a new `HexColor` derived from `self` with the red component
    /// of `r`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::MIN.with_r(255), HexColor::rgba(255, 0, 0, 0));
    /// ```
    #[must_use]
    #[inline]
    pub const fn with_r(self, r: u8) -> HexColor {
        let (_, g, b, a) = self.split_rgba();
        HexColor::rgba(r, g, b, a)
    }

    /// Constructs a new `HexColor` derived from `self` with the green component
    /// of `g`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::MIN.with_g(255), HexColor::rgba(0, 255, 0, 0));
    /// ```
    #[must_use]
    #[inline]
    pub const fn with_g(self, g: u8) -> HexColor {
        let (r, _, b, a) = self.split_rgba();
        HexColor::rgba(r, g, b, a)
    }

    /// Constructs a new `HexColor` derived from `self` with the blue component
    /// of `b`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::MIN.with_b(255), HexColor::rgba(0, 0, 255, 0));
    /// ```
    #[must_use]
    #[inline]
    pub const fn with_b(self, b: u8) -> HexColor {
        let (r, g, _, a) = self.split_rgba();
        HexColor::rgba(r, g, b, a)
    }

    /// Constructs a new `HexColor` derived from `self` with the alpha component
    /// of `a`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::MIN.with_a(255), HexColor::rgba(0, 0, 0, 255));
    /// ```
    #[must_use]
    #[inline]
    pub const fn with_a(self, a: u8) -> HexColor {
        let (r, g, b) = self.split_rgb();
        HexColor::rgba(r, g, b, a)
    }

    /// Deconstructs a `HexColor` into a tuple of its components: `(r, g, b,
    /// a)`.
    ///
    /// This primarily helps in cleaner deconstruction of `HexColor` instances,
    /// especially if the variable bindings aren't the same as the `struct`'s
    /// fields.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let slate_blue = HexColor::from_u24(0x6A5ACD);
    /// let (red, green, blue, alpha) = slate_blue.split_rgba();
    /// ```
    ///
    /// For contrast, here's what it would look like otherwise; it's not
    /// terrible, but en masse, it's subjectively annoying:
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let slate_blue = HexColor::from_u24(0x6A5ACD);
    /// let HexColor {
    ///     r: red,
    ///     g: green,
    ///     b: blue,
    ///     a: alpha,
    /// } = slate_blue;
    /// ```
    #[must_use]
    #[inline]
    pub const fn split_rgba(self) -> (u8, u8, u8, u8) {
        let HexColor { r, g, b, a } = self;
        (r, g, b, a)
    }

    /// Deconstructs a `HexColor` into a tuple of its components: `(r, g, b)`.
    ///
    /// This primarily helps in cleaner deconstruction of `HexColor` instances,
    /// especially if the variable bindings aren't the same as the `struct`'s
    /// fields.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let powder_blue = HexColor::from_u24(0xB6D0E2);
    /// let (red, green, blue) = powder_blue.split_rgb();
    /// ```
    ///
    /// For contrast, here's what it would look like otherwise; it's not
    /// terrible, but en masse, it's subjectively annoying:
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let powder_blue = HexColor::from_u24(0xB6D0E2);
    /// let HexColor {
    ///     r: red,
    ///     g: green,
    ///     b: blue,
    ///     ..
    /// } = powder_blue;
    /// ```
    #[must_use]
    #[inline]
    pub const fn split_rgb(self) -> (u8, u8, u8) {
        let HexColor { r, g, b, .. } = self;
        (r, g, b)
    }

    ////////////////////////////////////////////////////////////////////////////
    // Display
    ////////////////////////////////////////////////////////////////////////////

    /// Returns an object that implements [`fmt::Display`] for `HexColor`. By
    /// default, the alpha channel is hidden and the letters are uppercase.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let display_red = HexColor::RED.display_rgb();
    /// assert_eq!(format!("{display_red}"), "#FF0000");
    /// ```
    ///
    /// For more control over the display, see [`Display::with_alpha`] and
    /// [`Display::with_case`].
    #[must_use]
    #[inline]
    pub const fn display_rgb(self) -> Display {
        Display::new(self).with_alpha(Alpha::Hidden)
    }

    /// Returns an object that implements [`fmt::Display`] for `HexColor`. By
    /// default, the alpha channel is visible and the letters are uppercase.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let display_red = HexColor::RED.display_rgba();
    /// assert_eq!(format!("{display_red}"), "#FF0000FF");
    /// ```
    ///
    /// For more control over the display, see [`Display::with_alpha`] and
    /// [`Display::with_case`].
    #[must_use]
    #[inline]
    pub const fn display_rgba(self) -> Display {
        Display::new(self).with_alpha(Alpha::Visible)
    }

    ////////////////////////////////////////////////////////////////////////////
    // Arithmetic operations
    ////////////////////////////////////////////////////////////////////////////

    /// Adds two colors together.
    ///
    /// Each component is added separately. The alpha component is ignored
    /// entirely, always returning an RGB color (where alpha is the default
    /// [`u8::MAX`]).
    ///
    /// # Panics
    ///
    /// Panics if any overflow occurs.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::BLUE.add(HexColor::GREEN), HexColor::CYAN);
    /// assert_eq!(HexColor::RED.add(HexColor::BLUE), HexColor::MAGENTA);
    /// assert_eq!(HexColor::GREEN.add(HexColor::RED), HexColor::YELLOW);
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn add(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(r1 + r2, g1 + g2, b1 + b2)
    }

    /// Checked color addition. Computes `self + rhs`, returning [`None`] if
    /// overflow occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_white = HexColor::achromatic(254);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_white.checked_add(one), Some(HexColor::WHITE));
    /// assert_eq!(HexColor::WHITE.checked_add(one), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn checked_add(self, rhs: HexColor) -> Option<HexColor> {
        let (res, flag) = self.overflowing_add(rhs);
        // TODO: Use `unlikely!` or some equivalent hint when stable.
        if flag {
            None
        } else {
            Some(res)
        }
    }

    /// Calculates `self + rhs`.
    ///
    /// Returns a tuple of the addition along with a boolean indicating whether
    /// any arithmetic overflow would occur. If an overflow would have occurred,
    /// then the wrapped value is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_white = HexColor::achromatic(254);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_white.overflowing_add(one), (HexColor::WHITE, false),);
    /// assert_eq!(
    ///     HexColor::WHITE.overflowing_add(one),
    ///     (HexColor::BLACK, true),
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn overflowing_add(self, rhs: HexColor) -> (HexColor, bool) {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();

        let (r, r_flag) = r1.overflowing_add(r2);
        let (g, g_flag) = g1.overflowing_add(g2);
        let (b, b_flag) = b1.overflowing_add(b2);

        (HexColor::rgb(r, g, b), r_flag || g_flag || b_flag)
    }

    /// Saturating color addition. Computes `self + rhs`, saturating at the
    /// numeric bounds instead of overflowing.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// // Even though the green component should exceed 255, it saturates at
    /// // 255 instead:
    /// assert_eq!(
    ///     HexColor::YELLOW.saturating_add(HexColor::CYAN),
    ///     HexColor::WHITE,
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn saturating_add(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.saturating_add(r2),
            g1.saturating_add(g2),
            b1.saturating_add(b2),
        )
    }

    /// Wrapping (modular) addition. Computes `self + rhs`, wrapping around the
    /// boundary of [`u8`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_white = HexColor::achromatic(254);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_white.wrapping_add(one), HexColor::WHITE);
    /// assert_eq!(HexColor::WHITE.wrapping_add(one), HexColor::BLACK);
    /// ```
    #[inline]
    #[must_use]
    pub const fn wrapping_add(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.wrapping_add(r2),
            g1.wrapping_add(g2),
            b1.wrapping_add(b2),
        )
    }

    /// Subtracts one color from another.
    ///
    /// Each component is subtracted separately. The alpha component is ignored
    /// entirely, always returning an RGB color (where alpha is the default
    /// [`u8::MAX`]).
    ///
    /// # Panics
    ///
    /// Panics if any overflow occurs.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::MAGENTA.sub(HexColor::BLUE), HexColor::RED);
    /// assert_eq!(HexColor::YELLOW.sub(HexColor::RED), HexColor::GREEN);
    /// assert_eq!(HexColor::CYAN.sub(HexColor::GREEN), HexColor::BLUE);
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn sub(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(r1 - r2, g1 - g2, b1 - b2)
    }

    /// Subtracts `n` from the `HexColor`'s red, green, and blue components.
    ///
    /// # Panics
    ///
    /// Panics if overflow occurs.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::WHITE.sub_scalar(255), HexColor::BLACK);
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn sub_scalar(self, n: u8) -> HexColor {
        self.sub(HexColor::achromatic(n))
    }

    /// Checked color subtraction. Computes `self - rhs`, returning [`None`] if
    /// overflow occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_black = HexColor::achromatic(1);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_black.checked_sub(one), Some(HexColor::BLACK));
    /// assert_eq!(HexColor::BLACK.checked_sub(one), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, rhs: HexColor) -> Option<HexColor> {
        let (res, flag) = self.overflowing_sub(rhs);
        // TODO: Use `unlikely!` or some equivalent hint when stable.
        if flag {
            None
        } else {
            Some(res)
        }
    }

    /// Calculates `self - rhs`.
    ///
    /// Returns a tuple of the subtraction along with a boolean indicating
    /// whether any arithmetic overflow would occur. If an overflow would have
    /// occurred, then the wrapped value is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_black = HexColor::achromatic(1);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_black.overflowing_sub(one), (HexColor::BLACK, false),);
    /// assert_eq!(
    ///     HexColor::BLACK.overflowing_sub(one),
    ///     (HexColor::WHITE, true),
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn overflowing_sub(self, rhs: HexColor) -> (HexColor, bool) {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();

        let (r, r_flag) = r1.overflowing_sub(r2);
        let (g, g_flag) = g1.overflowing_sub(g2);
        let (b, b_flag) = b1.overflowing_sub(b2);

        (HexColor::rgb(r, g, b), r_flag || g_flag || b_flag)
    }

    /// Saturating color subtraction. Computes `self - rhs`, saturating at the
    /// numeric bounds instead of overflowing.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// // Even though the red component should overflow, it saturates at 0
    /// // instead:
    /// assert_eq!(
    ///     HexColor::CYAN.saturating_sub(HexColor::YELLOW),
    ///     HexColor::BLUE,
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn saturating_sub(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.saturating_sub(r2),
            g1.saturating_sub(g2),
            b1.saturating_sub(b2),
        )
    }

    /// Wrapping (modular) subtraction. Computes `self - rhs`, wrapping around
    /// the boundary of [`u8`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let almost_black = HexColor::achromatic(1);
    /// let one = HexColor::achromatic(1);
    ///
    /// assert_eq!(almost_black.wrapping_sub(one), HexColor::BLACK);
    /// assert_eq!(HexColor::BLACK.wrapping_sub(one), HexColor::WHITE);
    /// ```
    #[inline]
    #[must_use]
    pub const fn wrapping_sub(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.wrapping_sub(r2),
            g1.wrapping_sub(g2),
            b1.wrapping_sub(b2),
        )
    }

    /// Multiplies two colors together.
    ///
    /// Each component is multiplied separately. The alpha component is ignored
    /// entirely, always returning an RGB color (where alpha is the default
    /// [`u8::MAX`]).
    ///
    /// # Panics
    ///
    /// Panics if any overflow occurs.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let a = HexColor::rgb(1, 2, 3);
    /// let b = HexColor::rgb(4, 5, 6);
    ///
    /// assert_eq!(a.mul(b), HexColor::rgb(4, 10, 18));
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn mul(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(r1 * r2, g1 * g2, b1 * b2)
    }

    /// Checked color multiplication. Computes `self * rhs`, returning [`None`]
    /// if overflow occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(
    ///     HexColor::achromatic(5).checked_mul(HexColor::achromatic(2)),
    ///     Some(HexColor::achromatic(10)),
    /// );
    /// assert_eq!(HexColor::MAX.checked_mul(HexColor::achromatic(2)), None,);
    /// ```
    #[inline]
    #[must_use]
    pub const fn checked_mul(self, rhs: HexColor) -> Option<HexColor> {
        let (res, flag) = self.overflowing_mul(rhs);
        // TODO: Use `unlikely!` or some equivalent hint when stable.
        if flag {
            None
        } else {
            Some(res)
        }
    }

    /// Calculates `self * rhs`.
    ///
    /// Returns a tuple of the multiplication along with a boolean indicating
    /// whether any arithmetic overflow would occur. If an overflow would have
    /// occurred, then the wrapped value is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(
    ///     HexColor::achromatic(5).overflowing_mul(HexColor::achromatic(2)),
    ///     (HexColor::achromatic(10), false),
    /// );
    /// assert_eq!(
    ///     HexColor::achromatic(200).overflowing_mul(HexColor::achromatic(2)),
    ///     (HexColor::achromatic(144), true),
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn overflowing_mul(self, rhs: HexColor) -> (HexColor, bool) {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();

        let (r, r_flag) = r1.overflowing_mul(r2);
        let (g, g_flag) = g1.overflowing_mul(g2);
        let (b, b_flag) = b1.overflowing_mul(b2);

        (HexColor::rgb(r, g, b), r_flag || g_flag || b_flag)
    }

    /// Saturating color multiplication. Computes `self * rhs`, saturating at
    /// the numeric bounds instead of overflowing.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(
    ///     HexColor::achromatic(5).saturating_mul(HexColor::achromatic(2)),
    ///     HexColor::achromatic(10),
    /// );
    /// assert_eq!(
    ///     HexColor::achromatic(200).saturating_mul(HexColor::achromatic(2)),
    ///     HexColor::achromatic(255),
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn saturating_mul(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.saturating_mul(r2),
            g1.saturating_mul(g2),
            b1.saturating_mul(b2),
        )
    }

    /// Wrapping (modular) multiplication. Computes `self * rhs`, wrapping
    /// around at the boundary of the type.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(
    ///     HexColor::achromatic(5).wrapping_mul(HexColor::achromatic(2)),
    ///     HexColor::achromatic(10),
    /// );
    /// assert_eq!(
    ///     HexColor::achromatic(200).wrapping_mul(HexColor::achromatic(2)),
    ///     HexColor::achromatic(144),
    /// );
    /// ```
    #[inline]
    #[must_use]
    pub const fn wrapping_mul(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(
            r1.wrapping_mul(r2),
            g1.wrapping_mul(g2),
            b1.wrapping_mul(b2),
        )
    }

    /// Divides one color with another.
    ///
    /// Each component is divided separately. The alpha component is ignored
    /// entirely, always returning an RGB color (where alpha is the default
    /// [`u8::MAX`]).
    ///
    /// # Panics
    ///
    /// Panics if any component is divided by zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let a = HexColor::rgb(128, 64, 32);
    /// let b = HexColor::rgb(2, 4, 8);
    ///
    /// assert_eq!(a.div(b), HexColor::rgb(64, 16, 4));
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    pub const fn div(self, rhs: HexColor) -> HexColor {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        HexColor::rgb(r1 / r2, g1 / g2, b1 / b2)
    }

    /// Checked color division. Computes `self / rhs`, returning [`None`] if
    /// any component of `rhs` is zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(
    ///     HexColor::achromatic(128).checked_div(HexColor::achromatic(2)),
    ///     Some(HexColor::achromatic(64)),
    /// );
    /// assert_eq!(HexColor::WHITE.checked_div(HexColor::achromatic(0)), None,);
    /// ```
    #[inline]
    #[must_use]
    pub const fn checked_div(self, rhs: HexColor) -> Option<HexColor> {
        let (r1, g1, b1) = self.split_rgb();
        let (r2, g2, b2) = rhs.split_rgb();
        // TODO: Use `unlikely!` or some equivalent hint when stable.
        if r2 == 0 || g2 == 0 || b2 == 0 {
            None
        } else {
            Some(HexColor::rgb(r1 / r2, g1 / g2, b1 / b2))
        }
    }

    ////////////////////////////////////////////////////////////////////////////
    // "Complex" operations
    ////////////////////////////////////////////////////////////////////////////

    /// Scales the `r`, `g`, and `b` components of the [`HexColor`] by `f`.
    ///
    /// The alpha component is ignore entirely, but is preserved in the
    /// resulting color.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let amber = HexColor::from_u24(0xFFBF00);
    ///
    /// let lighter_amber = amber.scale(1.1); // 10% lighter (rounded)
    /// assert_eq!(lighter_amber, HexColor::from_u24(0xFFD200));
    ///
    /// let darker_amber = amber.scale(0.9); // 10% darker (rounded)
    /// assert_eq!(darker_amber, HexColor::from_u24(0xE6AC00));
    /// ```
    #[inline]
    #[must_use]
    #[track_caller]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn scale(self, f: f32) -> Self {
        let (r, g, b, a) = self.split_rgba();
        let r = (f32::from(r) * f).min(255.0).round() as u8;
        let g = (f32::from(g) * f).min(255.0).round() as u8;
        let b = (f32::from(b) * f).min(255.0).round() as u8;
        HexColor::rgba(r, g, b, a)
    }

    /// Linearly inverts the [`HexColor`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// assert_eq!(HexColor::RED.invert(), HexColor::CYAN);
    /// ```
    #[inline]
    #[must_use]
    pub const fn invert(self) -> HexColor {
        let (r, g, b, a) = self.split_rgba();
        HexColor::rgba(0xFF - r, 0xFF - g, 0xFF - b, a)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Arithmetic traits
////////////////////////////////////////////////////////////////////////////////

impl Add for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn add(self, rhs: Self) -> Self::Output {
        self.add(rhs)
    }
}

impl AddAssign for HexColor {
    #[inline]
    #[track_caller]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn sub(self, rhs: Self) -> Self::Output {
        self.sub(rhs)
    }
}

impl SubAssign for HexColor {
    #[inline]
    #[track_caller]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn mul(self, rhs: Self) -> Self::Output {
        self.mul(rhs)
    }
}

impl MulAssign for HexColor {
    #[inline]
    #[track_caller]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl Mul<f32> for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn mul(self, rhs: f32) -> Self::Output {
        self.scale(rhs)
    }
}

impl Mul<HexColor> for f32 {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn mul(self, rhs: HexColor) -> Self::Output {
        rhs.scale(self)
    }
}

impl MulAssign<f32> for HexColor {
    #[inline]
    #[track_caller]
    fn mul_assign(&mut self, rhs: f32) {
        *self = *self * rhs;
    }
}

impl Mul<f64> for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    #[allow(clippy::cast_possible_truncation)]
    fn mul(self, rhs: f64) -> Self::Output {
        self.scale(rhs as f32)
    }
}

impl Mul<HexColor> for f64 {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn mul(self, rhs: HexColor) -> Self::Output {
        rhs * self
    }
}

impl MulAssign<f64> for HexColor {
    #[inline]
    #[track_caller]
    fn mul_assign(&mut self, rhs: f64) {
        *self = *self * rhs;
    }
}

impl Div for HexColor {
    type Output = HexColor;

    #[inline]
    #[track_caller]
    fn div(self, rhs: Self) -> Self::Output {
        self.div(rhs)
    }
}

impl DivAssign for HexColor {
    #[inline]
    #[track_caller]
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}

////////////////////////////////////////////////////////////////////////////////
// Conversion traits
////////////////////////////////////////////////////////////////////////////////

impl From<(u8, u8, u8, u8)> for HexColor {
    /// Constructs a new `HexColor` from a tuple of `(r, g, b, a)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let brass = (0xE1, 0xC1, 0x6E, 0xFF);
    /// let color = HexColor::from(brass);
    ///
    /// assert_eq!(color.r, 0xE1);
    /// assert_eq!(color.g, 0xC1);
    /// assert_eq!(color.b, 0x6E);
    /// assert_eq!(color.a, 0xFF);
    /// ```
    #[inline]
    fn from((r, g, b, a): (u8, u8, u8, u8)) -> Self {
        HexColor::rgba(r, g, b, a)
    }
}

impl From<HexColor> for (u8, u8, u8, u8) {
    /// Deconstructs a `HexColor` into a tuple of its components: `(r, g, b,
    /// a)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let brass = HexColor::from_u24(0xE1C16E);
    /// let (red, green, blue, alpha) = brass.into();
    ///
    /// assert_eq!(red, 0xE1);
    /// assert_eq!(green, 0xC1);
    /// assert_eq!(blue, 0x6E);
    /// assert_eq!(alpha, 0xFF);
    fn from(hex_color: HexColor) -> Self {
        hex_color.split_rgba()
    }
}

impl From<(u8, u8, u8)> for HexColor {
    /// Constructs a new `HexColor` from a tuple of `(r, g, b)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let jade = (0x00, 0xA3, 0x6C);
    /// let color = HexColor::from(jade);
    ///
    /// assert_eq!(color.r, 0x00);
    /// assert_eq!(color.g, 0xA3);
    /// assert_eq!(color.b, 0x6C);
    /// ```
    #[inline]
    fn from((r, g, b): (u8, u8, u8)) -> Self {
        HexColor::rgb(r, g, b)
    }
}

impl From<HexColor> for (u8, u8, u8) {
    /// Deconstructs a `HexColor` into a tuple of its components: `(r, g, b)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let jade = HexColor::from_u24(0x00A36C);
    /// let (red, green, blue) = jade.into();
    ///
    /// assert_eq!(red, 0x00);
    /// assert_eq!(green, 0xA3);
    /// assert_eq!(blue, 0x6C);
    /// ```
    #[inline]
    fn from(hex_color: HexColor) -> Self {
        hex_color.split_rgb()
    }
}

impl From<u32> for HexColor {
    /// Constructs a new `HexColor` from a `u32` via [`HexColor::from_u32`].
    #[inline]
    fn from(n: u32) -> Self {
        HexColor::from_u32(n)
    }
}

impl From<HexColor> for u32 {
    /// Constructs a new `u32` from a `HexColor` via [`HexColor::to_u32`].
    #[inline]
    fn from(hex_color: HexColor) -> Self {
        hex_color.to_u32()
    }
}

////////////////////////////////////////////////////////////////////////////////
// Parsing Details
////////////////////////////////////////////////////////////////////////////////

impl FromStr for HexColor {
    type Err = ParseHexColorError;

    /// Semantically identical to [`HexColor::parse`]. For more information,
    /// refer to that function's documentation.
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HexColor::parse_internals(s, ParseMode::Any)
    }
}

#[derive(Debug, Copy, Clone)]
enum ParseMode {
    Any,
    Rgb,
    Rgba,
}

////////////////////////////////////////////////////////////////////////////////
// Display
////////////////////////////////////////////////////////////////////////////////

/// Helper struct for printing [`HexColor`] objects with [`format!`] and `{}`.
///
/// [`format!`]: https://doc.rust-lang.org/alloc/macro.format.html
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Display {
    color: HexColor,
    alpha: Alpha,
    case: Case,
}

impl Display {
    /// Constructs a new `Display` from a [`HexColor`]. By default, the alpha
    /// channel is hidden and the letters are uppercase.
    ///
    /// # Examples  
    ///
    /// ```
    /// use hex_color::{Display, HexColor};
    ///
    /// let gainsboro = HexColor::from_u24(0xDCDCDC);
    /// let display = Display::new(gainsboro);
    ///
    /// assert_eq!(format!("{display}"), "#DCDCDC");
    /// ```
    ///
    /// Consider using [`HexColor::display_rgb`] or [`HexColor::display_rgba`]
    /// if it's more convenient.
    #[must_use]
    #[inline]
    pub const fn new(color: HexColor) -> Self {
        Self {
            color,
            alpha: Alpha::Hidden,
            case: Case::Upper,
        }
    }

    /// Creates a new `Display` with the given [`Alpha`] display option.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::{Alpha, Display, HexColor};
    ///
    /// let transparent_gainsboro = HexColor::from_u32(0x7080907F);
    /// let display = Display::new(transparent_gainsboro).with_alpha(Alpha::Visible);
    ///
    /// assert_eq!(format!("{display}"), "#7080907F");
    /// ```
    #[must_use]
    #[inline]
    pub const fn with_alpha(mut self, alpha: Alpha) -> Self {
        self.alpha = alpha;
        self
    }

    /// Creates a new `Display` with the given [`Case`] display option.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::{HexColor, Display, Case};
    ///
    /// let gainsboro = HexColor::from_u24(0xDCDCDC);
    /// let display = Display::new(gainsboro).with_case(Case::Lower);
    ///
    /// assert_eq!(format!("{display}"), "#dcdcdc");
    #[must_use]
    #[inline]
    pub const fn with_case(mut self, case: Case) -> Self {
        self.case = case;
        self
    }

    /// Returns the [`HexColor`] that this `Display` was created from.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    ///
    /// let transparent_lavender_blush = HexColor::from_u32(0xFFF0F57F);
    /// let display = transparent_lavender_blush.display_rgb();
    ///
    /// assert_eq!(display.color(), transparent_lavender_blush);
    /// ```
    #[must_use]
    #[inline]
    pub const fn color(self) -> HexColor {
        self.color
    }
}

impl fmt::Display for Display {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (r, g, b) = self.color.split_rgb();

        match self.case {
            Case::Lower => write!(f, "#{r:02x}{g:02x}{b:02x}")?,
            Case::Upper => write!(f, "#{r:02X}{g:02X}{b:02X}")?,
        }

        if self.alpha == Alpha::Visible {
            let a = self.color.a;
            match self.case {
                Case::Lower => write!(f, "{a:02x}")?,
                Case::Upper => write!(f, "{a:02X}")?,
            }
        }

        Ok(())
    }
}

/// An option for whether or not to display the alpha channel when displaying a
/// hexadecimal color.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub enum Alpha {
    /// When displaying hexadecimal colors, do not display the alpha channel.
    #[default]
    Hidden,
    /// When displaying hexadecimal colors, display the alpha channel.
    Visible,
}

/// The case of the letters to use when displaying a hexadecimal color.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
pub enum Case {
    /// When displaying hexadecimal colors, use lowercase letters.
    Lower,
    /// When displaying hexadecimal colors, use uppercase letters.
    #[default]
    Upper,
}

////////////////////////////////////////////////////////////////////////////////
// Errors
////////////////////////////////////////////////////////////////////////////////

/// An error which can be returned when parsing a hex color.
///
/// # Potential causes
///
/// Among other causes, `ParseHexColorError` can be thrown because of leading
/// or trailing whitespace in the string e.g., when it is obtained from user
/// input. Using the [`str::trim()`] method ensures that no whitespace remains
/// before parsing.
#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ParseHexColorError {
    /// The input was empty.
    Empty,
    /// The string is of a malformed length or does not start with `#`.
    InvalidFormat,
    /// The format was presumably correct, but one of the digits wasn't
    /// hexadecimal.
    InvalidDigit,
}

impl fmt::Display for ParseHexColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data = match self {
            ParseHexColorError::Empty => "cannot parse hex color from empty string",
            ParseHexColorError::InvalidFormat => "invalid hexadecimal color format",
            ParseHexColorError::InvalidDigit => "invalid hexadecimal digit",
        };
        f.write_str(data)
    }
}

#[cfg(feature = "std")]
#[cfg_attr(doc_cfg, doc(cfg(feature = "std")))]
impl std::error::Error for ParseHexColorError {}
