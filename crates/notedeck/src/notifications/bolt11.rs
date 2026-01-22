//! BOLT11 invoice parsing for zap amount extraction.
//!
//! Extracts payment amounts from Lightning Network invoices embedded in zap receipts.

/// Parse amount from a BOLT11 invoice string.
///
/// BOLT11 format: `ln<prefix><amount><multiplier>1<data>`
/// - prefix: bc (mainnet), tb (testnet), bs (signet)
/// - amount: optional digits
/// - multiplier: optional m/u/n/p
/// - 1: separator (always present, and unique since bech32 data excludes '1')
/// - data: timestamp and tagged fields (bech32 encoded)
///
/// # Examples
/// - `lnbc1...` = no amount (1 is separator)
/// - `lnbc1000u1...` = 1000 micro-BTC = 100,000 sats
/// - `lnbc1m1...` = 1 milli-BTC = 100,000 sats
/// - `lnbc21...` = 2 whole BTC (rare, but valid)
///
/// # Returns
/// The amount in satoshis, or `None` if parsing fails or no amount specified.
pub fn parse_bolt11_amount(bolt11: &str) -> Option<i64> {
    let lower = bolt11.to_lowercase();

    // Find the amount portion after prefix (ln + 2-char network prefix)
    let after_prefix = lower
        .strip_prefix("lnbc")
        .or_else(|| lower.strip_prefix("lntb"))
        .or_else(|| lower.strip_prefix("lnbs"))?;

    // Find the bech32 separator '1' - it's always the LAST '1' in the string
    // because bech32 data encoding doesn't use the character '1'
    let separator_pos = after_prefix.rfind('1')?;

    // The human-readable amount part is everything before the separator
    let amount_part = &after_prefix[..separator_pos];

    if amount_part.is_empty() {
        // No amount specified (e.g., "lnbc1..." where '1' is immediately the separator)
        return None;
    }

    // Parse digits and optional multiplier from the amount part
    let chars: Vec<char> = amount_part.chars().collect();

    // Find where digits end
    let mut digit_end = 0;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            digit_end = i + 1;
        } else {
            break;
        }
    }

    if digit_end == 0 {
        // No digits found - invalid amount
        return None;
    }

    // Check for multiplier after digits
    let multiplier_char = if digit_end < chars.len() {
        let c = chars[digit_end];
        match c {
            'm' | 'u' | 'n' | 'p' => Some(c),
            _ => return None, // Invalid character after digits
        }
    } else {
        None // No multiplier - amount is in whole bitcoin
    };

    let amount_str: String = chars[..digit_end].iter().collect();
    let amount: i64 = amount_str.parse().ok()?;

    // Convert to millisatoshis based on multiplier, then to satoshis
    let msats = match multiplier_char {
        Some('m') => amount * 100_000_000, // milli-bitcoin = 0.001 BTC = 100,000 sats
        Some('u') => amount * 100_000,     // micro-bitcoin = 0.000001 BTC = 100 sats
        Some('n') => amount * 100,         // nano-bitcoin = 0.000000001 BTC = 0.1 sats
        Some('p') => amount / 10,          // pico-bitcoin = 0.000000000001 BTC
        None => amount * 100_000_000_000,  // whole bitcoin (rare in practice)
        Some(_) => unreachable!("multiplier already validated as m/u/n/p"),
    };

    // Convert millisatoshis to satoshis
    Some(msats / 1000)
}

/// Extract zap amount from a kind 9735 event's tags.
///
/// Looks for bolt11 tag and parses the invoice amount.
pub fn extract_zap_amount(event: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    let tags = event.get("tags")?.as_array()?;

    for tag in tags {
        let tag_arr = match tag.as_array() {
            Some(arr) => arr,
            None => continue,
        };
        if tag_arr.len() < 2 {
            continue;
        }
        let tag_name = match tag_arr[0].as_str() {
            Some(name) => name,
            None => continue,
        };
        if tag_name != "bolt11" {
            continue;
        }
        let bolt11 = match tag_arr[1].as_str() {
            Some(s) => s,
            None => continue,
        };
        return parse_bolt11_amount(bolt11);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bolt11_amount_parsing() {
        // Test micro-bitcoin (u) - 1000u = 100,000 sats
        assert_eq!(parse_bolt11_amount("lnbc1000u1pj9qrs"), Some(100_000));

        // Test milli-bitcoin (m) - 10m = 1,000,000 sats
        assert_eq!(parse_bolt11_amount("lnbc10m1pj9qrs"), Some(1_000_000));

        // Test nano-bitcoin (n) - 1000000n = 100,000 sats
        // 1 nano-BTC = 10^-9 BTC, so 1000000n = 10^-3 BTC = 100,000 sats
        assert_eq!(parse_bolt11_amount("lnbc1000000n1pj9qrs"), Some(100_000));

        // Test no-amount invoice (1 is separator, not amount)
        assert_eq!(parse_bolt11_amount("lnbc1pj9qrs"), None);

        // Test whole BTC with milli multiplier - 2000m = 2 BTC
        assert_eq!(parse_bolt11_amount("lnbc2000m1pj9qrs"), Some(200_000_000));

        // Test whole BTC without multiplier - uses rfind('1') to find separator
        // 2 whole BTC = 200,000,000 sats
        assert_eq!(parse_bolt11_amount("lnbc21pj9qrs"), Some(200_000_000));

        // Test whole BTC where amount contains '1' - the last '1' is the separator
        // "lnbc100001pj9qrs" -> amount = 10000 whole BTC = 1,000,000,000,000 sats
        assert_eq!(
            parse_bolt11_amount("lnbc100001pj9qrs"),
            Some(1_000_000_000_000)
        );

        // Test invalid prefix
        assert_eq!(parse_bolt11_amount("invalid"), None);

        // Test testnet prefix
        assert_eq!(parse_bolt11_amount("lntb1000u1pj9qrs"), Some(100_000));

        // Test signet prefix
        assert_eq!(parse_bolt11_amount("lnbs500u1pj9qrs"), Some(50_000));
    }
}
