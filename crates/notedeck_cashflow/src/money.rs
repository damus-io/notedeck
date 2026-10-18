//! How the views write an amount: in full, as the ledger writes it, or
//! compact for a chart's axis.
//!
//! ledgerdb's `Amount` holds an `i64`, but every query sums into an `i128`,
//! so these take the sum and a commodity rather than an `Amount`.

use ledgerdb::{Amount, Commodities, CommodityId};

/// `qty` smallest units of `commodity`, in the ledger's style for it:
/// `1800.00 CAD`.
pub fn money(qty: i128, commodity: CommodityId, commodities: &Commodities) -> String {
    match i64::try_from(qty) {
        Ok(qty) => Amount { qty, commodity }.display(commodities).to_string(),
        Err(_) => format!("{qty} units of {}", commodities.name(commodity)),
    }
}

/// `qty` smallest units of a commodity with `precision` decimals, in whole
/// units with a thousands or millions suffix and no commodity: `4.2k`,
/// `-850`, `1.25M`. For an axis, where the chart says the commodity once.
pub fn compact(qty: f64, precision: u8) -> String {
    let units = qty / 10f64.powi(i32::from(precision));
    let magnitude = units.abs();
    let (scaled, suffix) = if magnitude >= 1_000_000.0 {
        (units / 1_000_000.0, "M")
    } else if magnitude >= 1_000.0 {
        (units / 1_000.0, "k")
    } else {
        (units, "")
    };
    // Two significant decimals at most, and none that are zero.
    let text = format!("{scaled:.2}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    // A value that rounds to zero from below prints as `0`, never `-0`.
    if text == "-0" {
        return format!("0{suffix}");
    }
    format!("{text}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cents become whole units, with a suffix past a thousand.
    #[test]
    fn compact_scales_and_trims() {
        assert_eq!(compact(420_000.0, 2), "4.2k");
        assert_eq!(compact(-85_000.0, 2), "-850");
        assert_eq!(compact(125_000_000.0, 2), "1.25M");
        assert_eq!(compact(0.0, 2), "0");
        assert_eq!(compact(-0.0, 2), "0");
        assert_eq!(compact(1_000.0, 0), "1k");
        assert_eq!(compact(1_234.0, 2), "12.34");
    }
}
