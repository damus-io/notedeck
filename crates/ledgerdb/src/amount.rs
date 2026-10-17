//! Exact decimal amounts stored as integers.

use std::fmt;

use crate::commodity::{Commodities, CommodityId, Side, Style};

/// A quantity of one commodity, as an integer in the commodity's smallest
/// unit: `5.25 CAD` at precision 2 is `qty: 525`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Amount {
    /// The quantity in the commodity's smallest unit.
    pub qty: i64,
    /// The commodity it is a quantity of.
    pub commodity: CommodityId,
}

/// Why an amount could not be parsed or converted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// There is no number in the text.
    #[error("expected a number")]
    MissingNumber,
    /// The commodity is malformed, such as an unterminated quote.
    #[error("malformed commodity")]
    BadCommodity,
    /// More than one sign, as in `--5` or `-$-5`.
    #[error("more than one sign")]
    DoubleSign,
    /// Text follows the amount.
    #[error("unexpected text after the amount")]
    Trailing,
    /// The amount has more decimal places than its commodity's precision, so
    /// storing it would round.
    #[error("{decimals} decimal places is finer than the commodity's precision of {precision}")]
    PrecisionOverflow {
        /// The decimal places the amount was written with.
        decimals: u8,
        /// The commodity's precision.
        precision: u8,
    },
    /// More decimal places than an `i64` can scale to.
    #[error("{0} decimal places is more than the maximum of 18")]
    PrecisionTooLarge(u8),
    /// The commodity has already converted amounts at its precision, so the
    /// precision can no longer change.
    #[error("the commodity's precision is fixed at {precision} and cannot become {wanted}")]
    PrecisionFrozen {
        /// The fixed precision.
        precision: u8,
        /// The precision that was asked for.
        wanted: u8,
    },
    /// The quantity does not fit in an `i64` at the commodity's precision.
    #[error("the amount is too large")]
    Overflow,
    /// The commodity is not in the table; observe or declare it first.
    #[error("unknown commodity")]
    UnknownCommodity,
    /// The table already holds `u16::MAX` commodities.
    #[error("too many commodities")]
    TooManyCommodities,
}

/// A written amount split into its parts, borrowing the text it came from.
///
/// This is the step between text and [`Amount`]: it carries the decimal
/// places and style a [`Commodities`] table learns from, before anything is
/// converted to an integer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AmountText<'a> {
    /// Whether the amount is negative.
    pub negative: bool,
    /// The digits before the decimal point, possibly empty (`.5`).
    pub integer: &'a str,
    /// The digits after the decimal point, possibly empty.
    pub fraction: &'a str,
    /// The commodity name without quotes; empty for a bare number.
    pub commodity: &'a str,
    /// Where and how the commodity was written.
    pub style: Style,
}

impl<'a> AmountText<'a> {
    /// Split an amount such as `5.25 CAD`, `$-5`, `-$5.00`, `100 msat`,
    /// `3 "ACME 2024"` or a bare `12.5`. Surrounding whitespace is ignored.
    pub fn parse(text: &'a str) -> Result<Self, AmountError> {
        let mut rest = text.trim();
        let mut negative = take_sign(&mut rest);
        if rest.is_empty() {
            return Err(AmountError::MissingNumber);
        }
        if negative.is_some() && rest.starts_with(['-', '+']) {
            return Err(AmountError::DoubleSign);
        }

        let mut prefix = None;
        if !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            let name = take_commodity(&mut rest)?;
            let trimmed = rest.trim_start();
            let spaced = trimmed.len() != rest.len();
            rest = trimmed;
            prefix = Some(CommodityToken { name, spaced });
            let signed = take_sign(&mut rest);
            if negative.is_some() && signed.is_some() {
                return Err(AmountError::DoubleSign);
            }
            negative = negative.or(signed);
        }

        let integer = take_digits(&mut rest);
        let fraction = match rest.strip_prefix('.') {
            Some(after) => {
                rest = after;
                take_digits(&mut rest)
            }
            None => "",
        };
        if integer.is_empty() && fraction.is_empty() {
            return Err(AmountError::MissingNumber);
        }

