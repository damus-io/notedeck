use nostr::{PublicKey, Tag, Timestamp, UnsignedEvent};
use nostr_double_ratchet::{Error, Result};
use notedeck::DOUBLE_RATCHET_SIG_PREFIX;

/// Build an unsigned "inner" rumor event compatible with `nostr-double-ratchet` / iris-chat.
///
/// This mirrors the event normalization done in `nostr_double_ratchet::SessionManager`:
/// - ensures a recipient `p` tag is present
/// - ensures an `ms` tag exists so the rumor id is stable
/// - ensures the `id` matches the final content/tags
pub(crate) fn build_inner_rumor_event(
    owner_pubkey: PublicKey,
    recipient: PublicKey,
    kind: u16,
    content: String,
    mut tags: Vec<Tag>,
    created_at_s: u64,
    created_at_ms: u128,
) -> Result<UnsignedEvent> {
    let recipient_hex = hex::encode(recipient.to_bytes());
    let has_recipient_p_tag = tags.iter().any(|t| {
        let v = t.as_slice();
        v.first().map(|s| s.as_str()) == Some("p")
            && v.get(1).map(|s| s.as_str()) == Some(recipient_hex.as_str())
    });

    if !has_recipient_p_tag {
        tags.insert(
            0,
            Tag::parse(&["p".to_string(), recipient_hex])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        );
    }

    let has_ms_tag = tags.iter().any(|t| {
        let v = t.as_slice();
        v.first().map(|s| s.as_str()) == Some("ms")
    });
    if !has_ms_tag {
        tags.push(
            Tag::parse(&["ms".to_string(), created_at_ms.to_string()])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        );
    }

    let kind = nostr::Kind::from(kind);
    let mut rumor = nostr::EventBuilder::new(kind, &content)
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at_s))
        .build(owner_pubkey);

    rumor.ensure_id();
    Ok(rumor)
}

/// Convert an unsigned double-ratchet inner rumor into an `nostrdb`-ingestible event JSON string.
///
/// `nostrdb` requires a `sig` field in the note JSON and verifies signatures by default. For
/// double-ratchet inner rumors we store a marker signature (prefixed by
/// [`DOUBLE_RATCHET_SIG_PREFIX`]) and rely on Notedeck's `nostrdb` ingest filter to skip signature
/// validation for these local-only events.
pub(crate) fn unsigned_event_to_ndb_json(
    mut rumor: UnsignedEvent,
    outer_event_id_hex: Option<&str>,
) -> Result<String> {
    // Recompute the id regardless of whatever came over the wire; we only persist local, derived
    // rumors and want the id to match the final (pubkey/tags/content) payload.
    rumor.id = None;
    rumor.ensure_id();

    let mut v = serde_json::to_value(&rumor)
        .map_err(|e| Error::InvalidEvent(format!("serialize unsigned rumor: {e}")))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| Error::InvalidEvent("rumor json is not an object".to_string()))?;

    let mut sig = [0u8; 64];
    sig[..DOUBLE_RATCHET_SIG_PREFIX.len()].copy_from_slice(&DOUBLE_RATCHET_SIG_PREFIX);

    if let Some(hex_id) = outer_event_id_hex {
        if let Ok(id_bytes) = hex::decode(hex_id) {
            if id_bytes.len() == 32 {
                sig[32..].copy_from_slice(&id_bytes);
            }
        }
    }

    obj.insert(
        "sig".to_string(),
        serde_json::Value::String(hex::encode(sig)),
    );

    serde_json::to_string(&v).map_err(|e| Error::InvalidEvent(format!("serialize rumor json: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    #[test]
    fn build_inner_rumor_event_ensures_p_and_ms_tags_and_stable_id() {
        let owner_keys = Keys::generate();
        let recipient_keys = Keys::generate();

        let owner_pubkey = owner_keys.public_key();
        let recipient_pubkey = recipient_keys.public_key();

        let created_at_s = 1_700_000_000;
        let created_at_ms = 1_700_000_000_123u128;

        let rumor1 = build_inner_rumor_event(
            owner_pubkey,
            recipient_pubkey,
            u16::try_from(nostr_double_ratchet::CHAT_MESSAGE_KIND)
                .expect("CHAT_MESSAGE_KIND fits in nostr::Kind"),
            "hello".to_string(),
            Vec::new(),
            created_at_s,
            created_at_ms,
        )
        .expect("build rumor");

        let rumor2 = build_inner_rumor_event(
            owner_pubkey,
            recipient_pubkey,
            u16::try_from(nostr_double_ratchet::CHAT_MESSAGE_KIND)
                .expect("CHAT_MESSAGE_KIND fits in nostr::Kind"),
            "hello".to_string(),
            Vec::new(),
            created_at_s,
            created_at_ms,
        )
        .expect("build rumor again");

        assert_eq!(rumor1.id, rumor2.id, "rumor id should be stable");

        let tags: Vec<Vec<String>> = rumor1.tags.iter().map(|t| t.clone().to_vec()).collect();
        assert!(
            tags.iter()
                .any(|t| t.first().map(String::as_str) == Some("p")),
            "expected rumor to contain a p tag"
        );
        assert!(
            tags.iter()
                .any(|t| t.first().map(String::as_str) == Some("ms")),
            "expected rumor to contain an ms tag"
        );
    }

    #[test]
    fn unsigned_event_to_ndb_json_includes_marker_sig() {
        let owner_keys = Keys::generate();
        let recipient_keys = Keys::generate();

        let rumor = build_inner_rumor_event(
            owner_keys.public_key(),
            recipient_keys.public_key(),
            u16::try_from(nostr_double_ratchet::CHAT_MESSAGE_KIND)
                .expect("CHAT_MESSAGE_KIND fits in nostr::Kind"),
            "hello".to_string(),
            Vec::new(),
            1_700_000_000,
            1_700_000_000_123u128,
        )
        .expect("build rumor");

        let json = unsigned_event_to_ndb_json(rumor, None).expect("to ndb json");
        let v: serde_json::Value = serde_json::from_str(&json).expect("json parse");

        let sig = v["sig"].as_str().expect("sig str");
        assert_eq!(sig.len(), 128, "sig should be 64 bytes hex-encoded");
        assert!(
            sig.starts_with(&hex::encode(DOUBLE_RATCHET_SIG_PREFIX)),
            "sig should start with marker prefix"
        );
    }
}
