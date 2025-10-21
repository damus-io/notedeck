// All of the casts are validated and the "error" sections for all of these
// functions is equally too nebulous while also self-evident.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc
)]

use core::fmt::{self, Write};

use arrayvec::ArrayString;
use serde::de::{Error, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{HexColor, ParseMode};

impl HexColor {
    fn to_rgb_string(self) -> ArrayString<7> {
        let mut string = ArrayString::new();
        unsafe { write!(string, "{}", self.display_rgb()).unwrap_unchecked() };
        string
    }

    fn to_rgba_string(self) -> ArrayString<9> {
        let mut string = ArrayString::new();
        unsafe { write!(string, "{}", self.display_rgba()).unwrap_unchecked() };
        string
    }
}

struct HexColorStringVisitor {
    mode: ParseMode,
}

impl HexColorStringVisitor {
    fn new(mode: ParseMode) -> Self {
        HexColorStringVisitor { mode }
    }
}

impl<'de> Visitor<'de> for HexColorStringVisitor {
    type Value = HexColor;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let message = match self.mode {
            ParseMode::Any => "an RGB(A) hexadecimal color",
            ParseMode::Rgb => "an RGB hexadecimal color",
            ParseMode::Rgba => "an RGBA hexadecimal color",
        };
        formatter.write_str(message)
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        HexColor::parse_internals(s, self.mode)
            .map_err(|_| E::invalid_value(Unexpected::Str(s), &self))
    }
}

enum NumberMode {
    U24,
    U32,
}

struct HexColorNumberVisitor {
    mode: NumberMode,
}

impl HexColorNumberVisitor {
    fn new(mode: NumberMode) -> Self {
        HexColorNumberVisitor { mode }
    }
}

impl<'de> Visitor<'de> for HexColorNumberVisitor {
    type Value = HexColor;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let message = match self.mode {
            NumberMode::U24 => "a value in the range 0x0000_0000..=0x00FF_FFFF",
            NumberMode::U32 => "a value in the range 0x0000_0000..=0xFFFF_FFFF",
        };
        formatter.write_str(message)
    }

    fn visit_i64<E>(self, n: i64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        if n < 0x0000_0000 {
            return Err(E::invalid_type(Unexpected::Signed(n), &self));
        }

        match self.mode {
            NumberMode::U24 if n <= 0x00FF_FFFF => Ok(HexColor::from_u24(n as u32)),
            NumberMode::U32 if n <= 0xFFFF_FFFF => Ok(HexColor::from_u32(n as u32)),
            _ => Err(E::invalid_value(Unexpected::Unsigned(n as u64), &self)),
        }
    }

    fn visit_u64<E>(self, n: u64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match self.mode {
            NumberMode::U24 if n <= 0x00FF_FFFF => Ok(HexColor::from_u24(n as u32)),
            NumberMode::U32 if n <= 0xFFFF_FFFF => Ok(HexColor::from_u32(n as u32)),
            _ => Err(E::invalid_value(Unexpected::Unsigned(n), &self)),
        }
    }
}

/// Deserialize and serialize [`HexColor`] values as RGB strings.
///
/// # Examples
///
/// ```
/// use hex_color::HexColor;
/// use serde::{Deserialize, Serialize};
/// use serde_json::json;
///
/// #[derive(Debug, PartialEq, Deserialize, Serialize)]
/// struct Color {
///     name: String,
///     #[serde(with = "hex_color::rgb")]
///     value: HexColor,
/// }
///
/// # fn main() -> serde_json::Result<()> {
/// let dodger_blue = json!({
///     "name": "Dodger Blue",
///     "value": "#1E90FF",
/// });
/// assert_eq!(
///     serde_json::from_value::<Color>(dodger_blue)?,
///     Color {
///         name: String::from("Dodger Blue"),
///         value: HexColor::rgb(30, 144, 255),
///     },
/// );
///
/// let tomato = Color {
///     name: String::from("Tomato"),
///     value: HexColor::rgb(255, 99, 71),
/// };
/// assert_eq!(
///     serde_json::to_value(tomato)?,
///     json!({
///         "name": "Tomato",
///         "value": "#FF6347",
///     }),
/// );
/// # Ok(())
/// # }
/// ```
#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
pub mod rgb {
    use serde::{Deserializer, Serializer};

    use super::HexColorStringVisitor;
    use crate::{HexColor, ParseMode};