        let mut commodity = "";
        let mut style = Style::SUFFIX_SPACED;
        if let Some(token) = prefix {
            commodity = token.name;
            style = Style {
                side: Side::Prefix,
                spaced: token.spaced,
            };
        } else {
            let trimmed = rest.trim_start();
            let spaced = trimmed.len() != rest.len();
            rest = trimmed;
            if !rest.is_empty() {
                commodity = take_commodity(&mut rest)?;
                style = Style {
                    side: Side::Suffix,
                    spaced,
                };
            }
        }
        if !rest.is_empty() {
            return Err(AmountError::Trailing);
        }

        Ok(AmountText {
            negative: negative.unwrap_or(false),
            integer,
            fraction,
            commodity,
            style,
        })
    }

    /// The decimal places the amount was written with, saturating at 255.
    pub fn decimals(&self) -> u8 {
        u8::try_from(self.fraction.len()).unwrap_or(u8::MAX)
    }

    /// The quantity as an integer with `precision` decimal places. Fails
    /// rather than rounds if the amount is written more finely than that.
    pub fn qty_at(&self, precision: u8) -> Result<i64, AmountError> {
        let decimals = self.decimals();
        if decimals > precision {
            return Err(AmountError::PrecisionOverflow {
                decimals,
                precision,
            });
        }
        let mut magnitude: i128 = 0;
        for digit in self.integer.bytes().chain(self.fraction.bytes()) {
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|m| m.checked_add(i128::from(digit - b'0')))
                .ok_or(AmountError::Overflow)?;
        }
        for _ in decimals..precision {
            magnitude = magnitude.checked_mul(10).ok_or(AmountError::Overflow)?;
        }
        let signed = if self.negative { -magnitude } else { magnitude };
        i64::try_from(signed).map_err(|_| AmountError::Overflow)
    }
}

impl Amount {
    /// Parse a written amount, learning its commodity's style and precision
    /// into `commodities` and converting at that precision. See
    /// [`Commodities`] for when precision is fixed.
    pub fn parse(text: &str, commodities: &mut Commodities) -> Result<Self, AmountError> {
        let text = AmountText::parse(text)?;
        commodities.observe(&text)?;
        commodities.amount(&text)
    }

    /// The amount written in its commodity's style and full precision, so
    /// that parsing the result gives the same amount back.
    pub fn display(self, commodities: &Commodities) -> AmountDisplay<'_> {
        AmountDisplay {
            amount: self,
            commodities,
        }
    }
}

/// An [`Amount`] ready to format, from [`Amount::display`].
pub struct AmountDisplay<'a> {
    amount: Amount,
    commodities: &'a Commodities,
}

/// Writes ledger-cli style: `$-5.00`, `-5.00 CAD`, `100 msat`.
impl fmt::Display for AmountDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = self.amount.commodity;
        write_styled(
            f,
            self.amount.qty,
            self.commodities.precision(id),
            self.commodities.name(id),
            self.commodities.style(id),
        )
    }
}

/// Write `qty / 10^scale` beside commodity `name` in `style`, with exactly
/// `scale` decimal places and the commodity quoted when it has to be, so
/// that parsing the text gives the same number back.
pub(crate) fn write_styled(
    f: &mut fmt::Formatter<'_>,
    qty: i64,
    scale: u8,
    name: &str,
    style: Style,
) -> fmt::Result {
    let sign = if qty < 0 { "-" } else { "" };
    let sep = if style.spaced && !name.is_empty() {
        " "
    } else {
        ""
    };
    let quote = if needs_quotes(name) { "\"" } else { "" };

    let magnitude = qty.unsigned_abs();
    let unit = 10u64.pow(u32::from(scale));
    let whole = magnitude / unit;
    let part = magnitude % unit;

    if style.side == Side::Prefix {
        write!(f, "{quote}{name}{quote}{sep}")?;
    }
    write!(f, "{sign}{whole}")?;
    if scale > 0 {
        write!(f, ".{part:0width$}", width = usize::from(scale))?;
    }
    if style.side == Side::Suffix {
        write!(f, "{sep}{quote}{name}{quote}")?;
    }
    Ok(())
}

