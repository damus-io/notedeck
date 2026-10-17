//! The commodity table: names, decimal precision and display style.

use std::collections::HashMap;

use crate::amount::{AmountError, AmountText};

/// The id of a commodity in a [`Commodities`] table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommodityId(u16);

impl CommodityId {
    /// The id's position in its table, `0` for the first commodity seen.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Which side of the number a commodity is written on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// Before the number, as in `$5.00`.
    Prefix,
    /// After the number, as in `5.00 CAD`.
    Suffix,
}

/// How a commodity is written next to its number.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Style {
    /// Which side of the number the commodity goes on.
    pub side: Side,
    /// Whether a space separates the commodity from the number.
    pub spaced: bool,
}

impl Style {
    /// `5.00 CAD`: after the number, with a space. ledger-cli's default for a
    /// commodity it has not seen written.
    pub const SUFFIX_SPACED: Style = Style {
        side: Side::Suffix,
        spaced: true,
    };
}

/// The most decimal places a commodity may have: `10^18` is the largest
/// power of ten that fits in an `i64`.
pub const MAX_PRECISION: u8 = 18;

/// One commodity's entry in the table.
#[derive(Debug, Clone)]
struct Commodity {
    name: String,
    precision: u8,
    style: Style,
    /// Set once any amount has been converted at `precision`; from then on
    /// the precision cannot grow, because amounts already stored as integers
    /// would silently change value.
    frozen: bool,
}

/// Every commodity a ledger uses, with what was learned about each from the
/// amounts it was written in.
///
/// Amounts are stored as integers in a commodity's smallest unit, so a
/// commodity's precision has to be known before its first amount is
/// converted. [`observe`](Self::observe) learns precision and style from
/// written amounts (ledger-cli's rule: the style of the first one, the most
/// decimal places of any) and [`amount`](Self::amount) converts. Once a
/// commodity has converted an amount its precision is frozen, and observing
/// a finer amount is an error rather than a rescale; a reader that sees its
/// amounts in one pass should observe them all before converting any, or
/// [`declare`](Self::declare) the precision up front.
#[derive(Debug, Clone, Default)]
pub struct Commodities {
    list: Vec<Commodity>,
    by_name: HashMap<String, CommodityId>,
}

impl Commodities {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of commodity `name`, adding it with precision 0 and the
    /// default style if it is new. An empty name is the null commodity of a
    /// bare number.
    pub fn intern(&mut self, name: &str) -> Result<CommodityId, AmountError> {
        if let Some(id) = self.get(name) {
            return Ok(id);
        }
        let id = CommodityId(
            u16::try_from(self.list.len()).map_err(|_| AmountError::TooManyCommodities)?,
        );
        self.list.push(Commodity {
            name: name.to_owned(),
            precision: 0,
            style: Style::SUFFIX_SPACED,
            frozen: false,
        });
        self.by_name.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Add or update commodity `name` with a known precision and style,
    /// overriding anything learned. Fails if the commodity has already
    /// converted amounts at a different precision.
    pub fn declare(
        &mut self,
        name: &str,
        precision: u8,
        style: Style,
    ) -> Result<CommodityId, AmountError> {
        if precision > MAX_PRECISION {
            return Err(AmountError::PrecisionTooLarge(precision));
        }
        let id = self.intern(name)?;
        let entry = &mut self.list[id.index()];
        if entry.frozen && entry.precision != precision {
            return Err(AmountError::PrecisionFrozen {
                precision: entry.precision,
                wanted: precision,
            });
        }
        entry.precision = precision;
        entry.style = style;
        Ok(id)
    }

    /// Learn from one written amount: intern its commodity, take its style if
    /// this is the commodity's first observation, and raise the precision to
    /// the amount's decimal places.
    pub fn observe(&mut self, text: &AmountText<'_>) -> Result<CommodityId, AmountError> {
        let decimals = text.decimals();
        if decimals > MAX_PRECISION {
            return Err(AmountError::PrecisionTooLarge(decimals));
        }
        let fresh = self.get(text.commodity).is_none();
        let id = self.intern(text.commodity)?;
        let entry = &mut self.list[id.index()];
        if fresh {
            entry.style = text.style;
        }
        if decimals > entry.precision {
            if entry.frozen {
                return Err(AmountError::PrecisionFrozen {
                    precision: entry.precision,
                    wanted: decimals,
                });
            }
            entry.precision = decimals;
        }
        Ok(id)
    }

    /// Learn only the style of a written amount: intern its commodity and take
    /// its style if the commodity is new, leaving the precision alone. For
    /// amounts kept at their own scale rather than the commodity's, such as
    /// prices and costs, which ledger-cli does not let change a commodity's
    /// display precision either.
    pub fn observe_style(&mut self, text: &AmountText<'_>) -> Result<CommodityId, AmountError> {
        let fresh = self.get(text.commodity).is_none();
        let id = self.intern(text.commodity)?;
        if fresh {
            self.list[id.index()].style = text.style;
        }
        Ok(id)
    }

    /// Convert a written amount to an integer at its commodity's precision,
    /// freezing that precision. The commodity must already be in the table
    /// (see [`observe`](Self::observe)).
    pub fn amount(&mut self, text: &AmountText<'_>) -> Result<crate::Amount, AmountError> {
        let id = self
            .get(text.commodity)
            .ok_or(AmountError::UnknownCommodity)?;
        let entry = &mut self.list[id.index()];
        let qty = text.qty_at(entry.precision)?;
        entry.frozen = true;
        Ok(crate::Amount { qty, commodity: id })
    }

    /// Set the commodity's precision whether or not it is frozen, freezing
    /// it. The caller rescales every amount already stored at the old
    /// precision; the ledger does this when a segment brings a finer one.
    pub(crate) fn set_precision(&mut self, id: CommodityId, precision: u8) {
        debug_assert!(precision <= MAX_PRECISION);
        let entry = &mut self.list[id.index()];
        entry.precision = precision;
        entry.frozen = true;
    }

    /// Set the commodity's display style.
    pub(crate) fn set_style(&mut self, id: CommodityId, style: Style) {
        self.list[id.index()].style = style;
    }

    /// The id of commodity `name`, if the table has it.
    pub fn get(&self, name: &str) -> Option<CommodityId> {
        self.by_name.get(name).copied()
    }

    /// The commodity's name as written, without quotes.
    pub fn name(&self, id: CommodityId) -> &str {
        &self.list[id.index()].name
    }

    /// How many decimal places the commodity's smallest unit is.
    pub fn precision(&self, id: CommodityId) -> u8 {
        self.list[id.index()].precision
    }

    /// How the commodity is written next to a number.
    pub fn style(&self, id: CommodityId) -> Style {
        self.list[id.index()].style
    }

    /// How many commodities the table holds.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether the table holds no commodities.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Every commodity id, in the order they were first seen.
    pub fn ids(&self) -> impl Iterator<Item = CommodityId> + use<> {
        (0..self.list.len() as u16).map(CommodityId)
    }
}