    /// Deserializes a [`HexColor`] from a string using the same rules as
    /// [`HexColor::parse_rgb`].
    ///
    /// To strictly deserialize an RGBA value from a string, use
    /// [`rgba::deserialize`].
    ///
    /// [`rgba::deserialize`]: crate::rgba::deserialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Debug, PartialEq, Deserialize)]
    /// struct Color {
    ///     name: String,
    ///     #[serde(deserialize_with = "hex_color::rgb::deserialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> serde_json::Result<()> {
    /// let cadet_blue = json!({
    ///     "name": "Cadet Blue",
    ///     "value": "#5F9EA0",
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(cadet_blue)?,
    ///     Color {
    ///         name: String::from("Cadet Blue"),
    ///         value: HexColor::rgb(95, 158, 160),
    ///     },
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HexColor, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(HexColorStringVisitor::new(ParseMode::Rgb))
    }

    /// Serializes a [`HexColor`] as a string in the format `#RRGGBB`.
    ///
    /// To serialize a [`HexColor`] as a string in the format `#RRGGBBAA`, use
    /// [`rgba::serialize`]
    ///
    /// [`rgba::serialize`]: crate::rgba::serialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Serialize;
    /// use serde_json::json;
    ///
    /// #[derive(Serialize)]
    /// struct Color {
    ///     name: &'static str,
    ///     #[serde(serialize_with = "hex_color::rgb::serialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let mint_cream = Color {
    ///     name: "Mint Cream",
    ///     value: HexColor::rgb(245,255,250),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(mint_cream)?,
    ///     json!({
    ///         "name": "Mint Cream",
    ///         "value": "#F5FFFA",
    ///     }),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn serialize<S>(color: &HexColor, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&color.to_rgb_string())
    }
}

/// Deserialize and serialize [`HexColor`] values as RGBA strings.
///
/// # Examples
///
/// ```
/// use hex_color::HexColor;
/// use serde::{Deserialize, Serialize};
/// use serde_json::json;
///
/// #[derive(Debug, PartialEq, Deserialize, Serialize)]
/// struct Color {
///     name: String,
///     #[serde(with = "hex_color::rgba")]
///     value: HexColor,
/// }
///
/// # fn main() -> serde_json::Result<()> {
/// let transparent_ivory = json!({
///     "name": "Transparent Ivory",
///     "value": "#FFFFF080",
/// });
/// assert_eq!(
///     serde_json::from_value::<Color>(transparent_ivory)?,
///     Color {
///         name: String::from("Transparent Ivory"),
///         value: HexColor::rgba(255, 255, 240, 128),
///     },
/// );
///
/// let medium_purple = Color {
///     name: String::from("Medium Purple"),
///     value: HexColor::rgb(147, 112, 219),
/// };
/// assert_eq!(
///     serde_json::to_value(medium_purple)?,
///     json!({
///         "name": "Medium Purple",
///         "value": "#9370DBFF",
///     }),
/// );
/// # Ok(())
/// # }
/// ```
#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
pub mod rgba {
    use serde::{Deserializer, Serializer};

    use super::HexColorStringVisitor;
    use crate::{HexColor, ParseMode};

    /// Deserializes a [`HexColor`] from a string using the same rules as
    /// [`HexColor::parse_rgba`].
    ///
    /// To strictly deserialize an RGB value from a string, use
    /// [`rgb::deserialize`].
    ///
    /// [`rgb::deserialize`]: crate::rgb::deserialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Debug, PartialEq, Deserialize)]
    /// struct Color {
    ///     name: String,
    ///     #[serde(deserialize_with = "hex_color::rgba::deserialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> serde_json::Result<()> {
    /// let lavender = json!({
    ///     "name": "Lavender",
    ///     "value": "#E6E6FAFF",
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(lavender)?,
    ///     Color {
    ///         name: String::from("Lavender"),
    ///         value: HexColor::rgb(230, 230, 250),
    ///     },
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HexColor, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(HexColorStringVisitor::new(ParseMode::Rgba))
    }

    /// Serializes a [`HexColor`] as a string in the format `#RRGGBBAA`.
    ///
    /// To serialize a [`HexColor`] as a string in the format `#RRGGBB`, use
    /// [`rgb::serialize`]
    ///
    /// [`rgb::serialize`]: crate::rgb::serialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Serialize;
    /// use serde_json::json;
    ///
    /// #[derive(Serialize)]
    /// struct Color {
    ///     name: &'static str,
    ///     #[serde(serialize_with = "hex_color::rgba::serialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let transparent_bisque = Color {
    ///     name: "Transparent Bisque",
    ///     value: HexColor::rgba(255, 228, 196, 128),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(transparent_bisque)?,
    ///     json!({
    ///         "name": "Transparent Bisque",
    ///         "value": "#FFE4C480",
    ///     }),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn serialize<S>(color: &HexColor, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&color.to_rgba_string())
    }
}