/// A commodity read off the front of the text.
struct CommodityToken<'a> {
    name: &'a str,
    spaced: bool,
}

/// Characters that cannot appear in an unquoted commodity (ledger-cli's set).
const RESERVED: &str = ".,;:?!-+*/^&|=<>{}[]()@\"";

/// Whether `c` may appear in an unquoted commodity name.
fn is_symbol_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_ascii_digit() && !RESERVED.contains(c)
}

/// Whether `name` must be quoted to parse back as one commodity.
pub(crate) fn needs_quotes(name: &str) -> bool {
    name.chars().any(|c| !is_symbol_char(c))
}

/// Consume a leading `-` or `+`: `Some(true)` for negative, `Some(false)` for
/// an explicit `+`, `None` when there is no sign.
fn take_sign(rest: &mut &str) -> Option<bool> {
    let sign = match rest.chars().next() {
        Some('-') => true,
        Some('+') => false,
        _ => return None,
    };
    *rest = &rest[1..];
    Some(sign)
}

/// Consume a run of ASCII digits, possibly empty.
fn take_digits<'a>(rest: &mut &'a str) -> &'a str {
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    *rest = &rest[end..];
    digits
}

/// Consume a commodity: `"quoted name"` or a run of symbol characters.
pub(crate) fn take_commodity<'a>(rest: &mut &'a str) -> Result<&'a str, AmountError> {
    if let Some(quoted) = rest.strip_prefix('"') {
        let close = quoted.find('"').ok_or(AmountError::BadCommodity)?;
        if close == 0 {
            return Err(AmountError::BadCommodity);
        }
        *rest = &quoted[close + 1..];
        return Ok(&quoted[..close]);
    }
    let end = rest
        .find(|c: char| !is_symbol_char(c))
        .unwrap_or(rest.len());
    if end == 0 {
        return Err(AmountError::BadCommodity);
    }
    let name = &rest[..end];
    *rest = &rest[end..];
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `text` into a fresh table and format it back.
    fn round_trip(text: &str) -> String {
        let mut table = Commodities::new();
        let amount = Amount::parse(text, &mut table).unwrap();
        amount.display(&table).to_string()
    }

    #[test]
    fn canonical_text_round_trips() {
        for text in [
            "5.25 CAD",
            "-5.25 CAD",
            "0.07 CAD",
            "-0.07 CAD",
            "100 msat",
            "-100 msat",
            "0 msat",
            "$5.00",
            "$-5.00",
            "$ 12.50",
            "1.00000000 BTC",
            "-0.00000001 BTC",
            "12.5",
            "-3",
            "3 \"ACME 2024\"",
            "9223372036854775807 msat",
            "-9223372036854775808 msat",
            "0.000000000000000001 X",
        ] {
            assert_eq!(round_trip(text), text);
        }
    }

    #[test]
    fn alternate_spellings_normalise() {
        assert_eq!(round_trip("-$5.00"), "$-5.00");
        assert_eq!(round_trip("+5.00 CAD"), "5.00 CAD");
        assert_eq!(round_trip("  5.00CAD  "), "5.00CAD");
        assert_eq!(round_trip(".5 EUR"), "0.5 EUR");
        assert_eq!(round_trip("5. EUR"), "5 EUR");
    }

    #[test]
    fn quantities_are_scaled_to_precision() {
        let mut table = Commodities::new();
        let cad = table.declare("CAD", 2, Style::SUFFIX_SPACED).unwrap();
        let five = Amount::parse("5 CAD", &mut table).unwrap();
        assert_eq!(
            five,
            Amount {
                qty: 500,
                commodity: cad
            }
        );
        assert_eq!(Amount::parse("-5.2 CAD", &mut table).unwrap().qty, -520);
        assert_eq!(five.display(&table).to_string(), "5.00 CAD");
    }

    #[test]
    fn every_quantity_round_trips_at_every_precision() {
        let samples = [
            0,
            1,
            -1,
            7,
            -10,
            99,
            100,
            123_456_789,
            -987_654_321_012,
            i64::MAX,
            i64::MIN,
        ];
        for precision in 0..=crate::commodity::MAX_PRECISION {
            let mut table = Commodities::new();
            let id = table.declare("X", precision, Style::SUFFIX_SPACED).unwrap();
            for qty in samples {
                let amount = Amount { qty, commodity: id };
                let text = amount.display(&table).to_string();
                assert_eq!(Amount::parse(&text, &mut table), Ok(amount), "{text}");
            }
        }
    }

    #[test]
    fn finer_than_precision_is_an_error_not_a_rounding() {
        let mut table = Commodities::new();
        table.declare("CAD", 2, Style::SUFFIX_SPACED).unwrap();
        Amount::parse("1.00 CAD", &mut table).unwrap();
        assert_eq!(
            Amount::parse("1.005 CAD", &mut table),
            Err(AmountError::PrecisionFrozen {
                precision: 2,
                wanted: 3
            })
        );
        let text = AmountText::parse("1.005 CAD").unwrap();
        assert_eq!(
            text.qty_at(2),
            Err(AmountError::PrecisionOverflow {
                decimals: 3,
                precision: 2
            })
        );
    }

    #[test]
    fn precision_is_learned_until_the_first_conversion() {
        let mut table = Commodities::new();
        let a = AmountText::parse("5 USD").unwrap();
        let b = AmountText::parse("2.125 USD").unwrap();
        let usd = table.observe(&a).unwrap();
        table.observe(&b).unwrap();
        assert_eq!(table.precision(usd), 3);
        assert_eq!(table.amount(&a).unwrap().qty, 5_000);
        assert_eq!(table.amount(&b).unwrap().qty, 2_125);
        assert!(table.declare("USD", 4, Style::SUFFIX_SPACED).is_err());
    }

    #[test]
    fn first_observation_sets_the_style() {
        let mut table = Commodities::new();
        let usd = Amount::parse("$1.00", &mut table).unwrap();
        Amount::parse("2.00 $", &mut table).unwrap();
        assert_eq!(usd.display(&table).to_string(), "$1.00");
    }

    #[test]
    fn overflow_is_an_error() {
        let mut table = Commodities::new();
        assert_eq!(
            Amount::parse("9223372036854775808 msat", &mut table),
            Err(AmountError::Overflow)
        );
        table.declare("CAD", 2, Style::SUFFIX_SPACED).unwrap();
        assert_eq!(
            Amount::parse("92233720368547758.08 CAD", &mut table),
            Err(AmountError::Overflow)
        );
        assert_eq!(
            Amount::parse(&format!("{} CAD", "9".repeat(60)), &mut table),
            Err(AmountError::Overflow)
        );
        assert_eq!(
            Amount::parse("0.0000000000000000001 Y", &mut table),
            Err(AmountError::PrecisionTooLarge(19))
        );
    }

    #[test]
    fn malformed_text_is_rejected() {
        let cases = [
            ("", AmountError::MissingNumber),
            ("CAD", AmountError::MissingNumber),
            (".", AmountError::MissingNumber),
            ("--5", AmountError::DoubleSign),
            ("-$-5", AmountError::DoubleSign),
            ("5 CAD x", AmountError::Trailing),
            ("5 CAD;", AmountError::Trailing),
            ("5 \"CAD", AmountError::BadCommodity),
            ("5 \"\"", AmountError::BadCommodity),
            ("5 @", AmountError::BadCommodity),
        ];
        for (text, err) in cases {
            assert_eq!(AmountText::parse(text), Err(err), "{text:?}");
        }
    }
}
