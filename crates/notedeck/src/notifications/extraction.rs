//! Event extraction from relay messages.
//!
//! Parses raw JSON relay messages and extracts structured event data
//! for notification processing.

use super::bolt11::extract_zap_amount;
use super::types::ExtractedEvent;
use tracing::warn;

/// Extract all event fields from JSON using proper JSON parsing.
///
/// Note: enostr RelayMessage::Event passes the ENTIRE relay message `["EVENT", "sub_id", {...}]`
/// not just the event object, so we need to extract the third element.
///
/// # Arguments
/// * `relay_message` - Raw JSON string from relay (either full EVENT message or just event object)
///
/// # Returns
/// * `Some(ExtractedEvent)` - Successfully parsed event with all fields
/// * `None` - Failed to parse or missing required fields
#[profiling::function]
pub fn extract_event(relay_message: &str) -> Option<ExtractedEvent> {
    // Use serde_json for robust parsing that handles escaped strings correctly
    let value: serde_json::Value = match serde_json::from_str(relay_message) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse relay message JSON: {}", e);
            return None;
        }
    };

    // The relay message is ["EVENT", "sub_id", {event}] - extract the event object (index 2)
    let obj = if let Some(arr) = value.as_array() {
        // This is the expected format from enostr: ["EVENT", "sub_id", {event}]
        if arr.len() < 3 {
            warn!("EVENT message array too short: {} elements", arr.len());
            return None;
        }
        match arr[2].as_object() {
            Some(o) => o,
            None => {
                warn!("Third element of EVENT message is not an object");
                return None;
            }
        }
    } else if let Some(o) = value.as_object() {
        // Direct event object (shouldn't happen with enostr but handle it anyway)
        o
    } else {
        warn!("Relay message is neither array nor object");
        return None;
    };

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let kind = obj.get("kind").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let pubkey = obj
        .get("pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = obj
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Validate id is proper hex (64 hex chars = 32 bytes)
    if id.len() != 64 {
        warn!(
            "Dropping event with invalid id length {}: {}",
            id.len(),
            id.get(..16).unwrap_or(&id)
        );
        return None;
    }
    if hex::decode(&id).is_err() {
        warn!(
            "Dropping event with non-hex id: {}",
            id.get(..16).unwrap_or(&id)
        );
        return None;
    }

    // Validate pubkey is proper hex (64 hex chars = 32 bytes)
    if pubkey.len() != 64 {
        warn!(
            "Dropping event with invalid pubkey length {}: {}",
            pubkey.len(),
            pubkey.get(..16).unwrap_or(&pubkey)
        );
        return None;
    }
    if hex::decode(&pubkey).is_err() {
        warn!(
            "Dropping event with non-hex pubkey: {}",
            pubkey.get(..16).unwrap_or(&pubkey)
        );
        return None;
    }

    // Extract p-tags for event attribution
    let p_tags = extract_p_tags(obj);

    // Extract zap amount for kind 9735 (zap receipt) events
    let zap_amount_sats = if kind == 9735 {
        extract_zap_amount(obj)
    } else {
        None
    };

    // Serialize just the event object for broadcast (not the full relay message)
    let raw_json = serde_json::to_string(obj).unwrap_or_default();

    Some(ExtractedEvent {
        id,
        kind,
        pubkey,
        content,
        p_tags,
        zap_amount_sats,
        raw_json,
    })
}

/// Extract all p-tag pubkeys from an event's tags array.
///
/// P-tags (`["p", "<pubkey>", ...]`) indicate which pubkeys an event references.
/// Used for event attribution to determine which account(s) a notification targets.
pub fn extract_p_tags(event: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let Some(tags) = event.get("tags").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut p_tags = Vec::new();
    for tag in tags {
        let Some(tag_arr) = tag.as_array() else {
            continue;
        };
        if tag_arr.len() < 2 {
            continue;
        }
        let Some(tag_name) = tag_arr[0].as_str() else {
            continue;
        };
        if tag_name != "p" {
            continue;
        }
        let Some(pubkey) = tag_arr[1].as_str() else {
            continue;
        };
        // Validate pubkey is 64-char hex
        if pubkey.len() == 64 {
            p_tags.push(pubkey.to_string());
        }
    }
    p_tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_event() {
        // Note: enostr passes full relay message ["EVENT", "sub_id", {...}], not just event object
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"hello world"}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(
            event.id,
            "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234"
        );
        assert_eq!(
            event.pubkey,
            "def0123456789012345678901234567890123456789012345678901234567890"
        );
        assert_eq!(event.kind, 1);
        assert_eq!(event.content, "hello world");
        assert_eq!(event.zap_amount_sats, None);
    }

    #[test]
    fn test_extract_event_with_braces_in_content() {
        // This would break manual brace-matching but works with serde_json
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"json example: {\"foo\": \"bar\"}"}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.content, r#"json example: {"foo": "bar"}"#);
    }

    #[test]
    fn test_extract_event_empty_content() {
        let relay_msg = r#"["EVENT","sub_id",{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":7,"content":""}]"#;
        let event = extract_event(relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 7);
        assert_eq!(event.content, "");
    }

    #[test]
    fn test_extract_event_direct_object() {
        // Also handle direct event object format (fallback case)
        let json = r#"{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":1,"content":"hello"}"#;
        let event = extract_event(json);
        assert!(event.is_some());
        assert_eq!(event.unwrap().kind, 1);
    }

    #[test]
    fn test_extract_zap_event_with_amount() {
        // Real BOLT11 invoice from zap.rs test data (330 nano-BTC = 33 sats)
        let bolt11 = "lnbc330n1pn7dlrrpp566sfk69zda849huwjw6wepw3uzxxp4mp9np54qx49ruw8cuv86ushp52te27l4jadsz0u76jvgsk5uekl04tujpjkt9cc7duu0jfzp9zdtscqzzsxqyz5vqsp5m3tzc7ryp5f9fv90v27uyrrd4qfmj5lrwv9rvmvum3v50kdph23s9qxpqysgqut2ssf0m7nmtd73cwqk7qfw4sw6zlj598sjdxmdsepmvn0ptamnhf45c425h26juzcfupegltefwsf8qav2ldell7v9fpc0y23nl0kgqtf432g";
        let relay_msg = format!(
            r#"["EVENT","sub_id",{{"id":"abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234","pubkey":"def0123456789012345678901234567890123456789012345678901234567890","kind":9735,"content":"","tags":[["bolt11","{}"]]}}]"#,
            bolt11
        );
        let event = extract_event(&relay_msg);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, 9735);
        assert_eq!(event.zap_amount_sats, Some(33)); // 330 nano-BTC = 33 sats
    }
}