/// Deserialize and serialize [`HexColor`] values as `u32` values by truncating
/// the alpha byte.
///
/// # Examples
///
/// ```
/// use hex_color::HexColor;
/// use serde::{Deserialize, Serialize};
/// use serde_json::json;
///
/// #[derive(Debug, PartialEq, Deserialize, Serialize)]
/// struct Color {
///     name: String,
///     #[serde(with = "hex_color::u24")]
///     value: HexColor,
/// }
///
/// # fn main() -> serde_json::Result<()> {
/// let light_sky_blue = json!({
///     "name": "Light Sky Blue",
///     "value": 8_900_346_u32,
/// });
/// assert_eq!(
///     serde_json::from_value::<Color>(light_sky_blue)?,
///     Color {
///         name: String::from("Light Sky Blue"),
///         value: HexColor::from_u24(0x0087_CEFA),
///     },
/// );
///
/// let pale_violet_red = Color {
///     name: String::from("Pale Violet Red"),
///     value: HexColor::from_u24(0x00DB_7093),
/// };
/// assert_eq!(
///     serde_json::to_value(pale_violet_red)?,
///     json!({
///         "name": "Pale Violet Red",
///         "value": 14_381_203_u32,
///     }),
/// );
/// # Ok(())
/// # }
/// ```
#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
pub mod u24 {
    use serde::{Deserializer, Serializer};

    use super::{HexColorNumberVisitor, NumberMode};
    use crate::HexColor;

    /// Deserializes a [`HexColor`] from a `u32` in the range
    /// `0x0000_0000..=0x00FF_FFFF`.
    ///
    /// To deserialize a [`HexColor`] from a `u32` with the alpha component
    /// included, use [`u32::deserialize`]
    ///
    /// [`u32::deserialize`]: crate::u32::deserialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Debug, PartialEq, Deserialize)]
    /// struct Color {
    ///     name: String,
    ///     #[serde(deserialize_with = "hex_color::u24::deserialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> serde_json::Result<()> {
    /// let crimson = json!({
    ///     "name": "Crimson",
    ///     "value": 14_423_100_u32,
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(crimson)?,
    ///     Color {
    ///         name: String::from("Crimson"),
    ///         value: HexColor::from_u24(0x00DC_143C),
    ///     },
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HexColor, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_u64(HexColorNumberVisitor::new(NumberMode::U24))
    }

    /// Serializes a [`HexColor`] as a `u32` in the range
    /// `0x0000_0000..=0x00FF_FFFF`, truncating the alpha component.
    ///
    /// To serialize a [`HexColor`] as a `u32` with the alpha component
    /// included, use [`u32::serialize`].
    ///
    /// [`u32::serialize`]: crate::u32::serialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Serialize;
    /// use serde_json::json;
    ///
    /// #[derive(Serialize)]
    /// struct Color {
    ///     name: &'static str,
    ///     #[serde(serialize_with = "hex_color::u24::serialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let cornsilk = Color {
    ///     name: "Cornsilk",
    ///     value: HexColor::from_u24(0x00FF_F8DC),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(cornsilk)?,
    ///     json!({
    ///         "name": "Cornsilk",
    ///         "value": 16_775_388_u32,
    ///     }),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn serialize<S>(color: &HexColor, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_u32(color.to_u24())
    }
}

/// Deserialize and serialize [`HexColor`] values as `u32` values.
///
/// # Examples
///
/// ```
/// use hex_color::HexColor;
/// use serde::{Deserialize, Serialize};
/// use serde_json::json;
///
/// #[derive(Debug, PartialEq, Deserialize, Serialize)]
/// struct Color {
///     name: String,
///     #[serde(with = "hex_color::u32")]
///     value: HexColor,
/// }
///
/// # fn main() -> serde_json::Result<()> {
/// let sea_shell = json!({
///     "name": "Sea Shell",
///     "value": 4_294_307_583_u32,
/// });
/// assert_eq!(
///     serde_json::from_value::<Color>(sea_shell)?,
///     Color {
///         name: String::from("Sea Shell"),
///         value: HexColor::from_u32(0xFFF5_EEFF),
///     },
/// );
///
/// let spring_green = Color {
///     name: String::from("Spring Green"),
///     value: HexColor::from_u32(0x00FF_7FFF),
/// };
/// assert_eq!(
///     serde_json::to_value(spring_green)?,
///     json!({
///         "name": "Spring Green",
///         "value": 16_744_447_u32,
///     }),
/// );
/// # Ok(())
/// # }
/// ```
#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
pub mod u32 {
    use serde::{Deserializer, Serializer};

    use super::{HexColorNumberVisitor, NumberMode};
    use crate::HexColor;

