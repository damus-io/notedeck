//! Zap amount extraction from BOLT11 invoices in notification events.
//!
//! Uses the `lightning_invoice` crate's `Bolt11Invoice` parser (the same
//! parser used by `crate::zaps::zap`) instead of custom parsing.

use lightning_invoice::Bolt11Invoice;

/// Extract zap amount in satoshis from a kind 9735 event's bolt11 tag.
///
/// Parses the BOLT11 invoice using `lightning_invoice::Bolt11Invoice` and
/// converts the amount from millisatoshis to satoshis.
///
/// # Returns
/// Amount in satoshis, or `None` if no bolt11 tag or no amount specified.
pub fn extract_zap_amount(event: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    let tags = event.get("tags")?.as_array()?;

    for tag in tags {
        let tag_arr = tag.as_array()?;
        if tag_arr.len() < 2 {
            continue;
        }
        if tag_arr[0].as_str() != Some("bolt11") {
            continue;
        }
        let bolt11 = tag_arr[1].as_str()?;
        return bolt11
            .parse::<Bolt11Invoice>()
            .ok()
            .and_then(|inv| inv.amount_milli_satoshis())
            .map(|msats| (msats / 1000) as i64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_zap_event(bolt11: &str) -> serde_json::Map<String, serde_json::Value> {
        let json = serde_json::json!({
            "tags": [["bolt11", bolt11]]
        });
        json.as_object().unwrap().clone()
    }

    #[test]
    fn test_extract_zap_amount_real_invoice() {
        // Real invoice from the zap.rs test data (330 sats)
        let bolt11 = "lnbc330n1pn7dlrrpp566sfk69zda849huwjw6wepw3uzxxp4mp9np54qx49ruw8cuv86ushp52te27l4jadsz0u76jvgsk5uekl04tujpjkt9cc7duu0jfzp9zdtscqzzsxqyz5vqsp5m3tzc7ryp5f9fv90v27uyrrd4qfmj5lrwv9rvmvum3v50kdph23s9qxpqysgqut2ssf0m7nmtd73cwqk7qfw4sw6zlj598sjdxmdsepmvn0ptamnhf45c425h26juzcfupegltefwsf8qav2ldell7v9fpc0y23nl0kgqtf432g";
        let event = make_zap_event(bolt11);
        // 330 nano-BTC = 33 sats
        assert_eq!(extract_zap_amount(&event), Some(33));
    }

    #[test]
    fn test_extract_zap_amount_no_bolt11_tag() {
        let json = serde_json::json!({ "tags": [["p", "abc123"]] });
        let event = json.as_object().unwrap();
        assert_eq!(extract_zap_amount(event), None);
    }

    #[test]
    fn test_extract_zap_amount_invalid_invoice() {
        let event = make_zap_event("not_a_real_invoice");
        assert_eq!(extract_zap_amount(&event), None);
    }
}