    /// Deserializes a [`HexColor`] from a `u32`.
    ///
    /// To deserialize a value in the range `0x0000_0000..=0x00FF_FFFF` as only
    /// an RGB color, use [`u24::deserialize`] instead.
    ///
    /// [`u24::deserialize`]: crate::u24::deserialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Debug, PartialEq, Deserialize)]
    /// struct Color {
    ///     name: String,
    ///     #[serde(deserialize_with = "hex_color::u32::deserialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> serde_json::Result<()> {
    /// let transparent_moccasin = json!({
    ///     "name": "Transparent Moccasin",
    ///     "value": 4_293_178_752_u32,
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(transparent_moccasin)?,
    ///     Color {
    ///         name: String::from("Transparent Moccasin"),
    ///         value: HexColor::from_u32(0xFFE4_B580),
    ///     },
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HexColor, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_u64(HexColorNumberVisitor::new(NumberMode::U32))
    }

    /// Serializes a [`HexColor`] as a `u32`.
    ///
    /// To serialize only the red, green, and blue components, use
    /// [`u24::serialize`] instead.
    ///
    /// [`u24::serialize`]: crate::u24::serialize
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Serialize;
    /// use serde_json::json;
    ///
    /// #[derive(Serialize)]
    /// struct Color {
    ///     name: &'static str,
    ///     #[serde(serialize_with = "hex_color::u32::serialize")]
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let linen = Color {
    ///     name: "Linen",
    ///     value: HexColor::from_u32(0xFAF0_E6FF),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(linen)?,
    ///     json!({
    ///         "name": "Linen",
    ///         "value": 4_210_091_775_u32,
    ///     }),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn serialize<S>(color: &HexColor, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_u32(color.to_u32())
    }
}

#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
impl<'de> Deserialize<'de> for HexColor {
    /// Deserializes a `HexColor` from a string using the same rules as
    /// [`HexColor::parse`].
    ///
    /// To strictly deserialize either an RGB or RGBA value from a string, use
    /// [`rgb::deserialize`] or [`rgba::deserialize`] respectively.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Deserialize;
    /// use serde_json::json;
    ///
    /// #[derive(Debug, PartialEq, Deserialize)]
    /// struct Color {
    ///     name: String,
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let saddle_brown = json!({
    ///     "name": "Saddle Brown",
    ///     "value": "#8B4513",
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(saddle_brown)?,
    ///     Color {
    ///         name: String::from("Saddle Brown"),
    ///         value: HexColor::rgb(139, 69, 19),
    ///     },
    /// );
    ///
    /// let transparent_cyan = json!({
    ///     "name": "Transparent Cyan",
    ///     "value": "#00FFFF80",
    /// });
    /// assert_eq!(
    ///     serde_json::from_value::<Color>(transparent_cyan)?,
    ///     Color {
    ///         name: String::from("Transparent Cyan"),
    ///         value: HexColor::rgba(0, 255, 255, 128),
    ///     },
    /// );
    /// # Ok(())
    /// # }
    /// ```
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(HexColorStringVisitor::new(ParseMode::Any))
    }
}

#[cfg_attr(doc_cfg, doc(cfg(feature = "serde")))]
impl Serialize for HexColor {
    /// Serializes the `HexColor` as a string.
    ///
    /// By default, `HexColor` values get serialized in the form of `#RRGGBB`
    /// strings when the alpha value is set to [`u8::MAX`], or completely
    /// opaque. However, if any transparency exists, they are serialized in
    /// the form of `#RRGGBBAA` strings.
    ///
    /// To strictly enforce getting serialized as an RGB or RGBA string, use
    /// [`rgb::serialize`] or [`rgba::serialize`] respectively. In fact, using
    /// either of these options is highly suggested for more normalized results.
    ///
    /// # Examples
    ///
    /// ```
    /// use hex_color::HexColor;
    /// use serde::Serialize;
    /// use serde_json::json;
    ///
    /// #[derive(Serialize)]
    /// struct Color {
    ///     name: &'static str,
    ///     value: HexColor,
    /// }
    ///
    /// # fn main() -> Result<(), serde_json::Error> {
    /// let orange = Color {
    ///     name: "Orange",
    ///     value: HexColor::rgb(255, 165, 0),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(orange)?,
    ///     json!({
    ///         "name": "Orange",
    ///         "value": "#FFA500",
    ///     }),
    /// );
    ///
    /// let transparent_navy = Color {
    ///     name: "Transparent Navy",
    ///     value: HexColor::rgba(0, 0, 128, 128),
    /// };
    /// assert_eq!(
    ///     serde_json::to_value(transparent_navy)?,
    ///     json!({
    ///         "name": "Transparent Navy",
    ///         "value": "#00008080",
    ///     }),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.a == u8::MAX {
            serializer.serialize_str(&self.to_rgb_string())
        } else {
            serializer.serialize_str(&self.to_rgba_string())
        }
    }
}
